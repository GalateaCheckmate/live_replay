use biliup::uploader::credential::LoginInfo;
use live_replay_core::bilibili::{
    account_info, complete_qr_login, create_qr_challenge, refresh_login_if_needed,
};
use qrcode::{QrCode, render::svg};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::Manager;
use tokio::fs;
use tokio::time::timeout;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BilibiliQrStart {
    pub url: String,
    pub svg: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BilibiliAuthStatus {
    pub logged_in: bool,
    pub mid: Option<u64>,
    pub name: Option<String>,
    pub last_error: Option<String>,
}

fn credential_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("bilibili-login.json"))
        .map_err(|error| format!("无法获取 B站登录数据目录: {error}"))
}

fn pending_qr_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("bilibili-login-pending.json"))
        .map_err(|error| format!("无法获取 B站二维码数据目录: {error}"))
}

fn sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
    }
}

async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("创建 B站登录目录失败: {error}"))?;
    }
    let temp = PathBuf::from(format!("{}.tmp", path.to_string_lossy()));
    {
        let mut file = std::fs::File::create(&temp)
            .map_err(|error| format!("创建 B站登录临时文件失败: {error}"))?;
        file.write_all(bytes)
            .map_err(|error| format!("写入 B站登录临时文件失败: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("同步 B站登录临时文件失败: {error}"))?;
    }
    // Android/Linux rename over an existing path is atomic. Never delete the valid credential
    // first, otherwise a power loss between remove and rename logs the user out unnecessarily.
    fs::rename(&temp, path)
        .await
        .map_err(|error| format!("提交 B站登录文件失败: {error}"))?;
    sync_parent_dir(path);
    Ok(())
}

async fn save_login(app: &tauri::AppHandle, login: &LoginInfo) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(login)
        .map_err(|error| format!("序列化 B站登录信息失败: {error}"))?;
    atomic_write(&credential_path(app)?, &bytes).await
}

async fn read_login(app: &tauri::AppHandle) -> Result<LoginInfo, String> {
    let path = credential_path(app)?;
    let bytes = fs::read(&path)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => "尚未登录 B站。".to_string(),
            _ => format!("读取 B站登录信息失败: {error}"),
        })?;
    serde_json::from_slice(&bytes).map_err(|error| format!("B站登录信息损坏: {error}"))
}

fn cookie_header_from_login(login: &LoginInfo) -> Option<String> {
    let cookies = login.cookie_info.get("cookies")?.as_array()?;
    let header = cookies
        .iter()
        .filter_map(|cookie| {
            let name = cookie.get("name")?.as_str()?.trim();
            let value = cookie.get("value")?.as_str()?.trim();
            if name.is_empty() || value.is_empty() {
                None
            } else {
                Some(format!("{name}={value}"))
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    if header.is_empty() { None } else { Some(header) }
}

/// Reuse the account already logged into the Android app for Bilibili live probing/recording.
/// Reading the cached cookie is intentionally local-only; upload/auth workers remain responsible
/// for token refresh so a 20-second monitor loop never hammers the passport API.
pub async fn cached_recording_cookie(app: &tauri::AppHandle) -> Option<String> {
    read_login(app)
        .await
        .ok()
        .as_ref()
        .and_then(cookie_header_from_login)
}

pub async fn load_valid_login(app: &tauri::AppHandle) -> Result<LoginInfo, String> {
    let login = read_login(app).await?;
    let refreshed = refresh_login_if_needed(login).await?;
    save_login(app, &refreshed).await?;
    Ok(refreshed)
}

#[tauri::command]
pub async fn mobile_bilibili_auth_start(
    app: tauri::AppHandle,
) -> Result<BilibiliQrStart, String> {
    let challenge = create_qr_challenge().await?;
    let payload = serde_json::to_vec_pretty(&challenge.payload)
        .map_err(|error| format!("保存 B站二维码会话失败: {error}"))?;
    atomic_write(&pending_qr_path(&app)?, &payload).await?;

    let code = QrCode::new(challenge.url.as_bytes())
        .map_err(|error| format!("生成 B站登录二维码失败: {error}"))?;
    let svg = code
        .render::<svg::Color>()
        .min_dimensions(256, 256)
        .quiet_zone(true)
        .build();

    Ok(BilibiliQrStart {
        url: challenge.url,
        svg,
    })
}

#[tauri::command]
pub async fn mobile_bilibili_auth_complete(
    app: tauri::AppHandle,
) -> Result<BilibiliAuthStatus, String> {
    let pending = pending_qr_path(&app)?;
    let bytes = fs::read(&pending)
        .await
        .map_err(|error| format!("没有可继续的 B站扫码会话: {error}"))?;
    let payload: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("B站二维码会话损坏: {error}"))?;
    let login = timeout(Duration::from_secs(180), complete_qr_login(payload))
        .await
        .map_err(|_| "等待 B站扫码确认超时，请重新生成二维码。".to_string())??;
    save_login(&app, &login).await?;
    let _ = fs::remove_file(&pending).await;
    let account = account_info(login).await?;
    Ok(BilibiliAuthStatus {
        logged_in: true,
        mid: Some(account.mid),
        name: account.name,
        last_error: None,
    })
}

#[tauri::command]
pub async fn mobile_bilibili_auth_status(
    app: tauri::AppHandle,
) -> Result<BilibiliAuthStatus, String> {
    let login = match load_valid_login(&app).await {
        Ok(login) => login,
        Err(error) => {
            return Ok(BilibiliAuthStatus {
                logged_in: false,
                mid: None,
                name: None,
                last_error: Some(error),
            });
        }
    };
    match account_info(login).await {
        Ok(account) => Ok(BilibiliAuthStatus {
            logged_in: true,
            mid: Some(account.mid),
            name: account.name,
            last_error: None,
        }),
        Err(error) => Ok(BilibiliAuthStatus {
            logged_in: false,
            mid: None,
            name: None,
            last_error: Some(error),
        }),
    }
}

#[tauri::command]
pub async fn mobile_bilibili_logout(app: tauri::AppHandle) -> Result<(), String> {
    let credential = credential_path(&app)?;
    let pending = pending_qr_path(&app)?;
    for path in [credential, pending] {
        match fs::remove_file(&path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("删除 B站登录信息失败: {error}")),
        }
    }
    Ok(())
}
