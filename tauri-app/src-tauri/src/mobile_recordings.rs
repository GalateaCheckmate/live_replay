use live_replay_core::{
    new_stop_flag, probe_stream, record_direct_stream, request_stop, CoreCredentials, ProbeResult,
    StopFlag,
};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::Manager;

const MIN_START_FREE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const WARN_FREE_BYTES: u64 = 30 * 1024 * 1024 * 1024;

#[derive(Default)]
pub struct MultiRecordingState {
    runtime: Mutex<MultiRecordingRuntime>,
}

#[derive(Default)]
struct MultiRecordingRuntime {
    recordings: HashMap<String, ActiveRecording>,
    last_file: Option<String>,
    last_error: Option<String>,
}

struct ActiveRecording {
    room_url: String,
    display_name: String,
    current_file: String,
    started_at: i64,
    stop_flag: StopFlag,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordingStatusItem {
    pub room_url: String,
    pub display_name: String,
    pub current_file: String,
    pub started_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MultiRecordingStatus {
    pub active: bool,
    pub active_count: usize,
    pub recordings: Vec<RecordingStatusItem>,
    pub last_file: Option<String>,
    pub last_error: Option<String>,
    pub available_bytes: Option<u64>,
    pub low_space_warning: bool,
}

fn recordings_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("recordings"))
        .map_err(|error| format!("无法获取 Android App 数据目录: {error}"))
}

fn available_space(app: &tauri::AppHandle) -> Option<u64> {
    let dir = recordings_dir(app).ok()?;
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    fs2::available_space(dir).ok()
}

fn build_status(app: &tauri::AppHandle, runtime: &MultiRecordingRuntime) -> MultiRecordingStatus {
    let mut recordings: Vec<_> = runtime
        .recordings
        .values()
        .map(|recording| RecordingStatusItem {
            room_url: recording.room_url.clone(),
            display_name: recording.display_name.clone(),
            current_file: recording.current_file.clone(),
            started_at: recording.started_at,
        })
        .collect();
    recordings.sort_by_key(|item| item.started_at);
    let available_bytes = available_space(app);
    MultiRecordingStatus {
        active: !recordings.is_empty(),
        active_count: recordings.len(),
        recordings,
        last_file: runtime.last_file.clone(),
        last_error: runtime.last_error.clone(),
        low_space_warning: available_bytes.is_some_and(|bytes| bytes < WARN_FREE_BYTES),
        available_bytes,
    }
}

#[tauri::command]
pub fn mobile_recordings_status(app: tauri::AppHandle) -> Result<MultiRecordingStatus, String> {
    status(&app)
}

pub fn status(app: &tauri::AppHandle) -> Result<MultiRecordingStatus, String> {
    let state = app.state::<MultiRecordingState>();
    let runtime = state
        .runtime
        .lock()
        .map_err(|_| "Android 多路录制状态锁异常".to_string())?;
    Ok(build_status(app, &runtime))
}

pub fn is_recording(app: &tauri::AppHandle, room_url: &str) -> Result<bool, String> {
    let state = app.state::<MultiRecordingState>();
    let runtime = state
        .runtime
        .lock()
        .map_err(|_| "Android 多路录制状态锁异常".to_string())?;
    Ok(runtime.recordings.contains_key(room_url))
}

#[tauri::command]
pub async fn mobile_start_recording_multi(
    app: tauri::AppHandle,
    url: String,
    name: Option<String>,
    bilibili_cookie: Option<String>,
    douyin_cookie: Option<String>,
) -> Result<MultiRecordingStatus, String> {
    start_recording(
        app,
        url,
        name.unwrap_or_else(|| "Live Replay".to_string()),
        CoreCredentials {
            bilibili_cookie,
            douyin_cookie,
        },
    )
    .await
}

pub async fn start_recording(
    app: tauri::AppHandle,
    url: String,
    display_name: String,
    credentials: CoreCredentials,
) -> Result<MultiRecordingStatus, String> {
    let url = url.trim().to_string();
    if url.is_empty() {
        return Err("直播间地址为空。".to_string());
    }

    if is_recording(&app, &url)? {
        return status(&app);
    }

    let output_dir = recordings_dir(&app)?;
    std::fs::create_dir_all(&output_dir)
        .map_err(|error| format!("创建录制目录失败: {error}"))?;
    let free = fs2::available_space(&output_dir)
        .map_err(|error| format!("检查可用磁盘空间失败: {error}"))?;
    if free < MIN_START_FREE_BYTES {
        return Err(format!(
            "可用存储空间不足 10GB（当前约 {:.1}GB），拒绝启动新的录像；已有录像不会被强制中断。",
            free as f64 / 1024.0 / 1024.0 / 1024.0
        ));
    }

    let resolved = match probe_stream(&url, &display_name, credentials).await? {
        ProbeResult::Offline => return Err("主播当前未开播。".to_string()),
        ProbeResult::Live { stream } => stream,
    };

    // Re-check after network probing so two monitor ticks cannot start the same room twice.
    if is_recording(&app, &url)? {
        return status(&app);
    }

    let stop_flag = new_stop_flag();
    let started_at = chrono::Utc::now().timestamp();
    {
        let state = app.state::<MultiRecordingState>();
        let mut runtime = state
            .runtime
            .lock()
            .map_err(|_| "Android 多路录制状态锁异常".to_string())?;
        runtime.recordings.insert(
            url.clone(),
            ActiveRecording {
                room_url: url.clone(),
                display_name: display_name.clone(),
                current_file: output_dir.to_string_lossy().into_owned(),
                started_at,
                stop_flag: stop_flag.clone(),
            },
        );
        runtime.last_error = None;
    }

    let worker_app = app.clone();
    let worker_url = url.clone();
    let worker_name = display_name.clone();
    tauri::async_runtime::spawn(async move {
        let result = match record_direct_stream(resolved, &output_dir, stop_flag).await {
            Ok(recording) => {
                super::mobile_youtube::finalize_recording_and_enqueue(
                    &worker_app,
                    recording,
                    &worker_name,
                )
                .await
            }
            Err(error) => Err(error),
        };

        let state = worker_app.state::<MultiRecordingState>();
        if let Ok(mut runtime) = state.runtime.lock() {
            runtime.recordings.remove(&worker_url);
            match result {
                Ok(final_mp4) => {
                    runtime.last_file = Some(final_mp4);
                    runtime.last_error = None;
                }
                Err(error) => {
                    runtime.last_error = Some(error);
                }
            }
        }
    });

    status(&app)
}

#[tauri::command]
pub fn mobile_stop_recording_multi(
    app: tauri::AppHandle,
    room_url: Option<String>,
) -> Result<MultiRecordingStatus, String> {
    let state = app.state::<MultiRecordingState>();
    {
        let runtime = state
            .runtime
            .lock()
            .map_err(|_| "Android 多路录制状态锁异常".to_string())?;
        if let Some(room_url) = room_url.as_deref() {
            if let Some(recording) = runtime.recordings.get(room_url) {
                request_stop(&recording.stop_flag);
            }
        } else {
            for recording in runtime.recordings.values() {
                request_stop(&recording.stop_flag);
            }
        }
    }
    status(&app)
}

pub fn request_stop_all(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<MultiRecordingState>() {
        if let Ok(runtime) = state.runtime.lock() {
            for recording in runtime.recordings.values() {
                request_stop(&recording.stop_flag);
            }
        }
    }
}
