use crate::server::common::download::DownloadTask;
use crate::server::common::util::Recorder;
use crate::server::config::Config;
use crate::server::core::downloader::DownloadConfig;
use crate::server::core::live::streamer_info;
use crate::server::errors::{AppError, AppResult};
use crate::server::infrastructure::connection_pool::ConnectionPool;
use crate::server::infrastructure::models::StreamerInfo;
use crate::server::infrastructure::models::live_streamer::LiveStreamer;
use crate::server::infrastructure::models::upload_streamer::UploadStreamer;
use biliup::client::StatelessClient;
use biliup::downloader::live::LiveStream;
use core::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, RwLock};
use struct_patch::Patch;
use tracing::{error, info, warn};

const DEFAULT_SEGMENT_TIME: &str = "01:00:00";
const DEFAULT_DISK_WARNING_GB: u64 = 100;
const DEFAULT_DISK_STOP_GB: u64 = 30;
const GIB: u64 = 1024 * 1024 * 1024;

/// 应用程序上下文，包含工作器和扩展信息
#[derive(Debug, Clone)]
pub struct Context {
    id: i64,
    /// 工作器实例
    worker: Arc<Worker>,
    stream: LiveStream,
    streamer_info: StreamerInfo,
    pool: ConnectionPool,
}

impl Context {
    /// 创建新的上下文实例
    pub fn new(id: i64, worker: Arc<Worker>, pool: ConnectionPool, stream: LiveStream) -> Self {
        let mut streamer_info = streamer_info(&stream);
        streamer_info.id = id;
        Self {
            id,
            worker,
            stream,
            streamer_info,
            pool,
        }
    }

    pub fn worker_id(&self) -> i64 {
        self.worker.id()
    }

    pub fn id(&self) -> i64 {
        self.id
    }

    pub(crate) fn worker(&self) -> &Arc<Worker> {
        &self.worker
    }

    pub fn live_streamer(&self) -> &LiveStreamer {
        &self.worker.get_streamer()
    }

    pub fn stateless_client(&self) -> &StatelessClient {
        &self.worker.client
    }

    pub fn config(&self) -> Config {
        self.worker.get_config()
    }

    pub fn pool(&self) -> &ConnectionPool {
        &self.pool
    }

    pub async fn change_status(&self, stage: Stage, status: WorkerStatus) {
        self.worker.change_status(stage, status).await;
    }

    pub fn status(&self, stage: Stage) -> WorkerStatus {
        match stage {
            Stage::Download => self.worker.downloader_status.read().unwrap().clone(),
            Stage::Upload => self.worker.uploader_status.read().unwrap().clone(),
        }
    }

    pub fn upload_config(&self) -> &Option<UploadStreamer> {
        self.worker.get_upload_config()
    }

    pub fn recorder(&self, streamer_info: StreamerInfo) -> Recorder {
        Recorder::new(
            self.live_streamer()
                .filename_prefix
                .clone()
                .or(self.config().filename_prefix.clone()),
            streamer_info,
        )
    }

    pub fn live_stream(&self) -> &LiveStream {
        &self.stream
    }

    pub fn streamer_info(&self) -> &StreamerInfo {
        &self.streamer_info
    }

