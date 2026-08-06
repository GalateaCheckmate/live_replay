use crate::server::common::replay;
use crate::server::errors::{AppError, report_to_response};
use crate::server::infrastructure::connection_pool::ConnectionPool;
use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use error_stack::ResultExt;
use serde::{Deserialize, Serialize};
use sqlx::Row;

#[derive(Debug, Serialize)]
pub struct ReplaySessionResponse {
    pub id: i64,
    pub live_streamer_id: i64,
    pub streamer_name: String,
    pub streamer_url: String,
    pub live_title: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub status: String,
    pub submit_state: String,
    pub aid: Option<i64>,
    pub bvid: Option<String>,
    pub expected_parts: i64,
    pub verified_parts: i64,
    pub next_part_to_upload: i64,
    pub delete_after_success: bool,
    pub preserve_danmaku: bool,
    pub last_error: Option<String>,
    pub pending_parts: i64,
}

#[derive(Debug, Serialize)]
pub struct ReplayJobResponse {
    pub id: i64,
    pub session_id: i64,
    pub segment_id: i64,
    pub streamer_name: String,
    pub bvid: Option<String>,
    pub part_number: i64,
    pub file_path: String,
    pub original_file_path: Option<String>,
    pub processed_file_path: Option<String>,
    pub remote_filename: Option<String>,
    pub file_size: i64,
    pub segment_status: String,
    pub cleanup_state: String,
    pub job_status: String,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub next_attempt_at: Option<String>,
    pub uploaded_at: Option<String>,
    pub verified_at: Option<String>,
    pub deleted_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BindSubmissionRequest {
    pub aid: u64,
    pub bvid: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResetSubmissionRequest {
    pub confirm_no_remote_submission: bool,
}

pub async fn get_replay_sessions(
    State(pool): State<ConnectionPool>,
) -> Result<Json<Vec<ReplaySessionResponse>>, Response> {
    let rows = sqlx::query(
        "SELECT s.id, s.live_streamer_id, s.streamer_name, s.streamer_url, s.live_title, \
                s.started_at, s.ended_at, s.status, s.submit_state, s.aid, s.bvid, \
                s.expected_parts, s.verified_parts, s.next_part_to_upload, \
                s.delete_after_success, s.preserve_danmaku, s.last_error, \
                (SELECT COUNT(*) FROM recording_segments r WHERE r.session_id = s.id \
                 AND r.status NOT IN ('deleted', 'retained')) AS pending_parts \
         FROM live_sessions s ORDER BY s.id DESC LIMIT 200",
    )
    .fetch_all(&pool)
    .await
    .change_context(AppError::Unknown)
    .map_err(report_to_response)?;

    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        result.push(ReplaySessionResponse {
            id: row.try_get("id").map_err(sql_error)?,
            live_streamer_id: row.try_get("live_streamer_id").map_err(sql_error)?,
            streamer_name: row.try_get("streamer_name").map_err(sql_error)?,
            streamer_url: row.try_get("streamer_url").map_err(sql_error)?,
            live_title: row.try_get("live_title").map_err(sql_error)?,
            started_at: row.try_get("started_at").map_err(sql_error)?,
            ended_at: row.try_get("ended_at").map_err(sql_error)?,
            status: row.try_get("status").map_err(sql_error)?,
            submit_state: row.try_get("submit_state").map_err(sql_error)?,
            aid: row.try_get("aid").map_err(sql_error)?,
            bvid: row.try_get("bvid").map_err(sql_error)?,
            expected_parts: row.try_get("expected_parts").map_err(sql_error)?,
            verified_parts: row.try_get("verified_parts").map_err(sql_error)?,
            next_part_to_upload: row.try_get("next_part_to_upload").map_err(sql_error)?,
            delete_after_success: row
                .try_get::<i64, _>("delete_after_success")
                .map_err(sql_error)?
                != 0,
            preserve_danmaku: row
                .try_get::<i64, _>("preserve_danmaku")
                .map_err(sql_error)?
                != 0,
            last_error: row.try_get("last_error").map_err(sql_error)?,
            pending_parts: row.try_get("pending_parts").map_err(sql_error)?,
        });
    }
    Ok(Json(result))
}

pub async fn get_replay_jobs(
    State(pool): State<ConnectionPool>,
) -> Result<Json<Vec<ReplayJobResponse>>, Response> {
    let rows = sqlx::query(
        "SELECT j.id, r.session_id, r.id AS segment_id, s.streamer_name, s.bvid, \
                r.part_number, r.file_path, r.original_file_path, r.processed_file_path, \
                r.remote_filename, r.file_size, r.status AS segment_status, r.cleanup_state, \
                j.status AS job_status, j.attempts, COALESCE(j.last_error, r.last_error) AS last_error, \
                COALESCE(j.next_attempt_at, r.next_retry_at) AS next_attempt_at, \
                r.uploaded_at, r.verified_at, r.deleted_at \
         FROM upload_jobs j JOIN recording_segments r ON r.id = j.segment_id \
         JOIN live_sessions s ON s.id = r.session_id \
         ORDER BY CASE WHEN j.status IN ('queued', 'uploading', 'retry_wait', 'remote_verified', \
                  'submission_uncertain', 'conflict') THEN 0 ELSE 1 END, \
                  r.session_id DESC, r.part_number ASC LIMIT 500",
    )
    .fetch_all(&pool)
    .await
    .change_context(AppError::Unknown)
    .map_err(report_to_response)?;

    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        result.push(ReplayJobResponse {
            id: row.try_get("id").map_err(sql_error)?,
            session_id: row.try_get("session_id").map_err(sql_error)?,
            segment_id: row.try_get("segment_id").map_err(sql_error)?,
            streamer_name: row.try_get("streamer_name").map_err(sql_error)?,
            bvid: row.try_get("bvid").map_err(sql_error)?,
            part_number: row.try_get("part_number").map_err(sql_error)?,
            file_path: row.try_get("file_path").map_err(sql_error)?,
            original_file_path: row.try_get("original_file_path").map_err(sql_error)?,
            processed_file_path: row.try_get("processed_file_path").map_err(sql_error)?,
            remote_filename: row.try_get("remote_filename").map_err(sql_error)?,
            file_size: row.try_get("file_size").map_err(sql_error)?,
            segment_status: row.try_get("segment_status").map_err(sql_error)?,
            cleanup_state: row.try_get("cleanup_state").map_err(sql_error)?,
            job_status: row.try_get("job_status").map_err(sql_error)?,
            attempts: row.try_get("attempts").map_err(sql_error)?,
            last_error: row.try_get("last_error").map_err(sql_error)?,
            next_attempt_at: row.try_get("next_attempt_at").map_err(sql_error)?,
            uploaded_at: row.try_get("uploaded_at").map_err(sql_error)?,
            verified_at: row.try_get("verified_at").map_err(sql_error)?,
            deleted_at: row.try_get("deleted_at").map_err(sql_error)?,
        });
    }
    Ok(Json(result))
}

