use crate::{CoreCredentials, ProbeResult, ResolvedStream, StopFlag, probe_stream};
use biliup::client::StatelessClient;
use biliup::downloader::hls;
use biliup::downloader::httpflv::{self, Connection};
use biliup::downloader::util::{LifecycleFile, Segmentable};
use chrono::{DateTime, Local, TimeZone};
use reqwest::header::{ACCEPT_ENCODING, HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::sleep;

/// User-facing target is about 15 GB per recording file. Trigger slightly before 15,000,000,000
/// bytes so the next safe keyframe/HLS media boundary can land close to, rather than materially
/// above, the target.
pub const SEGMENT_TARGET_BYTES: u64 = 14_900_000_000;
pub const LIVE_OFFLINE_GRACE: Duration = Duration::from_secs(10 * 60);
const RECHECK_INTERVAL: Duration = Duration::from_secs(20);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(250);

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingSegment {
    pub live_session_id: String,
    pub segment_index: u32,
    pub streamer_name: String,
    pub platform: String,
    pub room_url: String,
    pub source_path: String,
    pub final_mp4_path: String,
    pub local_file_name: String,
    pub youtube_title: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub bytes_written: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingSessionResult {
    pub live_session_id: String,
    pub streamer_name: String,
    pub platform: String,
    pub room_url: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub stopped_by_user: bool,
    pub segments: Vec<RecordingSegment>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RecordingPlan {
    pub room_url: String,
    pub display_name: String,
    pub credentials: CoreCredentials,
    pub output_dir: PathBuf,
    pub live_session_id: Option<String>,
    /// Original liveSession start time. Set when resuming after a process restart.
    pub session_started_at: Option<i64>,
    /// First segment number emitted by this process. Normally 1; restored sessions continue N+1.
    pub next_segment_index: u32,
    /// Start timestamp for the segment currently being resumed/continued.
    pub segment_started_at: Option<i64>,
    pub segment_target_bytes: u64,
}

impl RecordingPlan {
    pub fn new(
        room_url: impl Into<String>,
        display_name: impl Into<String>,
        credentials: CoreCredentials,
        output_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            room_url: room_url.into(),
            display_name: display_name.into(),
            credentials,
            output_dir: output_dir.into(),
            live_session_id: None,
            session_started_at: None,
            next_segment_index: 1,
            segment_started_at: None,
            segment_target_bytes: SEGMENT_TARGET_BYTES,
        }
    }
}

#[derive(Debug)]
struct SegmentEmitterState {
    live_session_id: String,
    streamer_name: String,
    platform: String,
    room_url: String,
    output_dir: PathBuf,
    next_index: u32,
    segment_started: DateTime<Local>,
    segments: Vec<RecordingSegment>,
    errors: Vec<String>,
    tx: Option<UnboundedSender<RecordingSegment>>,
}

impl SegmentEmitterState {
    fn finalize_raw_segment(&mut self, raw_file_name: &str, extension: &str) {
        let raw_path = PathBuf::from(raw_file_name);
        let metadata = match std::fs::metadata(&raw_path) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.errors.push(format!("读取切段文件失败 {}: {error}", raw_path.display()));
                return;
            }
        };

        // LifecycleFile creates the next file immediately after a split. If the stream ends at
        // exactly that point, Drop can expose an empty/header-only tail. Never register it.
        let minimum_useful_size = if extension == "flv" { 14 } else { 1 };
        if metadata.len() < minimum_useful_size {
            let _ = std::fs::remove_file(&raw_path);
            return;
        }

        let ended = Local::now();
        let index = self.next_index;
        self.next_index = self.next_index.saturating_add(1);
        let base = safe_file_component(&self.streamer_name);
        let local_stem = format!(
            "{base}｜{}｜{}-{}",
            self.segment_started.format("%Y-%m-%d"),
            self.segment_started.format("%H：%M"),
            ended.format("%H：%M")
        );
        let youtube_title = format!(
            "{base}｜{}｜{}-{}",
            self.segment_started.format("%Y-%m-%d"),
            self.segment_started.format("%H:%M"),
            ended.format("%H:%M")
        );

        // Keep the source container crash-discoverable until MP4 remux + persistent queue handoff
        // finish. The user-facing MP4 name remains clean; only this intermediate file carries the
        // session/P identity so startup recovery can reconstruct it even if the process dies in the
        // narrow window before the async journal callback runs.
        let source_path = self.output_dir.join(format!(
            ".lr-{}-P{}-source.{}",
            self.live_session_id, index, extension
        ));
        if source_path.exists() {
            self.errors.push(format!(
                "发现同 session/P 的恢复源文件，拒绝覆盖: {}",
                source_path.display()
            ));
            return;
        }

        let mut final_mp4_path = self.output_dir.join(format!("{local_stem}.mp4"));
        if final_mp4_path.exists() {
            final_mp4_path = self.output_dir.join(format!("{local_stem}｜P{index}.mp4"));
            if final_mp4_path.exists() {
                self.errors.push(format!(
                    "最终 MP4 文件名冲突，拒绝覆盖: {}",
                    final_mp4_path.display()
                ));
                return;
            }
        }

        if let Err(error) = std::fs::rename(&raw_path, &source_path) {
            self.errors.push(format!(
                "提交切段文件失败 {} -> {}: {error}",
                raw_path.display(),
                source_path.display()
            ));
            return;
        }
        let bytes_written = std::fs::metadata(&source_path)
            .map(|value| value.len())
            .unwrap_or(metadata.len());

        let segment = RecordingSegment {
            live_session_id: self.live_session_id.clone(),
            segment_index: index,
            streamer_name: self.streamer_name.clone(),
            platform: self.platform.clone(),
            room_url: self.room_url.clone(),
            source_path: source_path.to_string_lossy().into_owned(),
            final_mp4_path: final_mp4_path.to_string_lossy().into_owned(),
            local_file_name: final_mp4_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_string(),
            youtube_title,
            started_at: self.segment_started.timestamp(),
            ended_at: ended.timestamp(),
            bytes_written,
        };
        self.segment_started = ended;
        self.segments.push(segment.clone());
        if let Some(tx) = &self.tx {
            let _ = tx.send(segment);
        }
    }
}

