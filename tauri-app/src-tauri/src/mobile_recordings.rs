use live_replay_core::recording::{RecordingPlan, RecordingSegment, record_live_session};
use live_replay_core::{
    CoreCredentials, ProbeResult, ResolvedStream, StopFlag, new_stop_flag, probe_stream, request_stop,
};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tauri::Manager;
use tauri_plugin_live_replay_android::{FinalizeMp4Request, LiveReplayAndroidExt};
use tokio::fs;
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep};

const MIN_START_FREE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const WARN_FREE_BYTES: u64 = 30 * 1024 * 1024 * 1024;
const JOURNAL_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);

#[derive(Default)]
struct MultiRecordingRuntime {
    recordings: HashMap<String, ActiveRecording>,
    last_file: Option<String>,
    last_error: Option<String>,
}

fn runtime() -> &'static Mutex<MultiRecordingRuntime> {
    static RUNTIME: OnceLock<Mutex<MultiRecordingRuntime>> = OnceLock::new();
    RUNTIME.get_or_init(|| Mutex::new(MultiRecordingRuntime::default()))
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

fn build_status(app: &tauri::AppHandle, state: &MultiRecordingRuntime) -> MultiRecordingStatus {
    let mut recordings: Vec<_> = state
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
        last_file: state.last_file.clone(),
        last_error: state.last_error.clone(),
        low_space_warning: available_bytes.is_some_and(|bytes| bytes < WARN_FREE_BYTES),
        available_bytes,
    }
}

#[tauri::command]
pub fn mobile_recordings_status(app: tauri::AppHandle) -> Result<MultiRecordingStatus, String> {
    status(&app)
}

pub fn status(app: &tauri::AppHandle) -> Result<MultiRecordingStatus, String> {
    let state = runtime()
        .lock()
        .map_err(|_| "Android 多路录制状态锁异常".to_string())?;
    Ok(build_status(app, &state))
}

pub fn is_recording(room_url: &str) -> Result<bool, String> {
    let state = runtime()
        .lock()
        .map_err(|_| "Android 多路录制状态锁异常".to_string())?;
    Ok(state.recordings.contains_key(room_url))
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
    if is_recording(&url)? {
        return status(&app);
    }

    let resolved = match probe_stream(&url, &display_name, credentials.clone()).await? {
        ProbeResult::Offline => return Err("主播当前未开播。".to_string()),
        ProbeResult::Live { stream } => stream,
    };
    start_recording_resolved(app, url, display_name, credentials, resolved).await
}

pub async fn start_recording_resolved(
    app: tauri::AppHandle,
    url: String,
    display_name: String,
    credentials: CoreCredentials,
    resolved: ResolvedStream,
) -> Result<MultiRecordingStatus, String> {
    start_recording_inner(app, url, display_name, credentials, resolved).await
}

