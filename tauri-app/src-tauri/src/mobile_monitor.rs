use live_replay_core::{CoreCredentials, ProbeResult, probe_stream};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::Manager;
#[cfg(target_os = "android")]
use tauri_plugin_live_replay_android::LiveReplayAndroidExt;
use tokio::fs;
use tokio::sync::Mutex;
use tokio::task::JoinSet;
use tokio::time::{Duration, sleep};

static TARGET_COUNTER: AtomicU64 = AtomicU64::new(0);

fn gate() -> &'static Mutex<()> {
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorTarget {
    pub id: String,
    pub url: String,
    pub name: String,
    pub enabled: bool,
    #[serde(default)]
    pub suppress_until_offline: bool,
    pub last_state: String,
    pub last_error: Option<String>,
    pub last_checked_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MonitorStore {
    #[serde(default)]
    pub targets: Vec<MonitorTarget>,
}

fn store_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("live-monitor-state.json"))
        .map_err(|error| format!("无法获取监控状态目录: {error}"))
}

fn backup_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.bak", path.to_string_lossy()))
}

async fn decode_store(path: &Path) -> Result<MonitorStore, String> {
    let bytes = fs::read(path)
        .await
        .map_err(|error| format!("读取主播监控状态文件失败 {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("解析主播监控状态失败 {}: {error}", path.display()))
}

async fn read_store(path: &Path) -> Result<MonitorStore, String> {
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
                        Ok(MonitorStore::default())
                    } else {
                        Err(backup_error)
                    }
                }
                Err(backup_error) => Err(format!(
                    "主播监控状态主文件与备份均不可用；主文件: {primary_error}; 备份: {backup_error}"
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

async fn save_store(path: &Path, store: &MonitorStore) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("创建监控状态目录失败: {error}"))?;
    }
    let temp = PathBuf::from(format!("{}.tmp", path.to_string_lossy()));
    let bytes = serde_json::to_vec_pretty(store)
        .map_err(|error| format!("序列化主播监控状态失败: {error}"))?;
    {
        let mut file = std::fs::File::create(&temp)
            .map_err(|error| format!("创建监控临时状态失败: {error}"))?;
        file.write_all(&bytes)
            .map_err(|error| format!("写入监控临时状态失败: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("同步监控临时状态失败: {error}"))?;
    }

    if fs::metadata(path).await.is_ok() {
        let backup = backup_path(path);
        let backup_temp = PathBuf::from(format!("{}.tmp", backup.to_string_lossy()));
        fs::copy(path, &backup_temp)
            .await
            .map_err(|error| format!("创建监控状态备份失败: {error}"))?;
        if let Ok(file) = std::fs::OpenOptions::new().read(true).open(&backup_temp) {
            file.sync_all()
                .map_err(|error| format!("同步监控状态备份失败: {error}"))?;
        }
        if fs::metadata(&backup).await.is_ok() {
            let _ = fs::remove_file(&backup).await;
        }
        fs::rename(&backup_temp, &backup)
            .await
            .map_err(|error| format!("提交监控状态备份失败: {error}"))?;
    }

    fs::rename(&temp, path)
        .await
        .map_err(|error| format!("提交监控状态文件失败: {error}"))?;
    sync_parent_dir(path);
    Ok(())
}

async fn mutate_store<F, T>(app: &tauri::AppHandle, mutate: F) -> Result<T, String>
where
    F: FnOnce(&mut MonitorStore) -> Result<T, String>,
{
    let _guard = gate().lock().await;
    let path = store_path(app)?;
    let mut store = read_store(&path).await?;
    let result = mutate(&mut store)?;
    save_store(&path, &store).await?;
    Ok(result)
}

pub(crate) async fn sync_background_active(app: &tauri::AppHandle) -> Result<(), String> {
    let monitor_active = {
        let _guard = gate().lock().await;
        read_store(&store_path(app)?)
            .await?
            .targets
            .iter()
            .any(|target| target.enabled)
    };
    let recording_active = super::mobile_recordings::status(app)?.active;
    let active = monitor_active || recording_active;
    #[cfg(target_os = "android")]
    {
        app.live_replay_android().set_background_active(active)?;
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = active;
    }
    Ok(())
}

fn validate_url(url: &str) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if !(lower.contains("bilibili.com") || lower.contains("douyin.com")) {
        return Err("当前录制源只支持 B站和抖音直播间地址。".to_string());
    }
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("请输入完整的 http/https 直播间地址。".to_string());
    }
    Ok(())
}

fn new_id() -> String {
    let now = chrono::Utc::now().timestamp_millis();
    let seq = TARGET_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("monitor-{now}-{seq}")
}

#[tauri::command]
pub async fn mobile_monitor_status(app: tauri::AppHandle) -> Result<MonitorStore, String> {
    let _guard = gate().lock().await;
    read_store(&store_path(&app)?).await
}

#[tauri::command]
pub async fn mobile_monitor_add(
    app: tauri::AppHandle,
    url: String,
    name: Option<String>,
) -> Result<MonitorStore, String> {
    let url = url.trim().to_string();
    validate_url(&url)?;
    let display_name = name.unwrap_or_default().trim().to_string();
    let target = mutate_store(&app, |store| {
        if store.targets.iter().any(|target| target.url == url) {
            return Err("这个直播间已经在监控列表里。".to_string());
        }
        let target = MonitorTarget {
            id: new_id(),
            url,
            name: if display_name.is_empty() {
                "Live Replay".to_string()
            } else {
                display_name
            },
            enabled: true,
            suppress_until_offline: false,
            last_state: "正在检测".to_string(),
            last_error: None,
            last_checked_at: None,
        };
        store.targets.push(target.clone());
        Ok(target)
    })
    .await?;

    sync_background_active(&app).await?;
    monitor_target_once(&app, &target).await?;
    mobile_monitor_status(app).await
}

