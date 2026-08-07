use crate::server::errors::report_to_response;
use crate::server::infrastructure::connection_pool::ConnectionPool;
use crate::server::replay_domain::{activity_for_streamer, user_state, ReplayUserState};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use sqlx::Row;

#[derive(Debug, Serialize)]
pub struct ReplayStreamerStateResponse {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub enabled: bool,
    pub user_state: ReplayUserState,
    pub pending_upload_parts: i64,
    pub active_session_id: Option<i64>,
    pub active_session_started_at: Option<String>,
}

/// Live Replay 自己的主播状态接口。
///
/// 与旧 `/v1/streamers` 不同，这个接口不暴露 downloader/uploader 的内部枚举，
/// 只返回稳定的四态模型以及 Session/上传队列摘要。PC Web 和未来 Android APK
/// 都可以依赖这个接口/模型，而不需要理解 biliup 的任务状态。
pub async fn get_replay_streamer_states(
    State(pool): State<ConnectionPool>,
) -> Result<Json<Vec<ReplayStreamerStateResponse>>, Response> {
    let rows = sqlx::query(
        "SELECT l.id, l.remark, l.url, l.enabled, \
                (SELECT s.id FROM live_sessions s \
                 WHERE s.live_streamer_id = l.id \
                   AND s.status = 'recording' AND s.ended_at IS NULL \
                 ORDER BY s.id DESC LIMIT 1) AS active_session_id, \
                (SELECT s.started_at FROM live_sessions s \
                 WHERE s.live_streamer_id = l.id \
                   AND s.status = 'recording' AND s.ended_at IS NULL \
                 ORDER BY s.id DESC LIMIT 1) AS active_session_started_at \
         FROM livestreamers l ORDER BY l.id DESC",
    )
    .fetch_all(&pool)
    .await
    .map_err(internal_error)?;

    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i64 = row.try_get("id").map_err(internal_error)?;
        let enabled: bool = row.try_get("enabled").map_err(internal_error)?;
        let active_session_id: Option<i64> = row
            .try_get("active_session_id")
            .map_err(internal_error)?;
        let activity = activity_for_streamer(&pool, id)
            .await
            .map_err(report_to_response)?;
        let state = user_state(
            enabled,
            if active_session_id.is_some() { "Working" } else { "Idle" },
            activity,
        );

        result.push(ReplayStreamerStateResponse {
            id,
            name: row.try_get("remark").map_err(internal_error)?,
            url: row.try_get("url").map_err(internal_error)?,
            enabled,
            user_state: state,
            pending_upload_parts: activity.pending_upload_parts,
            active_session_id,
            active_session_started_at: row
                .try_get("active_session_started_at")
                .map_err(internal_error)?,
        });
    }

    Ok(Json(result))
}

fn internal_error(error: sqlx::Error) -> Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        format!("Live Replay state query failed: {error}"),
    )
        .into_response()
}
