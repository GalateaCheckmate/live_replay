use crate::server::core::download_manager::DownloadManager;
use crate::server::errors::report_to_response;
use crate::server::infrastructure::connection_pool::ConnectionPool;
use crate::server::infrastructure::context::{Stage, WorkerStatus};
use crate::server::infrastructure::repositories::{get_all_streamer, get_upload_config};
use crate::server::infrastructure::service_register::ServiceRegister;
use crate::server::replay_domain::{ReplayActivity, ReplayUserState, user_state};
use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use std::sync::Arc;

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
    pub recording_elapsed_seconds: Option<u64>,
    pub recording_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct ReplayStreamerSettingsResponse {
    pub id: i64,
    pub url: String,
    pub name: String,
    pub enabled: bool,
    pub user_cookie: String,
    pub title: String,
    pub tid: u16,
    pub tags: Vec<String>,
    pub copyright: u8,
    pub copyright_source: String,
    pub description: String,
    pub is_only_self: u8,
    pub segment_minutes: u64,
    pub delete_after_success: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateReplayStreamerSettingsRequest {
    pub name: String,
    pub user_cookie: String,
    pub title: String,
    pub tid: u16,
    #[serde(default)]
    pub tags: Vec<String>,
    pub copyright: u8,
    #[serde(default)]
    pub copyright_source: String,
    #[serde(default)]
    pub description: String,
    pub is_only_self: u8,
    pub segment_minutes: u64,
    pub delete_after_success: bool,
}

#[derive(Debug, Deserialize)]
pub struct ReplayStreamerEnabledRequest {
    pub enabled: bool,
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
/// 状态摘要、录制计时和本地占用都从 Live Replay 领域层返回。新 UI 不再需要同时
/// 请求旧 `/v1/streamers` 才能拼出一张主播卡片。
pub async fn get_replay_streamer_states(
    State(pool): State<ConnectionPool>,
    State(managers): State<Arc<DownloadManager>>,
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
                 ORDER BY s.id DESC LIMIT 1) AS active_session_started_at, \
                (SELECT COUNT(*) FROM recording_segments r \
                 JOIN live_sessions s ON s.id = r.session_id \
                 WHERE s.live_streamer_id = l.id \
                   AND r.status NOT IN ('deleted', 'retained')) AS pending_upload_parts, \
                CASE WHEN \
                    EXISTS(SELECT 1 FROM recording_segments r \
                           JOIN live_sessions s ON s.id = r.session_id \
                           WHERE s.live_streamer_id = l.id \
                             AND r.status IN ('conflict', 'submission_uncertain')) \
                    OR EXISTS(SELECT 1 FROM live_sessions s \
                              WHERE s.live_streamer_id = l.id \
                                AND (s.status IN ('conflict', 'submission_uncertain') \
                                     OR s.submit_state = 'uncertain')) \
                THEN 1 ELSE 0 END AS needs_attention \
         FROM livestreamers l ORDER BY l.id DESC",
    )
    .fetch_all(&pool)
    .await
    .map_err(internal_error)?;

    let workers = managers.get_rooms().await;
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i64 = row.try_get("id").map_err(internal_error)?;
        let enabled: bool = row.try_get("enabled").map_err(internal_error)?;
        let pending_upload_parts: i64 = row
            .try_get("pending_upload_parts")
            .map_err(internal_error)?;
        let needs_attention = row
            .try_get::<i64, _>("needs_attention")
            .map_err(internal_error)?
            != 0;
        let activity = ReplayActivity {
            pending_upload_parts,
            needs_attention,
        };
        let worker = workers.iter().find(|worker| worker.id() == id);
        let is_recording = worker
            .map(|worker| {
                matches!(
                    &*worker.downloader_status.read().unwrap(),
                    WorkerStatus::Working(_)
                )
            })
            .unwrap_or(false);
        let state = user_state(
            enabled,
            if is_recording { "Working" } else { "Idle" },
            activity,
        );

        result.push(ReplayStreamerStateResponse {
            id,
            name: row.try_get("remark").map_err(internal_error)?,
            url: row.try_get("url").map_err(internal_error)?,
            enabled,
            user_state: state,
            pending_upload_parts,
            active_session_id: row
                .try_get("active_session_id")
                .map_err(internal_error)?,
            active_session_started_at: row
                .try_get("active_session_started_at")
                .map_err(internal_error)?,
            recording_elapsed_seconds: worker.and_then(|worker| {
                if is_recording {
                    worker.recording_elapsed_seconds()
                } else {
                    None
                }
            }),
            recording_bytes: worker.and_then(|worker| {
                if is_recording {
                    worker.recording_local_bytes()
                } else {
                    None
                }
            }),
        });
    }

    Ok(Json(result))
}

