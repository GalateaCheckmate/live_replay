use chrono::Utc;
use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, HeaderMap, HeaderValue, LOCATION, RANGE};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

const YOUTUBE_UPLOAD_SCOPE: &str = "https://www.googleapis.com/auth/youtube.upload";
const RESUMABLE_ENDPOINT: &str = "https://www.googleapis.com/upload/youtube/v3/videos?uploadType=resumable&part=snippet,status";
const DEFAULT_CHUNK_SIZE: u64 = 8 * 1024 * 1024;
static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn youtube_upload_scope() -> &'static str {
    YOUTUBE_UPLOAD_SCOPE
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UploadTaskState {
    Recording,
    ReadyToUpload,
    Uploading,
    WaitingForNetwork,
    RetryPending,
    AuthRequired,
    UploadSuccess,
    UploadResultUnknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YoutubeSettings {
    pub auto_upload: bool,
    pub privacy_status: String,
    pub delete_after_success: bool,
    pub account_label: Option<String>,
}

impl Default for YoutubeSettings {
    fn default() -> Self {
        Self {
            auto_upload: false,
            privacy_status: "private".to_string(),
            delete_after_success: true,
            account_label: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadTask {
    pub id: String,
    pub streamer_name: String,
    pub local_path: String,
    pub youtube_title: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub file_size: u64,
    pub state: UploadTaskState,
    pub resumable_session_url: Option<String>,
    pub confirmed_bytes: u64,
    pub youtube_video_id: Option<String>,
    pub attempts: u32,
    pub next_retry_at: i64,
    pub last_error: Option<String>,
    pub success_persisted: bool,
    pub local_deleted: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl UploadTask {
    pub fn new(
        streamer_name: String,
        local_path: String,
        youtube_title: String,
        started_at: i64,
        ended_at: i64,
        file_size: u64,
    ) -> Self {
        let now = Utc::now().timestamp();
        Self {
            id: new_id("youtube"),
            streamer_name,
            local_path,
            youtube_title,
            started_at,
            ended_at,
            file_size,
            state: UploadTaskState::ReadyToUpload,
            resumable_session_url: None,
            confirmed_bytes: 0,
            youtube_video_id: None,
            attempts: 0,
            next_retry_at: now,
            last_error: None,
            success_persisted: false,
            local_deleted: false,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn can_auto_upload(&self) -> bool {
        matches!(
            self.state,
            UploadTaskState::ReadyToUpload
                | UploadTaskState::WaitingForNetwork
                | UploadTaskState::RetryPending
                | UploadTaskState::Uploading
        )
    }

    pub fn has_confirmed_success(&self) -> bool {
        self.state == UploadTaskState::UploadSuccess
            && self.success_persisted
            && self
                .youtube_video_id
                .as_deref()
                .is_some_and(|id| !id.trim().is_empty())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct YoutubeStore {
    #[serde(default)]
    pub settings: YoutubeSettings,
    #[serde(default)]
    pub tasks: Vec<UploadTask>,
}

impl YoutubeStore {
    pub fn normalize_after_restart(&mut self) {
        let now = Utc::now().timestamp();
        for task in &mut self.tasks {
            task.updated_at = now;
            match task.state {
                UploadTaskState::Recording => {
                    // A recording that never reached finalize is not safe to upload.
                    task.state = UploadTaskState::RetryPending;
                    task.last_error = Some(
                        "App 在录像 finalize 前退出；保留本地文件，等待恢复检查。".to_string(),
                    );
                }
                UploadTaskState::Uploading => {
                    // Never create a new YouTube video after restart when a session already exists.
                    task.state = if task.resumable_session_url.is_some() {
                        UploadTaskState::RetryPending
                    } else {
                        UploadTaskState::UploadResultUnknown
                    };
                    task.last_error = Some(if task.resumable_session_url.is_some() {
                        "上传被 App 重启中断，将先查询已有 resumable session 再继续。".to_string()
                    } else {
                        "上传中断且没有可确认的 resumable session；为防止重复上传，停止自动重建任务。"
                            .to_string()
                    });
                }
                _ => {}
            }
        }
    }

    pub fn add_task_if_absent(&mut self, task: UploadTask) -> bool {
        if self.tasks.iter().any(|existing| {
            existing.local_path == task.local_path
                || (existing.streamer_name == task.streamer_name
                    && existing.started_at == task.started_at
                    && existing.ended_at == task.ended_at)
        }) {
            return false;
        }
        self.tasks.push(task);
        true
    }
}

#[derive(Debug)]
pub enum UploadStep {
    Progress(u64),
    Success(String),
    WaitingForNetwork(String),
    Retryable(String),
    AuthRequired(String),
    ResultUnknown(String),
}

pub async fn load_store(path: impl AsRef<Path>) -> Result<YoutubeStore, String> {
    let path = path.as_ref();
    match fs::read(path).await {
        Ok(bytes) => {
            let mut store: YoutubeStore = serde_json::from_slice(&bytes)
                .map_err(|error| format!("读取 YouTube 上传状态失败: {error}"))?;
            store.normalize_after_restart();
            Ok(store)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(YoutubeStore::default()),
        Err(error) => Err(format!("读取 YouTube 上传状态文件失败: {error}")),
    }
}

pub async fn save_store_atomic(path: impl AsRef<Path>, store: &YoutubeStore) -> Result<(), String> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("创建 YouTube 状态目录失败: {error}"))?;
    }
    let temp = PathBuf::from(format!("{}.tmp", path.to_string_lossy()));
    let bytes = serde_json::to_vec_pretty(store)
        .map_err(|error| format!("序列化 YouTube 上传状态失败: {error}"))?;
    fs::write(&temp, bytes)
        .await
        .map_err(|error| format!("写入 YouTube 临时状态失败: {error}"))?;
    if fs::metadata(path).await.is_ok() {
        let backup = PathBuf::from(format!("{}.bak", path.to_string_lossy()));
        let _ = fs::copy(path, backup).await;
        fs::remove_file(path)
            .await
            .map_err(|error| format!("替换 YouTube 状态文件失败: {error}"))?;
    }
    fs::rename(&temp, path)
        .await
        .map_err(|error| format!("提交 YouTube 状态文件失败: {error}"))
}

pub async fn create_resumable_session(
    access_token: &str,
    task: &UploadTask,
    privacy_status: &str,
) -> Result<String, UploadStep> {
    let client = youtube_client().map_err(UploadStep::Retryable)?;
    let body = json!({
        "snippet": {
            "title": task.youtube_title,
        },
        "status": {
            "privacyStatus": normalize_privacy(privacy_status),
            "selfDeclaredMadeForKids": false,
        }
    });
    let response = client
        .post(RESUMABLE_ENDPOINT)
        .header(AUTHORIZATION, bearer(access_token).map_err(UploadStep::AuthRequired)?)
        .header("X-Upload-Content-Length", task.file_size)
        .header("X-Upload-Content-Type", "video/mp4")
        .json(&body)
        .send()
        .await
        .map_err(|error| UploadStep::WaitingForNetwork(format!("创建 YouTube resumable session 失败: {error}")))?;

    if response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::FORBIDDEN {
        return Err(UploadStep::AuthRequired(response_text(response).await));
    }
    if response.status().is_server_error() || response.status() == StatusCode::TOO_MANY_REQUESTS {
        return Err(UploadStep::Retryable(response_text(response).await));
    }
    if !response.status().is_success() {
        return Err(UploadStep::Retryable(response_text(response).await));
    }
    response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.starts_with("https://"))
        .map(ToString::to_string)
        .ok_or_else(|| {
            UploadStep::ResultUnknown(
                "YouTube 创建上传会话成功但没有返回 Location；为防重复上传停止自动重试。"
                    .to_string(),
            )
        })
}

pub async fn query_resumable_session(
    access_token: &str,
    session_url: &str,
    total_size: u64,
) -> UploadStep {
    let client = match youtube_client() {
        Ok(client) => client,
        Err(error) => return UploadStep::Retryable(error),
    };
    let response = match client
        .put(session_url)
        .header(AUTHORIZATION, match bearer(access_token) {
            Ok(value) => value,
            Err(error) => return UploadStep::AuthRequired(error),
        })
        .header(CONTENT_LENGTH, "0")
        .header(CONTENT_RANGE, format!("bytes */{total_size}"))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return UploadStep::WaitingForNetwork(format!("查询 YouTube 上传进度失败: {error}"));
        }
    };
    classify_session_response(response, total_size).await
}

pub async fn upload_next_chunk(
    access_token: &str,
    session_url: &str,
    file_path: impl AsRef<Path>,
    confirmed_bytes: u64,
    total_size: u64,
) -> UploadStep {
    if confirmed_bytes >= total_size {
        return query_resumable_session(access_token, session_url, total_size).await;
    }
    let path = file_path.as_ref();
    let mut file = match fs::File::open(path).await {
        Ok(file) => file,
        Err(error) => return UploadStep::ResultUnknown(format!("本地录像无法打开，绝不删除: {error}")),
    };
    if let Err(error) = file.seek(std::io::SeekFrom::Start(confirmed_bytes)).await {
        return UploadStep::ResultUnknown(format!("本地录像无法定位上传断点，绝不删除: {error}"));
    }
    let remaining = total_size.saturating_sub(confirmed_bytes);
    let length = remaining.min(DEFAULT_CHUNK_SIZE) as usize;
    let mut bytes = vec![0_u8; length];
    if let Err(error) = file.read_exact(&mut bytes).await {
        return UploadStep::ResultUnknown(format!("读取待上传录像失败，绝不删除: {error}"));
    }
    let end = confirmed_bytes + length as u64 - 1;
    let client = match youtube_client() {
        Ok(client) => client,
        Err(error) => return UploadStep::Retryable(error),
    };
    let response = match client
        .put(session_url)
        .header(AUTHORIZATION, match bearer(access_token) {
            Ok(value) => value,
            Err(error) => return UploadStep::AuthRequired(error),
        })
        .header(CONTENT_TYPE, "video/mp4")
        .header(CONTENT_LENGTH, length)
        .header(CONTENT_RANGE, format!("bytes {confirmed_bytes}-{end}/{total_size}"))
        .body(bytes)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            // The server may have received part or all of this chunk. Keep the same session and
            // query it after connectivity returns; never open a second video here.
            return UploadStep::WaitingForNetwork(format!("YouTube 上传连接中断: {error}"));
        }
    };
    classify_session_response(response, total_size).await
}

pub async fn persist_success_then_maybe_delete(
    store_path: impl AsRef<Path>,
    store: &mut YoutubeStore,
    task_id: &str,
) -> Result<bool, String> {
    let task_index = store
        .tasks
        .iter()
        .position(|task| task.id == task_id)
        .ok_or_else(|| "YouTube 上传任务不存在。".to_string())?;
    {
        let task = &mut store.tasks[task_index];
        if task.state != UploadTaskState::UploadSuccess
            || task
                .youtube_video_id
                .as_deref()
                .is_none_or(|id| id.trim().is_empty())
        {
            return Err("YouTube 成功状态或 videoId 未确认，拒绝删除本地录像。".to_string());
        }
        task.success_persisted = true;
        task.updated_at = Utc::now().timestamp();
    }

    // Safety barrier: SUCCESS + videoId must reach durable app state before deletion is attempted.
    save_store_atomic(&store_path, store).await?;

    let delete_enabled = store.settings.delete_after_success;
    let local_path = store.tasks[task_index].local_path.clone();
    if !delete_enabled || local_path.trim().is_empty() {
        return Ok(false);
    }
    match fs::remove_file(&local_path).await {
        Ok(()) => {
            store.tasks[task_index].local_deleted = true;
            store.tasks[task_index].updated_at = Utc::now().timestamp();
            // If this second persistence fails, the next launch may retry deletion, which is safe.
            // It must never retry the upload because UPLOAD_SUCCESS is already durable.
            save_store_atomic(store_path, store).await?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            store.tasks[task_index].local_deleted = true;
            store.tasks[task_index].updated_at = Utc::now().timestamp();
            save_store_atomic(store_path, store).await?;
            Ok(true)
        }
        Err(error) => Err(format!("YouTube 已确认成功，但删除本地录像失败；文件将保留: {error}")),
    }
}

pub fn apply_upload_step(task: &mut UploadTask, step: UploadStep) {
    let now = Utc::now().timestamp();
    task.updated_at = now;
    match step {
        UploadStep::Progress(bytes) => {
            task.state = UploadTaskState::Uploading;
            task.confirmed_bytes = bytes.min(task.file_size);
            task.last_error = None;
        }
        UploadStep::Success(video_id) => {
            task.state = UploadTaskState::UploadSuccess;
            task.confirmed_bytes = task.file_size;
            task.youtube_video_id = Some(video_id);
            task.last_error = None;
            // Must be set only by persist_success_then_maybe_delete after the store write succeeds.
            task.success_persisted = false;
        }
        UploadStep::WaitingForNetwork(error) => {
            task.state = UploadTaskState::WaitingForNetwork;
            task.last_error = Some(error);
        }
        UploadStep::Retryable(error) => {
            task.attempts = task.attempts.saturating_add(1);
            task.state = UploadTaskState::RetryPending;
            task.next_retry_at = retry_at(task.attempts);
            task.last_error = Some(error);
        }
        UploadStep::AuthRequired(error) => {
            task.state = UploadTaskState::AuthRequired;
            task.last_error = Some(error);
        }
        UploadStep::ResultUnknown(error) => {
            task.state = UploadTaskState::UploadResultUnknown;
            task.last_error = Some(error);
        }
    }
}

async fn classify_session_response(response: reqwest::Response, total_size: u64) -> UploadStep {
    let status = response.status();
    if status.as_u16() == 308 {
        let confirmed = confirmed_bytes(response.headers(), total_size);
        return UploadStep::Progress(confirmed);
    }
    if status == StatusCode::OK || status == StatusCode::CREATED {
        let text = response.text().await.unwrap_or_default();
        if let Ok(value) = serde_json::from_str::<Value>(&text)
            && let Some(video_id) = value.get("id").and_then(Value::as_str)
            && !video_id.trim().is_empty()
        {
            return UploadStep::Success(video_id.to_string());
        }
        return UploadStep::ResultUnknown(
            "YouTube 返回成功状态但没有有效 videoId；保留本地录像并停止自动重传。".to_string(),
        );
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return UploadStep::AuthRequired(response_text(response).await);
    }
    if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
        return UploadStep::ResultUnknown(
            "YouTube resumable session 已失效；为避免创建重复视频，不自动新建上传。".to_string(),
        );
    }
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        return UploadStep::Retryable(response_text(response).await);
    }
    UploadStep::ResultUnknown(format!(
        "YouTube 上传返回未预期状态 {}；保留本地录像。{}",
        status,
        response_text(response).await
    ))
}