pub async fn record_live_session(
    mut plan: RecordingPlan,
    stop_flag: StopFlag,
    segment_tx: Option<UnboundedSender<RecordingSegment>>,
) -> Result<RecordingSessionResult, String> {
    std::fs::create_dir_all(&plan.output_dir)
        .map_err(|error| format!("创建录制目录失败: {error}"))?;
    if plan.segment_target_bytes == 0 {
        plan.segment_target_bytes = SEGMENT_TARGET_BYTES;
    }

    let session_started = plan
        .session_started_at
        .and_then(|timestamp| Local.timestamp_opt(timestamp, 0).single())
        .unwrap_or_else(Local::now);
    let segment_started = plan
        .segment_started_at
        .and_then(|timestamp| Local.timestamp_opt(timestamp, 0).single())
        .unwrap_or(session_started);
    let live_session_id = plan
        .live_session_id
        .clone()
        .unwrap_or_else(|| new_session_id(&plan.display_name, session_started));

    let mut emitter: Option<Arc<Mutex<SegmentEmitterState>>> = None;
    let mut platform = String::new();
    let mut offline_since: Option<Instant> = None;
    let mut last_error = None;

    loop {
        if stop_flag.load(Ordering::Acquire) {
            break;
        }

        let resolved = match probe_stream(
            &plan.room_url,
            &plan.display_name,
            plan.credentials.clone(),
        )
        .await
        {
            Ok(ProbeResult::Live { stream }) => {
                offline_since = None;
                last_error = None;
                stream
            }
            Ok(ProbeResult::Offline) => {
                let since = offline_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= LIVE_OFFLINE_GRACE {
                    break;
                }
                sleep(RECHECK_INTERVAL).await;
                continue;
            }
            Err(error) => {
                last_error = Some(error);
                let since = offline_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= LIVE_OFFLINE_GRACE {
                    break;
                }
                sleep(RECHECK_INTERVAL).await;
                continue;
            }
        };

        platform = resolved.platform.clone();
        let shared_emitter = emitter.get_or_insert_with(|| {
            Arc::new(Mutex::new(SegmentEmitterState {
                live_session_id: live_session_id.clone(),
                streamer_name: if resolved.name.trim().is_empty() {
                    plan.display_name.clone()
                } else {
                    resolved.name.clone()
                },
                platform: resolved.platform.clone(),
                room_url: plan.room_url.clone(),
                output_dir: plan.output_dir.clone(),
                next_index: plan.next_segment_index.max(1),
                segment_started,
                segments: Vec::new(),
                errors: Vec::new(),
                tx: segment_tx.clone(),
            }))
        });

        match record_connection(
            resolved,
            &plan.output_dir,
            plan.segment_target_bytes,
            stop_flag.clone(),
            shared_emitter.clone(),
            &live_session_id,
        )
        .await
        {
            Ok(()) => {
                // A clean EOF still needs an offline confirmation. Some CDNs close a connection
                // while the room immediately reconnects to a new source URL.
                offline_since.get_or_insert_with(Instant::now);
            }
            Err(error) => {
                last_error = Some(error);
                offline_since.get_or_insert_with(Instant::now);
            }
        }

        if stop_flag.load(Ordering::Acquire) {
            break;
        }
        sleep(Duration::from_secs(2)).await;
    }

    let ended_at = Local::now().timestamp();
    let (segments, callback_errors) = if let Some(emitter) = emitter {
        let state = emitter
            .lock()
            .map_err(|_| "录制分段状态锁异常".to_string())?;
        (state.segments.clone(), state.errors.clone())
    } else {
        (Vec::new(), Vec::new())
    };

    let last_error = if callback_errors.is_empty() {
        last_error
    } else {
        Some(callback_errors.join("; "))
    };

    Ok(RecordingSessionResult {
        live_session_id,
        streamer_name: plan.display_name,
        platform,
        room_url: plan.room_url,
        started_at: session_started.timestamp(),
        ended_at,
        stopped_by_user: stop_flag.load(Ordering::Acquire),
        segments,
        last_error,
    })
}