/// 返回主播的 Live Replay 设置视图。
/// 数据当前仍存放在历史表中，但表结构和 upload template 概念不再泄漏给前端。
pub async fn get_replay_streamer_settings(
    State(pool): State<ConnectionPool>,
    Path(id): Path<i64>,
) -> Result<Json<ReplayStreamerSettingsResponse>, Response> {
    load_replay_streamer_settings(&pool, id)
        .await?
        .map(Json)
        .ok_or_else(|| (axum::http::StatusCode::NOT_FOUND, "主播不存在").into_response())
}

/// 更新未来场次使用的主播/投稿设置。
///
/// 正在录制时拒绝修改，避免为了重载配置中途停止 Worker。已有场次使用创建时冻结的
/// session snapshot，因此本接口只影响之后的新场次。
pub async fn update_replay_streamer_settings(
    State(service_register): State<ServiceRegister>,
    State(managers): State<Arc<DownloadManager>>,
    State(pool): State<ConnectionPool>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateReplayStreamerSettingsRequest>,
) -> Result<Json<ReplayStreamerSettingsResponse>, Response> {
    validate_settings(&payload)?;

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

    let row = sqlx::query(
        "SELECT l.upload_streamers_id, l.enabled, l.\"override\" AS override_json, \
                u.extra_fields \
         FROM livestreamers l \
         LEFT JOIN uploadstreamers u ON u.id = l.upload_streamers_id \
         WHERE l.id = ?",
    )
    .bind(id)
    .fetch_optional(&pool)
    .await
    .map_err(internal_error)?;
    let Some(row) = row else {
        return Err((axum::http::StatusCode::NOT_FOUND, "主播不存在").into_response());
    };
    let upload_id: Option<i64> = row
        .try_get("upload_streamers_id")
        .map_err(internal_error)?;
    let Some(upload_id) = upload_id else {
        return Err((
            axum::http::StatusCode::CONFLICT,
            "这个旧主播没有关联投稿设置，请删除后重新添加",
        )
            .into_response());
    };
    let enabled: bool = row.try_get("enabled").map_err(internal_error)?;
    let override_json: Option<String> = row.try_get("override_json").map_err(internal_error)?;
    let extra_fields: Option<String> = row.try_get("extra_fields").map_err(internal_error)?;

    let mut override_value = parse_json_object(override_json.as_deref());
    override_value["segment_time"] = Value::String(format_segment_time(payload.segment_minutes));
    let mut extra_value = parse_json_object(extra_fields.as_deref());
    extra_value["live_replay_delete_after_success"] = Value::Bool(payload.delete_after_success);

    let mut tags: Vec<String> = payload
        .tags
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    tags.dedup();
    if tags.is_empty() {
        tags.push("游戏".to_string());
    }
    let tags_json = serde_json::to_string(&tags).map_err(json_error)?;
    let override_json = serde_json::to_string(&override_value).map_err(json_error)?;
    let extra_fields = serde_json::to_string(&extra_value).map_err(json_error)?;
    let copyright_source = if payload.copyright == 2 {
        if payload.copyright_source.trim().is_empty() {
            load_streamer_url(&pool, id).await?
        } else {
            payload.copyright_source.trim().to_string()
        }
    } else {
        String::new()
    };

    let mut tx = pool.begin().await.map_err(internal_error)?;
    sqlx::query(
        "UPDATE uploadstreamers SET user_cookie = ?, title = ?, tid = ?, tags = ?, \
                copyright = ?, copyright_source = ?, description = ?, is_only_self = ?, \
                extra_fields = ? WHERE id = ?",
    )
    .bind(payload.user_cookie.trim())
    .bind(payload.title.trim())
    .bind(i64::from(payload.tid))
    .bind(tags_json)
    .bind(i64::from(payload.copyright))
    .bind(copyright_source)
    .bind(payload.description.trim())
    .bind(i64::from(payload.is_only_self))
    .bind(extra_fields)
    .bind(upload_id)
    .execute(&mut *tx)
    .await
    .map_err(internal_error)?;
    sqlx::query("UPDATE livestreamers SET remark = ?, \"override\" = ? WHERE id = ?")
        .bind(payload.name.trim())
        .bind(override_json)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(internal_error)?;
    tx.commit().await.map_err(internal_error)?;

    // 配置已持久化后重新建立空闲监控 Worker。已有待上传 Session 不依赖这个 Worker，
    // 它们继续使用自己冻结的投稿配置。
    managers.del_room(id).await;
    if enabled {
        let streamer = get_all_streamer(&pool)
            .await
            .map_err(report_to_response)?
            .into_iter()
            .find(|streamer| streamer.id == id)
            .ok_or_else(|| (axum::http::StatusCode::NOT_FOUND, "主播不存在").into_response())?;
        let upload_config = get_upload_config(&pool, id)
            .await
            .map_err(report_to_response)?;
        managers
            .add_room(service_register.worker(streamer, upload_config))
            .await
            .ok_or_else(|| {
                (
                    axum::http::StatusCode::BAD_REQUEST,
                    "设置已保存，但重新建立直播监控失败，请检查直播间链接",
                )
                    .into_response()
            })?;
    }

    load_replay_streamer_settings(&pool, id)
        .await?
        .map(Json)
        .ok_or_else(|| (axum::http::StatusCode::NOT_FOUND, "主播不存在").into_response())
}

