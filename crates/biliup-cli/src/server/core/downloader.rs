pub mod cover_downloader;
/// FFmpeg下载器实现
pub mod ffmpeg_downloader;
/// Stream-gears下载器实现
pub mod stream_gears;
pub mod streamlink;
pub mod ytdlp;

use crate::server::common::util::Recorder;
use crate::server::core::downloader::ffmpeg_downloader::FfmpegDownloader;
use crate::server::core::downloader::stream_gears::StreamGears;
use crate::server::core::downloader::streamlink::Streamlink;
use crate::server::core::downloader::ytdlp::YouTubeDownloader;
use crate::server::errors::{AppError, AppResult};
use async_trait::async_trait;
use danmaku_client::{DanmakuRecorder, RecorderConfig, RecorderHandle};
use error_stack::Report;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Live Replay 以文件大小作为统一的录制分段标准。
/// 使用十进制 15 GB，给后续封装和平台侧限制留出余量。
pub const LIVE_REPLAY_SEGMENT_BYTES: u64 = 15_000_000_000;
const DOWNLOAD_FAILURE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(5);

/// 下载器配置
/// 包含下载过程中需要的各种参数和设置
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct DownloadConfig {
    /// 流 URL
    pub(crate) url: String,
    /// 分段时长 (格式: "HH:MM:SS")
    pub segment_time: Option<String>,

    /// 分段文件大小限制 (字节)
    pub file_size: Option<u64>,

    /// HTTP请求头
    pub headers: HashMap<String, String>,

    /// 录制器实例
    pub recorder: Recorder,

    /// 输出目录路径
    pub output_dir: PathBuf,

    pub suffix: String,
}

impl DownloadConfig {
    /// 生成输出文件名
    ///
    /// # 返回
    /// 返回完整的输出文件路径
    fn generate_output_filename(&self, suffix: &str) -> PathBuf {
        self.output_dir.join(self.recorder.generate_path(suffix))
    }

    fn apply_live_replay_segment_policy(&mut self) {
        self.segment_time = None;
        self.file_size = Some(LIVE_REPLAY_SEGMENT_BYTES);
    }
}

/// 下载器类型枚举
/// 定义支持的各种下载器类型
#[derive(PartialEq, Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DownloaderType {
    /// Ytarchive下载器
    Ytarchive,
    /// 同步下载器
    #[serde(rename = "sync-downloader")]
    SyncDownloader,
    /// 使用stream-gears
    #[serde(rename = "stream-gears")]
    StreamGears,
    /// FFmpeg下载器
    Ffmpeg,
    /// FFmpeg外部分段
    FfmpegExternal,
    /// FFmpeg内部分段
    FfmpegInternal,
    /// Streamlink下载器
    Streamlink,
    /// yt-dlp下载器
    YtDlp,
}

/// 实际的下载器枚举（包含实例）
pub enum DownloaderRuntime {
    Ffmpeg(FfmpegDownloader),
    StreamGears(StreamGears),
    StreamLink(Streamlink),
    YtDlp(YouTubeDownloader),
}

impl DownloaderRuntime {
    /// 从配置创建
    pub fn from_type(downloader_type: DownloaderType) -> Self {
        match downloader_type {
            DownloaderType::Ffmpeg => Self::Ffmpeg(FfmpegDownloader::new(
                Vec::new(),
                DownloaderType::FfmpegExternal,
            )),
            _ => Self::StreamGears(StreamGears::new(None)),
        }
    }

