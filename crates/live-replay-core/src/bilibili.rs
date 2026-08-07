use biliup::client::StatelessClient;
use biliup::error::Kind;
use biliup::uploader::bilibili::{BiliBili, Studio, Vid, Video};
use biliup::uploader::credential::{Credential, LoginInfo, bilibili_from_info};
use biliup::uploader::line;
use biliup::uploader::VideoFile;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BilibiliQrChallenge {
    /// Full response payload required by `Credential::login_by_qrcode`.
    pub payload: Value,
    /// URL encoded in the QR code. The UI should render this locally as a QR image.
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BilibiliAccount {
    pub mid: u64,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BilibiliSubmissionRef {
    pub aid: u64,
    pub bvid: Option<String>,
}

pub async fn create_qr_challenge() -> Result<BilibiliQrChallenge, String> {
    let credential = Credential::new(None);
    let payload = credential
        .get_qrcode()
        .await
        .map_err(|error| format!("获取 B站登录二维码失败: {error}"))?;
    let url = payload
        .pointer("/data/url")
        .and_then(Value::as_str)
        .or_else(|| payload.pointer("/data/qrcode_url").and_then(Value::as_str))
        .ok_or_else(|| format!("B站二维码响应缺少 URL: {payload}"))?
        .to_string();
    Ok(BilibiliQrChallenge { payload, url })
}

pub async fn complete_qr_login(payload: Value) -> Result<LoginInfo, String> {
    Credential::new(None)
        .login_by_qrcode(payload)
        .await
        .map_err(|error| format!("B站扫码登录失败: {error}"))
}

/// Validate cached credentials and refresh them when Bilibili requests it.
pub async fn refresh_login_if_needed(login_info: LoginInfo) -> Result<LoginInfo, String> {
    let credential = Credential::new(None);
    let need_refresh = credential
        .validate_tokens(&login_info)
        .await
        .map_err(|error| format!("验证 B站登录状态失败: {error}"))?;
    if need_refresh {
        credential
            .renew_tokens(login_info)
            .await
            .map_err(|error| format!("刷新 B站登录状态失败: {error}"))
    } else {
        Ok(login_info)
    }
}

pub async fn account_info(login_info: LoginInfo) -> Result<BilibiliAccount, String> {
    let mid = login_info.token_info.mid;
    let bili = bilibili_from_info(login_info, None)
        .map_err(|error| format!("创建 B站客户端失败: {error}"))?;
    let info = bili
        .my_info()
        .await
        .map_err(|error| format!("读取 B站账号信息失败: {error}"))?;
    Ok(BilibiliAccount {
        mid,
        name: info
            .pointer("/data/name")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Upload one local MP4 to Bilibili's UPOS and return the remote `Video` descriptor.
/// This does not create or edit a submission yet, so callers can persist the uploaded filename
/// before any remote manuscript mutation.
pub async fn upload_segment_file(
    login_info: LoginInfo,
    local_path: impl AsRef<Path>,
    concurrency: usize,
) -> Result<Video, String> {
    let bili = bilibili_from_info(login_info, None)
        .map_err(|error| format!("创建 B站客户端失败: {error}"))?;
    let file = VideoFile::new(local_path.as_ref())
        .map_err(|error| format!("打开 B站待上传录像失败: {error}"))?;
    let parcel = line::bldsa()
        .pre_upload(&bili, file)
        .await
        .map_err(|error| format!("B站预上传失败: {error}"))?;
    parcel
        .upload(StatelessClient::default(), concurrency.max(1), |stream| {
            stream.map(|chunk| {
                let bytes = chunk.map_err(Kind::IO)?;
                let len = bytes.len();
                Ok((bytes, len))
            })
        })
        .await
        .map_err(|error| format!("B站文件上传失败: {error}"))
}

pub fn build_live_replay_studio(
    title: &str,
    room_url: &str,
    description: &str,
    videos: Vec<Video>,
    only_self: bool,
) -> Result<Studio, String> {
    // Deserialize instead of constructing `Studio` directly because some nested fields are
    // intentionally private inside biliup. Keep the metadata conservative for automatic archives.
    serde_json::from_value(json!({
        "copyright": 2,
        "source": room_url,
        "tid": 171,
        "cover": "",
        "title": title,
        "desc_format_id": 0,
        "desc": description,
        "desc_v2": null,
        "dynamic": "",
        "subtitle": {},
        "tag": "直播回放",
        "videos": videos,
        "dtime": null,
        "open_subtitle": false,
        "interactive": 0,
        "mission_id": null,
        "dolby": 0,
        "lossless_music": 0,
        "no_reprint": 0,
        "is_only_self": if only_self { Some(1_u8) } else { None },
        "charging_pay": 0,
        "aid": null,
        "up_selection_reply": false,
        "up_close_reply": false,
        "up_close_danmu": false,
        "extra_fields": null
    }))
    .map_err(|error| format!("生成 B站投稿信息失败: {error}"))
}

pub async fn create_submission(
    login_info: LoginInfo,
    studio: &Studio,
) -> Result<BilibiliSubmissionRef, String> {
    let bili = bilibili_from_info(login_info, None)
        .map_err(|error| format!("创建 B站客户端失败: {error}"))?;
    let response = bili
        .submit_by_app(studio, None)
        .await
        .map_err(|error| format!("创建 B站投稿失败: {error}"))?;
    let data = response
        .data
        .ok_or_else(|| "B站投稿成功响应缺少 data，状态不明确。".to_string())?;
    let aid = data
        .get("aid")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("B站投稿响应缺少 aid，状态不明确: {data}"))?;
    let bvid = data
        .get("bvid")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(BilibiliSubmissionRef { aid, bvid })
}

/// Append exactly one uploaded `Video` to an existing submission.
/// The caller must serialize calls per liveSession so P order cannot race.
pub async fn append_submission_part(
    login_info: LoginInfo,
    aid: u64,
    video: Video,
) -> Result<(), String> {
    let bili: BiliBili = bilibili_from_info(login_info, None)
        .map_err(|error| format!("创建 B站客户端失败: {error}"))?;
    let mut studio = bili
        .studio_data(&Vid::Aid(aid), None)
        .await
        .map_err(|error| format!("读取 B站原投稿失败: {error}"))?;
    studio.videos.push(video);
    studio.aid = Some(aid);
    bili.edit_by_app(&studio, None)
        .await
        .map_err(|error| format!("追加 B站分P失败: {error}"))?;
    Ok(())
}
