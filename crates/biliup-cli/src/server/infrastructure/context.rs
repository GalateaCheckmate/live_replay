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

#[derive(Debug, Clone)]
pub struct Context {
    id: i64,
    worker: Arc<Worker>,
    stream: LiveStream,
    streamer_info: StreamerInfo,
    pool: ConnectionPool,
}

impl Context {
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
        self.worker.get_streamer()
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

    pub fn ensure_recording_space(&self) -> AppResult<()> {
        let directory = self.recording_output_dir();
        let Some(free_bytes) = free_space_bytes(&directory) else {
            warn!(directory = ?directory, "unable to read free disk space; recording is allowed");
            return Ok(());
        };

        let stop_gb = env_u64("LIVE_REPLAY_DISK_STOP_GB", DEFAULT_DISK_STOP_GB).max(1);
        let warning_gb = env_u64("LIVE_REPLAY_DISK_WARNING_GB", DEFAULT_DISK_WARNING_GB)
            .max(stop_gb);
        let free_gb = free_bytes as f64 / GIB as f64;

        if free_bytes < stop_gb.saturating_mul(GIB) {
            return Err(AppError::Custom(format!(
                "录像目录 {} 仅剩 {:.1} GB，低于停止阈值 {} GB；不会继续创建新分段",
                directory.display(),
                free_gb,
                stop_gb
            ))
            .into());
        }

        if free_bytes < warning_gb.saturating_mul(GIB) {
            warn!(directory = ?directory, free_gb, warning_gb, "recording disk space is below warning threshold");
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

        let segment_time = validated_segment_time(config.segment_time.as_deref())
            .or_else(|| Some(DEFAULT_SEGMENT_TIME.to_string()));

        DownloadConfig {
            url: stream.raw_stream_url.to_string(),
            segment_time,
            file_size: None,
            headers: stream.stream_headers.clone(),
            recorder: self.recorder(stream_info),
            output_dir: self.recording_output_dir(),
            suffix,
        }
    }
}

/// 仅接受 HH:MM:SS，分钟和秒必须小于60，总时长至少60秒。
/// 非法值不会传给下载器，而是回退到默认一小时。
fn validated_segment_time(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    let mut parts = value.split(':');
    let hours = parts.next()?.parse::<u64>().ok()?;
    let minutes = parts.next()?.parse::<u64>().ok()?;
    let seconds = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() || minutes >= 60 || seconds >= 60 {
        return None;
    }
    let total = hours
        .saturating_mul(3600)
        .saturating_add(minutes.saturating_mul(60))
        .saturating_add(seconds);
    if total < 60 {
        return None;
    }
    Some(format!("{hours:02}:{minutes:02}:{seconds:02}"))
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

#[cfg(test)]
mod tests {
    use super::validated_segment_time;

    #[test]
    fn validates_segment_time() {
        assert_eq!(validated_segment_time(Some("01:00:00")).as_deref(), Some("01:00:00"));
        assert_eq!(validated_segment_time(Some("1:2:3")).as_deref(), Some("01:02:03"));
        assert_eq!(validated_segment_time(Some("00:00:59")), None);
        assert_eq!(validated_segment_time(Some("00:60:00")), None);
        assert_eq!(validated_segment_time(Some("bad")), None);
    }
}