pub async fn retry_replay_job(
    State(pool): State<ConnectionPool>,
    Path(id): Path<i64>,
) -> Result<Json<()>, Response> {
    let mut tx = pool
        .begin()
        .await
        .change_context(AppError::Unknown)
        .map_err(report_to_response)?;
    let row = sqlx::query(
        "SELECT r.id AS segment_id, r.session_id FROM upload_jobs j \
         JOIN recording_segments r ON r.id = j.segment_id WHERE j.id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .change_context(AppError::Unknown)
    .map_err(report_to_response)?;
    let Some(row) = row else {
        return Err((axum::http::StatusCode::NOT_FOUND, "Replay job not found").into_response());
    };
    let segment_id: i64 = row.try_get("segment_id").map_err(sql_error)?;
    let session_id: i64 = row.try_get("session_id").map_err(sql_error)?;

    sqlx::query(
        "UPDATE recording_segments SET status = CASE WHEN status = 'conflict' THEN 'queued' ELSE status END, \
         next_retry_at = NULL, last_error = NULL, updated_at = CURRENT_TIMESTAMP \
         WHERE id = ? AND status NOT IN ('deleted', 'retained', 'submission_uncertain')",
    )
    .bind(segment_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)
    .map_err(report_to_response)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = CASE WHEN status = 'conflict' THEN 'queued' ELSE status END, \
         next_attempt_at = NULL, last_error = NULL, locked_at = NULL, updated_at = CURRENT_TIMESTAMP \
         WHERE id = ? AND status != 'complete'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)
    .map_err(report_to_response)?;
    sqlx::query(
        "UPDATE live_sessions SET status = CASE WHEN ended_at IS NULL THEN 'recording' ELSE 'recording_complete' END, \
         last_error = NULL, updated_at = CURRENT_TIMESTAMP WHERE id = ? AND status = 'conflict'",
    )
    .bind(session_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)
    .map_err(report_to_response)?;
    tx.commit()
        .await
        .change_context(AppError::Unknown)
        .map_err(report_to_response)?;
    replay::wake_session(session_id).await;
    Ok(Json(()))
}

