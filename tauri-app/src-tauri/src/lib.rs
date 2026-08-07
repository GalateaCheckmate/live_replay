#[cfg(desktop)]
use std::env;
#[cfg(desktop)]
use std::sync::{Arc, Mutex};
#[cfg(mobile)]
use std::sync::Mutex;

use tauri::{Manager, RunEvent};
#[cfg(desktop)]
use tauri::path::BaseDirectory;
#[cfg(desktop)]
use tauri::Emitter;
#[cfg(desktop)]
use tauri_plugin_shell::process::{CommandChild, CommandEvent, Encoding};
#[cfg(desktop)]
use tauri_plugin_shell::ShellExt;

#[cfg(mobile)]
use live_replay_core::{
    CoreCredentials, ProbeResult, StopFlag, new_stop_flag, probe_stream, record_direct_stream,
    request_stop,
};
#[cfg(mobile)]
use serde::Serialize;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg(mobile)]
#[derive(Default)]
struct MobileCoreState {
    runtime: Mutex<MobileRuntimeState>,
}

#[cfg(mobile)]
#[derive(Default)]
struct MobileRuntimeState {
    active: bool,
    room_url: Option<String>,
    display_name: Option<String>,
    current_file: Option<String>,
    last_file: Option<String>,
    last_error: Option<String>,
    stop_flag: Option<StopFlag>,
}

#[cfg(mobile)]
#[derive(Debug, Clone, Serialize)]
struct MobileCoreStatus {
    active: bool,
    room_url: Option<String>,
    display_name: Option<String>,
    current_file: Option<String>,
    last_file: Option<String>,
    last_error: Option<String>,
}

#[cfg(mobile)]
impl From<&MobileRuntimeState> for MobileCoreStatus {
    fn from(value: &MobileRuntimeState) -> Self {
        Self {
            active: value.active,
            room_url: value.room_url.clone(),
            display_name: value.display_name.clone(),
            current_file: value.current_file.clone(),
            last_file: value.last_file.clone(),
            last_error: value.last_error.clone(),
        }
    }
}

#[cfg(desktop)]
fn spawn_and_monitor_sidecar(app_handle: tauri::AppHandle) -> Result<(), String> {
    if let Some(state) = app_handle.try_state::<Arc<Mutex<Option<CommandChild>>>>() {
        let child_process = state.lock().map_err(|_| "Failed to lock sidecar state")?;
        if child_process.is_some() {
            println!("[tauri] Sidecar is already running. Skipping spawn.");
            return Ok(());
        }
    }

    let resource_path = app_handle
        .path()
        .resolve("binaries/biliup.exe", BaseDirectory::Resource)
        .map_err(|e| e.to_string())?;

    let exe_path = env::current_exe().map_err(|e| e.to_string())?;
    let exe_dir = exe_path
        .parent()
        .ok_or_else(|| "Failed to resolve executable directory".to_string())?;

    println!("[tauri] Sidecar directory: {}", exe_dir.display());

    let (mut rx, child) = app_handle
        .shell()
        .command(resource_path)
        .current_dir(exe_dir)
        .spawn()
        .map_err(|e| e.to_string())?;

    if let Some(state) = app_handle.try_state::<Arc<Mutex<Option<CommandChild>>>>() {
        println!("[tauri] Sidecar pid: {}", child.pid());
        *state.lock().map_err(|_| "Failed to lock sidecar state")? = Some(child);
    } else {
        return Err("Failed to access app state".to_string());
    }

    tauri::async_runtime::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stdout(line_bytes) => {
                    let encoding = Encoding::for_label("GBK".as_ref()).unwrap();
                    let line = encoding.decode_with_bom_removal(&line_bytes).0;
                    println!("Sidecar stdout: {}", line);
                    let _ = app_handle.emit("sidecar-stdout", line.to_string());
                }
                CommandEvent::Stderr(line_bytes) => {
                    let line = String::from_utf8_lossy(&line_bytes);
                    eprintln!("Sidecar stderr: {}", line);
                    let _ = app_handle.emit("sidecar-stderr", line.to_string());
                }
                _ => {}
            }
        }
    });

    Ok(())
}

