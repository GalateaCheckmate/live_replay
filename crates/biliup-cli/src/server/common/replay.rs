use crate::server::common::upload::{build_studio, submit_to_bilibili};
use crate::server::core::downloader::SegmentInfo;
use crate::server::errors::{AppError, AppResult};
use crate::server::infrastructure::connection_pool::ConnectionPool;
use crate::server::infrastructure::context::Context;
use crate::server::infrastructure::models::hook_step::HookStep;
use crate::server::infrastructure::models::upload_streamer::UploadStreamer;
use async_channel::Receiver;
use biliup::bilibili::{BiliBili, ResponseData, Vid, Video};
use biliup::client::StatelessClient;
use biliup::credential::login_by_cookies;
use biliup::uploader::line::{Line, Probe};
use biliup::uploader::{VideoFile, line};
use error_stack::ResultExt;
use futures::StreamExt;
use rand::random;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tokio::sync::{Mutex, Notify, Semaphore};
use tracing::{error, info, warn};

const VERIFY_ATTEMPTS: usize = 30;
const VERIFY_INTERVAL_SECONDS: u64 = 10;
const RETRY_DELAYS: [u64; 5] = [60, 300, 900, 1800, 3600];
const DEFAULT_REPLAY_TITLE: &str = "{streamer} 直播回放 %Y-%m-%d %H-%M";
const OUTBOX_DIR: &str = "data/replay-outbox";
const CREDENTIAL_DIR: &str = "data/replay-credentials";

#[derive(Debug, Clone)]
struct SessionRecord {
    id: i64,
    aid: Option<u64>,
    bvid: Option<String>,
    submit_state: String,
    delete_after_success: bool,
    preserve_danmaku: bool,
}

#[derive(Debug, Clone)]
struct SegmentRecord {
    id: i64,
    session_id: i64,
    part_number: i64,
    file_path: PathBuf,
    processed_file_path: Option<PathBuf>,
    danmaku_file_path: Option<PathBuf>,
    status: String,
    remote_filename: Option<String>,
    retry_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OutboxRecord {
    session_id: i64,
    part_number: i64,
    file_path: PathBuf,
    original_file_path: PathBuf,
    danmaku_file_path: Option<PathBuf>,
    file_size: i64,
    file_mtime_ns: i64,
    file_identity: String,
}

#[derive(Debug)]
enum RemotePartState {
    Missing,
    MatchingProcessing,
    MatchingReady,
    Conflict(String),
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

fn notifiers() -> &'static Mutex<HashMap<i64, Arc<Notify>>> {
    static NOTIFIERS: OnceLock<Mutex<HashMap<i64, Arc<Notify>>>> = OnceLock::new();
    NOTIFIERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn upload_slots() -> &'static Semaphore {
    static SLOTS: OnceLock<Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| Semaphore::new(1))
}

async fn session_notify(session_id: i64) -> Arc<Notify> {
    let mut map = notifiers().lock().await;
    map.entry(session_id)
        .or_insert_with(|| Arc::new(Notify::new()))
        .clone()
}

pub async fn wake_session(session_id: i64) {
    session_notify(session_id).await.notify_waiters();
}

async fn wait_or_wake(session_id: i64, seconds: u64) {
    let notify = session_notify(session_id).await;
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(seconds)) => {}
        _ = notify.notified() => {}
    }
}

pub async fn process_session(
    rx: Receiver<SegmentInfo>,
    ctx: Context,
    upload_config: UploadStreamer,
) {
    let (session, frozen_upload_config) = match ensure_session(&ctx, &upload_config).await {
        Ok(value) => value,
        Err(e) => {
            error!(error = ?e, url = ctx.live_streamer().url, "failed to create replay session");
            return;
        }
    };

    spawn_queue_worker(session.id, ctx.clone(), frozen_upload_config.clone()).await;

    while let Ok(event) = rx.recv().await {
        match register_segment(&ctx, session.id, &event).await {
            Ok(_) => {
                wake_session(session.id).await;
                spawn_queue_worker(session.id, ctx.clone(), frozen_upload_config.clone()).await;
            }
            Err(e) => {
                error!(error = ?e, file = ?event.prev_file_path, "segment persisted to filesystem outbox");
                let pool = ctx.pool().clone();
                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        match recover_filesystem_outbox(&pool).await {
                            Ok(_) => break,
                            Err(error) => error!(error = ?error, "delayed replay outbox recovery failed"),
                        }
                    }
                });
            }
        }
    }

    if let Err(e) = mark_recording_complete(ctx.pool(), session.id).await {
        error!(error = ?e, session_id = session.id, "failed to close replay session");
    }
    wake_session(session.id).await;
    spawn_queue_worker(session.id, ctx, frozen_upload_config).await;
}

pub async fn resume_session(session_id: i64, ctx: Context, upload_config: UploadStreamer) {
    spawn_queue_worker(session_id, ctx, upload_config).await;
}

async fn spawn_queue_worker(session_id: i64, ctx: Context, upload_config: UploadStreamer) {
    {
        let mut active = active_sessions().lock().await;
        if !active.insert(session_id) {
            return;
        }
    }

    tokio::spawn(async move {
        loop {
            match run_queue_worker(session_id, &ctx, &upload_config).await {
                Ok(()) => break,
                Err(e) => {
                    error!(error = ?e, session_id, "replay worker will restart");
                    wait_or_wake(session_id, 30).await;
                }
            }
        }
        active_sessions().lock().await.remove(&session_id);
        notifiers().lock().await.remove(&session_id);
    });
}

