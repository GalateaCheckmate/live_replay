use crate::server::errors::{AppError, AppResult};
use crate::server::infrastructure::connection_pool::ConnectionPool;
use error_stack::ResultExt;
use serde::Serialize;
use sqlx::Row;

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

/// 从持久化 Session / Segment 队列汇总一个主播当前的 Live Replay 活动。
/// 这里只读取 Live Replay 自己的表，不读取上传模板或旧 biliup task 状态。
pub async fn activity_for_streamer(
    pool: &ConnectionPool,
    live_streamer_id: i64,
) -> AppResult<ReplayActivity> {
    let row = sqlx::query(
        "SELECT \
            (SELECT COUNT(*) \
             FROM recording_segments r \
             JOIN live_sessions s ON s.id = r.session_id \
             WHERE s.live_streamer_id = ? \
               AND r.status NOT IN ('deleted', 'retained')) AS pending_upload_parts, \
            CASE WHEN \
                EXISTS(SELECT 1 \
                       FROM recording_segments r \
                       JOIN live_sessions s ON s.id = r.session_id \
                       WHERE s.live_streamer_id = ? \
                         AND r.status IN ('conflict', 'submission_uncertain')) \
                OR EXISTS(SELECT 1 \
                          FROM live_sessions s \
                          WHERE s.live_streamer_id = ? \
                            AND s.status IN ('conflict', 'submission_uncertain')) \
            THEN 1 ELSE 0 END AS needs_attention",
    )
    .bind(live_streamer_id)
    .bind(live_streamer_id)
    .bind(live_streamer_id)
    .fetch_one(pool)
    .await
    .change_context(AppError::Unknown)?;

    Ok(ReplayActivity {
        pending_upload_parts: row
            .try_get("pending_upload_parts")
            .change_context(AppError::Unknown)?,
        needs_attention: row
            .try_get::<i64, _>("needs_attention")
            .change_context(AppError::Unknown)?
            != 0,
    })
}

/// 将内部 worker 状态和持久化上传队列压缩成稳定的四态模型。
/// 优先级：需要人工处理 > 正在录制 > 有待处理分段 > 等待开播。
pub fn user_state(
    enabled: bool,
    worker_status: &str,
    activity: ReplayActivity,
) -> ReplayUserState {
    if activity.needs_attention {
        ReplayUserState::Error
    } else if enabled && worker_status == "Working" {
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
        assert_eq!(user_state(true, "Working", pending), ReplayUserState::Recording);
        assert_eq!(user_state(true, "Idle", pending), ReplayUserState::Uploading);
        assert_eq!(
            user_state(
                true,
                "Working",
                ReplayActivity {
                    pending_upload_parts: 1,
                    needs_attention: true,
                },
            ),
            ReplayUserState::Error
        );
        assert_eq!(
            user_state(false, "Pause", ReplayActivity::default()),
            ReplayUserState::Waiting
        );
    }
}
