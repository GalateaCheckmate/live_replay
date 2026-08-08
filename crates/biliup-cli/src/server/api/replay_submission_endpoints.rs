use crate::server::infrastructure::connection_pool::ConnectionPool;
use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use sqlx::Row;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionPartPhase {
    Recording,
    Waiting,
    Uploading,
    Completed,
    Error,
}

#[derive(Debug, Serialize)]
pub struct SubmissionPart {
    pub job_id: Option<i64>,
    pub part_number: i64,
    pub phase: SubmissionPartPhase,
    pub file_size: i64,
    pub last_error: Option<String>,
    pub can_retry: bool,
}

#[derive(Debug, Serialize)]
pub struct SubmissionSession {
    pub id: i64,
    pub streamer_name: String,
    pub bvid: Option<String>,
    pub requires_submission_reconciliation: bool,
    pub last_error: Option<String>,
    pub parts: Vec<SubmissionPart>,
}

#[derive(Debug, Serialize)]
pub struct SubmissionActivityResponse {
    pub sessions: Vec<SubmissionSession>,
}

pub async fn get_submission_activity(
    State(pool): State<ConnectionPool>,
) -> Result<Json<SubmissionActivityResponse>, Response> {
    let session_rows = sqlx::query(
        "SELECT id, streamer_name, bvid, ended_at, status, submit_state, expected_parts, last_error \
         FROM live_sessions ORDER BY id DESC LIMIT 200",
    )
    .fetch_all(&pool)
    .await
    .map_err(internal_error)?;

    let part_rows = sqlx::query(
        "SELECT j.id AS job_id, r.session_id, r.part_number, r.file_size, \
                r.status AS segment_status, COALESCE(j.status, 'queued') AS job_status, \
                COALESCE(j.last_error, r.last_error) AS last_error \
         FROM recording_segments r \
         LEFT JOIN upload_jobs j ON j.segment_id = r.id \
         WHERE r.session_id IN (SELECT id FROM live_sessions ORDER BY id DESC LIMIT 200) \
         ORDER BY r.session_id DESC, r.part_number ASC",
    )
    .fetch_all(&pool)
    .await
    .map_err(internal_error)?;

    let mut parts_by_session: HashMap<i64, Vec<SubmissionPart>> = HashMap::new();
    for row in part_rows {
        let session_id: i64 = row.try_get("session_id").map_err(internal_error)?;
        let segment_status: String = row.try_get("segment_status").map_err(internal_error)?;
        let job_status: String = row.try_get("job_status").map_err(internal_error)?;
        let last_error: Option<String> = row.try_get("last_error").map_err(internal_error)?;
        let phase = part_phase(&segment_status, &job_status, last_error.as_deref());
        let can_retry = phase == SubmissionPartPhase::Error
            && (segment_status == "retry_wait" || job_status == "retry_wait");

        parts_by_session
            .entry(session_id)
            .or_default()
            .push(SubmissionPart {
                job_id: row.try_get("job_id").map_err(internal_error)?,
                part_number: row.try_get("part_number").map_err(internal_error)?,
                phase,
                file_size: row.try_get("file_size").map_err(internal_error)?,
                last_error,
                can_retry,
            });
    }

    let mut sessions = Vec::with_capacity(session_rows.len());
    for row in session_rows {
        let id: i64 = row.try_get("id").map_err(internal_error)?;
        let ended_at: Option<String> = row.try_get("ended_at").map_err(internal_error)?;
        let status: String = row.try_get("status").map_err(internal_error)?;
        let submit_state: String = row.try_get("submit_state").map_err(internal_error)?;
        let expected_parts: i64 = row.try_get("expected_parts").map_err(internal_error)?;
        let mut parts = parts_by_session.remove(&id).unwrap_or_default();

        if ended_at.is_none() {
            parts.push(SubmissionPart {
                job_id: None,
                part_number: expected_parts.saturating_add(1),
                phase: SubmissionPartPhase::Recording,
                file_size: 0,
                last_error: None,
                can_retry: false,
            });
        }

        parts.sort_by_key(|part| part.part_number);
        let requires_submission_reconciliation = submit_state == "uncertain"
            || status == "submission_uncertain";

        sessions.push(SubmissionSession {
            id,
            streamer_name: row.try_get("streamer_name").map_err(internal_error)?,
            bvid: row.try_get("bvid").map_err(internal_error)?,
            requires_submission_reconciliation,
            last_error: row.try_get("last_error").map_err(internal_error)?,
            parts,
        });
    }

    Ok(Json(SubmissionActivityResponse { sessions }))
}

fn part_phase(
    segment_status: &str,
    job_status: &str,
    last_error: Option<&str>,
) -> SubmissionPartPhase {
    if matches!(segment_status, "conflict" | "submission_uncertain")
        || matches!(job_status, "conflict" | "submission_uncertain")
        || (matches!(segment_status, "retry_wait") || matches!(job_status, "retry_wait"))
            && last_error.is_some_and(|message| !message.trim().is_empty())
    {
        return SubmissionPartPhase::Error;
    }

    if job_status == "complete"
        || matches!(
            segment_status,
            "remote_verified" | "cleanup_pending" | "deleted" | "retained"
        )
    {
        return SubmissionPartPhase::Completed;
    }

    if matches!(
        job_status,
        "uploading" | "remote_processing" | "remote_verified"
    ) || matches!(
        segment_status,
        "uploading"
            | "uploaded_to_storage"
            | "submitting"
            | "remote_processing"
            | "remote_verified"
            | "cleanup_pending"
    ) {
        return SubmissionPartPhase::Uploading;
    }

    SubmissionPartPhase::Waiting
}

fn internal_error(error: sqlx::Error) -> Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        format!("Live Replay submission query failed: {error}"),
    )
        .into_response()
}