async fn record_connection(
    stream: ResolvedStream,
    output_dir: &Path,
    segment_target_bytes: u64,
    stop_flag: StopFlag,
    emitter: Arc<Mutex<SegmentEmitterState>>,
    live_session_id: &str,
) -> Result<(), String> {
    let ext = normalized_extension(&stream);
    let working_pattern = output_dir
        .join(format!(
            ".lr-{live_session_id}-raw-%Y%m%d-%H%M%S-%f"
        ))
        .to_string_lossy()
        .into_owned();
    let segmentable = Segmentable::new(None, Some(segment_target_bytes));

    match ext.as_str() {
        "m3u8" | "ts" => {
            let headers = to_header_map(&stream.headers)?;
            let mut client = StatelessClient::new(headers.clone(), None);
            client.headers = headers;
            let hook_emitter = emitter.clone();
            let file = LifecycleFile::with_hook(&working_pattern, "ts", move |file_name| {
                if let Ok(mut state) = hook_emitter.lock() {
                    state.finalize_raw_segment(file_name, "ts");
                }
            });
            let download = hls::download(&stream.stream_url, &client, file, segmentable);
            tokio::pin!(download);
            tokio::select! {
                result = &mut download => result.map_err(|error| format!("HLS 录制中断: {error}")),
                _ = wait_for_stop(stop_flag) => Ok(()),
            }
        }
        "flv" => {
            let mut headers = to_header_map(&stream.headers)?;
            headers.append(ACCEPT_ENCODING, HeaderValue::from_static("gzip, deflate"));
            let client = reqwest::Client::builder()
                .user_agent("Mozilla/5.0 (Linux; Android 10) AppleWebKit/537.36 Chrome/150 Mobile Safari/537.36")
                .connect_timeout(Duration::from_secs(30))
                .build()
                .map_err(|error| format!("创建 FLV 录制客户端失败: {error}"))?;
            let response = client
                .get(&stream.stream_url)
                .headers(headers)
                .send()
                .await
                .map_err(|error| format!("连接 FLV 直播源失败: {error}"))?
                .error_for_status()
                .map_err(|error| format!("FLV 直播源返回错误状态: {error}"))?;
            let mut connection = Connection::new(response);
            connection
                .read_frame(9)
                .await
                .map_err(|error| format!("读取 FLV header 失败: {error}"))?;
            let hook_emitter = emitter.clone();
            let file = LifecycleFile::with_hook(&working_pattern, "flv", move |file_name| {
                if let Ok(mut state) = hook_emitter.lock() {
                    state.finalize_raw_segment(file_name, "flv");
                }
            });
            let download = httpflv::download_checked(connection, file, segmentable);
            tokio::pin!(download);
            tokio::select! {
                result = &mut download => result.map_err(|error| format!("FLV 录制中断: {error}")),
                _ = wait_for_stop(stop_flag) => Ok(()),
            }
        }
        other => Err(format!(
            "15GB 安全切段当前仅支持 FLV/HLS/TS，直播源返回了 {other}。"
        )),
    }
}

async fn wait_for_stop(stop_flag: StopFlag) {
    while !stop_flag.load(Ordering::Acquire) {
        sleep(STOP_POLL_INTERVAL).await;
    }
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
    biliup::downloader::live::media_ext_from_url(&stream.stream_url)
        .unwrap_or_else(|| "flv".to_string())
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

fn new_session_id(display_name: &str, started: DateTime<Local>) -> String {
    let seq = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}-{}-{seq}",
        safe_file_component(display_name),
        started.format("%Y%m%d%H%M%S%3f")
    )
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
