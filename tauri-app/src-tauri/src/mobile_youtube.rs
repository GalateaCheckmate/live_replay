use live_replay_core::RecordingResult;
use live_replay_core::youtube::{
    UploadStep, UploadTask, UploadTaskState, YoutubeStore, apply_upload_step,
    create_resumable_session, query_resumable_session, save_store_atomic, upload_next_chunk,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tauri::Manager;
use tauri_plugin_live_replay_android::{
    FinalizeMp4Request, LiveReplayAndroidExt, YoutubeAuthResult,
};
use tokio::fs;
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep};

fn store_gate() -> &'static Mutex<()> {
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(()))
}

fn store_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("youtube-upload-state.json"))
        .map_err(|error| format!("无法获取 YouTube 状态目录: {error}"))
}

async fn read_store(path: &Path) -> Result<YoutubeStore, String> {
    match fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("读取 YouTube 上传状态失败: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(YoutubeStore::default()),
        Err(error) => Err(format!("读取 YouTube 上传状态文件失败: {error}")),
    }
}

async fn mutate_store<F, T>(
    app: &tauri::AppHandle,
    mutate: F,
) -> Result<T, String>
where
    F: FnOnce(&mut YoutubeStore) -> Result<T, String>,
{
    let _guard = store_gate().lock().await;
    let path = store_path(app)?;
    let mut store = read_store(&path).await?;
    let result = mutate(&mut store)?;
    save_store_atomic(&path, &store).await?;
    Ok(result)
}

pub async fn finalize_recording_and_enqueue(
    app: &tauri::AppHandle,
    recording: RecordingResult,
    streamer_name: &str,
) -> Result<String, String> {
    let source = PathBuf::from(&recording.file_path);
    let final_mp4 = PathBuf::from(&recording.final_mp4_path);
    let final_path = if source == final_mp4 {
        verify_nonempty_file(&source).await?;
        source
    } else {
        let request = FinalizeMp4Request {
            input_path: source.to_string_lossy().into_owned(),
            output_path: final_mp4.to_string_lossy().into_owned(),
        };
        let result = app.live_replay_android().finalize_mp4(request)?;
        let produced = PathBuf::from(result.output_path);
        verify_nonempty_file(&produced).await?;
        sync_file(&produced).await?;
        // At this point a complete, synced MP4 exists. Failure to remove the source is harmless:
        // it leaves a duplicate local copy and never affects the upload/delete safety barrier.
        if let Err(error) = fs::remove_file(&source).await {
            eprintln!(
                "[youtube] finalized MP4 is safe; source container kept because deletion failed: {error}"
            );
        }
        produced
    };

    let metadata = fs::metadata(&final_path)
        .await
        .map_err(|error| format!("读取最终 MP4 失败: {error}"))?;
    let task = UploadTask::new(
        streamer_name.to_string(),
        final_path.to_string_lossy().into_owned(),
        recording.youtube_title,
        recording.started_at,
        recording.ended_at,
        metadata.len(),
    );
    mutate_store(app, |store| {
        store.add_task_if_absent(task);
        Ok(())
    })
    .await?;
    Ok(final_path.to_string_lossy().into_owned())
}

async fn verify_nonempty_file(path: &Path) -> Result<(), String> {
    let metadata = fs::metadata(path)
        .await
        .map_err(|error| format!("录像文件不存在: {error}"))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(format!("录像文件为空，拒绝上传或删除: {}", path.display()));
    }
    Ok(())
}

async fn sync_file(path: &Path) -> Result<(), String> {
    let file = fs::OpenOptions::new()
        .read(true)
        .open(path)
        .await
        .map_err(|error| format!("打开最终 MP4 进行同步失败: {error}"))?;
    file.sync_all()
        .await
        .map_err(|error| format!("同步最终 MP4 失败: {error}"))
}

#[tauri::command]
pub async fn mobile_youtube_authorize(
    app: tauri::AppHandle,
) -> Result<YoutubeAuthResult, String> {
    let auth = app.live_replay_android().authorize_youtube()?;
    if !auth.authorized || auth.access_token.as_deref().is_none_or(str::is_empty) {
        return Err("YouTube 授权没有返回有效 access token。".to_string());
    }
    let label = auth.account_label.clone();
    mutate_store(&app, |store| {
        store.settings.account_label = label;
        Ok(())
    })
    .await?;
    Ok(auth)
}