#[cfg(desktop)]
fn shutdown_sidecar_impl(app_handle: &tauri::AppHandle) -> Result<String, String> {
    let state = app_handle
        .try_state::<Arc<Mutex<Option<CommandChild>>>>()
        .ok_or_else(|| "Sidecar process state not found".to_string())?;

    let mut child_process = state
        .lock()
        .map_err(|_| "Failed to acquire sidecar process lock".to_string())?;

    let Some(process) = child_process.take() else {
        return Ok("No active sidecar process.".to_string());
    };

    let pid = process.pid();
    println!("[tauri] Killing Windows sidecar tree. pid={pid}");

    let (mut rx, _) = app_handle
        .shell()
        .command("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .spawn()
        .map_err(|e| e.to_string())?;

    tauri::async_runtime::block_on(async move {
        let encoding = Encoding::for_label("GBK".as_ref()).unwrap();
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stdout(line) => {
                    println!("taskkill: {}", encoding.decode_with_bom_removal(&line).0)
                }
                CommandEvent::Stderr(line) => {
                    eprintln!("taskkill: {}", encoding.decode_with_bom_removal(&line).0)
                }
                _ => {}
            }
        }
    });

    Ok("Sidecar shutdown requested.".to_string())
}

#[cfg(desktop)]
#[tauri::command]
fn shutdown_sidecar(app_handle: tauri::AppHandle) -> Result<String, String> {
    shutdown_sidecar_impl(&app_handle)
}

#[cfg(mobile)]
#[tauri::command]
fn shutdown_sidecar() -> Result<String, String> {
    Ok("Android 使用内置 Rust Live Replay core，不使用 Windows sidecar。".to_string())
}

#[cfg(desktop)]
#[tauri::command]
fn start_sidecar(app_handle: tauri::AppHandle) -> Result<String, String> {
    spawn_and_monitor_sidecar(app_handle)?;
    Ok("Sidecar spawned and monitoring started.".to_string())
}

#[cfg(mobile)]
#[tauri::command]
fn start_sidecar() -> Result<String, String> {
    Ok("Android 原生 Rust Live Replay core 已加载。".to_string())
}

#[cfg(mobile)]
#[tauri::command]
async fn mobile_probe_stream(
    url: String,
    name: Option<String>,
    bilibili_cookie: Option<String>,
    douyin_cookie: Option<String>,
) -> Result<ProbeResult, String> {
    probe_stream(
        url.trim(),
        name.as_deref().unwrap_or(""),
        CoreCredentials {
            bilibili_cookie,
            douyin_cookie,
        },
    )
    .await
}

#[cfg(mobile)]
#[tauri::command]
fn mobile_core_status(app_handle: tauri::AppHandle) -> Result<MobileCoreStatus, String> {
    let state = app_handle.state::<MobileCoreState>();
    let runtime = state
        .runtime
        .lock()
        .map_err(|_| "Android core 状态锁异常".to_string())?;
    Ok(MobileCoreStatus::from(&*runtime))
}

