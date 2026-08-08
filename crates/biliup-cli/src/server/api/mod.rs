/// 认证相关API
pub mod auth;
/// B站API端点
pub mod bilibili_endpoints;
/// 通用API端点
pub mod endpoints;
/// Live Replay 主播识别端点
pub mod replay_detect_endpoints;
/// Live Replay 场次与上传队列端点
pub mod replay_endpoints;
/// Live Replay 稳定领域状态端点
pub mod replay_state_endpoints;
/// Live Replay 主播设置/开关的并发安全端点
pub mod replay_streamer_endpoints;
/// 单页应用静态文件处理
pub mod spa;
pub mod ws;