async fn run_queue_worker(
    session_id: i64,
    ctx: &Context,
    upload_config: &UploadStreamer,
) -> AppResult<()> {
    loop {
        if let Some(segment) = next_ready_segment(ctx.pool(), session_id).await? {
            if let Err(e) = process_segment(ctx, upload_config, &segment).await {
                let current = load_segment(ctx.pool(), segment.id).await?;
                if matches!(current.status.as_str(), "conflict" | "submission_uncertain") {
                    warn!(session_id, part = segment.part_number, status = current.status, "segment requires manual resolution");
                    wait_or_wake(session_id, 60).await;
                    continue;
                }
                let attempt = current.retry_count.saturating_add(1);
                let delay = retry_delay(attempt as usize);
                mark_retry(ctx.pool(), &current, attempt, delay, &format!("{e:?}")).await?;
                warn!(error = ?e, session_id, part = segment.part_number, retry_in_seconds = delay, "segment upload failed");
                wait_or_wake(session_id, delay).await;
            }
            continue;
        }

        if session_can_finish(ctx.pool(), session_id).await? {
            mark_session_complete(ctx.pool(), session_id).await?;
            info!(session_id, "Live Replay session completed");
            return Ok(());
        }

        wait_or_wake(session_id, 2).await;
    }
}

async fn process_segment(
    ctx: &Context,
    upload_config: &UploadStreamer,
    segment: &SegmentRecord,
) -> AppResult<()> {
    let session = load_session(ctx.pool(), segment.session_id).await?;
    if matches!(segment.status.as_str(), "remote_verified" | "cleanup_pending") {
        return resume_cleanup(ctx, &session, segment).await;
    }

    mark_uploading(ctx.pool(), segment.id).await?;
    let runtime = initialize_upload_runtime(ctx, upload_config).await?;
    let mut current = load_segment(ctx.pool(), segment.id).await?;

    let remote_filename = if let Some(filename) = current.remote_filename.clone() {
        filename
    } else {
        let paths = prepare_paths(
            ctx.pool(),
            &current,
            ctx.live_streamer()
                .segment_processor
                .as_deref()
                .unwrap_or_default(),
        )
        .await?;
        let upload_path = paths
            .first()
            .cloned()
            .ok_or_else(|| AppError::Custom("segment has no upload path".to_string()))?;
        let video = upload_single_file(&upload_path, &runtime).await?;
        save_uploaded_file(ctx.pool(), current.id, &upload_path, &video.filename).await?;
        current = load_segment(ctx.pool(), current.id).await?;
        video.filename
    };

    let mut video = Video::new(&remote_filename);
    video.title = Some(part_title(&current));

    let aid = if let Some(aid) = session.aid {
        match remote_part_state(
            &runtime.bilibili,
            aid,
            current.part_number as usize,
            &remote_filename,
        )
        .await?
        {
            RemotePartState::MatchingReady | RemotePartState::MatchingProcessing => aid,
            RemotePartState::Conflict(filename) => {
                mark_conflict(
                    ctx.pool(),
                    &current,
                    &format!(
                        "远端 P{} 文件标识为 {filename}，本地上传标识为 {remote_filename}",
                        current.part_number
                    ),
                )
                .await?;
                return Ok(());
            }
            RemotePartState::Missing => {
                append_video(
                    &runtime.bilibili,
                    aid,
                    session.bvid.as_deref(),
                    current.part_number,
                    video,
                    ctx.config().submit_api.as_deref(),
                )
                .await?;
                aid
            }
        }
    } else {
        if matches!(session.submit_state.as_str(), "submitting" | "uncertain") {
            mark_submission_uncertain(
                ctx.pool(),
                &current,
                "首个投稿请求可能已经被B站接收；为防止重复投稿，已暂停并等待绑定AID/BVID",
            )
            .await?;
            return Ok(());
        }

        let mut submission_config = upload_config.clone();
        if submission_config.copyright.is_none() {
            submission_config.copyright = Some(2);
        }
        if submission_config
            .copyright_source
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        {
            submission_config.copyright_source = Some(ctx.live_streamer().url.clone());
        }

        let mut recorder = ctx.recorder(ctx.streamer_info().clone());
        recorder.filename_prefix = submission_config
            .title
            .clone()
            .or_else(|| Some(DEFAULT_REPLAY_TITLE.to_string()));
        let studio = build_studio(
            &submission_config,
            &runtime.bilibili,
            vec![video],
            &recorder,
        )
        .await?;

        mark_submitting(ctx.pool(), current.session_id, current.id).await?;
        let response = match submit_to_bilibili(
            &runtime.bilibili,
            &studio,
            ctx.config().submit_api.as_deref(),
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                mark_submission_uncertain(
                    ctx.pool(),
                    &current,
                    "首个投稿返回结果不确定；已停止自动重投，避免生成重复稿件",
                )
                .await?;
                return Err(error);
            }
        };
        let (aid, bvid) = extract_remote_ids(&response)?;
        save_remote_ids(ctx.pool(), current.session_id, aid, bvid.as_deref()).await?;
        aid
    };

    match wait_for_remote_ready(
        &runtime.bilibili,
        aid,
        current.part_number as usize,
        &remote_filename,
    )
    .await
    {
        Ok(()) => {}
        Err(error) => {
            if let Ok(RemotePartState::Conflict(filename)) = remote_part_state(
                &runtime.bilibili,
                aid,
                current.part_number as usize,
                &remote_filename,
            )
            .await
            {
                mark_conflict(
                    ctx.pool(),
                    &current,
                    &format!(
                        "远端 P{} 文件标识为 {filename}，本地上传标识为 {remote_filename}",
                        current.part_number
                    ),
                )
                .await?;
                return Ok(());
            }
            return Err(error);
        }
    }

    mark_remote_verified(ctx.pool(), &current).await?;
    let verified = load_segment(ctx.pool(), current.id).await?;
    let session = load_session(ctx.pool(), current.session_id).await?;
    resume_cleanup(ctx, &session, &verified).await
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

