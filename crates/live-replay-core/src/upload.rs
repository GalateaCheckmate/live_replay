use biliup::client::StatelessClient;
use biliup::downloader::live::media_ext_from_url;
use biliup::uploader::bilibili::{Studio, Vid, Video};
use biliup::uploader::credential::{LoginInfo, bilibili_from_info};
use biliup::uploader::line::Probe;
use biliup::uploader::VideoFile;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionRef {
    pub aid: Option<u64>,
    pub bvid: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SubmissionMeta {
    pub title: String,
    pub description: String,
    pub tag: String,
    pub source: String,
    pub tid: u16,
    pub only_self: bool,
}

pub async fn validate_bilibili_login(login_info_json: &str) -> Result<String, String> {
    let login_info: LoginInfo = serde_json::from_str(login_info_json)
        .map_err(|error| format!("B站登录数据格式错误: {error}"))?;
    let bili = bilibili_from_info(login_info, None)
        .map_err(|error| format!("加载B站登录状态失败: {error}"))?;
    let info = bili
        .my_info()
        .await
        .map_err(|error| format!("验证B站账号失败: {error}"))?;
    if info.get("code").and_then(|v| v.as_i64()).unwrap_or(-1) != 0 {
        return Err(format!("B站登录状态无效: {info}"));
    }
    Ok(info
        .pointer("/data/name")
        .and_then(|v| v.as_str())
        .unwrap_or("已登录账号")
        .to_string())
}

pub async fn submit_first_part(
    login_info_json: &str,
    file_path: impl AsRef<Path>,
    meta: &SubmissionMeta,
) -> Result<SubmissionRef, String> {
    if meta.tid == 0 {
        return Err("B站投稿分区尚未设置。".to_string());
    }
    let login_info: LoginInfo = serde_json::from_str(login_info_json)
        .map_err(|error| format!("B站登录数据格式错误: {error}"))?;
    let bili = bilibili_from_info(login_info, None)
        .map_err(|error| format!("加载B站登录状态失败: {error}"))?;
    let video = upload_one(&bili, file_path.as_ref()).await?;
    let studio = make_studio(meta, vec![video], None)?;
    let response = bili
        .submit_by_app(&studio, None)
        .await
        .map_err(|error| format!("B站首次投稿失败: {error}"))?;
    let data = response.data.unwrap_or_default();
    let reference = SubmissionRef {
        aid: data.get("aid").and_then(|value| value.as_u64()),
        bvid: data
            .get("bvid")
            .and_then(|value| value.as_str())
            .map(ToString::to_string),
    };
    confirm_submission(&bili, &reference).await?;
    Ok(reference)
}

pub async fn append_part(
    login_info_json: &str,
    submission: &SubmissionRef,
    file_path: impl AsRef<Path>,
) -> Result<(), String> {
    let login_info: LoginInfo = serde_json::from_str(login_info_json)
        .map_err(|error| format!("B站登录数据格式错误: {error}"))?;
    let bili = bilibili_from_info(login_info, None)
        .map_err(|error| format!("加载B站登录状态失败: {error}"))?;
    let vid = as_vid(submission)?;
    let mut studio = bili
        .studio_data(&vid, None)
        .await
        .map_err(|error| format!("读取已有B站稿件失败: {error}"))?;
    let before = studio.videos.len();
    studio.videos.push(upload_one(&bili, file_path.as_ref()).await?);
    bili.edit_by_app(&studio, None)
        .await
        .map_err(|error| format!("追加B站分P失败: {error}"))?;
    let confirmed = bili
        .studio_data(&vid, None)
        .await
        .map_err(|error| format!("确认B站分P失败: {error}"))?;
    if confirmed.videos.len() <= before {
        return Err("B站接口返回成功，但远端分P数量没有增加；本地文件将保留。".to_string());
    }
    Ok(())
}

async fn upload_one(
    bili: &biliup::uploader::bilibili::BiliBili,
    file_path: &Path,
) -> Result<Video, String> {
    if !file_path.is_file() {
        return Err(format!("待上传文件不存在: {}", file_path.display()));
    }
    let client = StatelessClient::default();
    let line = Probe::probe(&client.client).await.unwrap_or_default();
    let video_file = VideoFile::new(file_path)
        .map_err(|error| format!("打开待上传文件失败: {error}"))?;
    let parcel = line
        .pre_upload(bili, video_file)
        .await
        .map_err(|error| format!("B站预上传失败: {error}"))?;
    parcel
        .upload(client, 3, |stream| {
            stream.map(|chunk| {
                let chunk = chunk.map_err(biliup::error::Kind::from)?;
                let len = chunk.len();
                Ok((chunk, len))
            })
        })
        .await
        .map_err(|error| format!("B站上传文件失败: {error}"))
}

fn make_studio(
    meta: &SubmissionMeta,
    videos: Vec<Video>,
    aid: Option<u64>,
) -> Result<Studio, String> {
    let value = json!({
        "copyright": 2,
        "source": meta.source,
        "tid": meta.tid,
        "title": truncate(&meta.title, 80),
        "desc": meta.description,
        "tag": meta.tag,
        "videos": videos,
        "dtime": null,
        "aid": aid,
        "is_only_self": if meta.only_self { 1 } else { 0 }
    });
    serde_json::from_value(value).map_err(|error| format!("生成B站投稿参数失败: {error}"))
}

async fn confirm_submission(
    bili: &biliup::uploader::bilibili::BiliBili,
    reference: &SubmissionRef,
) -> Result<(), String> {
    let vid = as_vid(reference)?;
    bili.video_data(&vid, None)
        .await
        .map_err(|error| format!("B站投稿未能远端确认: {error}"))?;
    Ok(())
}

fn as_vid(reference: &SubmissionRef) -> Result<Vid, String> {
    if let Some(aid) = reference.aid {
        return Ok(Vid::Aid(aid));
    }
    if let Some(bvid) = &reference.bvid {
        return Ok(Vid::Bvid(bvid.clone()));
    }
    Err("B站投稿成功响应中没有 aid/bvid；无法安全确认，保留本地文件。".to_string())
}

fn truncate(input: &str, max: usize) -> String {
    if input.chars().count() <= max {
        input.to_string()
    } else {
        input.chars().take(max.saturating_sub(3)).collect::<String>() + "..."
    }
}

#[allow(dead_code)]
fn _media_extension(path: &Path) -> Option<String> {
    media_ext_from_url(&path.to_string_lossy())
}