fn confirmed_bytes(headers: &HeaderMap, total_size: u64) -> u64 {
    let Some(range) = headers.get(RANGE).and_then(|value| value.to_str().ok()) else {
        return 0;
    };
    let Some(end) = range
        .strip_prefix("bytes=")
        .and_then(|value| value.split('-').nth(1))
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return 0;
    };
    end.saturating_add(1).min(total_size)
}

fn youtube_client() -> Result<Client, String> {
    Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|error| format!("创建 YouTube 网络客户端失败: {error}"))
}

fn bearer(token: &str) -> Result<HeaderValue, String> {
    HeaderValue::from_str(&format!("Bearer {}", token.trim()))
        .map_err(|error| format!("YouTube OAuth token 无效: {error}"))
}

fn normalize_privacy(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "public" => "public",
        "unlisted" => "unlisted",
        _ => "private",
    }
}

async fn response_text(response: reqwest::Response) -> String {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if text.trim().is_empty() {
        status.to_string()
    } else {
        format!("{status}: {text}")
    }
}

fn retry_at(attempts: u32) -> i64 {
    let seconds = (1_u64 << attempts.min(10)).saturating_mul(5).min(3600);
    Utc::now().timestamp().saturating_add(seconds as i64)
}

fn new_id(prefix: &str) -> String {
    let seq = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{}-{seq}", Utc::now().timestamp_millis())
}