async fn remote_part_state(
    bilibili: &BiliBili,
    aid: u64,
    part_number: usize,
    expected_filename: &str,
) -> AppResult<RemotePartState> {
    let data = bilibili
        .video_data(&Vid::Aid(aid), None)
        .await
        .change_context(AppError::Unknown)?;
    let Some(part) = data
        .get("videos")
        .and_then(|value| value.as_array())
        .and_then(|videos| videos.get(part_number.saturating_sub(1)))
    else {
        return Ok(RemotePartState::Missing);
    };
    let filename = part
        .get("filename")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    if filename.is_empty() {
        return Ok(RemotePartState::MatchingProcessing);
    }
    if filename != expected_filename {
        return Ok(RemotePartState::Conflict(filename));
    }
    let duration = part
        .get("duration")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    if duration > 0 {
        Ok(RemotePartState::MatchingReady)
    } else {
        Ok(RemotePartState::MatchingProcessing)
    }
}

async fn wait_for_remote_ready(
    bilibili: &BiliBili,
    aid: u64,
    part_number: usize,
    expected_filename: &str,
) -> AppResult<()> {
    for _ in 0..VERIFY_ATTEMPTS {
        match remote_part_state(bilibili, aid, part_number, expected_filename).await? {
            RemotePartState::MatchingReady => return Ok(()),
            RemotePartState::Conflict(filename) => {
                return Err(AppError::Custom(format!(
                    "remote identity conflict at P{part_number}: expected {expected_filename}, got {filename}"
                ))
                .into());
            }
            RemotePartState::Missing | RemotePartState::MatchingProcessing => {
                tokio::time::sleep(Duration::from_secs(VERIFY_INTERVAL_SECONDS)).await;
            }
        }
    }
    Err(AppError::Custom(format!(
        "remote part verification timed out: aid={aid}, part={part_number}"
    ))
    .into())
}

async fn resume_cleanup(
    ctx: &Context,
    session: &SessionRecord,
    segment: &SegmentRecord,
) -> AppResult<()> {
    let paths = prepared_paths(segment);
    if !session.delete_after_success {
        if let Some(processed) = &segment.processed_file_path
            && processed != &segment.file_path
        {
            let _ = tokio::fs::remove_file(&segment.file_path).await;
        }
        return mark_terminal(ctx.pool(), segment, "retained").await;
    }

    let unsafe_postprocessor = ctx
        .live_streamer()
        .postprocessor
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|step| !matches!(step, HookStep::Remove(command) if command == "rm"));
    if unsafe_postprocessor {
        warn!(session_id = segment.session_id, part = segment.part_number, "custom postprocessor is not crash-idempotent; retaining verified files");
        return mark_terminal(ctx.pool(), segment, "retained").await;
    }

    mark_cleanup_state(ctx.pool(), segment.id, "cleanup_pending").await?;
    safe_delete_paths(segment, &paths, session.preserve_danmaku).await?;
    mark_terminal(ctx.pool(), segment, "deleted").await
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
        threads: ctx.config().threads.max(1) as usize,
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
    let _permit = upload_slots()
        .acquire()
        .await
        .change_context(AppError::Custom("upload semaphore closed".to_string()))?;
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