/// 显式设置主播主开关，替代旧 API 的 toggle 语义，避免并发点击造成状态反转。
pub async fn set_replay_streamer_enabled(
    State(service_register): State<ServiceRegister>,
    State(managers): State<Arc<DownloadManager>>,
    State(pool): State<ConnectionPool>,
    Path(id): Path<i64>,
    Json(payload): Json<ReplayStreamerEnabledRequest>,
) -> Result<Json<()>, Response> {
    let current: Option<i64> = sqlx::query_scalar("SELECT enabled FROM livestreamers WHERE id = ?")
        .bind(id)
        .fetch_optional(&pool)
        .await
        .map_err(internal_error)?;
    let Some(current) = current else {
        return Err((axum::http::StatusCode::NOT_FOUND, "主播不存在").into_response());
    };

    if (current != 0) == payload.enabled {
        return Ok(Json(()));
    }

    sqlx::query("UPDATE livestreamers SET enabled = ? WHERE id = ?")
        .bind(payload.enabled)
        .bind(id)
        .execute(&pool)
        .await
        .map_err(internal_error)?;

    if payload.enabled {
        if let Some(worker) = managers.get_room_by_id(id).await {
            worker.change_status(Stage::Download, WorkerStatus::Idle).await;
            managers.wake_waker(id).await;
        } else {
            let streamer = get_all_streamer(&pool)
                .await
                .map_err(report_to_response)?
                .into_iter()
                .find(|streamer| streamer.id == id)
                .ok_or_else(|| {
                    (axum::http::StatusCode::NOT_FOUND, "主播不存在").into_response()
                })?;
            let upload_config = get_upload_config(&pool, id)
                .await
                .map_err(report_to_response)?;
            managers
                .add_room(service_register.worker(streamer, upload_config))
                .await
                .ok_or_else(|| {
                    (
                        axum::http::StatusCode::BAD_REQUEST,
                        "不支持这个直播间链接",
                    )
                        .into_response()
                })?;
        }
    } else if let Some(worker) = managers.get_room_by_id(id).await {
        // Worker::change_status 会先安全停止当前 DownloadTask，再进入 Pause。
        worker.change_status(Stage::Download, WorkerStatus::Pause).await;
        managers.make_waker(id).await;
    }

    Ok(Json(()))
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
            can_retry: !completed
                && job_status != "submission_uncertain"
                && segment_status != "submission_uncertain",
        });
    }

    Ok(Json(ReplayActivityResponse { sessions, segments }))
}

