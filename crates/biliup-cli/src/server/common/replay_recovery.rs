use crate::server::common::replay;
use crate::server::errors::{AppError, AppResult};
use crate::server::infrastructure::connection_pool::{ConnectionPool, startup_cutoff};
use crate::server::infrastructure::context::{Context, Worker};
use crate::server::infrastructure::models::upload_streamer::UploadStreamer;
use biliup::downloader::live::{DownloaderHint, LiveStream};
use chrono::{DateTime, Utc};
use error_stack::ResultExt;
use sqlx::Row;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Debug, Clone)]
struct PendingSession {
    id: i64,
    live_streamer_id: i64,
    source_streamer_info_id: i64,
    streamer_name: String,
    streamer_url: String,
    live_title: String,
    started_at: String,
    upload_config_json: Option<String>,
}

/// 服务启动时恢复文件系统 outbox 和所有未完成上传场次。
pub async fn recover_pending_sessions(
    pool: ConnectionPool,
    workers: Vec<Arc<Worker>>,
) -> AppResult<usize> {
    let cutoff = startup_cutoff();
    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;

    // 只关闭本进程启动前已经停止活动的场次。监控器可能在恢复器运行前发现开播，
    // 并创建新场次或重新启用旧场次；这些场次的 last_activity_at 会晚于 cutoff，
    // 不能被当成上次崩溃遗留而误关。
    sqlx::query(
        "UPDATE live_sessions SET ended_at = COALESCE(ended_at, CURRENT_TIMESTAMP), \
         status = CASE \
           WHEN status IN ('submission_uncertain', 'conflict') THEN status \
           WHEN expected_parts = verified_parts THEN 'complete' \
           ELSE 'recording_complete' \
         END, updated_at = CURRENT_TIMESTAMP \
         WHERE ended_at IS NULL \
           AND datetime(COALESCE(last_activity_at, updated_at, created_at)) < datetime(?)",
    )
    .bind(cutoff)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;

    // 只恢复已经关闭的旧场次。当前进程刚启动的上传可能已经进入 uploading，
    // 绝不能被恢复器重置为 queued。
    sqlx::query(
        "UPDATE recording_segments SET status = CASE \
           WHEN remote_filename IS NOT NULL THEN 'uploaded_to_storage' ELSE 'queued' END, \
         next_retry_at = NULL, updated_at = CURRENT_TIMESTAMP \
         WHERE status = 'uploading' AND session_id IN \
           (SELECT id FROM live_sessions WHERE ended_at IS NOT NULL)",
    )
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = 'queued', next_attempt_at = NULL, locked_at = NULL, \
         updated_at = CURRENT_TIMESTAMP WHERE status = 'uploading' AND segment_id IN \
           (SELECT r.id FROM recording_segments r JOIN live_sessions s ON s.id = r.session_id \
            WHERE s.ended_at IS NOT NULL)",
    )
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;

    // 首稿请求发出后没有保存到 AID 的窗口无法自动判定。宁可暂停，也绝不重复投稿。
    // 同样只处理已经关闭的旧场次，避免干扰当前仍在等待网络响应的提交。
    sqlx::query(
        "UPDATE recording_segments SET status = 'submission_uncertain', \
         last_error = COALESCE(last_error, '首个投稿在程序退出时处于提交中；已暂停自动重投') \
         WHERE status = 'submitting' AND session_id IN \
           (SELECT id FROM live_sessions \
            WHERE ended_at IS NOT NULL AND aid IS NULL AND submit_state = 'submitting')",
    )
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = 'submission_uncertain', locked_at = NULL, \
         last_error = COALESCE(last_error, '首个投稿结果不确定') \
         WHERE segment_id IN ( \
           SELECT r.id FROM recording_segments r JOIN live_sessions s ON s.id = r.session_id \
           WHERE r.status = 'submission_uncertain' AND s.ended_at IS NOT NULL \
         )",
    )
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE live_sessions SET submit_state = 'uncertain', status = 'submission_uncertain', \
         last_error = COALESCE(last_error, '首个投稿结果不确定，等待绑定AID/BVID') \
         WHERE ended_at IS NOT NULL AND aid IS NULL AND submit_state = 'submitting'",
    )
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    tx.commit().await.change_context(AppError::Unknown)?;

    // 先关闭旧场次，再导入文件系统 outbox。否则 outbox 恢复会刷新 last_activity_at，
    // 让真正的崩溃遗留场次看起来像本次启动后仍在录制。
    let outbox_count = replay::recover_filesystem_outbox(&pool).await?;
    if outbox_count > 0 {
        info!(outbox_count, "restored filesystem replay outbox records");
    }
    let cleaned_credentials = replay::cleanup_completed_credentials(&pool).await?;
    if cleaned_credentials > 0 {
        info!(
            cleaned_credentials,
            "cleaned completed replay credential snapshots"
        );
    }

    let rows = sqlx::query(
        "SELECT s.id, s.live_streamer_id, s.source_streamer_info_id, s.streamer_name, \
                s.streamer_url, s.live_title, s.started_at, s.upload_config_json \
         FROM live_sessions s WHERE EXISTS ( \
             SELECT 1 FROM recording_segments r WHERE r.session_id = s.id \
               AND r.status NOT IN ('deleted', 'retained') \
         ) ORDER BY s.id",
    )
    .fetch_all(&pool)
    .await
    .change_context(AppError::Unknown)?;

    let mut recovered = 0usize;
    for row in rows {
        let session = PendingSession {
            id: row.try_get("id").change_context(AppError::Unknown)?,
            live_streamer_id: row
                .try_get("live_streamer_id")
                .change_context(AppError::Unknown)?,
            source_streamer_info_id: row
                .try_get("source_streamer_info_id")
                .change_context(AppError::Unknown)?,
            streamer_name: row
                .try_get("streamer_name")
                .change_context(AppError::Unknown)?,
            streamer_url: row
                .try_get("streamer_url")
                .change_context(AppError::Unknown)?,
            live_title: row
                .try_get("live_title")
                .change_context(AppError::Unknown)?,
            started_at: row
                .try_get("started_at")
                .change_context(AppError::Unknown)?,
            upload_config_json: row
                .try_get("upload_config_json")
                .change_context(AppError::Unknown)?,
        };

        let Some(worker) = workers
            .iter()
            .find(|worker| worker.id() == session.live_streamer_id)
            .cloned()
        else {
            warn!(
                session_id = session.id,
                streamer_id = session.live_streamer_id,
                "pending replay session has no monitor worker; queue is preserved"
            );
            continue;
        };

        // 有快照时必须严格使用快照。快照损坏不能退回当前模板，否则可能换账号续传。
        let upload_config = match session.upload_config_json.as_deref() {
            Some(json) => match serde_json::from_str::<UploadStreamer>(json) {
                Ok(config) => config,
                Err(error) => {
                    warn!(
                        error = ?error,
                        session_id = session.id,
                        "upload snapshot is invalid; queue is paused to prevent account mismatch"
                    );
                    continue;
                }
            },
            None => match worker.get_upload_config().clone() {
                Some(config) => config,
                None => {
                    warn!(
                        session_id = session.id,
                        "pending replay session has no upload config"
                    );
                    continue;
                }
            },
        };
        if upload_config.is_noop_uploader() {
            warn!(
                session_id = session.id,
                "pending replay session snapshot uses Noop uploader"
            );
            continue;
        }

        let started_at = DateTime::parse_from_rfc3339(&session.started_at)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let synthetic_stream = LiveStream {
            name: session.streamer_name,
            url: session.streamer_url,
            title: session.live_title,
            date: started_at,
            live_cover_url: String::new(),
            raw_stream_url: String::new(),
            platform: "recovery".to_string(),
            stream_headers: HashMap::new(),
            suffix: "mp4".to_string(),
            danmaku: None,
            downloader_hint: DownloaderHint::Ffmpeg,
            runtime_options: None,
        };
        let ctx = Context::new(
            session.source_streamer_info_id,
            worker,
            pool.clone(),
            synthetic_stream,
        );
        replay::resume_session(session.id, ctx, upload_config).await;
        recovered += 1;
    }

    if recovered > 0 {
        info!(recovered, "restored persistent Live Replay upload sessions");
    }
    Ok(recovered)
}
