use live_replay_core::{CoreCredentials, ProbeResult, prime_resolved_stream, probe_stream};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use tauri::Manager;
use tokio::fs;
use tokio::sync::Mutex;
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

async fn read_store(path: &Path) -> Result<MonitorStore, String> {
    match fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("读取主播监控状态失败: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(MonitorStore::default()),
        Err(error) => Err(format!("读取主播监控状态文件失败: {error}")),
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
    fs::write(&temp, bytes)
        .await
        .map_err(|error| format!("写入监控临时状态失败: {error}"))?;
    if fs::metadata(path).await.is_ok() {
        let backup = PathBuf::from(format!("{}.bak", path.to_string_lossy()));
        let _ = fs::copy(path, backup).await;
        fs::remove_file(path)
            .await
            .map_err(|error| format!("替换监控状态文件失败: {error}"))?;
    }
    fs::rename(&temp, path)
        .await
        .map_err(|error| format!("提交监控状态文件失败: {error}"))
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
            last_state: "正在检测".to_string(),
            last_error: None,
            last_checked_at: None,
        };
        store.targets.push(target.clone());
        Ok(target)
    })
    .await?;

    // Do not wait for the 20-second background loop. Adding a live target is an explicit user
    // action, so run exactly one probe now and start recording immediately when it is live.
    monitor_target_once(&app, &target).await?;
    mobile_monitor_status(app).await
}

#[tauri::command]
pub async fn mobile_monitor_remove(
    app: tauri::AppHandle,
    target_id: String,
) -> Result<MonitorStore, String> {
    mutate_store(&app, |store| {
        let before = store.targets.len();
        store.targets.retain(|target| target.id != target_id);
        if store.targets.len() == before {
            return Err("监控任务不存在。".to_string());
        }
        Ok(store.clone())
    })
    .await
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
        target.last_state = if enabled { "正在检测" } else { "已暂停" }.to_string();
        target.last_error = None;
        Ok(target.clone())
    })
    .await?;

    if enabled {
        monitor_target_once(&app, &target).await?;
    }
    mobile_monitor_status(app).await
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
    Ok(store.targets.into_iter().filter(|target| target.enabled).collect())
}

async fn monitor_target_once(app: &tauri::AppHandle, target: &MonitorTarget) -> Result<(), String> {
    if super::mobile_recordings::is_recording(&target.url)? {
        return update_target_state(app, &target.id, "正在录制", None).await;
    }

    update_target_state(app, &target.id, "正在检测", None).await?;
    let credentials = CoreCredentials::default();
    match probe_stream(&target.url, &target.name, credentials.clone()).await {
        Ok(ProbeResult::Offline) => {
            update_target_state(app, &target.id, "等待开播", None).await
        }
        Ok(ProbeResult::Live { stream }) => {
            // start_recording currently crosses two probe call-sites (its guard and the recorder
            // startup). Prime both with this exact ResolvedStream so neither call hits the platform
            // again. The cache expires quickly and is consumed, so reconnects still resolve fresh.
            prime_resolved_stream(&target.url, stream, 2);
            match super::mobile_recordings::start_recording(
                app.clone(),
                target.url.clone(),
                target.name.clone(),
                credentials,
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
    for target in enabled_targets(app).await? {
        monitor_target_once(app, &target).await?;
    }
    Ok(())
}

pub fn start_monitor_worker(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        // First pass runs immediately. The 20-second interval only applies after a completed pass.
        loop {
            if let Err(error) = monitor_tick(&app).await {
                eprintln!("[monitor] worker error: {error}");
            }
            sleep(Duration::from_secs(20)).await;
        }
    });
}
