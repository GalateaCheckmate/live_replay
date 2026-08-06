use crate::server::common::replay;
use crate::server::errors::{AppError, AppResult};
use crate::server::infrastructure::connection_pool::ConnectionPool;
use crate::server::infrastructure::context::{Context, Worker};
use biliup::downloader::live::{DownloaderHint, LiveStream};
use chrono::{DateTime, Utc};
use error_stack::ResultExt;
use sqlx::Row;
use std::collections::{BTreeMap, HashMap, HashSet};
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
    ended_at: Option<String>,
}

#[derive(Debug, Clone)]
struct SessionSnapshot {
    id: i64,
    ended_at: Option<String>,
    status: String,
}

/// 服务启动时恢复未完成的上传队列。
///
/// 原版监控 Context 只在主播开播时存在；这里使用数据库中的场次元数据重建一个
/// 只供上传使用的 Context，因此主播已经下播时也能继续上传。`process_session` 会
/// 复用原来的顺序上传、远端幂等校验和安全删除逻辑。
pub async fn recover_pending_sessions(
    pool: ConnectionPool,
    workers: Vec<Arc<Worker>>,
) -> AppResult<usize> {
    // 进程被强制结束时，上传中的任务没有机会回写状态。重新排队即可；
    // 远端分P数量校验会防止“远端成功、本地未落库”造成重复追加。
    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE recording_segments SET status = 'queued', next_retry_at = NULL, \
         updated_at = CURRENT_TIMESTAMP WHERE status = 'uploading'",
    )
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = 'queued', next_attempt_at = NULL, locked_at = NULL, \
         updated_at = CURRENT_TIMESTAMP WHERE status = 'uploading'",
    )
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE live_sessions SET ended_at = COALESCE(ended_at, CURRENT_TIMESTAMP), \
         status = 'recording_complete', updated_at = CURRENT_TIMESTAMP \
         WHERE EXISTS (SELECT 1 FROM recording_segments r \
                       WHERE r.session_id = live_sessions.id \
                         AND r.status NOT IN ('verified', 'deleted'))",
    )
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    tx.commit().await.change_context(AppError::Unknown)?;

    let rows = sqlx::query(
        "SELECT s.id, s.live_streamer_id, s.source_streamer_info_id, s.streamer_name, \
                s.streamer_url, s.live_title, s.started_at, s.ended_at \
         FROM live_sessions s \
         WHERE EXISTS (SELECT 1 FROM recording_segments r \
                       WHERE r.session_id = s.id \
                         AND r.status NOT IN ('verified', 'deleted')) \
         ORDER BY s.live_streamer_id, s.id",
    )
    .fetch_all(&pool)
    .await
    .change_context(AppError::Unknown)?;

    let mut groups: BTreeMap<i64, Vec<PendingSession>> = BTreeMap::new();
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
            ended_at: row.try_get("ended_at").change_context(AppError::Unknown)?,
        };
        groups
            .entry(session.live_streamer_id)
            .or_default()
            .push(session);
    }

    let mut recovered = 0usize;
    for (streamer_id, sessions) in groups {
        let Some(worker) = workers.iter().find(|worker| worker.id() == streamer_id).cloned() else {
            warn!(streamer_id, "pending replay sessions have no matching monitor worker");
            continue;
        };
        let Some(upload_config) = worker.get_upload_config().clone() else {
            warn!(streamer_id, "pending replay sessions have no upload template");
            continue;
        };
        if upload_config.is_noop_uploader() {
            warn!(streamer_id, "pending replay sessions use the Noop uploader");
            continue;
        }

        // 快照该主播的所有场次，包括已经完成但时间很近的场次。
        // 恢复某个待续传场次时，临时把其余全部隔离，避免 ensure_session 误选到
        // 一个更新、更近但已完成的 BV。
        let snapshots = load_session_snapshots(&pool, streamer_id).await?;
        let pending_ids: HashSet<i64> = sessions.iter().map(|session| session.id).collect();

        for session in &sessions {
            sqlx::query(
                "UPDATE live_sessions SET ended_at = datetime('now', '-2 days') \
                 WHERE live_streamer_id = ? AND id != ?",
            )
            .bind(streamer_id)
            .bind(session.id)
            .execute(&pool)
            .await
            .change_context(AppError::Unknown)?;
            sqlx::query(
                "UPDATE live_sessions SET ended_at = NULL, status = 'recording_complete', \
                 updated_at = CURRENT_TIMESTAMP WHERE id = ?",
            )
            .bind(session.id)
            .execute(&pool)
            .await
            .change_context(AppError::Unknown)?;

            let started_at = DateTime::parse_from_rfc3339(&session.started_at)
                .map(|value| value.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let synthetic_stream = LiveStream {
                name: session.streamer_name.clone(),
                url: session.streamer_url.clone(),
                title: session.live_title.clone(),
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
                worker.clone(),
                pool.clone(),
                synthetic_stream,
            );
            let (tx, rx) = async_channel::unbounded();
            drop(tx);
            replay::process_session(rx, ctx, upload_config.clone()).await;
            recovered += 1;
        }

        // 非待恢复场次精确还原原状态；待恢复场次只恢复 ended_at，保留工作器刚写入的
        // queued/retrying/complete 状态，避免把实时上传结果覆盖回旧状态。
        for snapshot in snapshots {
            if pending_ids.contains(&snapshot.id) {
                if let Some(ended_at) = snapshot.ended_at {
                    sqlx::query(
                        "UPDATE live_sessions SET ended_at = ?, updated_at = CURRENT_TIMESTAMP \
                         WHERE id = ? AND status != 'complete'",
                    )
                    .bind(ended_at)
                    .bind(snapshot.id)
                    .execute(&pool)
                    .await
                    .change_context(AppError::Unknown)?;
                } else {
                    sqlx::query(
                        "UPDATE live_sessions SET ended_at = COALESCE(ended_at, CURRENT_TIMESTAMP), \
                         updated_at = CURRENT_TIMESTAMP WHERE id = ? AND status != 'complete'",
                    )
                    .bind(snapshot.id)
                    .execute(&pool)
                    .await
                    .change_context(AppError::Unknown)?;
                }
            } else {
                sqlx::query(
                    "UPDATE live_sessions SET ended_at = ?, status = ?, \
                     updated_at = CURRENT_TIMESTAMP WHERE id = ?",
                )
                .bind(snapshot.ended_at)
                .bind(snapshot.status)
                .bind(snapshot.id)
                .execute(&pool)
                .await
                .change_context(AppError::Unknown)?;
            }
        }
    }

    if recovered > 0 {
        info!(recovered, "restored persistent Live Replay upload sessions");
    }
    Ok(recovered)
}

async fn load_session_snapshots(
    pool: &ConnectionPool,
    streamer_id: i64,
) -> AppResult<Vec<SessionSnapshot>> {
    let rows = sqlx::query(
        "SELECT id, ended_at, status FROM live_sessions \
         WHERE live_streamer_id = ? ORDER BY id",
    )
    .bind(streamer_id)
    .fetch_all(pool)
    .await
    .change_context(AppError::Unknown)?;

    rows.into_iter()
        .map(|row| {
            Ok(SessionSnapshot {
                id: row.try_get("id").change_context(AppError::Unknown)?,
                ended_at: row.try_get("ended_at").change_context(AppError::Unknown)?,
                status: row.try_get("status").change_context(AppError::Unknown)?,
            })
        })
        .collect()
}