#[tauri::command]
pub async fn mobile_youtube_cached_auth(
    app: tauri::AppHandle,
) -> Result<YoutubeAuthResult, String> {
    let auth = app.live_replay_android().cached_youtube_auth()?;
    if auth.authorized {
        let label = auth.account_label.clone();
        mutate_store(&app, |store| {
            store.settings.account_label = label;
            Ok(())
        })
        .await?;
    }
    Ok(auth)
}

#[tauri::command]
pub async fn mobile_youtube_logout(app: tauri::AppHandle) -> Result<(), String> {
    app.live_replay_android().logout_youtube()?;
    mutate_store(&app, |store| {
        store.settings.account_label = None;
        store.settings.auto_upload = false;
        for task in &mut store.tasks {
            if task.state != UploadTaskState::UploadSuccess {
                task.state = UploadTaskState::AuthRequired;
                task.last_error = Some("YouTube 账号已退出；本地录像保留。".to_string());
            }
        }
        Ok(())
    })
    .await
}

#[derive(Debug, Clone, Serialize)]
pub struct YoutubeStatus {
    pub store: YoutubeStore,
    pub authorized: bool,
}

#[tauri::command]
pub async fn mobile_youtube_status(app: tauri::AppHandle) -> Result<YoutubeStatus, String> {
    let store = {
        let _guard = store_gate().lock().await;
        read_store(&store_path(&app)?).await?
    };
    let authorized = app
        .live_replay_android()
        .cached_youtube_auth()
        .map(|result| result.authorized && result.access_token.as_deref().is_some_and(|v| !v.is_empty()))
        .unwrap_or(false);
    Ok(YoutubeStatus { store, authorized })
}

#[tauri::command]
pub async fn mobile_youtube_set_settings(
    app: tauri::AppHandle,
    auto_upload: bool,
    delete_after_success: bool,
) -> Result<YoutubeStore, String> {
    mutate_store(&app, |store| {
        store.settings.auto_upload = auto_upload;
        store.settings.privacy_status = "private".to_string();
        store.settings.delete_after_success = delete_after_success;
        Ok(store.clone())
    })
    .await
}

#[tauri::command]
pub async fn mobile_youtube_enqueue_mp4(
    app: tauri::AppHandle,
    path: String,
    title: Option<String>,
    streamer_name: Option<String>,
) -> Result<YoutubeStore, String> {
    let file_path = PathBuf::from(path.trim());
    if file_path.extension().and_then(|value| value.to_str()).map(|value| value.eq_ignore_ascii_case("mp4")) != Some(true) {
        return Err("当前 YouTube 手动验证只接受完整 MP4。".to_string());
    }
    verify_nonempty_file(&file_path).await?;
    let metadata = fs::metadata(&file_path)
        .await
        .map_err(|error| format!("读取 MP4 失败: {error}"))?;
    let now = chrono::Utc::now().timestamp();
    let fallback_title = file_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Live Replay")
        .replace('：', ":");
    let task = UploadTask::new(
        streamer_name.unwrap_or_else(|| "Live Replay".to_string()),
        file_path.to_string_lossy().into_owned(),
        title.filter(|value| !value.trim().is_empty()).unwrap_or(fallback_title),
        now,
        now,
        metadata.len(),
    );
    mutate_store(&app, |store| {
        store.add_task_if_absent(task);
        Ok(store.clone())
    })
    .await
}

#[tauri::command]
pub async fn mobile_youtube_retry_task(
    app: tauri::AppHandle,
    task_id: String,
) -> Result<YoutubeStore, String> {
    mutate_store(&app, |store| {
        let task = store
            .tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .ok_or_else(|| "YouTube 上传任务不存在。".to_string())?;
        if task.state == UploadTaskState::UploadResultUnknown {
            return Err(
                "这个任务的远端结果不明确。为防重复视频，不能自动重建上传 session。"
                    .to_string(),
            );
        }
        if task.state != UploadTaskState::UploadSuccess {
            task.state = UploadTaskState::RetryPending;
            task.next_retry_at = chrono::Utc::now().timestamp();
            task.last_error = None;
        }
        Ok(store.clone())
    })
    .await
}