async fn ensure_session(
    ctx: &Context,
    upload_config: &UploadStreamer,
) -> AppResult<(SessionRecord, UploadStreamer)> {
    let reconnect_window = env_u64(
        "LIVE_REPLAY_RECONNECT_WINDOW_SECONDS",
        ctx.config().delay.max(1),
    );
    let recent_modifier = format!("-{reconnect_window} seconds");
    if let Some(row) = sqlx::query(
        "SELECT id, aid, bvid, submit_state, delete_after_success, preserve_danmaku, \
                upload_config_json, session_key FROM live_sessions \
         WHERE live_streamer_id = ? AND live_title = ? \
           AND (ended_at IS NULL OR ended_at >= datetime('now', ?)) \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(ctx.worker_id())
    .bind(&ctx.streamer_info().title)
    .bind(&recent_modifier)
    .fetch_optional(ctx.pool())
    .await
    .change_context(AppError::Unknown)?
    {
        let session = session_from_row(&row)?;
        let snapshot_json: Option<String> = row
            .try_get("upload_config_json")
            .change_context(AppError::Unknown)?;
        let snapshot: UploadStreamer = if let Some(json) = snapshot_json {
            serde_json::from_str(&json).change_context(AppError::Unknown)?
        } else {
            let key: Option<String> = row
                .try_get("session_key")
                .change_context(AppError::Unknown)?;
            let legacy_key = key.unwrap_or_else(|| format!("legacy-session-{}", session.id));
            let frozen = freeze_upload_config(upload_config, &legacy_key).await?;
            sqlx::query(
                "UPDATE live_sessions SET session_key = COALESCE(session_key, ?), \
                 upload_config_json = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
            )
            .bind(&legacy_key)
            .bind(serde_json::to_string(&frozen).change_context(AppError::Unknown)?)
            .bind(session.id)
            .execute(ctx.pool())
            .await
            .change_context(AppError::Unknown)?;
            frozen
        };
        sqlx::query(
            "UPDATE live_sessions SET ended_at = NULL, status = 'recording', last_error = NULL, \
             last_activity_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(session.id)
        .execute(ctx.pool())
        .await
        .change_context(AppError::Unknown)?;
        return Ok((session, snapshot));
    }

    let token = format!(
        "lr-{}-{}-{:016x}",
        ctx.worker_id(),
        unix_nanos(),
        random::<u64>()
    );
    let frozen_upload_config = freeze_upload_config(upload_config, &token).await?;
    let upload_json =
        serde_json::to_string(&frozen_upload_config).change_context(AppError::Unknown)?;
    let delete_after_success = env_bool("LIVE_REPLAY_DELETE_AFTER_SUCCESS", false);
    let preserve_danmaku = env_bool("LIVE_REPLAY_PRESERVE_DANMAKU", true);
    let result = sqlx::query(
        "INSERT INTO live_sessions \
         (live_streamer_id, source_streamer_info_id, streamer_name, streamer_url, live_title, \
          started_at, session_key, submit_token, upload_config_json, delete_after_success, \
          preserve_danmaku, last_activity_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
    )
    .bind(ctx.worker_id())
    .bind(ctx.id())
    .bind(&ctx.streamer_info().name)
    .bind(&ctx.streamer_info().url)
    .bind(&ctx.streamer_info().title)
    .bind(ctx.streamer_info().date.to_rfc3339())
    .bind(&token)
    .bind(&token)
    .bind(upload_json)
    .bind(delete_after_success)
    .bind(preserve_danmaku)
    .execute(ctx.pool())
    .await
    .change_context(AppError::Unknown)?;
    let session = load_session(ctx.pool(), result.last_insert_rowid()).await?;
    Ok((session, frozen_upload_config))
}

async fn freeze_upload_config(
    upload_config: &UploadStreamer,
    session_key: &str,
) -> AppResult<UploadStreamer> {
    let mut frozen = upload_config.clone();
    let source = frozen
        .user_cookie
        .clone()
        .unwrap_or_else(|| "cookies.json".to_string());
    let source_path = Path::new(&source);
    if source_path.exists() {
        tokio::fs::create_dir_all(CREDENTIAL_DIR)
            .await
            .change_context(AppError::Unknown)?;
        let target = Path::new(CREDENTIAL_DIR).join(format!("{session_key}.json"));
        tokio::fs::copy(source_path, &target)
            .await
            .change_context(AppError::Unknown)?;
        frozen.user_cookie = Some(target.display().to_string());
    }
    Ok(frozen)
}

async fn register_segment(
    ctx: &Context,
    session_id: i64,
    event: &SegmentInfo,
) -> AppResult<SegmentRecord> {
    let part_number = next_part_number(ctx.pool(), session_id).await?;
    let staged = stage_completed_segment(session_id, part_number, event).await?;
    let outbox_path = match write_outbox(&staged).await {
        Ok(path) => path,
        Err(error) => {
            rollback_staged_segment(&staged).await;
            return Err(error);
        }
    };

    let mut last_error = None;
    for attempt in 0..6u64 {
        match persist_outbox_record(ctx.pool(), ctx.id(), &staged).await {
            Ok(segment) => {
                let _ = tokio::fs::remove_file(&outbox_path).await;
                if let Err(error) = mark_session_recording(ctx.pool(), session_id).await {
                    warn!(error = ?error, session_id, "segment persisted but session status could not be refreshed");
                }
                return Ok(segment);
            }
            Err(error) => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(200 * (attempt + 1))).await;
            }
        }
    }
    Err(last_error
        .unwrap_or_else(|| AppError::Custom("failed to persist segment".to_string()).into()))
}

async fn next_part_number(pool: &ConnectionPool, session_id: i64) -> AppResult<i64> {
    let database_max: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(part_number), 0) FROM ( \
           SELECT part_number FROM recording_segments WHERE session_id = ? \
           UNION ALL SELECT part_number FROM replay_outbox WHERE session_id = ? \
         )",
    )
    .bind(session_id)
    .bind(session_id)
    .fetch_one(pool)
    .await
    .change_context(AppError::Unknown)?;
    let filesystem_max = filesystem_outbox_max_part(session_id).await;
    Ok(database_max.max(filesystem_max).saturating_add(1))
}

async fn filesystem_outbox_max_part(session_id: i64) -> i64 {
    let mut maximum = 0i64;
    let Ok(mut entries) = tokio::fs::read_dir(OUTBOX_DIR).await else {
        return maximum;
    };
    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            _ => break,
        };
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = tokio::fs::read(&path).await else {
            continue;
        };
        let Ok(record) = serde_json::from_slice::<OutboxRecord>(&bytes) else {
            continue;
        };
        if record.session_id == session_id {
            maximum = maximum.max(record.part_number);
        }
    }
    maximum
}

