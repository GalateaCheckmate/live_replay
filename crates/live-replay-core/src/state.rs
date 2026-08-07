use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::fs;

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreSettings {
    pub monitor_interval_secs: u64,
    pub segment_minutes: u64,
    pub same_session_gap_secs: i64,
    pub warn_free_bytes: u64,
    pub stop_new_recording_free_bytes: u64,
    pub auto_upload: bool,
}

impl Default for CoreSettings {
    fn default() -> Self {
        Self {
            monitor_interval_secs: 30,
            segment_minutes: 60,
            same_session_gap_secs: 10 * 60,
            warn_free_bytes: 30 * 1024 * 1024 * 1024,
            stop_new_recording_free_bytes: 10 * 1024 * 1024 * 1024,
            auto_upload: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamerConfig {
    pub id: String,
    pub name: String,
    pub url: String,
    pub enabled: bool,
    #[serde(default)]
    pub bilibili_cookie: Option<String>,
    #[serde(default)]
    pub douyin_cookie: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_probe_at: Option<i64>,
    #[serde(default)]
    pub currently_live: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplaySession {
    pub id: String,
    pub streamer_id: String,
    pub streamer_name: String,
    pub platform: String,
    pub started_at: i64,
    pub last_live_at: i64,
    pub ended_at: Option<i64>,
    pub next_part_index: u32,
    pub bilibili_bvid: Option<String>,
    pub bilibili_aid: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SegmentState {
    Recording,
    Ready,
    Uploading,
    Uploaded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentRecord {
    pub id: String,
    pub session_id: String,
    pub streamer_id: String,
    pub part_index: u32,
    pub file_path: String,
    pub bytes: u64,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub state: SegmentState,
    pub remote_confirmed: bool,
    pub remote_part_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UploadState {
    Pending,
    Uploading,
    RetryWait,
    Uploaded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadJob {
    pub id: String,
    pub segment_id: String,
    pub session_id: String,
    pub file_path: String,
    pub part_index: u32,
    pub state: UploadState,
    pub attempts: u32,
    pub next_retry_at: i64,
    pub last_error: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BilibiliUploadConfig {
    pub enabled: bool,
    pub login_info_json: Option<String>,
    pub visibility_public: bool,
    #[serde(default = "default_bilibili_tid")]
    pub tid: u16,
    pub tag: String,
    pub description_template: String,
}

fn default_bilibili_tid() -> u16 {
    // Kept configurable in the UI/core because Bilibili partition IDs can change.
    // 65 is the traditional online-game partition and is only a fallback.
    65
}

impl Default for BilibiliUploadConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            login_info_json: None,
            visibility_public: true,
            tid: default_bilibili_tid(),
            tag: "直播回放".to_string(),
            description_template: "{streamer} 直播回放\n直播时间：{time}".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PersistentState {
    #[serde(default)]
    pub settings: CoreSettings,
    #[serde(default)]
    pub bilibili: BilibiliUploadConfig,
    #[serde(default)]
    pub streamers: Vec<StreamerConfig>,
    #[serde(default)]
    pub sessions: Vec<ReplaySession>,
    #[serde(default)]
    pub segments: Vec<SegmentRecord>,
    #[serde(default)]
    pub upload_queue: Vec<UploadJob>,
}

impl PersistentState {
    pub fn normalize_after_restart(&mut self) {
        for segment in &mut self.segments {
            if segment.state == SegmentState::Recording {
                segment.state = SegmentState::Failed;
            } else if segment.state == SegmentState::Uploading && !segment.remote_confirmed {
                segment.state = SegmentState::Ready;
            }
        }
        for job in &mut self.upload_queue {
            if job.state == UploadState::Uploading {
                job.state = UploadState::Pending;
                job.next_retry_at = Utc::now().timestamp();
            }
        }
        self.upload_queue.retain(|job| job.state != UploadState::Uploaded);

        let missing_jobs = self
            .segments
            .iter()
            .filter(|segment| {
                segment.state == SegmentState::Ready
                    && !segment.remote_confirmed
                    && !self.upload_queue.iter().any(|job| job.segment_id == segment.id)
            })
            .cloned()
            .collect::<Vec<_>>();
        for segment in missing_jobs {
            self.enqueue_segment_job(&segment);
        }
    }

    pub fn active_session_for_streamer(&mut self, streamer_id: &str) -> Option<&mut ReplaySession> {
        let now = Utc::now().timestamp();
        let max_gap = self.settings.same_session_gap_secs;
        self.sessions.iter_mut().rev().find(|session| {
            session.streamer_id == streamer_id
                && session.ended_at.is_none()
                && now.saturating_sub(session.last_live_at) <= max_gap
        })
    }

    pub fn enqueue_segment(&mut self, segment: SegmentRecord) {
        if segment.remote_confirmed
            || self
                .segments
                .iter()
                .any(|existing| existing.id == segment.id || existing.file_path == segment.file_path)
        {
            return;
        }
        if self.settings.auto_upload && self.bilibili.enabled {
            self.enqueue_segment_job(&segment);
        }
        self.segments.push(segment);
    }

    pub fn enqueue_segment_job(&mut self, segment: &SegmentRecord) {
        if segment.remote_confirmed
            || self
                .upload_queue
                .iter()
                .any(|job| job.segment_id == segment.id && job.state != UploadState::Uploaded)
        {
            return;
        }
        self.upload_queue.push(UploadJob {
            id: new_id("upload"),
            segment_id: segment.id.clone(),
            session_id: segment.session_id.clone(),
            file_path: segment.file_path.clone(),
            part_index: segment.part_index,
            state: UploadState::Pending,
            attempts: 0,
            next_retry_at: Utc::now().timestamp(),
            last_error: None,
            created_at: Utc::now().timestamp(),
        });
    }
}

pub fn new_id(prefix: &str) -> String {
    let seq = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{}-{seq}", Utc::now().timestamp_millis())
}

pub fn next_retry_timestamp(attempts: u32) -> i64 {
    let shift = attempts.min(10);
    let seconds = (1_u64 << shift).saturating_mul(5).min(60 * 60);
    Utc::now().timestamp().saturating_add(seconds as i64)
}

pub async fn load_state(path: impl AsRef<Path>) -> Result<PersistentState, String> {
    let path = path.as_ref();
    match fs::read(path).await {
        Ok(bytes) => {
            let mut state: PersistentState = serde_json::from_slice(&bytes)
                .map_err(|error| format!("读取 Live Replay 状态失败: {error}"))?;
            state.normalize_after_restart();
            Ok(state)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(PersistentState::default()),
        Err(error) => Err(format!("读取 Live Replay 状态文件失败: {error}")),
    }
}

pub async fn save_state_atomic(
    path: impl AsRef<Path>,
    state: &PersistentState,
) -> Result<(), String> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("创建状态目录失败: {error}"))?;
    }
    let temp = temp_path(path);
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("序列化 Live Replay 状态失败: {error}"))?;
    fs::write(&temp, bytes)
        .await
        .map_err(|error| format!("写入临时状态文件失败: {error}"))?;
    if fs::metadata(path).await.is_ok() {
        let backup = path.with_extension("json.bak");
        let _ = fs::copy(path, &backup).await;
        let _ = fs::remove_file(path).await;
    }
    fs::rename(&temp, path)
        .await
        .map_err(|error| format!("提交 Live Replay 状态失败: {error}"))
}

fn temp_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tmp", path.to_string_lossy()))
}