#[cfg(mobile)]
#[tauri::command]
async fn mobile_start_recording(
    app_handle: tauri::AppHandle,
    url: String,
    name: Option<String>,
    bilibili_cookie: Option<String>,
    douyin_cookie: Option<String>,
) -> Result<MobileCoreStatus, String> {
    {
        let state = app_handle.state::<MobileCoreState>();
        let runtime = state
            .runtime
            .lock()
            .map_err(|_| "Android core 状态锁异常".to_string())?;
        if runtime.active {
            return Err("当前已经有录制任务在运行。".to_string());
        }
    }

    let display_name = name.unwrap_or_else(|| "Live Replay".to_string());
    let resolved = match probe_stream(
        url.trim(),
        &display_name,
        CoreCredentials {
            bilibili_cookie,
            douyin_cookie,
        },
    )
    .await?
    {
        ProbeResult::Offline => return Err("主播当前未开播。".to_string()),
        ProbeResult::Live { stream } => stream,
    };

    let output_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法获取 Android App 数据目录: {error}"))?
        .join("recordings");
    let stop_flag = new_stop_flag();

    {
        let state = app_handle.state::<MobileCoreState>();
        let mut runtime = state
            .runtime
            .lock()
            .map_err(|_| "Android core 状态锁异常".to_string())?;
        if runtime.active {
            return Err("当前已经有录制任务在运行。".to_string());
        }
        runtime.active = true;
        runtime.room_url = Some(url.trim().to_string());
        runtime.display_name = Some(display_name.clone());
        runtime.current_file = Some(output_dir.to_string_lossy().into_owned());
        runtime.last_error = None;
        runtime.stop_flag = Some(stop_flag.clone());
    }

    let worker_app = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        let result = record_direct_stream(resolved, &output_dir, stop_flag).await;
        {
            let state = worker_app.state::<MobileCoreState>();
            if let Ok(mut runtime) = state.runtime.lock() {
                runtime.active = false;
                runtime.current_file = None;
                runtime.stop_flag = None;
                match result {
                    Ok(recording) => {
                        runtime.last_file = Some(recording.file_path);
                        runtime.last_error = None;
                    }
                    Err(error) => {
                        runtime.last_error = Some(error);
                    }
                }
            };
        }
    });

    mobile_core_status(app_handle)
}

#[cfg(mobile)]
#[tauri::command]
fn mobile_stop_recording(app_handle: tauri::AppHandle) -> Result<MobileCoreStatus, String> {
    let state = app_handle.state::<MobileCoreState>();
    {
        let runtime = state
            .runtime
            .lock()
            .map_err(|_| "Android core 状态锁异常".to_string())?;
        let Some(flag) = runtime.stop_flag.as_ref() else {
            return Ok(MobileCoreStatus::from(&*runtime));
        };
        request_stop(flag);
    }
    mobile_core_status(app_handle)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_opener::init());

    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_shell::init());

    #[cfg(desktop)]
    let builder = builder.invoke_handler(tauri::generate_handler![
        greet,
        start_sidecar,
        shutdown_sidecar
    ]);

    #[cfg(mobile)]
    let builder = builder.invoke_handler(tauri::generate_handler![
        greet,
        start_sidecar,
        shutdown_sidecar,
        mobile_probe_stream,
        mobile_core_status,
        mobile_start_recording,
        mobile_stop_recording
    ]);

    builder
        .setup(|app| {
            #[cfg(desktop)]
            {
                app.manage(Arc::new(Mutex::new(None::<CommandChild>)));
                let app_handle = app.handle().clone();
                println!("[tauri] Creating Windows sidecar...");
                if let Err(error) = spawn_and_monitor_sidecar(app_handle) {
                    eprintln!("[tauri] Failed to start sidecar: {error}");
                }
            }

            #[cfg(mobile)]
            {
                app.manage(MobileCoreState::default());
                println!("[tauri] Android native Live Replay core loaded.");
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app_handle, event| {
            if let RunEvent::ExitRequested { .. } = event {
                #[cfg(desktop)]
                if let Err(error) = shutdown_sidecar_impl(app_handle) {
                    eprintln!("[tauri] Sidecar shutdown failed: {error}");
                }

                #[cfg(mobile)]
                {
                    let state = app_handle.state::<MobileCoreState>();
                    if let Ok(runtime) = state.runtime.lock() {
                        if let Some(flag) = runtime.stop_flag.as_ref() {
                            request_stop(flag);
                        }
                    };
                }
            }
        });
}