async fn stage_completed_segment(
    session_id: i64,
    part_number: i64,
    event: &SegmentInfo,
) -> AppResult<OutboxRecord> {
    let source = &event.prev_file_path;
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    let queue_dir = parent
        .join(".live-replay-queue")
        .join(session_id.to_string());
    tokio::fs::create_dir_all(&queue_dir)
        .await
        .change_context(AppError::Unknown)?;
    let original_name = source
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("segment.bin");
    let staged_path = queue_dir.join(format!(
        "{:06}-{}-{original_name}",
        part_number,
        unix_nanos()
    ));
    move_file(source, &staged_path).await?;

    let staged_danmaku = if let Some(danmaku) = &event.danmaku_file_path {
        if danmaku.exists() {
            let path = staged_path.with_extension("xml");
            if let Err(error) = move_file(danmaku, &path).await {
                let _ = move_file(&staged_path, source).await;
                return Err(error);
            }
            Some(path)
        } else {
            None
        }
    } else {
        None
    };
    let metadata = match tokio::fs::metadata(&staged_path).await {
        Ok(metadata) => metadata,
        Err(error) => {
            let _ = move_file(&staged_path, source).await;
            if let Some(staged_xml) = &staged_danmaku {
                let original_xml = source.with_extension("xml");
                let _ = move_file(staged_xml, &original_xml).await;
            }
            return Err(error).change_context(AppError::Unknown);
        }
    };
    let mtime_ns = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_nanos() as i64)
        .unwrap_or(0);
    let identity = format!("{}:{}:{}", metadata.len(), mtime_ns, staged_path.display());
    Ok(OutboxRecord {
        session_id,
        part_number,
        file_path: staged_path,
        original_file_path: source.clone(),
        danmaku_file_path: staged_danmaku,
        file_size: metadata.len() as i64,
        file_mtime_ns: mtime_ns,
        file_identity: identity,
    })
}

async fn rollback_staged_segment(record: &OutboxRecord) {
    let _ = move_file(&record.file_path, &record.original_file_path).await;
    if let Some(staged_xml) = &record.danmaku_file_path {
        let original_xml = record.original_file_path.with_extension("xml");
        let _ = move_file(staged_xml, &original_xml).await;
    }
}

async fn move_file(source: &Path, destination: &Path) -> AppResult<()> {
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .change_context(AppError::Unknown)?;
    }
    match tokio::fs::rename(source, destination).await {
        Ok(()) => Ok(()),
        Err(_) => {
            tokio::fs::copy(source, destination)
                .await
                .change_context(AppError::Unknown)?;
            tokio::fs::remove_file(source)
                .await
                .change_context(AppError::Unknown)?;
            Ok(())
        }
    }
}

async fn write_outbox(record: &OutboxRecord) -> AppResult<PathBuf> {
    tokio::fs::create_dir_all(OUTBOX_DIR)
        .await
        .change_context(AppError::Unknown)?;
    let path = Path::new(OUTBOX_DIR).join(format!(
        "{}-{:06}-{}.json",
        record.session_id,
        record.part_number,
        unix_nanos()
    ));
    let temp = path.with_extension("json.tmp");
    tokio::fs::write(
        &temp,
        serde_json::to_vec_pretty(record).change_context(AppError::Unknown)?,
    )
    .await
    .change_context(AppError::Unknown)?;
    tokio::fs::rename(&temp, &path)
        .await
        .change_context(AppError::Unknown)?;
    Ok(path)
}

pub async fn recover_filesystem_outbox(pool: &ConnectionPool) -> AppResult<usize> {
    let mut recovered = 0usize;
    let mut entries = match tokio::fs::read_dir(OUTBOX_DIR).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).change_context(AppError::Unknown),
    };
    while let Some(entry) = entries
        .next_entry()
        .await
        .change_context(AppError::Unknown)?
    {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) => {
                warn!(file = ?path, error = ?error, "cannot read replay outbox file");
                continue;
            }
        };
        let record: OutboxRecord = match serde_json::from_slice(&bytes) {
            Ok(record) => record,
            Err(error) => {
                warn!(file = ?path, error = ?error, "quarantining malformed replay outbox file");
                let _ = tokio::fs::rename(&path, path.with_extension("json.bad")).await;
                continue;
            }
        };
        let source_info_id: i64 = match sqlx::query_scalar(
            "SELECT source_streamer_info_id FROM live_sessions WHERE id = ?",
        )
        .bind(record.session_id)
        .fetch_optional(pool)
        .await
        .change_context(AppError::Unknown)?
        {
            Some(id) => id,
            None => {
                warn!(session_id = record.session_id, file = ?record.file_path, "outbox session is missing; preserving files");
                continue;
            }
        };
        persist_outbox_record(pool, source_info_id, &record).await?;
        let _ = tokio::fs::remove_file(&path).await;
        wake_session(record.session_id).await;
        recovered += 1;
    }
    Ok(recovered)
}