async fn load_replay_streamer_settings(
    pool: &ConnectionPool,
    id: i64,
) -> Result<Option<ReplayStreamerSettingsResponse>, Response> {
    let row = sqlx::query(
        "SELECT l.id, l.url, l.remark, l.enabled, l.\"override\" AS override_json, \
                u.user_cookie, u.title, u.tid, u.tags, u.copyright, u.copyright_source, \
                u.description, u.is_only_self, u.extra_fields \
         FROM livestreamers l \
         LEFT JOIN uploadstreamers u ON u.id = l.upload_streamers_id \
         WHERE l.id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(internal_error)?;
    let Some(row) = row else {
        return Ok(None);
    };

    let tags_raw: Option<String> = row.try_get("tags").map_err(internal_error)?;
    let tags = tags_raw
        .as_deref()
        .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
        .unwrap_or_default();
    let override_json: Option<String> = row.try_get("override_json").map_err(internal_error)?;
    let extra_fields: Option<String> = row.try_get("extra_fields").map_err(internal_error)?;
    let extra = parse_json_object(extra_fields.as_deref());

    Ok(Some(ReplayStreamerSettingsResponse {
        id: row.try_get("id").map_err(internal_error)?,
        url: row.try_get("url").map_err(internal_error)?,
        name: row.try_get("remark").map_err(internal_error)?,
        enabled: row.try_get("enabled").map_err(internal_error)?,
        user_cookie: row
            .try_get::<Option<String>, _>("user_cookie")
            .map_err(internal_error)?
            .unwrap_or_default(),
        title: row
            .try_get::<Option<String>, _>("title")
            .map_err(internal_error)?
            .unwrap_or_else(|| "{streamer} 直播回放 %Y-%m-%d %H-%M".to_string()),
        tid: row
            .try_get::<Option<i64>, _>("tid")
            .map_err(internal_error)?
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(0),
        tags,
        copyright: row
            .try_get::<Option<i64>, _>("copyright")
            .map_err(internal_error)?
            .and_then(|value| u8::try_from(value).ok())
            .unwrap_or(2),
        copyright_source: row
            .try_get::<Option<String>, _>("copyright_source")
            .map_err(internal_error)?
            .unwrap_or_default(),
        description: row
            .try_get::<Option<String>, _>("description")
            .map_err(internal_error)?
            .unwrap_or_default(),
        is_only_self: row
            .try_get::<Option<i64>, _>("is_only_self")
            .map_err(internal_error)?
            .and_then(|value| u8::try_from(value).ok())
            .unwrap_or(1),
        segment_minutes: segment_minutes_from_override(override_json.as_deref()),
        delete_after_success: extra
            .get("live_replay_delete_after_success")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    }))
}

fn validate_settings(payload: &UpdateReplayStreamerSettingsRequest) -> Result<(), Response> {
    if payload.name.trim().is_empty() || payload.user_cookie.trim().is_empty() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "主播名称和投稿账号不能为空",
        )
            .into_response());
    }
    if payload.title.trim().is_empty() {
        return Err((axum::http::StatusCode::BAD_REQUEST, "视频标题不能为空").into_response());
    }
    if payload.tid == 0 {
        return Err((axum::http::StatusCode::BAD_REQUEST, "请选择有效的B站分区").into_response());
    }
    if !matches!(payload.copyright, 1 | 2) {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "投稿类型只能是自制或转载",
        )
            .into_response());
    }
    if payload.is_only_self > 1 {
        return Err((axum::http::StatusCode::BAD_REQUEST, "可见范围参数无效").into_response());
    }
    if !(1..=1440).contains(&payload.segment_minutes) {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "单段时长必须在1到1440分钟之间",
        )
            .into_response());
    }
    Ok(())
}

async fn load_streamer_url(pool: &ConnectionPool, id: i64) -> Result<String, Response> {
    sqlx::query_scalar::<_, String>("SELECT url FROM livestreamers WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(internal_error)
}

fn parse_json_object(raw: Option<&str>) -> Value {
    raw.and_then(|value| serde_json::from_str::<Value>(value).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}))
}

fn segment_minutes_from_override(raw: Option<&str>) -> u64 {
    let Some(value) = raw
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| value.get("segment_time").and_then(Value::as_str).map(str::to_string))
    else {
        return 60;
    };
    let mut parts = value.split(':');
    let Some(hours) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
        return 60;
    };
    let Some(minutes) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
        return 60;
    };
    let Some(seconds) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
        return 60;
    };
    if parts.next().is_some() || minutes >= 60 || seconds >= 60 {
        return 60;
    }
    let total_seconds = hours
        .saturating_mul(3600)
        .saturating_add(minutes.saturating_mul(60))
        .saturating_add(seconds);
    ((total_seconds.saturating_add(59)) / 60).clamp(1, 1440)
}

fn format_segment_time(minutes: u64) -> String {
    let minutes = minutes.clamp(1, 1440);
    format!("{:02}:{:02}:00", minutes / 60, minutes % 60)
}

fn json_error(error: serde_json::Error) -> Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        format!("Live Replay JSON encode failed: {error}"),
    )
        .into_response()
}

fn internal_error(error: sqlx::Error) -> Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        format!("Live Replay state query failed: {error}"),
    )
        .into_response()
}
