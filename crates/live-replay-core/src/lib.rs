pub mod youtube;

use biliup::downloader::live::{
    LiveCredentials, LiveOptions, LiveRequest, LiveStatus, builtin_plugins, media_ext_from_url,
};
use chrono::Local;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::fs;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoreCredentials {
    pub bilibili_cookie: Option<String>,
    pub douyin_cookie: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedStream {
    pub name: String,
    pub title: String,
    pub platform: String,
    pub room_url: String,
    pub stream_url: String,
    pub suffix: String,
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProbeResult {
    Live { stream: ResolvedStream },
    Offline,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingResult {
    pub file_path: String,
    pub bytes_written: u64,
    pub stopped_by_user: bool,
}

pub type StopFlag = Arc<AtomicBool>;

pub fn new_stop_flag() -> StopFlag {
    Arc::new(AtomicBool::new(false))
}

pub fn request_stop(flag: &StopFlag) {
    flag.store(true, Ordering::Release);
}

pub async fn probe_stream(
    url: &str,
    display_name: &str,
    credentials: CoreCredentials,
) -> Result<ProbeResult, String> {
    let plugin = builtin_plugins()
        .into_iter()
        .find(|plugin| plugin.matches(url))
        .ok_or_else(|| format!("暂不支持这个直播地址: {url}"))?;

    let client = reqwest::Client::builder()
        .user_agent(
            "Mozilla/5.0 (Linux; Android 10) AppleWebKit/537.36 Chrome/150 Mobile Safari/537.36",
        )
        .connect_timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| format!("创建网络客户端失败: {error}"))?;

    let mut live_credentials = LiveCredentials::default();
    live_credentials.bilibili_cookie = credentials.bilibili_cookie;
    live_credentials.douyin_cookie = credentials.douyin_cookie;

    let request = LiveRequest {
        client,
        url: url.to_string(),
        name: if display_name.trim().is_empty() {
            "Live Replay".to_string()
        } else {
            display_name.trim().to_string()
        },
        options: LiveOptions::default(),
        credentials: live_credentials,
    };

    match plugin
        .check_stream(request)
        .await
        .map_err(|error| format!("直播检测失败: {error}"))?
    {
        LiveStatus::Offline => Ok(ProbeResult::Offline),
        LiveStatus::Live { stream } => Ok(ProbeResult::Live {
            stream: ResolvedStream {
                name: stream.name,
                title: stream.title,
                platform: stream.platform,
                room_url: stream.url,
                stream_url: stream.raw_stream_url,
                suffix: stream.suffix,
                headers: stream.stream_headers,
            },
        }),
    }
}

pub async fn record_direct_stream(
    stream: ResolvedStream,
    output_dir: impl AsRef<Path>,
    stop_flag: StopFlag,
) -> Result<RecordingResult, String> {
    let ext = normalized_extension(&stream);
    if ext == "m3u8" {
        return Err("当前 Android 内核第一阶段暂不直接写入 HLS/m3u8；请使用 FLV/直链直播源。".to_string());
    }

    let output_dir = output_dir.as_ref();
    fs::create_dir_all(output_dir)
        .await
        .map_err(|error| format!("创建录制目录失败: {error}"))?;

    let base_name = safe_file_component(if stream.name.trim().is_empty() {
        &stream.platform
    } else {
        &stream.name
    });
    let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
    let final_path = output_dir.join(format!("{base_name}_{timestamp}.{ext}"));
    let part_path = PathBuf::from(format!("{}.part", final_path.display()));

    let headers = to_header_map(&stream.headers)?;
    let client = reqwest::Client::builder()
        .user_agent(
            "Mozilla/5.0 (Linux; Android 10) AppleWebKit/537.36 Chrome/150 Mobile Safari/537.36",
        )
        .connect_timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| format!("创建录制客户端失败: {error}"))?;

    let response = client
        .get(&stream.stream_url)
        .headers(headers)
        .send()
        .await
        .map_err(|error| format!("连接直播源失败: {error}"))?
        .error_for_status()
        .map_err(|error| format!("直播源返回错误状态: {error}"))?;

    let mut file = fs::File::create(&part_path)
        .await
        .map_err(|error| format!("创建录制文件失败: {error}"))?;
    let mut body = response.bytes_stream();
    let mut bytes_written = 0_u64;

    while let Some(chunk) = body.next().await {
        if stop_flag.load(Ordering::Acquire) {
            break;
        }
        let chunk = chunk.map_err(|error| format!("读取直播流失败: {error}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|error| format!("写入录制文件失败: {error}"))?;
        bytes_written = bytes_written.saturating_add(chunk.len() as u64);
    }

    file.flush()
        .await
        .map_err(|error| format!("刷新录制文件失败: {error}"))?;
    drop(file);

    if bytes_written == 0 {
        let _ = fs::remove_file(&part_path).await;
        return Err("直播源没有返回任何媒体数据。".to_string());
    }

    fs::rename(&part_path, &final_path)
        .await
        .map_err(|error| format!("完成录制文件失败: {error}"))?;

    Ok(RecordingResult {
        file_path: final_path.to_string_lossy().into_owned(),
        bytes_written,
        stopped_by_user: stop_flag.load(Ordering::Acquire),
    })
}

fn normalized_extension(stream: &ResolvedStream) -> String {
    let suffix = stream
        .suffix
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    if matches!(suffix.as_str(), "flv" | "ts" | "mp4" | "m4s" | "m3u8") {
        return suffix;
    }
    media_ext_from_url(&stream.stream_url).unwrap_or_else(|| "flv".to_string())
}

fn to_header_map(input: &HashMap<String, String>) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    for (name, value) in input {
        let name = HeaderName::from_str(name)
            .map_err(|error| format!("直播源请求头名称无效 {name}: {error}"))?;
        let value = HeaderValue::from_str(value)
            .map_err(|error| format!("直播源请求头值无效: {error}"))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

fn safe_file_component(input: &str) -> String {
    let cleaned: String = input
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            control if control.is_control() => '_',
            other => other,
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.');
    if cleaned.is_empty() {
        "live-replay".to_string()
    } else {
        cleaned.chars().take(80).collect()
    }
}
