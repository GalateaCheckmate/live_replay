use live_replay_core::recording::RecordingSegment;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tauri::Manager;
use tokio::fs;
use tokio::sync::Mutex;

fn gate() -> &'static Mutex<()> {
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(()))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BilibiliSegmentState {
    ReadyToUpload,
    UploadingFile,
    FileUploaded,
    Submitting,
    RemoteProcessing,
    RemoteVerified,
    RetryPending,
    AuthRequired,
    SubmissionUncertain,
    Conflict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BilibiliSegmentTask {
    pub live_session_id: String,
    pub segment_index: u32,
    pub local_path: String,
    pub file_size: u64,
    pub started_at: i64,
    pub ended_at: i64,
    pub state: BilibiliSegmentState,
    pub remote_filename: Option<String>,
    pub retry_count: u32,
    pub next_retry_at: i64,
    pub last_error: Option<String>,
    pub local_deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BilibiliSessionTask {
    pub live_session_id: String,
    pub streamer_name: String,
    pub room_url: String,
    pub platform: String,
    pub session_started_at: i64,
    pub session_ended_at: Option<i64>,
    pub recording_complete: bool,
    pub aid: Option<u64>,
    pub bvid: Option<String>,
    pub submission_state: String,
    #[serde(default)]
    pub segments: Vec<BilibiliSegmentTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BilibiliSettings {
    pub auto_upload: bool,
    pub delete_after_success: bool,
    pub account_label: Option<String>,
}

impl Default for BilibiliSettings {
    fn default() -> Self {
        Self {
            auto_upload: false,
            delete_after_success: true,
            account_label: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BilibiliStore {
    #[serde(default)]
    pub settings: BilibiliSettings,
    #[serde(default)]
    pub sessions: Vec<BilibiliSessionTask>,
}

fn store_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("bilibili-upload-state.json"))
        .map_err(|error| format!("无法获取 B站状态目录: {error}"))
}

async fn read_store(path: &Path) -> Result<BilibiliStore, String> {
    match fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("读取 B站上传状态失败: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BilibiliStore::default()),
        Err(error) => Err(format!("读取 B站上传状态文件失败: {error}")),
    }
}

async fn save_store(path: &Path, store: &BilibiliStore) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("创建 B站状态目录失败: {error}"))?;
    }
    let temp = PathBuf::from(format!("{}.tmp", path.to_string_lossy()));
    let bytes = serde_json::to_vec_pretty(store)
        .map_err(|error| format!("序列化 B站上传状态失败: {error}"))?;
    fs::write(&temp, bytes)
        .await
        .map_err(|error| format!("写入 B站临时状态失败: {error}"))?;
    if fs::metadata(path).await.is_ok() {
        let backup = PathBuf::from(format!("{}.bak", path.to_string_lossy()));
        let _ = fs::copy(path, backup).await;
        fs::remove_file(path)
            .await
            .map_err(|error| format!("替换 B站状态失败: {error}"))?;
    }
    fs::rename(&temp, path)
        .await
        .map_err(|error| format!("提交 B站状态失败: {error}"))
}

pub(crate) async fn mutate_store<F, T>(app: &tauri::AppHandle, mutate: F) -> Result<T, String>
where
    F: FnOnce(&mut BilibiliStore) -> Result<T, String>,
{
    let _guard = gate().lock().await;
    let path = store_path(app)?;
    let mut store = read_store(&path).await?;
    let result = mutate(&mut store)?;
    save_store(&path, &store).await?;
    Ok(result)
}

pub(crate) async fn snapshot(app: &tauri::AppHandle) -> Result<BilibiliStore, String> {
    let _guard = gate().lock().await;
    read_store(&store_path(app)?).await
}

pub async fn enqueue_finalized_segment(
    app: &tauri::AppHandle,
    segment: &RecordingSegment,
    final_mp4_path: &str,
) -> Result<(), String> {
    let metadata = fs::metadata(final_mp4_path)
        .await
        .map_err(|error| format!("读取 B站待上传 MP4 失败: {error}"))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err("B站待上传 MP4 为空，拒绝入队。".to_string());
    }

    mutate_store(app, |store| {
        let session_pos = store
            .sessions
            .iter()
            .position(|session| session.live_session_id == segment.live_session_id);
        let session = if let Some(index) = session_pos {
            &mut store.sessions[index]
        } else {
            store.sessions.push(BilibiliSessionTask {
                live_session_id: segment.live_session_id.clone(),
                streamer_name: segment.streamer_name.clone(),
                room_url: segment.room_url.clone(),
                platform: segment.platform.clone(),
                session_started_at: segment.started_at,
                session_ended_at: None,
                recording_complete: false,
                aid: None,
                bvid: None,
                submission_state: "NOT_SUBMITTED".to_string(),
                segments: Vec::new(),
            });
            store.sessions.last_mut().expect("just inserted Bilibili session")
        };

        if session
            .segments
            .iter()
            .any(|item| item.segment_index == segment.segment_index)
        {
            return Ok(());
        }
        session.segments.push(BilibiliSegmentTask {
            live_session_id: segment.live_session_id.clone(),
            segment_index: segment.segment_index,
            local_path: final_mp4_path.to_string(),
            file_size: metadata.len(),
            started_at: segment.started_at,
            ended_at: segment.ended_at,
            state: BilibiliSegmentState::ReadyToUpload,
            remote_filename: None,
            retry_count: 0,
            next_retry_at: 0,
            last_error: None,
            local_deleted: false,
        });
        session.segments.sort_by_key(|item| item.segment_index);
        Ok(())
    })
    .await
}

pub async fn mark_session_recording_complete(
    app: &tauri::AppHandle,
    live_session_id: &str,
    ended_at: i64,
) -> Result<(), String> {
    mutate_store(app, |store| {
        if let Some(session) = store
            .sessions
            .iter_mut()
            .find(|session| session.live_session_id == live_session_id)
        {
            session.recording_complete = true;
            session.session_ended_at = Some(ended_at);
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn mobile_bilibili_status(app: tauri::AppHandle) -> Result<BilibiliStore, String> {
    snapshot(&app).await
}

#[tauri::command]
pub async fn mobile_bilibili_set_settings(
    app: tauri::AppHandle,
    auto_upload: bool,
    delete_after_success: bool,
) -> Result<BilibiliStore, String> {
    mutate_store(&app, |store| {
        store.settings.auto_upload = auto_upload;
        store.settings.delete_after_success = delete_after_success;
        Ok(store.clone())
    })
    .await
}
