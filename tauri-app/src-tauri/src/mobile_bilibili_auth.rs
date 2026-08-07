use biliup::uploader::credential::LoginInfo;
use live_replay_core::bilibili::{
    account_info, complete_qr_login, create_qr_challenge, refresh_login_if_needed,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::Manager;
use tokio::fs;
use tokio::time::timeout;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BilibiliQrStart {
    pub url: String,
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

async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("创建 B站登录目录失败: {error}"))?;
    }
    let temp = PathBuf::from(format!("{}.tmp", path.to_string_lossy()));
    fs::write(&temp, bytes)
        .await
        .map_err(|error| format!("写入 B站登录临时文件失败: {error}"))?;
    if fs::metadata(path).await.is_ok() {
        fs::remove_file(path)
            .await
            .map_err(|error| format!("替换 B站登录文件失败: {error}"))?;
    }
    fs::rename(&temp, path)
        .await
        .map_err(|error| format!("提交 B站登录文件失败: {error}"))
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

pub async fn load_valid_login(app: &tauri::AppHandle) -> Result<LoginInfo, String> {
    let login = read_login(app).await?;
    let refreshed = refresh_login_if_needed(login).await?;
    // Persist refreshed tokens before returning them to an uploader. A crash after this point can
    // always restart from the newest credential set.
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
    Ok(BilibiliQrStart { url: challenge.url })
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