    /// Live Replay 默认优先写入 D 盘，避免长期录像占满系统盘。
    /// `LIVE_REPLAY_OUTPUT_DIR` 可覆盖默认目录。
    pub fn recording_output_dir(&self) -> PathBuf {
        if let Ok(value) = std::env::var("LIVE_REPLAY_OUTPUT_DIR")
            && !value.trim().is_empty()
        {
            return ensure_directory(PathBuf::from(value));
        }

        #[cfg(windows)]
        {
            let d_drive = Path::new(r"D:\");
            if d_drive.exists() {
                return ensure_directory(PathBuf::from(r"D:\LiveReplay\Recordings"));
            }
        }

        ensure_directory(PathBuf::from("recordings"))
    }

    /// 新场次开始前检查磁盘空间。空间低于停止阈值时拒绝启动新的录像；
    /// 已完成且未验证上传的文件绝不会因此被删除。
    pub fn ensure_recording_space(&self) -> AppResult<()> {
        let directory = self.recording_output_dir();
        let Some(free_bytes) = free_space_bytes(&directory) else {
            warn!(directory = ?directory, "unable to read free disk space; recording is allowed");
            return Ok(());
        };

        let warning_gb = env_u64("LIVE_REPLAY_DISK_WARNING_GB", DEFAULT_DISK_WARNING_GB);
        let stop_gb = env_u64("LIVE_REPLAY_DISK_STOP_GB", DEFAULT_DISK_STOP_GB);
        let free_gb = free_bytes as f64 / GIB as f64;

        if free_bytes < stop_gb.saturating_mul(GIB) {
            return Err(AppError::Custom(format!(
                "录像目录 {} 仅剩 {:.1} GB，低于停止阈值 {} GB；不会开始新的录像",
                directory.display(),
                free_gb,
                stop_gb
            ))
            .into());
        }

        if free_bytes < warning_gb.saturating_mul(GIB) {
            warn!(
                directory = ?directory,
                free_gb,
                warning_gb,
                "recording disk space is below warning threshold"
            );
        }
        Ok(())
    }

    pub fn download_config(&self, stream: &LiveStream) -> DownloadConfig {
        let config = self.config();
        let suffix = self
            .live_streamer()
            .format
            .clone()
            .unwrap_or_else(|| stream.suffix.to_string());
        let mut stream_info = streamer_info(stream);
        if stream.url == self.stream.url {
            stream_info.id = self.streamer_info.id;
        }

        // Live Replay 默认每60分钟生成一个完整文件。用户显式配置时仍以用户配置为准。
        let segment_time = config
            .segment_time
            .or_else(|| Some(DEFAULT_SEGMENT_TIME.to_string()));

        DownloadConfig {
            url: stream.raw_stream_url.to_string(),
            segment_time,
            // 使用时间分段时关闭大小分段，避免同一直播产生不可预测的小分片。
            file_size: None,
            headers: stream.stream_headers.clone(),
            recorder: self.recorder(stream_info),
            output_dir: self.recording_output_dir(),
            suffix,
        }
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn free_space_bytes(path: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        let path_text = path.to_string_lossy();
        let drive = path_text
            .chars()
            .next()
            .filter(|_| path_text.chars().nth(1) == Some(':'))?;
        let script = format!(
            "$d = Get-PSDrive -Name '{}'; if ($d) {{ [Console]::Write($d.Free) }}",
            drive
        );
        let output = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        return String::from_utf8(output.stdout)
            .ok()?
            .trim()
            .parse::<u64>()
            .ok();
    }

    #[cfg(not(windows))]
    {
        let output = Command::new("df").arg("-Pk").arg(path).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        let available_kib = text
            .lines()
            .last()?
            .split_whitespace()
            .nth(3)?
            .parse::<u64>()
            .ok()?;
        Some(available_kib.saturating_mul(1024))
    }
}

fn ensure_directory(path: PathBuf) -> PathBuf {
    if let Err(error) = std::fs::create_dir_all(&path) {
        warn!(directory = ?path, error = ?error, "failed to create recording directory; using current directory");
        PathBuf::from(".")
    } else {
        path
    }
}

/// 工作器结构体，管理单个主播的录制和上传任务
#[derive(Debug)]
pub struct Worker {
    pub downloader_status: RwLock<WorkerStatus>,
    pub uploader_status: RwLock<WorkerStatus>,
    pub live_streamer: LiveStreamer,
    pub upload_streamer: Option<UploadStreamer>,
    config: Arc<RwLock<Config>>,
    pub client: StatelessClient,
}

impl Worker {
    pub fn new(
        live_streamer: LiveStreamer,
        upload_streamer: Option<UploadStreamer>,
        config: Arc<RwLock<Config>>,
        client: StatelessClient,
    ) -> Self {
        Self {
            downloader_status: RwLock::new(Default::default()),
            uploader_status: Default::default(),
            live_streamer,
            upload_streamer,
            config,
            client,
        }
    }

    pub fn id(&self) -> i64 {
        self.live_streamer.id
    }

    pub fn get_streamer(&self) -> &LiveStreamer {
        &self.live_streamer
    }

    pub fn get_upload_config(&self) -> &Option<UploadStreamer> {
        &self.upload_streamer
    }

    pub fn get_config(&self) -> Config {
        let mut cfg = self.config.read().unwrap().clone();
        if let Some(cfg_p) = self.live_streamer.override_cfg.clone() {
            cfg.apply(cfg_p)
        }
        cfg
    }

    pub async fn change_status(&self, stage: Stage, status: WorkerStatus) {
        match stage {
            Stage::Download => {
                let task = if let WorkerStatus::Working(task) =
                    &*self.downloader_status.read().unwrap()
                    && !matches!(status, WorkerStatus::Working(_))
                {
                    Some(task.clone())
                } else {
                    None
                };

                *self.downloader_status.write().unwrap() = status;

                if let Some(task) = task
                    && let Err(e) = task.stop().await
                {
                    error!(error = ?e, "Failed to stop downloader");
                }
            }
            Stage::Upload => {
                *self.uploader_status.write().unwrap() = status;
            }
        }
    }
}

pub fn find_worker(workers: &[Arc<Worker>], id: i64) -> Option<&Arc<Worker>> {
    workers.iter().find(|worker| worker.live_streamer.id == id)
}

impl Drop for Worker {
    fn drop(&mut self) {
        info!("Dropping worker {}", self.live_streamer.id);
    }
}

impl PartialEq for Worker {
    fn eq(&self, other: &Self) -> bool {
        self.live_streamer.id == other.live_streamer.id
    }
}

impl Eq for Worker {}

#[derive(Debug)]
pub enum Stage {
    Download,
    Upload,
}

#[derive(Default, Clone)]
pub enum WorkerStatus {
    Working(Arc<DownloadTask>),
    Pending,
    #[default]
    Idle,
    Pause,
}

impl fmt::Debug for WorkerStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            WorkerStatus::Working(_) => "Working",
            WorkerStatus::Pending => "Pending",
            WorkerStatus::Idle => "Idle",
            WorkerStatus::Pause => "Pause",
        };
        f.write_str(name)
    }
}
