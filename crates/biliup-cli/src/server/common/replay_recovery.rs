use crate::server::common::replay;
use crate::server::errors::{AppError, AppResult};
use crate::server::infrastructure::connection_pool::ConnectionPool;
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
    let outbox_count = replay::recover_filesystem_outbox(&pool).await?;
    if outbox_count > 0 {
        info!(outbox_count, "restored filesystem replay outbox records");
    }

    // 上传文件到存储节点后、提交或清理中断，都可以按持久状态继续。
    // 首稿处于 submitting 且没有 AID 时结果无法安全判定，转为人工处理，绝不自动重投。
    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE recording_segments SET status = 'queued', next_retry_at = NULL, updated_at = CURRENT_TIMESTAMP \
         WHERE status = 'uploading'",
    )
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = 'queued', next_attempt_at = NULL, locked_at = NULL, updated_at = CURRENT_TIMESTAMP \
         WHERE status = 'uploading'",
    )
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE recording_segments SET status = 'submission_uncertain', \
         last_error = COALESCE(last_error, '首个投稿在程序退出时处于提交中；已暂停自动重投') \
         WHERE status = 'submitting' AND session_id IN \
           (SELECT id FROM live_sessions WHERE aid IS NULL AND submit_state = 'submitting')",
    )
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = 'submission_uncertain', locked_at = NULL, \
         last_error = COALESCE(last_error, '首个投稿结果不确定') \
         WHERE segment_id IN (SELECT id FROM recording_segments WHERE status = 'submission_uncertain')",
    )
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE live_sessions SET submit_state = 'uncertain', status = 'submission_uncertain', \
         last_error = COALESCE(last_error, '首个投稿结果不确定，等待绑定AID/BVID') \
         WHERE aid IS NULL AND submit_state = 'submitting'",
    )
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    tx.commit().await.change_context(AppError::Unknown)?;

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

        let upload_config = session
            .upload_config_json
            .as_deref()
            .and_then(|json| serde_json::from_str::<UploadStreamer>(json).ok())
            .or_else(|| worker.get_upload_config().clone());
        let Some(upload_config) = upload_config else {
            warn!(session_id = session.id, "pending replay session has no upload config snapshot");
            continue;
        };
        if upload_config.is_noop_uploader() {
            warn!(session_id = session.id, "pending replay session snapshot uses Noop uploader");
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
