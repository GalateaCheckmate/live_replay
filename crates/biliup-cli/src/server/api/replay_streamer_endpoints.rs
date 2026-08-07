use crate::server::api::replay_state_endpoints::{
    ReplayStreamerEnabledRequest, ReplayStreamerSettingsResponse,
    UpdateReplayStreamerSettingsRequest, set_replay_streamer_enabled,
    update_replay_streamer_settings,
};
use crate::server::core::download_manager::DownloadManager;
use crate::server::infrastructure::connection_pool::ConnectionPool;
use crate::server::infrastructure::context::WorkerStatus;
use crate::server::infrastructure::service_register::ServiceRegister;
use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

/// 保存设置时先把这个主播临时从可录制状态摘下，再更新兼容存储并重建 Worker。
///
/// 这样即使 Monitor 恰好在前一个状态检查与真正更新之间拿到了房间，数据库 `enabled`
/// 也会先变成 false；`start_download_workflow` 的最终二次校验会丢弃那个晚到的 Live
/// 结果，不会用旧配置短暂启动一次录制。
pub async fn update_replay_streamer_settings_safe(
    State(service_register): State<ServiceRegister>,
    State(managers): State<Arc<DownloadManager>>,
    State(pool): State<ConnectionPool>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateReplayStreamerSettingsRequest>,
) -> Result<Json<ReplayStreamerSettingsResponse>, Response> {
    if let Some(worker) = managers.get_room_by_id(id).await
        && matches!(
            &*worker.downloader_status.read().unwrap(),
            WorkerStatus::Working(_)
        )
    {
        return Err((
            axum::http::StatusCode::CONFLICT,
            "主播正在录制，请在本场直播结束后修改设置",
        )
            .into_response());
    }

    let was_enabled =
        sqlx::query_scalar::<_, i64>("SELECT enabled FROM livestreamers WHERE id = ?")
            .bind(id)
            .fetch_optional(&pool)
            .await
            .map_err(db_error)?
            .ok_or_else(|| (axum::http::StatusCode::NOT_FOUND, "主播不存在").into_response())?
            != 0;

    if was_enabled {
        set_replay_streamer_enabled(
            State(service_register.clone()),
            State(managers.clone()),
            State(pool.clone()),
            Path(id),
            Json(ReplayStreamerEnabledRequest { enabled: false }),
        )
        .await?;
    }

    let update_result = update_replay_streamer_settings(
        State(service_register.clone()),
        State(managers.clone()),
        State(pool.clone()),
        Path(id),
        Json(payload),
    )
    .await;

    if was_enabled {
        let restore_result = set_replay_streamer_enabled(
            State(service_register),
            State(managers),
            State(pool),
            Path(id),
            Json(ReplayStreamerEnabledRequest { enabled: true }),
        )
        .await;

        match (update_result, restore_result) {
            (Ok(response), Ok(_)) => Ok(response),
            (Err(update_error), Ok(_)) => Err(update_error),
            (Ok(_), Err(restore_error)) => Err(restore_error),
            (Err(update_error), Err(_restore_error)) => Err(update_error),
        }
    } else {
        update_result
    }
}

/// 显式 enable/disable 本身可以在 Pending 探测期间执行。
/// 关闭会先落库并把 Worker 置为 Pause，而录制入口还会再次检查 Worker 身份、Pause
/// 与数据库 enabled，因此晚到的探测结果不会再启动录制。
pub async fn set_replay_streamer_enabled_safe(
    State(service_register): State<ServiceRegister>,
    State(managers): State<Arc<DownloadManager>>,
    State(pool): State<ConnectionPool>,
    Path(id): Path<i64>,
    Json(payload): Json<ReplayStreamerEnabledRequest>,
) -> Result<Json<()>, Response> {
    set_replay_streamer_enabled(
        State(service_register),
        State(managers),
        State(pool),
        Path(id),
        Json(payload),
    )
    .await
}

fn db_error(error: sqlx::Error) -> Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        format!("Live Replay streamer mutation failed: {error}"),
    )
        .into_response()
}
