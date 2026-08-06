use crate::server::common::upload::{build_studio, execute_postprocessor, submit_to_bilibili};
use crate::server::core::downloader::SegmentInfo;
use crate::server::errors::{AppError, AppResult};
use crate::server::infrastructure::context::Context;
use crate::server::infrastructure::models::hook_step::{HookStep, process_video_paths};
use crate::server::infrastructure::models::upload_streamer::UploadStreamer;
use async_channel::Receiver;
use biliup::bilibili::{BiliBili, ResponseData, Vid, Video};
use biliup::client::StatelessClient;
use biliup::credential::login_by_cookies;
use biliup::uploader::line::{Line, Probe};
use biliup::uploader::{VideoFile, line};
use error_stack::ResultExt;
use futures::StreamExt;
use sqlx::{Row, Sqlite, Transaction};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::sync::{Mutex, Semaphore};
use tracing::{error, info, warn};

const VERIFY_ATTEMPTS: usize = 30;
const VERIFY_INTERVAL_SECONDS: u64 = 10;
const RETRY_DELAYS: [u64; 5] = [60, 300, 900, 1800, 3600];

#[derive(Debug, Clone)]
struct SessionRecord {
    id: i64,
    aid: Option<u64>,
    bvid: Option<String>,
    status: String,
}

#[derive(Debug, Clone)]
struct SegmentRecord {
    id: i64,
    session_id: i64,
    part_number: i64,
    file_path: PathBuf,
    processed_file_path: Option<PathBuf>,
    danmaku_file_path: Option<PathBuf>,
    retry_count: i64,
}

struct UploadRuntime {
    bilibili: BiliBili,
    line: Line,
    threads: usize,
    client: StatelessClient,
}

fn active_sessions() -> &'static Mutex<HashSet<i64>> {
    static ACTIVE: OnceLock<Mutex<HashSet<i64>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

fn upload_slots(size: usize) -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(size.max(1))))
        .clone()
}

/// 接收一个直播场次产生的所有完整分段。
///
/// 分段登记与上传完全解耦：登记循环只写 SQLite，上传失败或长时间重试不会阻塞录制。
/// 同一场次的上传工作器严格按照 part_number 串行处理，保证分P顺序稳定。
pub async fn process_session(
    rx: Receiver<SegmentInfo>,
    ctx: Context,
    upload_config: UploadStreamer,
) {
    let session = match ensure_session(&ctx).await {
        Ok(session) => session,
        Err(e) => {
            error!(error = ?e, url = ctx.live_streamer().url, "failed to create replay session");
            return;
        }
    };

    spawn_queue_worker(session.id, ctx.clone(), upload_config).await;

    while let Ok(event) = rx.recv().await {
        if let Err(e) = register_segment(&ctx, session.id, &event).await {
            error!(
                error = ?e,
                file = ?event.prev_file_path,
                "failed to persist completed segment"
            );
        }
    }

    if let Err(e) = mark_recording_complete(ctx.pool(), session.id).await {
        error!(error = ?e, session_id = session.id, "failed to close replay session");
    }
}

async fn spawn_queue_worker(session_id: i64, ctx: Context, upload_config: UploadStreamer) {
    {
        let mut active = active_sessions().lock().await;
        if !active.insert(session_id) {
            return;
        }
    }

    tokio::spawn(async move {
        if let Err(e) = run_queue_worker(session_id, &ctx, &upload_config).await {
            error!(error = ?e, session_id, "replay upload worker stopped");
        }
        active_sessions().lock().await.remove(&session_id);
    });
}

