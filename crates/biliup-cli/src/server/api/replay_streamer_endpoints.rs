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

/// Monitor 在 `Pending` 时可能已经持有一次正在进行的开播探测请求。
/// 此时直接删除/重建 Worker 会留下一个旧 Arc；如果那个请求随后返回 Live，旧 Worker
/// 仍有机会用旧配置启动录制。配置变更因此只允许在 Idle/Pause 时执行。
pub async fn update_replay_streamer_settings_safe(
    State(service_register): State<ServiceRegister>,
    State(managers): State<Arc<DownloadManager>>,
    State(pool): State<ConnectionPool>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateReplayStreamerSettingsRequest>,
) -> Result<Json<ReplayStreamerSettingsResponse>, Response> {
    if let Some(worker) = managers.get_room_by_id(id).await {
        let status = worker.downloader_status.read().unwrap().clone();
        match status {
            WorkerStatus::Working(_) => {
                return Err((
                    axum::http::StatusCode::CONFLICT,
                    "主播正在录制，请在本场直播结束后修改设置",
                )
                    .into_response());
            }
            WorkerStatus::Pending => {
                return Err((
                    axum::http::StatusCode::CONFLICT,
                    "正在检查主播是否开播，请稍后再保存一次设置",
                )
                    .into_response());
            }
            WorkerStatus::Idle | WorkerStatus::Pause => {}
        }
    }

    update_replay_streamer_settings(
        State(service_register),
        State(managers),
        State(pool),
        Path(id),
        Json(payload),
    )
    .await
}

/// 开关在 Pending 探测窗口也不做 Worker 切换，避免“用户已经关闭，但旧探测刚好返回
/// Live 又启动了一次录制”的竞态。正在录制时关闭仍然允许，由 Worker::change_status
/// 安全停止当前 DownloadTask 并让 Session 正常收尾。
pub async fn set_replay_streamer_enabled_safe(
    State(service_register): State<ServiceRegister>,
    State(managers): State<Arc<DownloadManager>>,
    State(pool): State<ConnectionPool>,
    Path(id): Path<i64>,
    Json(payload): Json<ReplayStreamerEnabledRequest>,
) -> Result<Json<()>, Response> {
    if let Some(worker) = managers.get_room_by_id(id).await
        && matches!(
            &*worker.downloader_status.read().unwrap(),
            WorkerStatus::Pending
        )
    {
        return Err((
            axum::http::StatusCode::CONFLICT,
            "正在检查主播是否开播，请稍后再切换自动录制",
        )
            .into_response());
    }

    set_replay_streamer_enabled(
        State(service_register),
        State(managers),
        State(pool),
        Path(id),
        Json(payload),
    )
    .await
}