pub fn start_upload_worker(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = recover_after_restart(&app).await {
            eprintln!("[youtube] recovery failed: {error}");
        }
        loop {
            if let Err(error) = upload_one_step(&app).await {
                eprintln!("[youtube] worker error: {error}");
            }
            sleep(Duration::from_secs(2)).await;
        }
    });
}

async fn recover_after_restart(app: &tauri::AppHandle) -> Result<(), String> {
    let _guard = store_gate().lock().await;
    let path = store_path(app)?;
    let mut store = read_store(&path).await?;
    store.normalize_after_restart();
    save_store_atomic(path, &store).await
}

async fn upload_one_step(app: &tauri::AppHandle) -> Result<(), String> {
    // Finish a previously-confirmed safe deletion first. This can never trigger a re-upload.
    if let Some(task_id) = find_pending_safe_delete(app).await? {
        safe_delete_successful_task(app, &task_id).await?;
        return Ok(());
    }

    let now = chrono::Utc::now().timestamp();
    let (task, privacy, auto_upload) = {
        let _guard = store_gate().lock().await;
        let store = read_store(&store_path(app)?).await?;
        let task = store
            .tasks
            .iter()
            .find(|task| {
                task.next_retry_at <= now
                    && matches!(
                        task.state,
                        UploadTaskState::ReadyToUpload
                            | UploadTaskState::Uploading
                            | UploadTaskState::WaitingForNetwork
                            | UploadTaskState::RetryPending
                            | UploadTaskState::AuthRequired
                    )
            })
            .cloned();
        (task, store.settings.privacy_status.clone(), store.settings.auto_upload)
    };
    if !auto_upload {
        return Ok(());
    }
    let Some(mut task) = task else {
        return Ok(());
    };
    verify_nonempty_file(Path::new(&task.local_path)).await?;

    let auth = match app.live_replay_android().cached_youtube_auth() {
        Ok(auth) if auth.authorized && auth.access_token.as_deref().is_some_and(|token| !token.is_empty()) => auth,
        Ok(_) => {
            set_task_auth_required(app, &task.id, "YouTube 需要重新授权；录像保留。").await?;
            return Ok(());
        }
        Err(error) => {
            set_task_auth_required(app, &task.id, &format!("读取 YouTube 授权失败: {error}"))
                .await?;
            return Ok(());
        }
    };
    let token = auth.access_token.as_deref().expect("validated access token");

    if task.resumable_session_url.is_none() {
        // Persist the ambiguous-creation barrier before the network POST. If the process dies now,
        // restart recovery will refuse to create a second YouTube video automatically.
        mark_session_creation_started(app, &task.id).await?;
        match create_resumable_session(token, &task, &privacy).await {
            Ok(session_url) => {
                save_session_url(app, &task.id, session_url).await?;
                return Ok(());
            }
            Err(step) => {
                apply_step_and_persist(app, &task.id, step).await?;
                return Ok(());
            }
        }
    }

    let session_url = task.resumable_session_url.clone().unwrap();
    if task.state != UploadTaskState::Uploading || task.confirmed_bytes == 0 {
        let step = query_resumable_session(token, &session_url, task.file_size).await;
        let stop = !matches!(step, UploadStep::Progress(_));
        apply_step_and_persist(app, &task.id, step).await?;
        if stop {
            return Ok(());
        }
        task = load_task(app, &task.id).await?;
        if task.confirmed_bytes >= task.file_size {
            return Ok(());
        }
    }

    let step = upload_next_chunk(
        token,
        &session_url,
        &task.local_path,
        task.confirmed_bytes,
        task.file_size,
    )
    .await;
    let success = matches!(step, UploadStep::Success(_));
    apply_step_and_persist(app, &task.id, step).await?;
    if success {
        safe_delete_successful_task(app, &task.id).await?;
    }
    Ok(())
}