async fn persist_outbox_record(
    pool: &ConnectionPool,
    streamer_info_id: i64,
    record: &OutboxRecord,
) -> AppResult<SegmentRecord> {
    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;
    sqlx::query(
        "INSERT OR IGNORE INTO replay_outbox \
         (session_id, part_number, file_path, original_file_path, danmaku_file_path, file_size, file_mtime_ns, file_identity) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(record.session_id)
    .bind(record.part_number)
    .bind(record.file_path.display().to_string())
    .bind(record.original_file_path.display().to_string())
    .bind(
        record
            .danmaku_file_path
            .as_ref()
            .map(|path| path.display().to_string()),
    )
    .bind(record.file_size)
    .bind(record.file_mtime_ns)
    .bind(&record.file_identity)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;

    sqlx::query(
        "INSERT OR IGNORE INTO recording_segments \
         (session_id, part_number, file_path, original_file_path, danmaku_file_path, file_size, file_mtime_ns, file_identity) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(record.session_id)
    .bind(record.part_number)
    .bind(record.file_path.display().to_string())
    .bind(record.original_file_path.display().to_string())
    .bind(
        record
            .danmaku_file_path
            .as_ref()
            .map(|path| path.display().to_string()),
    )
    .bind(record.file_size)
    .bind(record.file_mtime_ns)
    .bind(&record.file_identity)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    let segment_id: i64 = sqlx::query_scalar(
        "SELECT id FROM recording_segments WHERE session_id = ? AND part_number = ?",
    )
    .bind(record.session_id)
    .bind(record.part_number)
    .fetch_one(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query("INSERT OR IGNORE INTO upload_jobs (segment_id) VALUES (?)")
        .bind(segment_id)
        .execute(&mut *tx)
        .await
        .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE live_sessions SET expected_parts = MAX(expected_parts, ?), \
         last_activity_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(record.part_number)
    .bind(record.session_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "INSERT INTO filelist (file, streamer_info_id) SELECT ?, ? \
         WHERE NOT EXISTS (SELECT 1 FROM filelist WHERE file = ?)",
    )
    .bind(record.file_path.display().to_string())
    .bind(streamer_info_id)
    .bind(record.file_path.display().to_string())
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query("DELETE FROM replay_outbox WHERE session_id = ? AND part_number = ?")
        .bind(record.session_id)
        .bind(record.part_number)
        .execute(&mut *tx)
        .await
        .change_context(AppError::Unknown)?;
    tx.commit().await.change_context(AppError::Unknown)?;
    load_segment(pool, segment_id).await
}

async fn mark_session_recording(pool: &ConnectionPool, session_id: i64) -> AppResult<()> {
    sqlx::query(
        "UPDATE live_sessions SET status = 'recording', ended_at = NULL, \
         last_activity_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(session_id)
    .execute(pool)
    .await
    .change_context(AppError::Unknown)?;
    Ok(())
}

async fn next_ready_segment(
    pool: &ConnectionPool,
    session_id: i64,
) -> AppResult<Option<SegmentRecord>> {
    let row = sqlx::query(
        "SELECT r.id, r.session_id, r.part_number, r.file_path, r.processed_file_path, \
                r.danmaku_file_path, r.status, r.remote_filename, r.retry_count \
         FROM recording_segments r JOIN live_sessions s ON s.id = r.session_id \
         WHERE r.session_id = ? AND r.part_number = s.next_part_to_upload \
           AND r.status NOT IN ('deleted', 'retained', 'conflict', 'submission_uncertain') \
           AND (r.next_retry_at IS NULL OR r.next_retry_at <= CURRENT_TIMESTAMP) LIMIT 1",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await
    .change_context(AppError::Unknown)?;
    row.as_ref().map(segment_from_row).transpose()
}

async fn prepare_paths(
    pool: &ConnectionPool,
    segment: &SegmentRecord,
    processors: &[HookStep],
) -> AppResult<Vec<PathBuf>> {
    if segment.processed_file_path.is_some() {
        return Ok(prepared_paths(segment));
    }
    let mut paths = vec![segment.file_path.clone()];
    if segment
        .file_path
        .extension()
        .and_then(|value| value.to_str())
        .is_none_or(|ext| !ext.eq_ignore_ascii_case("mp4"))
    {
        paths[0] = remux_to_mp4(&segment.file_path).await?;
    }
    if let Some(path) = &segment.danmaku_file_path {
        paths.push(path.clone());
    }

    for processor in processors {
        match processor {
            HookStep::Move { mv } => {
                let target = Path::new(mv);
                tokio::fs::create_dir_all(target)
                    .await
                    .change_context(AppError::Unknown)?;
                let mut moved = Vec::with_capacity(paths.len());
                for source in &paths {
                    let destination = target.join(source.file_name().ok_or_else(|| {
                        AppError::Custom("invalid processor path".to_string())
                    })?);
                    move_file(source, &destination).await?;
                    moved.push(destination);
                }
                paths = moved;
            }
            HookStep::Remux { .. } => {}
            HookStep::Run { .. } | HookStep::Remove(_) => {
                return Err(AppError::Custom(
                    "上传前处理器不允许 run/rm；它们无法保证路径和重试幂等，请改到上传后手动处理"
                        .to_string(),
                )
                .into());
            }
        }
    }

    let processed = paths
        .first()
        .cloned()
        .ok_or_else(|| AppError::Custom("processor removed upload path".to_string()))?;
    let danmaku = paths
        .iter()
        .skip(1)
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("xml"))
        .cloned();
    sqlx::query(
        "UPDATE recording_segments SET processed_file_path = ?, danmaku_file_path = ?, \
         updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(processed.display().to_string())
    .bind(
        danmaku
            .as_ref()
            .map(|path| path.display().to_string()),
    )
    .bind(segment.id)
    .execute(pool)
    .await
    .change_context(AppError::Unknown)?;
    Ok(paths)
}

async fn remux_to_mp4(source: &Path) -> AppResult<PathBuf> {
    let destination = source.with_extension("mp4");
    if destination.exists()
        && tokio::fs::metadata(&destination)
            .await
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false)
    {
        return Ok(destination);
    }
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "warning", "-y", "-i"]);
    command
        .arg(source)
        .args(["-map", "0:v?", "-map", "0:a?", "-c", "copy"]);
    if matches!(extension.as_str(), "ts" | "m2ts") {
        command.args(["-bsf:a", "aac_adtstoasc"]);
    }
    command
        .args([
            "-movflags",
            "+faststart",
            "-avoid_negative_ts",
            "make_zero",
        ])
        .arg(&destination)
        .kill_on_drop(true);
    let status = command
        .status()
        .await
        .change_context(AppError::Custom(
            "failed to start ffmpeg remux".to_string(),
        ))?;
    if !status.success() {
        let _ = tokio::fs::remove_file(&destination).await;
        return Err(AppError::Custom(format!(
            "ffmpeg remux failed for {}",
            source.display()
        ))
        .into());
    }
    let metadata = tokio::fs::metadata(&destination)
        .await
        .change_context(AppError::Unknown)?;
    if metadata.len() == 0 {
        let _ = tokio::fs::remove_file(&destination).await;
        return Err(AppError::Custom("ffmpeg produced an empty MP4".to_string()).into());
    }
    Ok(destination)
}

fn prepared_paths(segment: &SegmentRecord) -> Vec<PathBuf> {
    let mut paths = vec![segment
        .processed_file_path
        .clone()
        .unwrap_or_else(|| segment.file_path.clone())];
    if let Some(path) = &segment.danmaku_file_path {
        paths.push(path.clone());
    }
    paths
}

async fn safe_delete_paths(
    segment: &SegmentRecord,
    paths: &[PathBuf],
    preserve_danmaku: bool,
) -> AppResult<()> {
    let mut unique = HashSet::new();
    unique.insert(segment.file_path.clone());
    if let Some(path) = &segment.processed_file_path {
        unique.insert(path.clone());
    }
    unique.extend(paths.iter().cloned());
    if !preserve_danmaku {
        if let Some(path) = &segment.danmaku_file_path {
            unique.insert(path.clone());
        }
    }
    for path in unique {
        if preserve_danmaku && path.extension().and_then(|value| value.to_str()) == Some("xml") {
            continue;
        }
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

fn part_title(segment: &SegmentRecord) -> String {
    let stem = segment
        .processed_file_path
        .as_ref()
        .unwrap_or(&segment.file_path)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("直播回放");
    Video::truncate_title(&format!("P{} {stem}", segment.part_number), 80)
}

async fn load_session(pool: &ConnectionPool, id: i64) -> AppResult<SessionRecord> {
    let row = sqlx::query(
        "SELECT id, aid, bvid, submit_state, delete_after_success, preserve_danmaku \
         FROM live_sessions WHERE id = ?",
    )
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
        submit_state: row
            .try_get("submit_state")
            .change_context(AppError::Unknown)?,
        delete_after_success: row
            .try_get::<i64, _>("delete_after_success")
            .change_context(AppError::Unknown)?
            != 0,
        preserve_danmaku: row
            .try_get::<i64, _>("preserve_danmaku")
            .change_context(AppError::Unknown)?
            != 0,
    })
}

async fn load_segment(pool: &ConnectionPool, id: i64) -> AppResult<SegmentRecord> {
    let row = sqlx::query(
        "SELECT id, session_id, part_number, file_path, processed_file_path, \
                danmaku_file_path, status, remote_filename, retry_count \
         FROM recording_segments WHERE id = ?",
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
        session_id: row
            .try_get("session_id")
            .change_context(AppError::Unknown)?,
        part_number: row
            .try_get("part_number")
            .change_context(AppError::Unknown)?,
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
        status: row.try_get("status").change_context(AppError::Unknown)?,
        remote_filename: row
            .try_get("remote_filename")
            .change_context(AppError::Unknown)?,
        retry_count: row
            .try_get("retry_count")
            .change_context(AppError::Unknown)?,
    })
}

async fn save_uploaded_file(
    pool: &ConnectionPool,
    segment_id: i64,
    upload_path: &Path,
    remote_filename: &str,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE recording_segments SET status = 'uploaded_to_storage', uploaded_filename = ?, \
         remote_filename = ?, uploaded_at = CURRENT_TIMESTAMP, last_error = NULL, \
         updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(upload_path.display().to_string())
    .bind(remote_filename)
    .bind(segment_id)
    .execute(pool)
    .await
    .change_context(AppError::Unknown)?;
    Ok(())
}

async fn save_remote_ids(
    pool: &ConnectionPool,
    session_id: i64,
    aid: u64,
    bvid: Option<&str>,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE live_sessions SET aid = ?, bvid = ?, submit_state = 'created', last_error = NULL, \
         updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(aid as i64)
    .bind(bvid)
    .bind(session_id)
    .execute(pool)
    .await
    .change_context(AppError::Unknown)?;
    Ok(())
}

async fn mark_uploading(pool: &ConnectionPool, segment_id: i64) -> AppResult<()> {
    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE recording_segments SET status = 'uploading', last_error = NULL, next_retry_at = NULL, \
         updated_at = CURRENT_TIMESTAMP WHERE id = ? AND status NOT IN ('remote_verified', 'cleanup_pending')",
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

async fn mark_submitting(
    pool: &ConnectionPool,
    session_id: i64,
    segment_id: i64,
) -> AppResult<()> {
    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE live_sessions SET submit_state = 'submitting', updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(session_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE recording_segments SET status = 'submitting', updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(segment_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    tx.commit().await.change_context(AppError::Unknown)?;
    Ok(())
}

async fn mark_submission_uncertain(
    pool: &ConnectionPool,
    segment: &SegmentRecord,
    message: &str,
) -> AppResult<()> {
    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE live_sessions SET submit_state = 'uncertain', status = 'submission_uncertain', \
         last_error = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(message)
    .bind(segment.session_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE recording_segments SET status = 'submission_uncertain', last_error = ?, \
         updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(message)
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = 'submission_uncertain', last_error = ?, \
         updated_at = CURRENT_TIMESTAMP WHERE segment_id = ?",
    )
    .bind(message)
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    tx.commit().await.change_context(AppError::Unknown)?;
    Ok(())
}

async fn mark_conflict(
    pool: &ConnectionPool,
    segment: &SegmentRecord,
    message: &str,
) -> AppResult<()> {
    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE recording_segments SET status = 'conflict', last_error = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(message)
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = 'conflict', last_error = ?, updated_at = CURRENT_TIMESTAMP WHERE segment_id = ?",
    )
    .bind(message)
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE live_sessions SET status = 'conflict', last_error = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(message)
    .bind(segment.session_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    tx.commit().await.change_context(AppError::Unknown)?;
    Ok(())
}

async fn mark_remote_verified(
    pool: &ConnectionPool,
    segment: &SegmentRecord,
) -> AppResult<()> {
    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE recording_segments SET status = 'remote_verified', cleanup_state = 'pending', \
         verified_at = CURRENT_TIMESTAMP, last_error = NULL, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = 'remote_verified', last_error = NULL, updated_at = CURRENT_TIMESTAMP WHERE segment_id = ?",
    )
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    tx.commit().await.change_context(AppError::Unknown)?;
    Ok(())
}

async fn mark_cleanup_state(
    pool: &ConnectionPool,
    segment_id: i64,
    state: &str,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE recording_segments SET status = ?, cleanup_state = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(state)
    .bind(state)
    .bind(segment_id)
    .execute(pool)
    .await
    .change_context(AppError::Unknown)?;
    Ok(())
}

async fn mark_terminal(
    pool: &ConnectionPool,
    segment: &SegmentRecord,
    terminal: &str,
) -> AppResult<()> {
    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;
    let timestamp_column = if terminal == "deleted" {
        "deleted_at"
    } else {
        "verified_at"
    };
    let query = format!(
        "UPDATE recording_segments SET status = ?, cleanup_state = ?, {timestamp_column} = COALESCE({timestamp_column}, CURRENT_TIMESTAMP), \
         last_error = NULL, updated_at = CURRENT_TIMESTAMP WHERE id = ?"
    );
    sqlx::query(&query)
        .bind(terminal)
        .bind(terminal)
        .bind(segment.id)
        .execute(&mut *tx)
        .await
        .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = 'complete', last_error = NULL, locked_at = NULL, \
         updated_at = CURRENT_TIMESTAMP WHERE segment_id = ?",
    )
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE live_sessions SET verified_parts = MAX(verified_parts, ?), \
         next_part_to_upload = CASE WHEN next_part_to_upload = ? THEN ? + 1 ELSE next_part_to_upload END, \
         last_error = NULL, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(segment.part_number)
    .bind(segment.part_number)
    .bind(segment.part_number)
    .bind(segment.session_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    tx.commit().await.change_context(AppError::Unknown)?;
    wake_session(segment.session_id).await;
    Ok(())
}

async fn mark_retry(
    pool: &ConnectionPool,
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
        "UPDATE upload_jobs SET status = 'retry_wait', last_error = ?, next_attempt_at = datetime('now', ?), \
         locked_at = NULL, updated_at = CURRENT_TIMESTAMP WHERE segment_id = ?",
    )
    .bind(error_message)
    .bind(&modifier)
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE live_sessions SET status = CASE WHEN ended_at IS NULL THEN 'retrying' ELSE 'recording_complete' END, \
         last_error = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(error_message)
    .bind(segment.session_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    tx.commit().await.change_context(AppError::Unknown)?;
    Ok(())
}

async fn mark_recording_complete(pool: &ConnectionPool, session_id: i64) -> AppResult<()> {
    sqlx::query(
        "UPDATE live_sessions SET status = CASE WHEN expected_parts = verified_parts THEN 'complete' ELSE 'recording_complete' END, \
         ended_at = CURRENT_TIMESTAMP, last_activity_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(session_id)
    .execute(pool)
    .await
    .change_context(AppError::Unknown)?;
    Ok(())
}

async fn session_can_finish(pool: &ConnectionPool, session_id: i64) -> AppResult<bool> {
    let row = sqlx::query(
        "SELECT status, (SELECT COUNT(*) FROM recording_segments WHERE session_id = ? \
         AND status NOT IN ('deleted', 'retained')) AS pending FROM live_sessions WHERE id = ?",
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

async fn mark_session_complete(pool: &ConnectionPool, session_id: i64) -> AppResult<()> {
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
        .unwrap_or(*RETRY_DELAYS.last().expect("retry schedule is not empty"))
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
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
        let response: ResponseData = serde_json::from_value(json!({
            "code": 0,
            "data": {"aid": 123, "bvid": "BV1test"},
            "message": "",
            "ttl": 1
        }))
        .unwrap();
        let (aid, bvid) = extract_remote_ids(&response).unwrap();
        assert_eq!(aid, 123);
        assert_eq!(bvid.as_deref(), Some("BV1test"));
    }

    #[test]
    fn delete_is_opt_in() {
        assert!(!env_bool("LIVE_REPLAY_MISSING_TEST_FLAG", false));
    }
}
