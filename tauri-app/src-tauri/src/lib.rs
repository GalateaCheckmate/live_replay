#[cfg(desktop)]
use std::env;
#[cfg(desktop)]
use std::sync::{Arc, Mutex};

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
mod mobile_bilibili;
#[cfg(mobile)]
mod mobile_bilibili_auth;
#[cfg(mobile)]
mod mobile_bilibili_worker;
#[cfg(mobile)]
mod mobile_monitor;
#[cfg(mobile)]
mod mobile_recording_journal;
#[cfg(mobile)]
mod mobile_recordings;
#[cfg(mobile)]
mod mobile_youtube;

#[cfg(mobile)]
use live_replay_core::{probe_stream, CoreCredentials, ProbeResult};

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_opener::init());

    #[cfg(target_os = "android")]
    let builder = builder.plugin(tauri_plugin_live_replay_android::init());

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
        mobile_recordings::mobile_recordings_status,
        mobile_recordings::mobile_start_recording_multi,
        mobile_recordings::mobile_stop_recording_multi,
        mobile_monitor::mobile_monitor_status,
        mobile_monitor::mobile_monitor_add,
        mobile_monitor::mobile_monitor_remove,
        mobile_monitor::mobile_monitor_set_enabled,
        mobile_bilibili::mobile_bilibili_status,
        mobile_bilibili::mobile_bilibili_set_settings,
        mobile_bilibili_auth::mobile_bilibili_auth_start,
        mobile_bilibili_auth::mobile_bilibili_auth_complete,
        mobile_bilibili_auth::mobile_bilibili_auth_status,
        mobile_bilibili_auth::mobile_bilibili_logout,
        mobile_youtube::mobile_youtube_authorize,
        mobile_youtube::mobile_youtube_cached_auth,
        mobile_youtube::mobile_youtube_logout,
        mobile_youtube::mobile_youtube_status,
        mobile_youtube::mobile_youtube_set_settings,
        mobile_youtube::mobile_youtube_enqueue_mp4,
        mobile_youtube::mobile_youtube_retry_task
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
                mobile_monitor::start_monitor_worker(app.handle().clone());
                mobile_bilibili_worker::start_upload_worker(app.handle().clone());
                // Keep the already-built YouTube worker alive for existing tasks, but new 15GB
                // recording segments now feed Bilibili first. YouTube session merging comes later.
                mobile_youtube::start_upload_worker(app.handle().clone());
                println!("[tauri] Android monitor + 15GB recorder + Bilibili multi-P worker + frozen YouTube worker loaded.");
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
                mobile_recordings::request_stop_all();
            }
        });
}
