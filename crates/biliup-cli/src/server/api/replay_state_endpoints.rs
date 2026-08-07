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

#[derive(Debug, Serialize)]
pub struct ReplaySessionSummary {
    pub id: i64,
    pub streamer_name: String,
    pub live_title: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub user_state: ReplayUserState,
    pub completed: bool,
    pub expected_parts: i64,
    pub verified_parts: i64,
    pub pending_parts: i64,
    pub bvid: Option<String>,
    pub last_error: Option<String>,
    pub requires_submission_reconciliation: bool,
}

#[derive(Debug, Serialize)]
pub struct ReplaySegmentSummary {
    pub job_id: i64,
    pub session_id: i64,
    pub streamer_name: String,
    pub part_number: i64,
    pub user_state: ReplayUserState,
    pub completed: bool,
    pub file_size: i64,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub next_attempt_at: Option<String>,
    pub can_retry: bool,
}

#[derive(Debug, Serialize)]
pub struct ReplayActivityResponse {
    pub sessions: Vec<ReplaySessionSummary>,
    pub segments: Vec<ReplaySegmentSummary>,
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

/// 新 UI 使用的聚合场次/分段接口。
///
/// raw session/job 状态仍保留在旧接口中供恢复工具和排障使用，但普通页面不再
/// 识别 queued/retry_wait/remote_processing/cleanup_pending 等实现状态。
pub async fn get_replay_activity(
    State(pool): State<ConnectionPool>,
) -> Result<Json<ReplayActivityResponse>, Response> {
    let session_rows = sqlx::query(
        "SELECT s.id, s.streamer_name, s.live_title, s.started_at, s.ended_at, s.status, \
                s.submit_state, s.expected_parts, s.verified_parts, s.bvid, s.last_error, \
                (SELECT COUNT(*) FROM recording_segments r \
                 WHERE r.session_id = s.id AND r.status NOT IN ('deleted', 'retained')) AS pending_parts, \
                CASE WHEN EXISTS(SELECT 1 FROM recording_segments r \
                                 WHERE r.session_id = s.id \
                                   AND r.status IN ('conflict', 'submission_uncertain')) \
                     OR s.status IN ('conflict', 'submission_uncertain') \
                     OR s.submit_state = 'uncertain' \
                THEN 1 ELSE 0 END AS needs_attention \
         FROM live_sessions s ORDER BY s.id DESC LIMIT 200",
    )
    .fetch_all(&pool)
    .await
    .map_err(internal_error)?;

    let mut sessions = Vec::with_capacity(session_rows.len());
    for row in session_rows {
        let status: String = row.try_get("status").map_err(internal_error)?;
        let ended_at: Option<String> = row.try_get("ended_at").map_err(internal_error)?;
        let pending_parts: i64 = row.try_get("pending_parts").map_err(internal_error)?;
        let needs_attention = row
            .try_get::<i64, _>("needs_attention")
            .map_err(internal_error)?
            != 0;
        let completed = status == "complete" && pending_parts == 0;
        let state = if needs_attention {
            ReplayUserState::Error
        } else if status == "recording" && ended_at.is_none() {
            ReplayUserState::Recording
        } else if completed {
            ReplayUserState::Waiting
        } else {
            ReplayUserState::Uploading
        };

        sessions.push(ReplaySessionSummary {
            id: row.try_get("id").map_err(internal_error)?,
            streamer_name: row.try_get("streamer_name").map_err(internal_error)?,
            live_title: row.try_get("live_title").map_err(internal_error)?,
            started_at: row.try_get("started_at").map_err(internal_error)?,
            ended_at,
            user_state: state,
            completed,
            expected_parts: row.try_get("expected_parts").map_err(internal_error)?,
            verified_parts: row.try_get("verified_parts").map_err(internal_error)?,
            pending_parts,
            bvid: row.try_get("bvid").map_err(internal_error)?,
            last_error: row.try_get("last_error").map_err(internal_error)?,
            requires_submission_reconciliation: needs_attention
                && row
                    .try_get::<String, _>("submit_state")
                    .map_err(internal_error)?
                    == "uncertain",
        });
    }

    let segment_rows = sqlx::query(
        "SELECT j.id AS job_id, r.session_id, s.streamer_name, r.part_number, r.file_size, \
                r.status AS segment_status, j.status AS job_status, j.attempts, \
                COALESCE(j.last_error, r.last_error) AS last_error, \
                COALESCE(j.next_attempt_at, r.next_retry_at) AS next_attempt_at \
         FROM upload_jobs j \
         JOIN recording_segments r ON r.id = j.segment_id \
         JOIN live_sessions s ON s.id = r.session_id \
         ORDER BY CASE WHEN j.status = 'complete' THEN 1 ELSE 0 END, \
                  r.session_id DESC, r.part_number ASC LIMIT 500",
    )
    .fetch_all(&pool)
    .await
    .map_err(internal_error)?;

    let mut segments = Vec::with_capacity(segment_rows.len());
    for row in segment_rows {
        let segment_status: String = row.try_get("segment_status").map_err(internal_error)?;
        let job_status: String = row.try_get("job_status").map_err(internal_error)?;
        let completed = job_status == "complete";
        let needs_attention = matches!(
            segment_status.as_str(),
            "conflict" | "submission_uncertain"
        ) || matches!(job_status.as_str(), "conflict" | "submission_uncertain");

        segments.push(ReplaySegmentSummary {
            job_id: row.try_get("job_id").map_err(internal_error)?,
            session_id: row.try_get("session_id").map_err(internal_error)?,
            streamer_name: row.try_get("streamer_name").map_err(internal_error)?,
            part_number: row.try_get("part_number").map_err(internal_error)?,
            user_state: if needs_attention {
                ReplayUserState::Error
            } else if completed {
                ReplayUserState::Waiting
            } else {
                ReplayUserState::Uploading
            },
            completed,
            file_size: row.try_get("file_size").map_err(internal_error)?,
            attempts: row.try_get("attempts").map_err(internal_error)?,
            last_error: row.try_get("last_error").map_err(internal_error)?,
            next_attempt_at: row.try_get("next_attempt_at").map_err(internal_error)?,
            can_retry: !completed && job_status != "submission_uncertain",
        });
    }

    Ok(Json(ReplayActivityResponse { sessions, segments }))
}

fn internal_error(error: sqlx::Error) -> Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        format!("Live Replay state query failed: {error}"),
    )
        .into_response()
}