    pub async fn download<'a>(
        &self,
        callback: Box<dyn FnMut(SegmentEvent) + Send + Sync + 'a>,
        mut download_config: DownloadConfig,
    ) -> AppResult<DownloadStatus> {
        // 所有 Live Replay 录制引擎都从这里拿到同一套分段策略，避免某个引擎
        // 继续沿用旧的按时长分段配置。
        download_config.apply_live_replay_segment_policy();

        let started = tokio::time::Instant::now();
        let result = match self {
            Self::Ffmpeg(d) => d.download(callback, download_config).await,
            Self::StreamGears(d) => d.download(callback, download_config).await,
            DownloaderRuntime::StreamLink(d) => d.download(callback, download_config).await,
            Self::YtDlp(d) => d.download(callback, download_config).await,
        };

        // 正常的外部分段需要立刻开始下一段，不能人为等待。只有流结束或失败时
        // 才设置最小冷却时间，避免源站异常时反复创建进程、占满 CPU 和日志。
        let needs_cooldown = matches!(
            &result,
            Err(_) | Ok(DownloadStatus::StreamEnded) | Ok(DownloadStatus::Error(_))
        );
        if needs_cooldown {
            let elapsed = started.elapsed();
            if elapsed < DOWNLOAD_FAILURE_COOLDOWN {
                tokio::time::sleep(DOWNLOAD_FAILURE_COOLDOWN - elapsed).await;
            }
        }

        result
    }

    pub async fn stop(&self) -> AppResult<()> {
        match self {
            Self::Ffmpeg(d) => d.stop().await,
            Self::StreamGears(d) => d.stop().await,
            DownloaderRuntime::StreamLink(d) => d.stop().await,
            Self::YtDlp(d) => d.stop().await,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SegmentInfo {
    /// 分段文件路径
    pub prev_file_path: PathBuf,
    pub danmaku_file_path: Option<PathBuf>,
    pub next_file_path: Option<PathBuf>,
    /// 分段序号
    pub segment_index: usize,
}

impl SegmentInfo {
    pub fn new(
        prev_file_path: PathBuf,
        danmaku_file_path: Option<PathBuf>,
        next_file_path: Option<PathBuf>,
        segment_index: usize,
    ) -> Self {
        Self {
            prev_file_path,
            danmaku_file_path,
            next_file_path,
            segment_index,
        }
    }
}

/// 分段事件
/// 当下载器完成一个分段时触发的事件
#[derive(Debug, Clone)]
pub enum SegmentEvent {
    Start {
        /// 分段文件路径
        next_file_path: PathBuf,
    },
    Segment(SegmentInfo),
}

/// 下载状态
/// 表示下载器当前的状态
#[derive(Debug, Clone, PartialEq)]
pub enum DownloadStatus {
    /// 正在下载
    Downloading,
    /// 正常分段（外部分段触发）
    SegmentCompleted,
    /// 直播流结束
    StreamEnded,
    /// 错误
    Error(String),
}

#[async_trait]
pub trait DanmakuClient {
    /// Starts danmaku recording and manages lifecycle
    async fn download(&self) -> AppResult<()>;

    async fn stop(&self) -> AppResult<()>;

    /// 返回 true 表示本次滚动保存产生了可交给后处理的弹幕文件。
    fn rolling(&self, _file_name: &str) -> Result<bool, Box<dyn std::error::Error>> {
        Ok(false)
    }
}

pub struct RustDanmakuClient {
    config: RecorderConfig,
    handle: Mutex<Option<RecorderHandle>>,
}

impl RustDanmakuClient {
    pub fn new(config: RecorderConfig) -> Self {
        Self {
            config,
            handle: Mutex::new(None),
        }
    }
}

#[async_trait]
impl DanmakuClient for RustDanmakuClient {
    async fn download(&self) -> AppResult<()> {
        let mut handle = self.handle.lock().unwrap();
        if handle.is_some() {
            return Ok(());
        }

        let recorder = DanmakuRecorder::new(self.config.clone())
            .map_err(|e| Report::new(AppError::Custom(e.to_string())))?;
        *handle = Some(recorder.start());
        Ok(())
    }

    async fn stop(&self) -> AppResult<()> {
        let handle = self.handle.lock().unwrap().take();
        if let Some(handle) = handle {
            handle
                .stop()
                .await
                .map_err(|e| Report::new(AppError::Custom(e.to_string())))?;
        }
        Ok(())
    }

    fn rolling(&self, file_name: &str) -> Result<bool, Box<dyn std::error::Error>> {
        let handle = self
            .handle
            .lock()
            .map_err(|_| "danmaku handle lock poisoned")?
            .clone();
        if let Some(handle) = handle {
            return tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(handle.rolling(Some(PathBuf::from(file_name))))
            })
            .map_err(Into::into);
        }
        Ok(false)
    }
}

/// 解析时长字符串 "HH:MM:SS" 为秒数
fn parse_duration(duration: &str) -> u64 {
    let parts: Vec<&str> = duration.split(':').collect();
    if parts.len() == 3 {
        let hours: u64 = parts[0].parse().unwrap_or(0);
        let minutes: u64 = parts[1].parse().unwrap_or(0);
        let seconds: u64 = parts[2].parse().unwrap_or(0);
        hours * 3600 + minutes * 60 + seconds
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_replay_uses_15gb_size_segments() {
        let mut config = DownloadConfig::default();
        config.segment_time = Some("01:00:00".to_string());
        config.file_size = Some(1024);

        config.apply_live_replay_segment_policy();

        assert_eq!(config.segment_time, None);
        assert_eq!(config.file_size, Some(15_000_000_000));
    }
}
