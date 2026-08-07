use serde::Serialize;

/// Live Replay 对用户公开的运行状态。
///
/// 底层 recorder / uploader 可以继续拥有更细的内部状态，但 Web / Android
/// 不应该依赖那些实现细节。这样以后替换 biliup 协议实现时，UI 和业务状态模型
/// 不需要跟着重写。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayUserState {
    Waiting,
    Recording,
    Uploading,
    Error,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ReplayActivity {
    pub pending_upload_parts: i64,
    pub needs_attention: bool,
}

/// 将录制活动和持久化上传队列压缩成稳定的四态模型。
/// 优先级：需要人工处理 > 正在录制 > 有待处理分段 > 等待开播。
///
/// 这里故意只接收 `is_recording`，不接收 WorkerStatus/DownloaderType 等底层枚举，
/// 保证 Live Replay 领域模型可以独立于当前 biliup 实现存在。
pub fn user_state(
    enabled: bool,
    is_recording: bool,
    activity: ReplayActivity,
) -> ReplayUserState {
    if activity.needs_attention {
        ReplayUserState::Error
    } else if enabled && is_recording {
        ReplayUserState::Recording
    } else if activity.pending_upload_parts > 0 {
        ReplayUserState::Uploading
    } else {
        ReplayUserState::Waiting
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_state_has_stable_priority() {
        let pending = ReplayActivity {
            pending_upload_parts: 2,
            needs_attention: false,
        };
        assert_eq!(user_state(true, true, pending), ReplayUserState::Recording);
        assert_eq!(user_state(true, false, pending), ReplayUserState::Uploading);
        assert_eq!(
            user_state(
                true,
                true,
                ReplayActivity {
                    pending_upload_parts: 1,
                    needs_attention: true,
                },
            ),
            ReplayUserState::Error
        );
        assert_eq!(
            user_state(false, false, ReplayActivity::default()),
            ReplayUserState::Waiting
        );
    }
}
