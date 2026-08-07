use live_replay_core::recording::{LIVE_OFFLINE_GRACE, RecordingSegment};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::Manager;
use tokio::fs;
use tokio::sync::Mutex;

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

fn gate() -> &'static Mutex<()> {
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingJournalSession {
    pub room_url: String,
    pub streamer_name: String,
    pub live_session_id: String,
    pub next_segment_index: u32,
    pub current_segment_started_at: i64,
    pub last_heartbeat_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecordingJournalStore {
    #[serde(default)]
    pub sessions: Vec<RecordingJournalSession>,
    #[serde(default)]
    pub pending_segments: Vec<RecordingSegment>,
}

#[derive(Debug, Clone)]
pub struct PreparedRecordingSession {
    pub live_session_id: String,
    pub next_segment_index: u32,
    pub stale_session: Option<(String, i64)>,
}

fn store_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("recording-journal.json"))
        .map_err(|error| format!("无法获取录像恢复状态目录: {error}"))
}

async fn read_store(path: &Path) -> Result<RecordingJournalStore, String> {
    match fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("读取录像恢复状态失败: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(RecordingJournalStore::default())
        }
        Err(error) => Err(format!("读取录像恢复状态文件失败: {error}")),
    }
}

async fn save_store(path: &Path, store: &RecordingJournalStore) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("创建录像恢复状态目录失败: {error}"))?;
    }
    let temp = PathBuf::from(format!("{}.tmp", path.to_string_lossy()));
    let bytes = serde_json::to_vec_pretty(store)
        .map_err(|error| format!("序列化录像恢复状态失败: {error}"))?;
    fs::write(&temp, bytes)
        .await
        .map_err(|error| format!("写入录像恢复临时状态失败: {error}"))?;
    if fs::metadata(path).await.is_ok() {
        let backup = PathBuf::from(format!("{}.bak", path.to_string_lossy()));
        let _ = fs::copy(path, backup).await;
        fs::remove_file(path)
            .await
            .map_err(|error| format!("替换录像恢复状态失败: {error}"))?;
    }
    fs::rename(&temp, path)
        .await
        .map_err(|error| format!("提交录像恢复状态失败: {error}"))
}

async fn mutate_store<F, T>(app: &tauri::AppHandle, mutate: F) -> Result<T, String>
where
    F: FnOnce(&mut RecordingJournalStore) -> Result<T, String>,
{
    let _guard = gate().lock().await;
    let path = store_path(app)?;
    let mut store = read_store(&path).await?;
    let result = mutate(&mut store)?;
    save_store(&path, &store).await?;
    Ok(result)
}

pub async fn snapshot(app: &tauri::AppHandle) -> Result<RecordingJournalStore, String> {
    let _guard = gate().lock().await;
    read_store(&store_path(app)?).await
}

fn new_session_id() -> String {
    let now = chrono::Utc::now().timestamp_millis();
    let seq = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("android-{now}-{seq}")
}

pub async fn prepare_session(
    app: &tauri::AppHandle,
    room_url: &str,
    streamer_name: &str,
) -> Result<PreparedRecordingSession, String> {
    let now = chrono::Utc::now().timestamp();
    let grace = LIVE_OFFLINE_GRACE.as_secs() as i64;
    mutate_store(app, |store| {
        let mut stale_session = None;
        if let Some(position) = store.sessions.iter().position(|item| item.room_url == room_url) {
            let existing = store.sessions[position].clone();
            if now.saturating_sub(existing.last_heartbeat_at) <= grace {
                let session = &mut store.sessions[position];
                session.streamer_name = streamer_name.to_string();
                session.last_heartbeat_at = now;
                return Ok(PreparedRecordingSession {
                    live_session_id: session.live_session_id.clone(),
                    next_segment_index: session.next_segment_index.max(1),
                    stale_session: None,
                });
            }
            stale_session = Some((existing.live_session_id, existing.last_heartbeat_at));
            store.sessions.remove(position);
        }

        let live_session_id = new_session_id();
        store.sessions.push(RecordingJournalSession {
            room_url: room_url.to_string(),
            streamer_name: streamer_name.to_string(),
            live_session_id: live_session_id.clone(),
            next_segment_index: 1,
            current_segment_started_at: now,
            last_heartbeat_at: now,
        });
        Ok(PreparedRecordingSession {
            live_session_id,
            next_segment_index: 1,
            stale_session,
        })
    })
    .await
}

pub async fn heartbeat(app: &tauri::AppHandle, room_url: &str) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp();
    mutate_store(app, |store| {
        if let Some(session) = store.sessions.iter_mut().find(|item| item.room_url == room_url) {
            session.last_heartbeat_at = now;
        }
        Ok(())
    })
    .await
}

/// Persist a completed raw segment before MP4 remux or uploader enqueue starts. This is the
/// hand-off barrier: after this write succeeds, a process crash can always retry the segment.
pub async fn register_completed_segment(
    app: &tauri::AppHandle,
    room_url: &str,
    segment: &RecordingSegment,
) -> Result<(), String> {
    mutate_store(app, |store| {
        if !store.pending_segments.iter().any(|item| {
            item.live_session_id == segment.live_session_id
                && item.segment_index == segment.segment_index
        }) {
            store.pending_segments.push(segment.clone());
            store.pending_segments.sort_by(|a, b| {
                a.live_session_id
                    .cmp(&b.live_session_id)
                    .then(a.segment_index.cmp(&b.segment_index))
            });
        }
        if let Some(session) = store.sessions.iter_mut().find(|item| item.room_url == room_url) {
            session.next_segment_index = session
                .next_segment_index
                .max(segment.segment_index.saturating_add(1));
            session.current_segment_started_at = segment.ended_at;
            session.last_heartbeat_at = chrono::Utc::now().timestamp();
        }
        Ok(())
    })
    .await
}

/// Remove only after the finalized MP4 is durably present and the Bilibili persistent queue has
/// accepted the segment. A crash before this call leaves the segment recoverable.
pub async fn complete_segment_handoff(
    app: &tauri::AppHandle,
    live_session_id: &str,
    segment_index: u32,
) -> Result<(), String> {
    mutate_store(app, |store| {
        store.pending_segments.retain(|item| {
            !(item.live_session_id == live_session_id && item.segment_index == segment_index)
        });
        Ok(())
    })
    .await
}

pub async fn finish_session(app: &tauri::AppHandle, room_url: &str) -> Result<(), String> {
    mutate_store(app, |store| {
        store.sessions.retain(|item| item.room_url != room_url);
        Ok(())
    })
    .await
}

pub async fn advance_recovered_segment(
    app: &tauri::AppHandle,
    live_session_id: &str,
    next_segment_index: u32,
    next_started_at: i64,
) -> Result<(), String> {
    mutate_store(app, |store| {
        if let Some(session) = store
            .sessions
            .iter_mut()
            .find(|item| item.live_session_id == live_session_id)
        {
            session.next_segment_index = session.next_segment_index.max(next_segment_index);
            session.current_segment_started_at = next_started_at;
        }
        Ok(())
    })
    .await
}