pub async fn bind_replay_submission(
    State(pool): State<ConnectionPool>,
    Path(id): Path<i64>,
    Json(payload): Json<BindSubmissionRequest>,
) -> Result<Json<()>, Response> {
    let aid = match i64::try_from(payload.aid) {
        Ok(value) if value > 0 => value,
        _ => {
            return Err((axum::http::StatusCode::BAD_REQUEST, "AID 格式不正确").into_response());
        }
    };
    let bvid = payload
        .bvid
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let mut tx = pool
        .begin()
        .await
        .change_context(AppError::Unknown)
        .map_err(report_to_response)?;
    let row = sqlx::query("SELECT submit_state, aid, ended_at FROM live_sessions WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .change_context(AppError::Unknown)
        .map_err(report_to_response)?;
    let Some(row) = row else {
        return Err((
            axum::http::StatusCode::NOT_FOUND,
            "Replay session not found",
        )
            .into_response());
    };
    let submit_state: String = row.try_get("submit_state").map_err(sql_error)?;
    let existing_aid: Option<i64> = row.try_get("aid").map_err(sql_error)?;
    let ended_at: Option<String> = row.try_get("ended_at").map_err(sql_error)?;
    if submit_state != "uncertain" || existing_aid.is_some() {
        return Err((
            axum::http::StatusCode::CONFLICT,
            "只有首稿结果不确定且尚未绑定AID的场次才能绑定稿件",
        )
            .into_response());
    }

    sqlx::query(
        "UPDATE live_sessions SET aid = ?, bvid = ?, submit_state = 'created', \
         status = ?, last_error = NULL, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(aid)
    .bind(bvid)
    .bind(if ended_at.is_none() {
        "recording"
    } else {
        "recording_complete"
    })
    .bind(id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)
    .map_err(report_to_response)?;
    sqlx::query(
        "UPDATE recording_segments SET status = 'queued', last_error = NULL, next_retry_at = NULL, \
         updated_at = CURRENT_TIMESTAMP WHERE session_id = ? AND status = 'submission_uncertain'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)
    .map_err(report_to_response)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = 'queued', last_error = NULL, next_attempt_at = NULL, \
         locked_at = NULL, updated_at = CURRENT_TIMESTAMP WHERE status = 'submission_uncertain' \
         AND segment_id IN (SELECT id FROM recording_segments WHERE session_id = ?)",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)
    .map_err(report_to_response)?;
    tx.commit()
        .await
        .change_context(AppError::Unknown)
        .map_err(report_to_response)?;
    replay::wake_session(id).await;
    Ok(Json(()))
}

pub async fn reset_replay_submission(
    State(pool): State<ConnectionPool>,
    Path(id): Path<i64>,
    Json(payload): Json<ResetSubmissionRequest>,
) -> Result<Json<()>, Response> {
    if !payload.confirm_no_remote_submission {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "必须确认B站端不存在该稿件",
        )
            .into_response());
    }
    let mut tx = pool
        .begin()
        .await
        .change_context(AppError::Unknown)
        .map_err(report_to_response)?;
    let row = sqlx::query("SELECT submit_state, aid, ended_at FROM live_sessions WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .change_context(AppError::Unknown)
        .map_err(report_to_response)?;
    let Some(row) = row else {
        return Err((
            axum::http::StatusCode::NOT_FOUND,
            "Replay session not found",
        )
            .into_response());
    };
    let submit_state: String = row.try_get("submit_state").map_err(sql_error)?;
    let existing_aid: Option<i64> = row.try_get("aid").map_err(sql_error)?;
    let ended_at: Option<String> = row.try_get("ended_at").map_err(sql_error)?;
    if submit_state != "uncertain" || existing_aid.is_some() {
        return Err((
            axum::http::StatusCode::CONFLICT,
            "只有首稿结果不确定且尚未绑定AID的场次才能确认无稿并重建",
        )
            .into_response());
    }
    let uncertain_first_part: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM recording_segments \
         WHERE session_id = ? AND part_number = 1 AND status = 'submission_uncertain'",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .change_context(AppError::Unknown)
    .map_err(report_to_response)?;
    if uncertain_first_part.is_none() {
        return Err((
            axum::http::StatusCode::CONFLICT,
            "当前场次不是首P投稿结果不确定，不能重建稿件",
        )
            .into_response());
    }

    sqlx::query(
        "UPDATE live_sessions SET aid = NULL, bvid = NULL, submit_state = 'new', \
         status = ?, last_error = NULL, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(if ended_at.is_none() {
        "recording"
    } else {
        "recording_complete"
    })
    .bind(id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)
    .map_err(report_to_response)?;
    // 明确确认无稿后，清除临时上传标识并重新上传本地文件，避免复用已过期的B站存储对象。
    sqlx::query(
        "UPDATE recording_segments SET status = 'queued', remote_filename = NULL, \
         uploaded_filename = NULL, uploaded_at = NULL, last_error = NULL, next_retry_at = NULL, \
         updated_at = CURRENT_TIMESTAMP WHERE session_id = ? AND status = 'submission_uncertain'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)
    .map_err(report_to_response)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = 'queued', last_error = NULL, next_attempt_at = NULL, \
         locked_at = NULL, updated_at = CURRENT_TIMESTAMP WHERE status = 'submission_uncertain' \
         AND segment_id IN (SELECT id FROM recording_segments WHERE session_id = ?)",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)
    .map_err(report_to_response)?;
    tx.commit()
        .await
        .change_context(AppError::Unknown)
        .map_err(report_to_response)?;
    replay::wake_session(id).await;
    Ok(Json(()))
}

fn sql_error(error: sqlx::Error) -> Response {
    report_to_response(
        error_stack::Report::new(AppError::Unknown).attach_printable(error.to_string()),
    )
}
