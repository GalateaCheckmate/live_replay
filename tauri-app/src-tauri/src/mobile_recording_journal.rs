use live_replay_core::recording::{LIVE_OFFLINE_GRACE, RecordingSegment};
use serde::{Deserialize, Serialize};
use std::io::Write;
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
    #[serde(default)]
    pub session_started_at: i64,
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
    pub session_started_at: i64,
    pub next_segment_index: u32,
    pub current_segment_started_at: i64,
    pub stale_session: Option<(String, i64)>,
}

fn store_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("recording-journal.json"))
        .map_err(|error| format!("无法获取录像恢复状态目录: {error}"))
}

fn backup_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.bak", path.to_string_lossy()))
}

async fn decode_store(path: &Path) -> Result<RecordingJournalStore, String> {
    let bytes = fs::read(path)
        .await
        .map_err(|error| format!("读取录像恢复状态文件失败 {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("解析录像恢复状态失败 {}: {error}", path.display()))
}

async fn read_store(path: &Path) -> Result<RecordingJournalStore, String> {
    match decode_store(path).await {
        Ok(store) => Ok(store),
        Err(primary_error) => {
            let primary_missing = fs::metadata(path)
                .await
                .err()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound);
            let backup = backup_path(path);
            match decode_store(&backup).await {
                Ok(store) => Ok(store),
                Err(backup_error) if primary_missing => {
                    let backup_missing = fs::metadata(&backup)
                        .await
                        .err()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound);
                    if backup_missing {
                        Ok(RecordingJournalStore::default())
                    } else {
                        Err(backup_error)
                    }
                }
                Err(backup_error) => Err(format!(
                    "录像恢复状态主文件与备份均不可用；主文件: {primary_error}; 备份: {backup_error}"
                )),
            }
        }
    }
}

fn sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
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
    {
        let mut file = std::fs::File::create(&temp)
            .map_err(|error| format!("创建录像恢复临时状态失败: {error}"))?;
        file.write_all(&bytes)
            .map_err(|error| format!("写入录像恢复临时状态失败: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("同步录像恢复临时状态失败: {error}"))?;
    }

    if fs::metadata(path).await.is_ok() {
        let backup = backup_path(path);
        let backup_temp = PathBuf::from(format!("{}.tmp", backup.to_string_lossy()));
        fs::copy(path, &backup_temp)
            .await
            .map_err(|error| format!("创建录像恢复状态备份失败: {error}"))?;
        if let Ok(file) = std::fs::OpenOptions::new().read(true).open(&backup_temp) {
            file.sync_all()
                .map_err(|error| format!("同步录像恢复状态备份失败: {error}"))?;
        }
        if fs::metadata(&backup).await.is_ok() {
            let _ = fs::remove_file(&backup).await;
        }
        fs::rename(&backup_temp, &backup)
            .await
            .map_err(|error| format!("提交录像恢复状态备份失败: {error}"))?;
    }

    fs::rename(&temp, path)
        .await
        .map_err(|error| format!("提交录像恢复状态文件失败: {error}"))?;
    sync_parent_dir(path);
    Ok(())
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
        if let Some(position) = store
            .sessions
            .iter()
            .position(|item| item.room_url == room_url)
        {
            let existing = store.sessions[position].clone();
            if now.saturating_sub(existing.last_heartbeat_at) <= grace {
                let session = &mut store.sessions[position];
                session.streamer_name = streamer_name.to_string();
                if session.session_started_at <= 0 {
                    session.session_started_at = session.current_segment_started_at.max(1);
                }
                session.last_heartbeat_at = now;
                return Ok(PreparedRecordingSession {
                    live_session_id: session.live_session_id.clone(),
                    session_started_at: session.session_started_at,
                    next_segment_index: session.next_segment_index.max(1),
                    current_segment_started_at: session.current_segment_started_at,
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
            session_started_at: now,
            next_segment_index: 1,
            current_segment_started_at: now,
            last_heartbeat_at: now,
        });
        Ok(PreparedRecordingSession {
            live_session_id,
            session_started_at: now,
            next_segment_index: 1,
            current_segment_started_at: now,
            stale_session,
        })
    })
    .await
}

pub async fn heartbeat(app: &tauri::AppHandle, room_url: &str) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp();
    mutate_store(app, |store| {
        if let Some(session) = store
            .sessions
            .iter_mut()
            .find(|item| item.room_url == room_url)
        {
            session.last_heartbeat_at = now;
        }
        Ok(())
    })
    .await
}

fn register_segment_in_store(
    store: &mut RecordingJournalStore,
    room_url: &str,
    segment: &RecordingSegment,
    heartbeat_override: Option<i64>,
) {
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
    if let Some(session) = store
        .sessions
        .iter_mut()
        .find(|item| item.room_url == room_url)
    {
        session.next_segment_index = session
            .next_segment_index
            .max(segment.segment_index.saturating_add(1));
        session.current_segment_started_at = segment.ended_at;
        session.last_heartbeat_at =
            heartbeat_override.unwrap_or_else(|| chrono::Utc::now().timestamp());
    }
}

pub async fn register_completed_segment(
    app: &tauri::AppHandle,
    room_url: &str,
    segment: &RecordingSegment,
) -> Result<(), String> {
    mutate_store(app, |store| {
        register_segment_in_store(store, room_url, segment, None);
        Ok(())
    })
    .await
}

/// Startup recovery must advance P numbering without pretending the recorder was alive during the
/// downtime. Preserving the pre-crash heartbeat lets `close_stale_sessions` correctly decide
/// whether the old liveSession has exceeded the offline grace period.
pub async fn register_recovered_segment(
    app: &tauri::AppHandle,
    room_url: &str,
    segment: &RecordingSegment,
    preserved_heartbeat_at: i64,
) -> Result<(), String> {
    mutate_store(app, |store| {
        register_segment_in_store(
            store,
            room_url,
            segment,
            Some(preserved_heartbeat_at),
        );
        Ok(())
    })
    .await
}

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