async fn load_task(app: &tauri::AppHandle, task_id: &str) -> Result<UploadTask, String> {
    let _guard = store_gate().lock().await;
    let store = read_store(&store_path(app)?).await?;
    store
        .tasks
        .into_iter()
        .find(|task| task.id == task_id)
        .ok_or_else(|| "YouTube 上传任务不存在。".to_string())
}

async fn mark_session_creation_started(app: &tauri::AppHandle, task_id: &str) -> Result<(), String> {
    mutate_store(app, |store| {
        let task = store
            .tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .ok_or_else(|| "YouTube 上传任务不存在。".to_string())?;
        task.state = UploadTaskState::Uploading;
        task.resumable_session_url = None;
        task.last_error = None;
        task.updated_at = chrono::Utc::now().timestamp();
        Ok(())
    })
    .await
}

async fn save_session_url(
    app: &tauri::AppHandle,
    task_id: &str,
    session_url: String,
) -> Result<(), String> {
    mutate_store(app, |store| {
        let task = store
            .tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .ok_or_else(|| "YouTube 上传任务不存在。".to_string())?;
        task.resumable_session_url = Some(session_url);
        task.state = UploadTaskState::Uploading;
        task.confirmed_bytes = 0;
        task.last_error = None;
        task.updated_at = chrono::Utc::now().timestamp();
        Ok(())
    })
    .await
}

async fn set_task_auth_required(
    app: &tauri::AppHandle,
    task_id: &str,
    message: &str,
) -> Result<(), String> {
    mutate_store(app, |store| {
        let task = store
            .tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .ok_or_else(|| "YouTube 上传任务不存在。".to_string())?;
        task.state = UploadTaskState::AuthRequired;
        task.last_error = Some(message.to_string());
        task.next_retry_at = chrono::Utc::now().timestamp() + 60;
        Ok(())
    })
    .await
}

async fn apply_step_and_persist(
    app: &tauri::AppHandle,
    task_id: &str,
    step: UploadStep,
) -> Result<(), String> {
    mutate_store(app, |store| {
        let task = store
            .tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .ok_or_else(|| "YouTube 上传任务不存在。".to_string())?;
        apply_upload_step(task, step);
        Ok(())
    })
    .await
}

async fn find_pending_safe_delete(app: &tauri::AppHandle) -> Result<Option<String>, String> {
    let _guard = store_gate().lock().await;
    let store = read_store(&store_path(app)?).await?;
    if !store.settings.delete_after_success {
        return Ok(None);
    }
    Ok(store
        .tasks
        .iter()
        .find(|task| task.has_confirmed_success() && !task.local_deleted)
        .map(|task| task.id.clone()))
}

async fn safe_delete_successful_task(app: &tauri::AppHandle, task_id: &str) -> Result<(), String> {
    let _guard = store_gate().lock().await;
    let path = store_path(app)?;
    let mut store = read_store(&path).await?;
    let task_index = store
        .tasks
        .iter()
        .position(|task| task.id == task_id)
        .ok_or_else(|| "YouTube 上传任务不存在。".to_string())?;

    {
        let task = &mut store.tasks[task_index];
        if task.state != UploadTaskState::UploadSuccess
            || task.youtube_video_id.as_deref().is_none_or(str::is_empty)
        {
            return Err("YouTube success/videoId 未确认，拒绝删除本地录像。".to_string());
        }
        task.success_persisted = true;
        task.updated_at = chrono::Utc::now().timestamp();
    }

    // Critical safety barrier: durable SUCCESS + videoId first.
    save_store_atomic(&path, &store).await?;
    if !store.settings.delete_after_success {
        return Ok(());
    }

    let local_path = PathBuf::from(&store.tasks[task_index].local_path);
    match fs::remove_file(&local_path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "YouTube 已确认成功，但删除本地录像失败；录像继续保留: {error}"
            ));
        }
    }
    store.tasks[task_index].local_deleted = true;
    store.tasks[task_index].updated_at = chrono::Utc::now().timestamp();
    // Failure here is safe: SUCCESS was already durable, so restart can only retry deletion.
    save_store_atomic(path, &store).await
}
