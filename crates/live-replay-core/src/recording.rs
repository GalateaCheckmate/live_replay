use crate::{ResolvedStream, StopFlag};
use biliup::client::StatelessClient;
use biliup::downloader::flv_parser::header;
use biliup::downloader::hls;
use biliup::downloader::httpflv::{self, Connection};
use biliup::downloader::util::{LifecycleFile, Segmentable};
use biliup::uploader::line::{recording_finished, recording_started};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::fs;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::sleep;

#[derive(Debug, Clone)]
pub struct SegmentedRecordingResult {
    pub files: Vec<String>,
    pub stopped_by_user: bool,
}

struct RecordingGuard;

impl RecordingGuard {
    fn new() -> Self {
        recording_started();
        Self
    }
}

impl Drop for RecordingGuard {
    fn drop(&mut self) {
        recording_finished();
    }
}

pub async fn record_segmented_stream(
    stream: ResolvedStream,
    output_dir: impl AsRef<Path>,
    stop_flag: StopFlag,
    segment_minutes: u64,
) -> Result<SegmentedRecordingResult, String> {
    record_segmented_stream_with_events(stream, output_dir, stop_flag, segment_minutes, None).await
}

pub async fn record_segmented_stream_with_events(
    stream: ResolvedStream,
    output_dir: impl AsRef<Path>,
    stop_flag: StopFlag,
    segment_minutes: u64,
    segment_tx: Option<UnboundedSender<String>>,
) -> Result<SegmentedRecordingResult, String> {
    let _recording_guard = RecordingGuard::new();
    let output_dir = output_dir.as_ref();
    fs::create_dir_all(output_dir)
        .await
        .map_err(|error| format!("创建录制目录失败: {error}"))?;

    let headers = to_header_map(&stream.headers)?;
    let client = StatelessClient::new(headers, None);
    let response = client
        .retryable(&stream.stream_url)
        .await
        .map_err(|error| format!("连接直播源失败: {error}"))?;
    let mut connection = Connection::new(response);
    let first = connection
        .read_frame(9)
        .await
        .map_err(|error| format!("读取直播流头失败: {error}"))?;

    let completed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let completed_hook = std::sync::Arc::clone(&completed);
    let template = output_dir
        .join(format!("{}_%Y-%m-%d_%H-%M-%S", safe_file_component(&stream.name)))
        .to_string_lossy()
        .into_owned();
    let segment = Segmentable::new(
        Some(Duration::from_secs(segment_minutes.max(1).saturating_mul(60))),
        None,
    );

    {
        let download = async move {
            let hook = move |path: &str| {
                let mut should_emit = false;
                if let Ok(mut files) = completed_hook.lock()
                    && !files.iter().any(|existing| existing == path)
                {
                    files.push(path.to_string());
                    should_emit = true;
                }
                if should_emit && let Some(tx) = &segment_tx {
                    let _ = tx.send(path.to_string());
                }
            };

            if header(&first).is_ok() {
                let file = LifecycleFile::with_hook(&template, "flv", hook);
                httpflv::download(connection, file, segment).await;
                Ok::<(), String>(())
            } else {
                let file = LifecycleFile::with_hook(&template, "ts", hook);
                hls::download(&stream.stream_url, &client, file, segment)
                    .await
                    .map_err(|error| format!("HLS 录制失败: {error}"))
            }
        };

        tokio::pin!(download);
        loop {
            tokio::select! {
                result = &mut download => {
                    result?;
                    break;
                }
                _ = sleep(Duration::from_millis(250)) => {
                    if stop_flag.load(Ordering::Acquire) {
                        break;
                    }
                }
            }
        }
    }

    // The downloader future is now dropped, which finalizes the active .part
    // through FlvFile/TsFile::drop and sends its completion event.
    tokio::task::yield_now().await;

    let files = completed
        .lock()
        .map_err(|_| "录制分段结果锁异常".to_string())?
        .clone();

    Ok(SegmentedRecordingResult {
        files,
        stopped_by_user: stop_flag.load(Ordering::Acquire),
    })
}

fn to_header_map(input: &std::collections::HashMap<String, String>) -> Result<HeaderMap, String> {
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