#[tauri::command]
pub async fn mobile_monitor_remove(
    app: tauri::AppHandle,
    target_id: String,
) -> Result<MonitorStore, String> {
    let store = mutate_store(&app, |store| {
        let before = store.targets.len();
        store.targets.retain(|target| target.id != target_id);
        if store.targets.len() == before {
            return Err("监控任务不存在。".to_string());
        }
        Ok(store.clone())
    })
    .await?;
    sync_background_active(&app).await?;
    Ok(store)
}

#[tauri::command]
pub async fn mobile_monitor_set_enabled(
    app: tauri::AppHandle,
    target_id: String,
    enabled: bool,
) -> Result<MonitorStore, String> {
    let target = mutate_store(&app, |store| {
        let target = store
            .targets
            .iter_mut()
            .find(|target| target.id == target_id)
            .ok_or_else(|| "监控任务不存在。".to_string())?;
        target.enabled = enabled;
        target.suppress_until_offline = false;
        target.last_state = if enabled { "正在检测" } else { "已暂停" }.to_string();
        target.last_error = None;
        Ok(target.clone())
    })
    .await?;

    sync_background_active(&app).await?;
    if enabled {
        monitor_target_once(&app, &target).await?;
    }
    mobile_monitor_status(app).await
}

pub async fn suppress_until_offline(app: &tauri::AppHandle, room_url: &str) -> Result<(), String> {
    mutate_store(app, |store| {
        if let Some(target) = store.targets.iter_mut().find(|target| target.url == room_url) {
            target.suppress_until_offline = true;
            target.last_state = "本场已停止，等待下播".to_string();
            target.last_error = None;
            target.last_checked_at = Some(chrono::Utc::now().timestamp());
        }
        Ok(())
    })
    .await
}

async fn clear_suppression_after_offline(
    app: &tauri::AppHandle,
    id: &str,
) -> Result<(), String> {
    mutate_store(app, |store| {
        if let Some(target) = store.targets.iter_mut().find(|target| target.id == id) {
            target.suppress_until_offline = false;
            target.last_state = "等待开播".to_string();
            target.last_error = None;
            target.last_checked_at = Some(chrono::Utc::now().timestamp());
        }
        Ok(())
    })
    .await
}

async fn update_target_state(
    app: &tauri::AppHandle,
    id: &str,
    state: &str,
    error: Option<String>,
) -> Result<(), String> {
    mutate_store(app, |store| {
        if let Some(target) = store.targets.iter_mut().find(|target| target.id == id) {
            target.last_state = state.to_string();
            target.last_error = error;
            target.last_checked_at = Some(chrono::Utc::now().timestamp());
        }
        Ok(())
    })
    .await
}

async fn enabled_targets(app: &tauri::AppHandle) -> Result<Vec<MonitorTarget>, String> {
    let _guard = gate().lock().await;
    let store = read_store(&store_path(app)?).await?;
    Ok(store
        .targets
        .into_iter()
        .filter(|target| target.enabled)
        .collect())
}

async fn monitor_target_once(app: &tauri::AppHandle, target: &MonitorTarget) -> Result<(), String> {
    if super::mobile_recordings::is_recording(&target.url)? {
        return update_target_state(app, &target.id, "正在录制", None).await;
    }

    update_target_state(
        app,
        &target.id,
        if target.suppress_until_offline {
            "本场已停止，检查是否下播"
        } else {
            "正在检测"
        },
        None,
    )
    .await?;

    let lower_url = target.url.to_ascii_lowercase();
    let credentials = CoreCredentials {
        bilibili_cookie: if lower_url.contains("bilibili.com") {
            super::mobile_bilibili_auth::cached_recording_cookie(app).await
        } else {
            None
        },
        douyin_cookie: None,
    };
    match probe_stream(&target.url, &target.name, credentials.clone()).await {
        Ok(ProbeResult::Offline) => {
            if target.suppress_until_offline {
                clear_suppression_after_offline(app, &target.id).await
            } else {
                update_target_state(app, &target.id, "等待开播", None).await
            }
        }
        Ok(ProbeResult::Live { stream: _ }) if target.suppress_until_offline => {
            update_target_state(app, &target.id, "本场已停止，等待下播", None).await
        }
        Ok(ProbeResult::Live { stream }) => {
            match super::mobile_recordings::start_recording_resolved(
                app.clone(),
                target.url.clone(),
                target.name.clone(),
                credentials,
                stream,
            )
            .await
            {
                Ok(_) => update_target_state(app, &target.id, "正在录制", None).await,
                Err(error) => {
                    update_target_state(app, &target.id, "启动录制失败", Some(error)).await
                }
            }
        }
        Err(error) => update_target_state(app, &target.id, "检测失败", Some(error)).await,
    }
}

async fn monitor_tick(app: &tauri::AppHandle) -> Result<(), String> {
    let targets = enabled_targets(app).await?;
    let mut set = JoinSet::new();
    for target in targets {
        let worker_app = app.clone();
        set.spawn(async move { monitor_target_once(&worker_app, &target).await });
    }

    let mut errors = Vec::new();
    while let Some(result) = set.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => errors.push(error),
            Err(error) => errors.push(format!("监控任务异常退出: {error}")),
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

pub fn start_monitor_worker(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = sync_background_active(&app).await {
            eprintln!("[monitor] foreground-service state sync failed: {error}");
        }
        loop {
            if let Err(error) = monitor_tick(&app).await {
                eprintln!("[monitor] worker error: {error}");
            }
            sleep(Duration::from_secs(20)).await;
        }
    });
}