async fn run_queue_worker(
    session_id: i64,
    ctx: &Context,
    upload_config: &UploadStreamer,
) -> AppResult<()> {
    let runtime = initialize_upload_runtime(ctx, upload_config).await?;
    let processors: Vec<HookStep> = ctx
        .live_streamer()
        .segment_processor
        .clone()
        .unwrap_or_default();

    loop {
        if let Some(segment) = next_ready_segment(ctx.pool(), session_id).await? {
            let permit = upload_slots(ctx.config().pool2_size as usize)
                .acquire_owned()
                .await
                .change_context(AppError::Custom("upload semaphore closed".to_string()))?;

            let result = process_segment(ctx, upload_config, &runtime, &processors, &segment).await;
            drop(permit);

            if let Err(e) = result {
                let attempt = segment.retry_count.saturating_add(1);
                let delay = retry_delay(attempt as usize);
                mark_retry(ctx.pool(), &segment, attempt, delay, &format!("{e:?}")).await?;
                warn!(
                    error = ?e,
                    session_id,
                    part = segment.part_number,
                    retry_in_seconds = delay,
                    "segment upload failed; recording remains unaffected"
                );
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            }
            continue;
        }

        if session_can_finish(ctx.pool(), session_id).await? {
            mark_session_complete(ctx.pool(), session_id).await?;
            info!(session_id, "Live Replay session completed");
            return Ok(());
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

async fn process_segment(
    ctx: &Context,
    upload_config: &UploadStreamer,
    runtime: &UploadRuntime,
    processors: &[HookStep],
    segment: &SegmentRecord,
) -> AppResult<()> {
    mark_uploading(ctx.pool(), segment.id).await?;
    let mut session = load_session(ctx.pool(), segment.session_id).await?;

    // 幂等恢复：如果上次 edit/submit 已成功但本地状态尚未来得及提交，先检查远端分P数量。
    if let Some(aid) = session.aid
        && remote_part_count(&runtime.bilibili, aid).await.unwrap_or(0)
            >= segment.part_number as usize
    {
        verify_and_delete(ctx, &runtime.bilibili, segment, aid, prepared_paths(segment)).await?;
        return Ok(());
    }

    let paths = prepare_paths(ctx.pool(), segment, processors).await?;
    let upload_path = paths
        .first()
        .cloned()
        .ok_or_else(|| AppError::Custom("segment has no upload path".to_string()))?;

    if !upload_path.exists() {
        return Err(AppError::Custom(format!(
            "local segment is missing: {}",
            upload_path.display()
        ))
        .into());
    }

    let mut video = upload_single_file(&upload_path, runtime).await?;
    let part_title = upload_path
        .file_stem()
        .and_then(|value| value.to_str())
        .map(|value| format!("P{} {}", segment.part_number, value))
        .unwrap_or_else(|| format!("P{}", segment.part_number));
    video.title = Some(Video::truncate_title(&part_title, 80));

    let aid = if let Some(aid) = session.aid {
        append_video(
            &runtime.bilibili,
            aid,
            session.bvid.as_deref(),
            segment.part_number,
            video,
            ctx.config().submit_api.as_deref(),
        )
        .await?;
        aid
    } else {
        let mut recorder = ctx.recorder(ctx.streamer_info().clone());
        recorder.filename_prefix = upload_config.title.clone();
        let studio = build_studio(upload_config, &runtime.bilibili, vec![video], &recorder).await?;
        let response = submit_to_bilibili(
            &runtime.bilibili,
            &studio,
            ctx.config().submit_api.as_deref(),
        )
        .await?;
        let (aid, bvid) = extract_remote_ids(&response)?;
        save_remote_ids(ctx.pool(), segment.session_id, aid, bvid.as_deref()).await?;
        session.aid = Some(aid);
        session.bvid = bvid;
        aid
    };

    mark_uploaded(ctx.pool(), segment.id, &upload_path).await?;
    verify_and_delete(ctx, &runtime.bilibili, segment, aid, paths).await
}

async fn append_video(
    bilibili: &BiliBili,
    aid: u64,
    bvid: Option<&str>,
    part_number: i64,
    video: Video,
    submit_api: Option<&str>,
) -> AppResult<()> {
    let vid = bvid
        .filter(|value| !value.is_empty())
        .map(|value| Vid::Bvid(value.to_string()))
        .unwrap_or(Vid::Aid(aid));
    let mut studio = bilibili
        .studio_data(&vid, None)
        .await
        .change_context(AppError::Unknown)?;

    if studio.videos.len() >= part_number as usize {
        return Ok(());
    }
    if studio.videos.len() + 1 != part_number as usize {
        return Err(AppError::Custom(format!(
            "remote part order mismatch: remote={}, expected part={part_number}",
            studio.videos.len()
        ))
        .into());
    }

    studio.aid = Some(aid);
    studio.videos.push(video);
    if submit_api.is_some_and(|value| value.eq_ignore_ascii_case("web")) {
        bilibili
            .edit_by_web(&studio)
            .await
            .change_context(AppError::Unknown)?;
    } else {
        bilibili
            .edit_by_app(&studio, None)
            .await
            .change_context(AppError::Unknown)?;
    }
    Ok(())
}

async fn verify_and_delete(
    ctx: &Context,
    bilibili: &BiliBili,
    segment: &SegmentRecord,
    aid: u64,
    paths: Vec<PathBuf>,
) -> AppResult<()> {
    verify_remote_part(bilibili, aid, segment.part_number as usize).await?;
    mark_verified(ctx.pool(), segment).await?;

    // 兼容用户自定义后处理；无论是否配置，远端验证通过后都执行 Live Replay 的安全删除。
    execute_postprocessor(paths.clone(), ctx).await?;
    safe_delete_paths(segment, &paths).await?;
    mark_deleted(ctx.pool(), segment).await?;
    Ok(())
}

async fn verify_remote_part(bilibili: &BiliBili, aid: u64, part_number: usize) -> AppResult<()> {
    for _ in 0..VERIFY_ATTEMPTS {
        match bilibili.video_data(&Vid::Aid(aid), None).await {
            Ok(data) => {
                if let Some(videos) = data.get("videos").and_then(|value| value.as_array())
                    && let Some(part) = videos.get(part_number.saturating_sub(1))
                {
                    let duration = part.get("duration").and_then(|value| value.as_u64()).unwrap_or(0);
                    let filename_ready = part
                        .get("filename")
                        .and_then(|value| value.as_str())
                        .is_some_and(|value| !value.is_empty());
                    if duration > 0 && filename_ready {
                        return Ok(());
                    }
                }
            }
            Err(e) => warn!(error = ?e, aid, part_number, "remote verification not ready"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(VERIFY_INTERVAL_SECONDS)).await;
    }

    Err(AppError::Custom(format!(
        "remote part verification timed out: aid={aid}, part={part_number}"
    ))
    .into())
}

async fn remote_part_count(bilibili: &BiliBili, aid: u64) -> AppResult<usize> {
    let studio = bilibili
        .studio_data(&Vid::Aid(aid), None)
        .await
        .change_context(AppError::Unknown)?;
    Ok(studio.videos.len())
}

async fn initialize_upload_runtime(
    ctx: &Context,
    upload_config: &UploadStreamer,
) -> AppResult<UploadRuntime> {
    let cookie_file = upload_config
        .user_cookie
        .clone()
        .unwrap_or_else(|| "cookies.json".to_string());
    let bilibili = login_by_cookies(&cookie_file, None)
        .await
        .change_context(AppError::Unknown)?;
    let client = ctx.stateless_client().clone();
    let line = get_upload_line(&client.client, &ctx.config().lines).await;

    Ok(UploadRuntime {
        bilibili,
        line,
        threads: ctx.config().threads as usize,
        client,
    })
}

async fn get_upload_line(client: &reqwest::Client, selected: &str) -> Line {
    match selected {
        "bda2" => line::bda2(),
        "bda" => line::bda(),
        "tx" => line::tx(),
        "txa" => line::txa(),
        "bldsa" => line::bldsa(),
        "alia" => line::alia(),
        _ => Probe::probe(client).await.unwrap_or_default(),
    }
}

async fn upload_single_file(file_path: &Path, runtime: &UploadRuntime) -> AppResult<Video> {
    info!(file = ?file_path, line = ?runtime.line, "starting Live Replay upload");
    let video_file = VideoFile::new(file_path).change_context(AppError::Unknown)?;
    let total_size = video_file.total_size;
    let file_name = video_file.file_name.clone();
    let uploader = runtime
        .line
        .pre_upload(&runtime.bilibili, video_file)
        .await
        .change_context(AppError::Unknown)?;
    let started = Instant::now();
    let video = uploader
        .upload(runtime.client.clone(), runtime.threads, |stream| {
            stream.map(|chunk| {
                let chunk = chunk?;
                let len = chunk.len();
                Ok((chunk, len))
            })
        })
        .await
        .change_context(AppError::Unknown)?;
    let elapsed_ms = started.elapsed().as_millis().max(1);
    info!(
        file = file_name,
        seconds = elapsed_ms as f64 / 1000.0,
        mb_per_second = total_size as f64 / 1000.0 / elapsed_ms as f64,
        "Live Replay upload completed"
    );
    Ok(video)
}

fn extract_remote_ids(response: &ResponseData) -> AppResult<(u64, Option<String>)> {
    let data = response
        .data
        .as_ref()
        .ok_or_else(|| AppError::Custom(format!("submit response has no data: {response}")))?;
    let aid = data
        .get("aid")
        .and_then(|value| value.as_u64())
        .or_else(|| data.pointer("/archive/aid").and_then(|value| value.as_u64()))
        .ok_or_else(|| AppError::Custom(format!("submit response has no aid: {response}")))?;
    let bvid = data
        .get("bvid")
        .and_then(|value| value.as_str())
        .or_else(|| data.pointer("/archive/bvid").and_then(|value| value.as_str()))
        .map(str::to_string);
    Ok((aid, bvid))
}

async fn ensure_session(ctx: &Context) -> AppResult<SessionRecord> {
    if let Some(row) = sqlx::query(
        "SELECT id, aid, bvid, status FROM live_sessions \
         WHERE live_streamer_id = ? AND ended_at IS NULL \
           AND status IN ('recording', 'retrying') \
           AND updated_at >= datetime('now', '-24 hours') \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(ctx.worker_id())
    .fetch_optional(ctx.pool())
    .await
    .change_context(AppError::Unknown)?
    {
        return session_from_row(&row);
    }

    let result = sqlx::query(
        "INSERT INTO live_sessions \
         (live_streamer_id, source_streamer_info_id, streamer_name, streamer_url, live_title, started_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(ctx.worker_id())
    .bind(ctx.id())
    .bind(&ctx.streamer_info().name)
    .bind(&ctx.streamer_info().url)
    .bind(&ctx.streamer_info().title)
    .bind(ctx.streamer_info().date.to_rfc3339())
    .execute(ctx.pool())
    .await
    .change_context(AppError::Unknown)?;

    load_session(ctx.pool(), result.last_insert_rowid()).await
}

async fn register_segment(
    ctx: &Context,
    session_id: i64,
    event: &SegmentInfo,
) -> AppResult<SegmentRecord> {
    let file_path = event.prev_file_path.display().to_string();
    let danmaku = event
        .danmaku_file_path
        .as_ref()
        .map(|path| path.display().to_string());
    let file_size = std::fs::metadata(&event.prev_file_path)
        .map(|metadata| metadata.len() as i64)
        .unwrap_or(0);
    let mut tx = ctx.pool().begin().await.change_context(AppError::Unknown)?;

    if let Some(row) = sqlx::query(
        "SELECT id, session_id, part_number, file_path, processed_file_path, \
                danmaku_file_path, retry_count \
         FROM recording_segments WHERE file_path = ? LIMIT 1",
    )
    .bind(&file_path)
    .fetch_optional(&mut *tx)
    .await
    .change_context(AppError::Unknown)?
    {
        tx.commit().await.change_context(AppError::Unknown)?;
        return segment_from_row(&row);
    }

    let part_number: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(part_number), 0) + 1 FROM recording_segments WHERE session_id = ?",
    )
    .bind(session_id)
    .fetch_one(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;

    let result = sqlx::query(
        "INSERT INTO recording_segments \
         (session_id, part_number, file_path, danmaku_file_path, file_size) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind(part_number)
    .bind(&file_path)
    .bind(&danmaku)
    .bind(file_size)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    let segment_id = result.last_insert_rowid();

    sqlx::query("INSERT INTO upload_jobs (segment_id) VALUES (?)")
        .bind(segment_id)
        .execute(&mut *tx)
        .await
        .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE live_sessions SET expected_parts = MAX(expected_parts, ?), \
         status = 'recording', updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(part_number)
    .bind(session_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "INSERT INTO filelist (file, streamer_info_id) \
         SELECT ?, ? WHERE NOT EXISTS (SELECT 1 FROM filelist WHERE file = ?)",
    )
    .bind(&file_path)
    .bind(ctx.id())
    .bind(&file_path)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;

    tx.commit().await.change_context(AppError::Unknown)?;
    load_segment(ctx.pool(), segment_id).await
}

async fn next_ready_segment(
    pool: &sqlx::Pool<Sqlite>,
    session_id: i64,
) -> AppResult<Option<SegmentRecord>> {
    let row = sqlx::query(
        "SELECT id, session_id, part_number, file_path, processed_file_path, \
                danmaku_file_path, retry_count \
         FROM recording_segments \
         WHERE id = ( \
             SELECT id FROM recording_segments \
             WHERE session_id = ? AND status NOT IN ('verified', 'deleted') \
             ORDER BY part_number LIMIT 1 \
         ) \
         AND (next_retry_at IS NULL OR next_retry_at <= CURRENT_TIMESTAMP)",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await
    .change_context(AppError::Unknown)?;
    row.as_ref().map(segment_from_row).transpose()
}

async fn prepare_paths(
    pool: &sqlx::Pool<Sqlite>,
    segment: &SegmentRecord,
    processors: &[HookStep],
) -> AppResult<Vec<PathBuf>> {
    if segment.processed_file_path.is_some() {
        return Ok(prepared_paths(segment));
    }

    let mut paths = prepared_paths(segment);
    if !processors.is_empty() {
        process_video_paths(&mut paths, processors).await?;
    }
    let processed = paths.first().cloned().unwrap_or_else(|| segment.file_path.clone());
    sqlx::query(
        "UPDATE recording_segments SET processed_file_path = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(processed.display().to_string())
    .bind(segment.id)
    .execute(pool)
    .await
    .change_context(AppError::Unknown)?;
    Ok(paths)
}

fn prepared_paths(segment: &SegmentRecord) -> Vec<PathBuf> {
    let mut paths = vec![
        segment
            .processed_file_path
            .clone()
            .unwrap_or_else(|| segment.file_path.clone()),
    ];
    if let Some(path) = &segment.danmaku_file_path {
        paths.push(path.clone());
    }
    paths
}

async fn safe_delete_paths(segment: &SegmentRecord, paths: &[PathBuf]) -> AppResult<()> {
    let mut unique = HashSet::new();
    unique.insert(segment.file_path.clone());
    if let Some(path) = &segment.processed_file_path {
        unique.insert(path.clone());
    }
    if let Some(path) = &segment.danmaku_file_path {
        unique.insert(path.clone());
    }
    unique.extend(paths.iter().cloned());

    for path in unique {
        match tokio::fs::remove_file(&path).await {
            Ok(()) => info!(file = ?path, "deleted verified local segment"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(AppError::Custom(format!(
                    "failed to delete verified file {}: {e}",
                    path.display()
                ))
                .into());
            }
        }
    }
    Ok(())
}

async fn load_session(pool: &sqlx::Pool<Sqlite>, id: i64) -> AppResult<SessionRecord> {
    let row = sqlx::query("SELECT id, aid, bvid, status FROM live_sessions WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .change_context(AppError::Unknown)?;
    session_from_row(&row)
}

fn session_from_row(row: &sqlx::sqlite::SqliteRow) -> AppResult<SessionRecord> {
    Ok(SessionRecord {
        id: row.try_get("id").change_context(AppError::Unknown)?,
        aid: row
            .try_get::<Option<i64>, _>("aid")
            .change_context(AppError::Unknown)?
            .map(|value| value as u64),
        bvid: row.try_get("bvid").change_context(AppError::Unknown)?,
        status: row.try_get("status").change_context(AppError::Unknown)?,
    })
}

async fn load_segment(pool: &sqlx::Pool<Sqlite>, id: i64) -> AppResult<SegmentRecord> {
    let row = sqlx::query(
        "SELECT id, session_id, part_number, file_path, processed_file_path, \
                danmaku_file_path, retry_count FROM recording_segments WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .change_context(AppError::Unknown)?;
    segment_from_row(&row)
}

fn segment_from_row(row: &sqlx::sqlite::SqliteRow) -> AppResult<SegmentRecord> {
    Ok(SegmentRecord {
        id: row.try_get("id").change_context(AppError::Unknown)?,
        session_id: row.try_get("session_id").change_context(AppError::Unknown)?,
        part_number: row.try_get("part_number").change_context(AppError::Unknown)?,
        file_path: PathBuf::from(
            row.try_get::<String, _>("file_path")
                .change_context(AppError::Unknown)?,
        ),
        processed_file_path: row
            .try_get::<Option<String>, _>("processed_file_path")
            .change_context(AppError::Unknown)?
            .map(PathBuf::from),
        danmaku_file_path: row
            .try_get::<Option<String>, _>("danmaku_file_path")
            .change_context(AppError::Unknown)?
            .map(PathBuf::from),
        retry_count: row.try_get("retry_count").change_context(AppError::Unknown)?,
    })
}

async fn save_remote_ids(
    pool: &sqlx::Pool<Sqlite>,
    session_id: i64,
    aid: u64,
    bvid: Option<&str>,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE live_sessions SET aid = ?, bvid = ?, status = 'uploading', \
         last_error = NULL, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(aid as i64)
    .bind(bvid)
    .bind(session_id)
    .execute(pool)
    .await
    .change_context(AppError::Unknown)?;
    Ok(())
}

async fn mark_uploading(pool: &sqlx::Pool<Sqlite>, segment_id: i64) -> AppResult<()> {
    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE recording_segments SET status = 'uploading', retry_count = retry_count + 1, \
         last_error = NULL, next_retry_at = NULL, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(segment_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = 'uploading', attempts = attempts + 1, \
         locked_at = CURRENT_TIMESTAMP, last_error = NULL, next_attempt_at = NULL, \
         updated_at = CURRENT_TIMESTAMP WHERE segment_id = ?",
    )
    .bind(segment_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    tx.commit().await.change_context(AppError::Unknown)?;
    Ok(())
}

async fn mark_uploaded(
    pool: &sqlx::Pool<Sqlite>,
    segment_id: i64,
    upload_path: &Path,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE recording_segments SET status = 'uploaded', uploaded_filename = ?, \
         uploaded_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(upload_path.display().to_string())
    .bind(segment_id)
    .execute(pool)
    .await
    .change_context(AppError::Unknown)?;
    Ok(())
}

async fn mark_verified(pool: &sqlx::Pool<Sqlite>, segment: &SegmentRecord) -> AppResult<()> {
    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE recording_segments SET status = 'verified', verified_at = CURRENT_TIMESTAMP, \
         last_error = NULL, next_retry_at = NULL, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = 'verified', last_error = NULL, next_attempt_at = NULL, \
         updated_at = CURRENT_TIMESTAMP WHERE segment_id = ?",
    )
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE live_sessions SET verified_parts = MAX(verified_parts, ?), \
         last_error = NULL, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(segment.part_number)
    .bind(segment.session_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    tx.commit().await.change_context(AppError::Unknown)?;
    Ok(())
}

async fn mark_deleted(pool: &sqlx::Pool<Sqlite>, segment: &SegmentRecord) -> AppResult<()> {
    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE recording_segments SET status = 'deleted', deleted_at = CURRENT_TIMESTAMP, \
         updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = 'complete', updated_at = CURRENT_TIMESTAMP WHERE segment_id = ?",
    )
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    tx.commit().await.change_context(AppError::Unknown)?;
    Ok(())
}

async fn mark_retry(
    pool: &sqlx::Pool<Sqlite>,
    segment: &SegmentRecord,
    attempt: i64,
    delay_seconds: u64,
    error_message: &str,
) -> AppResult<()> {
    let modifier = format!("+{delay_seconds} seconds");
    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE recording_segments SET status = 'retry_wait', retry_count = ?, last_error = ?, \
         next_retry_at = datetime('now', ?), updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(attempt)
    .bind(error_message)
    .bind(&modifier)
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = 'retry_wait', last_error = ?, \
         next_attempt_at = datetime('now', ?), locked_at = NULL, updated_at = CURRENT_TIMESTAMP \
         WHERE segment_id = ?",
    )
    .bind(error_message)
    .bind(&modifier)
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE live_sessions SET status = 'retrying', last_error = ?, \
         updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(error_message)
    .bind(segment.session_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    tx.commit().await.change_context(AppError::Unknown)?;
    Ok(())
}

async fn mark_recording_complete(pool: &sqlx::Pool<Sqlite>, session_id: i64) -> AppResult<()> {
    sqlx::query(
        "UPDATE live_sessions SET status = CASE WHEN expected_parts = verified_parts \
         THEN 'complete' ELSE 'recording_complete' END, ended_at = CURRENT_TIMESTAMP, \
         updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(session_id)
    .execute(pool)
    .await
    .change_context(AppError::Unknown)?;
    Ok(())
}

async fn session_can_finish(pool: &sqlx::Pool<Sqlite>, session_id: i64) -> AppResult<bool> {
    let row = sqlx::query(
        "SELECT status, \
         (SELECT COUNT(*) FROM recording_segments \
          WHERE session_id = ? AND status NOT IN ('verified', 'deleted')) AS pending \
         FROM live_sessions WHERE id = ?",
    )
    .bind(session_id)
    .bind(session_id)
    .fetch_one(pool)
    .await
    .change_context(AppError::Unknown)?;
    let status: String = row.try_get("status").change_context(AppError::Unknown)?;
    let pending: i64 = row.try_get("pending").change_context(AppError::Unknown)?;
    Ok(matches!(status.as_str(), "recording_complete" | "complete") && pending == 0)
}

async fn mark_session_complete(pool: &sqlx::Pool<Sqlite>, session_id: i64) -> AppResult<()> {
    sqlx::query(
        "UPDATE live_sessions SET status = 'complete', last_error = NULL, \
         ended_at = COALESCE(ended_at, CURRENT_TIMESTAMP), updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(session_id)
    .execute(pool)
    .await
    .change_context(AppError::Unknown)?;
    Ok(())
}

fn retry_delay(attempt: usize) -> u64 {
    RETRY_DELAYS
        .get(attempt.saturating_sub(1))
        .copied()
        .unwrap_or(*RETRY_DELAYS.last().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retry_schedule_caps_at_one_hour() {
        assert_eq!(retry_delay(1), 60);
        assert_eq!(retry_delay(2), 300);
        assert_eq!(retry_delay(3), 900);
        assert_eq!(retry_delay(4), 1800);
        assert_eq!(retry_delay(5), 3600);
        assert_eq!(retry_delay(20), 3600);
    }

    #[test]
    fn extracts_standard_submit_ids() {
        let response = ResponseData {
            code: 0,
            data: Some(json!({"aid": 123, "bvid": "BV1test"})),
            message: String::new(),
            ttl: Some(1),
        };
        let (aid, bvid) = extract_remote_ids(&response).unwrap();
        assert_eq!(aid, 123);
        assert_eq!(bvid.as_deref(), Some("BV1test"));
    }
}
