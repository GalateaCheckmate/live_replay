pub mod bilibili;
pub mod recording;
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
use tokio::time::timeout;

const PROBE_REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const PROBE_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// Complete source-container recording (FLV/TS/MP4). Never time segmented.
    pub file_path: String,
    /// Required final MP4 path. When the source is already MP4 this equals file_path.
    pub final_mp4_path: String,
    pub youtube_title: String,
    pub started_at: i64,
    pub ended_at: i64,
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
        .connect_timeout(Duration::from_secs(10))
        .timeout(PROBE_REQUEST_TIMEOUT)
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

    let status = timeout(PROBE_TOTAL_TIMEOUT, plugin.check_stream(request))
        .await
        .map_err(|_| {
            "直播检测超时（30 秒）。可能是当前网络、模拟器网络或平台接口暂时无响应；按钮已恢复，可稍后重试。"
                .to_string()
        })?
        .map_err(|error| format!("直播检测失败: {error}"))?;

    match status {
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

/// Legacy one-file recorder kept for the frozen YouTube path while Android migrates to the
/// session/segment recorder in `recording`. Do not add new segmentation behavior here.
pub async fn record_direct_stream(
    stream: ResolvedStream,
    output_dir: impl AsRef<Path>,
    stop_flag: StopFlag,
) -> Result<RecordingResult, String> {
    let ext = normalized_extension(&stream);
    if ext == "m3u8" {
        return Err(
            "当前直写录制器不能把 m3u8 播放列表本身当录像文件；该直播源需要交给 Android 媒体录制器。"
                .to_string(),
        );
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
    let started = Local::now();
    let started_at = started.timestamp();
    let working_name = format!(
        ".{base_name}｜{}｜{}-录制中.{ext}.part",
        started.format("%Y-%m-%d"),
        started.format("%H：%M")
    );
    let part_path = output_dir.join(working_name);

    if fs::metadata(&part_path).await.is_ok() {
        return Err(format!("发现同名未完成录像，拒绝覆盖: {}", part_path.display()));
    }

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
        let chunk = chunk.map_err(|error| {
            format!(
                "读取直播流失败: {error}。未完成 .part 录像将保留，不会进入上传或删除流程。"
            )
        })?;
        file.write_all(&chunk)
            .await
            .map_err(|error| format!("写入录制文件失败: {error}"))?;
        bytes_written = bytes_written.saturating_add(chunk.len() as u64);
    }

    file.flush()
        .await
        .map_err(|error| format!("刷新录制文件失败: {error}"))?;
    file.sync_all()
        .await
        .map_err(|error| format!("同步录制文件失败: {error}"))?;
    drop(file);

    if bytes_written == 0 {
        let _ = fs::remove_file(&part_path).await;
        return Err("直播源没有返回任何媒体数据。".to_string());
    }

    let ended = Local::now();
    let ended_at = ended.timestamp();
    let local_stem = format!(
        "{base_name}｜{}｜{}-{}",
        started.format("%Y-%m-%d"),
        started.format("%H：%M"),
        ended.format("%H：%M")
    );
    let youtube_title = format!(
        "{base_name}｜{}｜{}-{}",
        started.format("%Y-%m-%d"),
        started.format("%H:%M"),
        ended.format("%H:%M")
    );
    let source_path = output_dir.join(format!("{local_stem}.{ext}"));
    let final_mp4_path = output_dir.join(format!("{local_stem}.mp4"));

    if fs::metadata(&source_path).await.is_ok() && source_path != part_path {
        return Err(format!(
            "完整录像目标已存在，拒绝覆盖；未完成文件保留在 {}",
            part_path.display()
        ));
    }
    if ext != "mp4" && fs::metadata(&final_mp4_path).await.is_ok() {
        return Err(format!(
            "最终 MP4 目标已存在，拒绝覆盖；未完成文件保留在 {}",
            part_path.display()
        ));
    }

    fs::rename(&part_path, &source_path)
        .await
        .map_err(|error| format!("完成整场录像文件失败: {error}"))?;

    Ok(RecordingResult {
        file_path: source_path.to_string_lossy().into_owned(),
        final_mp4_path: final_mp4_path.to_string_lossy().into_owned(),
        youtube_title,
        started_at,
        ended_at,
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
