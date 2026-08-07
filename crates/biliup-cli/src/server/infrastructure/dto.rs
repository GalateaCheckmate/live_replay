use crate::server::infrastructure::models::live_streamer::LiveStreamer;
use crate::server::replay_domain::ReplayUserState;
use serde::Serialize;

/// 直播主播响应数据传输对象。
///
/// `status` / `upload_status` 暂时保留给旧页面和诊断代码兼容；新 UI 只应该依赖
/// `user_state` 与 `pending_upload_parts`，避免继续绑定到底层 recorder/uploader 状态机。
#[derive(Serialize)]
pub struct LiveStreamerResponse {
    #[serde(flatten)]
    pub inner: LiveStreamer,

    /// Live Replay 稳定的用户四态：waiting / recording / uploading / error。
    pub user_state: ReplayUserState,
    /// 当前尚未进入 deleted/retained 终态的分段数量。
    pub pending_upload_parts: i64,

    /// 兼容字段：内部录制 worker 状态。
    pub status: String,
    /// 兼容字段：内部上传 worker 状态。
    pub upload_status: String,
    /// 当前连续录制时长（秒），只有正在录制时存在。
    pub recording_elapsed_seconds: Option<u64>,
    /// 当前这场录制仍占用的本地空间（含当前文件与安全队列）。
    pub recording_bytes: Option<u64>,
}
