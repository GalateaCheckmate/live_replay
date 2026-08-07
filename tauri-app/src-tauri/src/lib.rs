#[cfg(desktop)]
use std::env;
#[cfg(desktop)]
use std::sync::{Arc, Mutex};

use tauri::RunEvent;
#[cfg(desktop)]
use tauri::path::BaseDirectory;
#[cfg(desktop)]
use tauri::{Emitter, Manager};
#[cfg(desktop)]
use tauri_plugin_shell::process::{CommandChild, CommandEvent, Encoding};
#[cfg(desktop)]
use tauri_plugin_shell::ShellExt;

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
    Ok("Android does not use the Windows sidecar.".to_string())
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
    Err("Android native Live Replay core is not wired yet.".to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_opener::init());

    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_shell::init());

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
                let _ = app;
                println!("[tauri] Android bootstrap started without Windows sidecar.");
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![greet, start_sidecar, shutdown_sidecar])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app_handle, event| {
            if let RunEvent::ExitRequested { .. } = event {
                #[cfg(desktop)]
                if let Err(error) = shutdown_sidecar_impl(app_handle) {
                    eprintln!("[tauri] Sidecar shutdown failed: {error}");
                }

                #[cfg(mobile)]
                let _ = app_handle;
            }
        });
}