async fn start_recording_inner(
    app: tauri::AppHandle,
    url: String,
    display_name: String,
    credentials: CoreCredentials,
    initial_stream: ResolvedStream,
) -> Result<MultiRecordingStatus, String> {
    let url = url.trim().to_string();
    if url.is_empty() {
        return Err("直播间地址为空。".to_string());
    }
    if is_recording(&url)? {
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

    let prepared =
        super::mobile_recording_journal::prepare_session(&app, &url, &display_name).await?;
    if let Some((stale_session_id, stale_ended_at)) = &prepared.stale_session {
        if let Err(error) = super::mobile_bilibili::mark_session_recording_complete(
            &app,
            stale_session_id,
            *stale_ended_at,
        )
        .await
        {
            return Err(format!(
                "关闭旧 liveSession 失败，为避免拆稿已停止新录制: {error}"
            ));
        }
    }

    let stop_flag = new_stop_flag();
    let started_at = prepared.session_started_at;
    {
        let mut state = runtime()
            .lock()
            .map_err(|_| "Android 多路录制状态锁异常".to_string())?;
        state.recordings.insert(
            url.clone(),
            ActiveRecording {
                room_url: url.clone(),
                display_name: display_name.clone(),
                current_file: output_dir.to_string_lossy().into_owned(),
                started_at,
                stop_flag: stop_flag.clone(),
            },
        );
        state.last_error = None;
    }
    if let Err(error) = super::mobile_monitor::sync_background_active(&app).await {
        set_last_error(format!("录像已启动，但更新 Android 后台保活状态失败: {error}"));
    }

    let worker_app = app.clone();
    let worker_url = url.clone();
    let worker_name = display_name.clone();
    tauri::async_runtime::spawn(async move {
        let (segment_tx, mut segment_rx) = mpsc::unbounded_channel::<RecordingSegment>();
        let segment_app = worker_app.clone();
        let segment_worker = tauri::async_runtime::spawn(async move {
            while let Some(segment) = segment_rx.recv().await {
                if let Err(error) = super::mobile_recording_journal::register_completed_segment(
                    &segment_app,
                    &segment.room_url,
                    &segment,
                )
                .await
                {
                    set_last_error(format!(
                        "P{} 已安全切出但 journal 持久化失败，停止后处理且保留源文件: {error}",
                        segment.segment_index
                    ));
                    continue;
                }

                match finalize_segment_mp4(&segment_app, &segment).await {
                    Ok(final_mp4) => {
                        if let Err(error) = super::mobile_bilibili::enqueue_finalized_segment(
                            &segment_app,
                            &segment,
                            &final_mp4,
                        )
                        .await
                        {
                            set_last_error(format!(
                                "P{} 已生成但加入 B站队列失败，本地文件及 journal 均保留: {error}",
                                segment.segment_index
                            ));
                        } else if let Err(error) =
                            super::mobile_recording_journal::complete_segment_handoff(
                                &segment_app,
                                &segment.live_session_id,
                                segment.segment_index,
                            )
                            .await
                        {
                            set_last_error(format!(
                                "P{} 已进入 B站队列，但清理 journal 标记失败，将在重启后安全复核: {error}",
                                segment.segment_index
                            ));
                        } else {
                            set_last_file(final_mp4);
                        }
                    }
                    Err(error) => {
                        set_last_error(format!(
                            "P{} MP4 收尾失败，源录像和 journal 均保留: {error}",
                            segment.segment_index
                        ));
                    }
                }
            }
        });

        let heartbeat_app = worker_app.clone();
        let heartbeat_url = worker_url.clone();
        let heartbeat_worker = tauri::async_runtime::spawn(async move {
            loop {
                sleep(JOURNAL_HEARTBEAT_INTERVAL).await;
                if let Err(error) =
                    super::mobile_recording_journal::heartbeat(&heartbeat_app, &heartbeat_url).await
                {
                    eprintln!("[recording] journal heartbeat failed: {error}");
                }
            }
        });

        let mut plan = RecordingPlan::new(
            worker_url.clone(),
            worker_name,
            credentials,
            output_dir,
        );
        plan.initial_stream = Some(initial_stream);
        plan.live_session_id = Some(prepared.live_session_id.clone());
        plan.session_started_at = Some(prepared.session_started_at);
        plan.next_segment_index = prepared.next_segment_index;
        plan.segment_started_at = Some(prepared.current_segment_started_at);
        let result = record_live_session(plan, stop_flag, Some(segment_tx)).await;
        heartbeat_worker.abort();

        match result {
            Ok(session) => {
                let _ = segment_worker.await;
                match super::mobile_bilibili::mark_session_recording_complete(
                    &worker_app,
                    &session.live_session_id,
                    session.ended_at,
                )
                .await
                {
                    Ok(()) => {
                        if let Err(error) =
                            super::mobile_recording_journal::finish_session(&worker_app, &worker_url)
                                .await
                        {
                            set_last_error(format!(
                                "B站 session 已标记结束，但清理 recording journal 失败: {error}"
                            ));
                        }
                    }
                    Err(error) => {
                        set_last_error(format!(
                            "保存 B站 session 完成状态失败，recording journal 保留: {error}"
                        ));
                    }
                }
                if let Some(error) = session.last_error {
                    set_last_error(error);
                }
            }
            Err(error) => {
                let _ = segment_worker.await;
                set_last_error(error);
            }
        }

        if let Ok(mut state) = runtime().lock() {
            state.recordings.remove(&worker_url);
        }
        if let Err(error) = super::mobile_monitor::sync_background_active(&worker_app).await {
            eprintln!("[recording] failed to refresh Android background activity state: {error}");
        }
    });

    status(&app)
}

pub(crate) async fn finalize_segment_mp4(
    app: &tauri::AppHandle,
    segment: &RecordingSegment,
) -> Result<String, String> {
    let source = PathBuf::from(&segment.source_path);
    let final_mp4 = PathBuf::from(&segment.final_mp4_path);

    if source == final_mp4 {
        verify_nonempty_file(&source).await?;
        sync_file(&source).await?;
        return Ok(source.to_string_lossy().into_owned());
    }

    let source_exists = fs::metadata(&source).await.is_ok();
    if fs::metadata(&final_mp4).await.is_ok() {
        if !source_exists {
            // Files created by this version only become visible at final_mp4 after an atomic
            // rename from .remuxing. Legacy installs may also reach this path after source cleanup.
            verify_nonempty_file(&final_mp4).await?;
            sync_file(&final_mp4).await?;
            return Ok(final_mp4.to_string_lossy().into_owned());
        }

        // Older builds wrote FFmpeg output directly to the final filename. If the source still
        // exists we cannot prove that MP4 survived a crash, so preserve it for inspection and
        // rebuild from the durable source instead of ever uploading an ambiguous partial file.
        let quarantine = PathBuf::from(format!(
            "{}.unverified-{}",
            final_mp4.to_string_lossy(),
            chrono::Utc::now().timestamp_millis()
        ));
        fs::rename(&final_mp4, &quarantine)
            .await
            .map_err(|error| format!("隔离旧版未确认 MP4 失败: {error}"))?;
    }

    verify_nonempty_file(&source).await?;
    let temp_mp4 = PathBuf::from(format!("{}.remuxing", final_mp4.to_string_lossy()));
    if fs::metadata(&temp_mp4).await.is_ok() {
        fs::remove_file(&temp_mp4)
            .await
            .map_err(|error| format!("清理上次未完成 MP4 临时文件失败: {error}"))?;
    }

    let result = app
        .live_replay_android()
        .finalize_mp4(FinalizeMp4Request {
            input_path: source.to_string_lossy().into_owned(),
            output_path: temp_mp4.to_string_lossy().into_owned(),
        })?;
    let produced = PathBuf::from(result.output_path);
    if produced != temp_mp4 {
        return Err(format!(
            "Android MP4 finalize 返回了意外路径: {}",
            produced.display()
        ));
    }
    verify_nonempty_file(&temp_mp4).await?;
    sync_file(&temp_mp4).await?;

    fs::rename(&temp_mp4, &final_mp4)
        .await
        .map_err(|error| format!("原子提交最终 MP4 失败: {error}"))?;
    sync_parent_dir(&final_mp4);
    verify_nonempty_file(&final_mp4).await?;

    if let Err(error) = fs::remove_file(&source).await {
        eprintln!("[recording] MP4 is safe; source container kept: {error}");
    }
    Ok(final_mp4.to_string_lossy().into_owned())
}

async fn verify_nonempty_file(path: &Path) -> Result<(), String> {
    let metadata = fs::metadata(path)
        .await
        .map_err(|error| format!("录像文件不存在 {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(format!("录像文件为空: {}", path.display()));
    }
    Ok(())
}

async fn sync_file(path: &Path) -> Result<(), String> {
    let file = fs::OpenOptions::new()
        .read(true)
        .open(path)
        .await
        .map_err(|error| format!("打开 MP4 同步失败: {error}"))?;
    file.sync_all()
        .await
        .map_err(|error| format!("同步 MP4 失败: {error}"))
}

fn sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
    }
}

fn set_last_file(path: String) {
    if let Ok(mut state) = runtime().lock() {
        state.last_file = Some(path);
        state.last_error = None;
    }
}

fn set_last_error(error: String) {
    if let Ok(mut state) = runtime().lock() {
        state.last_error = Some(error);
    }
}

#[tauri::command]
pub async fn mobile_stop_recording_multi(
    app: tauri::AppHandle,
    room_url: Option<String>,
) -> Result<MultiRecordingStatus, String> {
    let stopped_urls = {
        let state = runtime()
            .lock()
            .map_err(|_| "Android 多路录制状态锁异常".to_string())?;
        if let Some(room_url) = room_url.as_deref() {
            if let Some(recording) = state.recordings.get(room_url) {
                request_stop(&recording.stop_flag);
                vec![recording.room_url.clone()]
            } else {
                Vec::new()
            }
        } else {
            let urls = state.recordings.keys().cloned().collect::<Vec<_>>();
            for recording in state.recordings.values() {
                request_stop(&recording.stop_flag);
            }
            urls
        }
    };

    // Stopping a recording is scoped to the current live session. Keep the monitor enabled, but do
    // not immediately restart the same broadcast. Once an offline probe is observed, monitoring
    // automatically arms itself for the next live session.
    for url in stopped_urls {
        if let Err(error) = super::mobile_monitor::suppress_until_offline(&app, &url).await {
            set_last_error(format!("停止录像成功，但保存本场抑制状态失败: {error}"));
        }
    }
    status(&app)
}

pub fn request_stop_all() {
    if let Ok(state) = runtime().lock() {
        for recording in state.recordings.values() {
            request_stop(&recording.stop_flag);
        }
    }
}
