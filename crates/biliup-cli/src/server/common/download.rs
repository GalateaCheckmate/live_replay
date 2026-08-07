use crate::server::common::replay;
use crate::server::common::upload::UploaderMessage;
use crate::server::common::util::FileValidator;
use crate::server::core::downloader::cover_downloader;
use crate::server::core::downloader::{
    DanmakuClient, DownloadStatus, DownloaderRuntime, SegmentEvent, SegmentInfo,
};
use crate::server::core::live::{danmaku_client, downloader_runtime, live_request};
use crate::server::core::monitor::Monitor;
use crate::server::errors::{AppError, AppResult};
use crate::server::infrastructure::context::{Context, Stage, WorkerStatus};
use crate::server::infrastructure::models::hook_step::process;
use async_channel::Sender;
use biliup::downloader::live::{LivePlugin, LiveStatus, LiveStream};
use biliup::uploader::line::{recording_finished, recording_started};
use error_stack::ResultExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

struct RecordingActivityGuard;

impl RecordingActivityGuard {
    fn new() -> Self {
        recording_started();
        Self
    }
}

impl Drop for RecordingActivityGuard {
    fn drop(&mut self) {
        recording_finished();
    }
}

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl RetryPolicy {
    pub fn exponential(max_attempts: u32) -> Self {
        Self {
            max_attempts,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(30),
        }
    }
}

pub struct SegmentEventProcessor {
    channel: Option<Sender<SegmentInfo>>,
    uploader: Sender<UploaderMessage>,
    ctx: Context,
    file_validator: FileValidator,
}

impl SegmentEventProcessor {
    pub fn new(uploader: Sender<UploaderMessage>, ctx: Context) -> Self {
        // 自动上传主播的尾段可能只有几秒。不能沿用原版的 20MB 过滤并直接删除；
        // 有效性由安全队列中的 ffprobe 检查决定，失败时保留文件并显示错误。
        let minimum_size = if ctx
            .upload_config()
            .as_ref()
            .is_some_and(|config| !config.is_noop_uploader())
        {
            0
        } else {
            ctx.config().filtering_threshold * 1000 * 1000
        };
        Self {
            channel: None,
            uploader,
            file_validator: FileValidator::new(minimum_size, true),
            ctx,
        }
    }

    pub fn process(&mut self, event: SegmentInfo) -> AppResult<()> {
        self.file_validator.validate(&event.prev_file_path)?;

        if let Some(tx) = &self.channel
            && tx.is_closed()
        {
            warn!(
                url = self.ctx.live_streamer().url,
                "segment channel closed, reopening"
            );
            self.channel = None;
        }

        match &self.channel {
            None => {
                let (tx, rx) = async_channel::unbounded();
                match self.ctx.upload_config().clone() {
                    Some(config) if !config.is_noop_uploader() => {
                        let ctx = self.ctx.clone();
                        tokio::spawn(async move {
                            // process_session 在建立数据库场次或冻结 Cookie 失败时会返回。
                            // 保留 Receiver 并重试，未消费的 SegmentInfo 会继续留在无界通道中，
                            // 不会因为上传初始化的瞬时失败而丢掉完整录像分段。
                            loop {
                                replay::process_session(rx.clone(), ctx.clone(), config.clone())
                                    .await;
                                if rx.is_closed() && rx.is_empty() {
                                    break;
                                }
                                warn!(
                                    url = ctx.live_streamer().url,
                                    "replay session stopped before draining segment queue; retrying initialization"
                                );
                                tokio::time::sleep(Duration::from_secs(30)).await;
                            }
                        });
                    }
                    _ => {
                        let res = self
                            .uploader
                            .force_send(UploaderMessage::SegmentEvent(rx, self.ctx.clone()))
                            .change_context(AppError::Custom(
                                "Failed to send to uploader".to_string(),
                            ))?;
                        if let Some(prev) = res {
                            warn!(SegmentEvent = ?prev, "replace an existing uploader message");
                        }
                    }
                }
                let res = tx.force_send(event).change_context(AppError::Custom(
                    "Failed to persist segment event".to_string(),
                ))?;
                if let Some(prev) = res {
                    warn!(SegmentEvent = ?prev, "replace an existing segment event");
                }
                self.channel = Some(tx);
            }
            Some(tx) => {
                let res = tx.force_send(event).change_context(AppError::Custom(
                    "Failed to persist segment event".to_string(),
                ))?;
                if let Some(prev) = res {
                    warn!(SegmentEvent = ?prev, "replace an existing segment event");
                }
            }
        }

        // 每个完整分段结束后重新检查空间。低于停止阈值时先保住刚完成的文件，
        // 然后优雅停止当前下载器，不再继续无限创建新分段。
        if let Err(space_error) = self.ctx.ensure_recording_space() {
            let ctx = self.ctx.clone();
            tokio::spawn(async move {
                error!(error = ?space_error, url = ctx.live_streamer().url, "stopping recording after completed segment because disk is low");
                ctx.change_status(Stage::Download, WorkerStatus::Pause)
                    .await;
            });
        }

        Ok(())
    }
}

pub struct DownloadTask {
    token: CancellationToken,
    done_notify: Notify,
    downloader: DownloaderRuntime,
}

impl DownloadTask {
    pub fn new(downloader: DownloaderRuntime) -> Self {
        Self {
            token: CancellationToken::new(),
            done_notify: Notify::new(),
            downloader,
        }
    }

    pub(self) async fn execute(
        &self,
        ctx: &Context,
        sender: Sender<UploaderMessage>,
        plugin: Arc<dyn LivePlugin + Send + Sync>,
        rooms_handle: Arc<Monitor>,
    ) -> AppResult<()> {
        let _recording_activity = RecordingActivityGuard::new();
        let mut retry_count = 0;
        let max_retries = 3;
        let base_delay = Duration::from_secs(2);
        let max_delay = Duration::from_secs(ctx.config().delay.max(1));
        let url = ctx.live_streamer().url.clone();
        let mut stream = ctx.live_stream().clone();
        let filename_prefix = ctx
            .live_streamer()
            .filename_prefix
            .clone()
            .or_else(|| ctx.config().filename_prefix.clone());
        let danmaku_client = danmaku_client(
            stream.danmaku.as_ref(),
            filename_prefix.as_deref(),
            &stream.name,
        );
        if let Some(ref client) = danmaku_client {
            info!("Starting danmaku client for stream: {}", url);
            client.download().await?;
        }

        let mut processor = SegmentEventProcessor::new(sender, ctx.clone());
        let result = loop {
            let components = self
                .download(&mut processor, ctx.clone(), danmaku_client.clone(), &stream)
                .await;
            info!("initialize_components completed: {url}");

            if self.token.is_cancelled() {
                info!(url = url, "task is cancelled");
                break components;
            }
            match plugin.check_stream(live_request(ctx.worker())).await {
                Ok(LiveStatus::Live {
                    stream: next_stream,
                }) => {
                    stream = *next_stream;
                    info!(
                        url = url,
                        "Stream is still live, preparing to retry. attempt: {}", retry_count
                    );
                    retry_count = 0;
                }
                Ok(LiveStatus::Offline) => {
                    retry_count += 1;
                    info!(url = url, "Stream went offline, stopping download");
                }
                Err(e) => {
                    retry_count += 1;
                    warn!(
                        url = url,
                        "Failed to check stream status: {:?}, stopping download", e
                    );
                }
            }

            if retry_count >= max_retries {
                warn!(
                    url = url,
                    "Maximum retry attempts ({}) reached, stopping", max_retries
                );
                break components;
            }

            let delay = if retry_count != 0 {
                base_delay * 2_u32.pow(retry_count)
            } else {
                Duration::ZERO
            }
            .min(max_delay);
            info!("Retrying download in {:?}...", delay);
            tokio::time::sleep(delay).await;
        };
        if let Some(client) = danmaku_client.clone()
            && let Err(e) = client.stop().await
        {
            error!("Error stopping danmaku client: {}", e);
        }
        rooms_handle.wake_waker(ctx.worker_id()).await;
        info!("Download task completed: {:?}", result);
        self.done_notify.notify_one();
        Ok(())
    }

    async fn download(
        &self,
        processor: &mut SegmentEventProcessor,
        ctx: Context,
        danmaku_client: Option<Arc<dyn DanmakuClient + Send + Sync>>,
        stream: &LiveStream,
    ) -> AppResult<DownloadStatus> {
        let streamer = ctx.live_streamer();
        let hook = |event| match event {
            SegmentEvent::Start { .. } => warn!("Ignoring unexpected segment start event"),
            SegmentEvent::Segment(mut event) => {
                if let Some(ref client) = danmaku_client {
                    let danmaku_file_path = event.prev_file_path.with_extension("xml");
                    match client.rolling(&danmaku_file_path.display().to_string()) {
                        Ok(true) => event.danmaku_file_path = Some(danmaku_file_path),
                        Ok(false) => {}
                        Err(e) => error!("Danmaku rolling error: {}", e),
                    }
                }
                if let Err(e) = processor.process(event) {
                    error!("Failed to process segment event: {}", e);
                }
            }
        };

        let download_config = ctx.download_config(stream);
        info!(
            page_url = streamer.url,
            stream_url = download_config.url,
            platform = stream.platform,
            suffix = download_config.suffix,
            "开始下载，已解析流直链"
        );
        let result = self
            .downloader
            .download(Box::new(hook), download_config)
            .await
            .change_context(AppError::Custom("Failed to download segment".into()))?;
        info!(url=streamer.url,result=?result, "finished downloading");
        Ok(result)
    }

    pub(crate) async fn stop(&self) -> AppResult<()> {
        self.token.cancel();
        self.downloader.stop().await?;
        self.done_notify.notified().await;
        Ok(())
    }
}

pub async fn start_download_workflow(
    downloader: Arc<dyn LivePlugin + Send + Sync>,
    ctx: Context,
    sender: Sender<UploaderMessage>,
    rooms_handle: Arc<Monitor>,
) {
    if let Err(error) = ctx.ensure_recording_space() {
        error!(error = ?error, url = ctx.live_streamer().url, "recording skipped by disk-space protection");
        rooms_handle.wake_waker(ctx.worker_id()).await;
        return;
    }

    let task = Arc::new(DownloadTask::new(downloader_runtime(
        ctx.config().downloader,
        ctx.live_stream(),
    )));
    let recording_template = ctx
        .recorder(ctx.streamer_info().clone())
        .filename_template();
    ctx.worker().mark_recording_started(recording_template);
    ctx.change_status(Stage::Download, WorkerStatus::Working(task.clone()))
        .await;

    tokio::spawn({
        let streamer_info = ctx.streamer_info();
        let live_cover_url = streamer_info.live_cover_path.clone();
        let format_filename = ctx.recorder(streamer_info.clone()).format_filename();
        let client = ctx.stateless_client().client.clone();
        let enabled = ctx
            .config()
            .use_live_cover
            .map(|u| u && !live_cover_url.is_empty())
            .unwrap_or(false);
        async move {
            cover_downloader::download_cover_with(
                &live_cover_url,
                enabled,
                &format_filename,
                client,
            )
            .await
        }
    });

    process(&[], &ctx.live_streamer().preprocessor).await;
    let _ = task.execute(&ctx, sender, downloader, rooms_handle).await;
    ctx.worker().mark_recording_finished();
    process(&[], &ctx.live_streamer().downloaded_processor).await;
    info!(
        "Download workflow completed {} => {:?}",
        ctx.live_streamer().url,
        ctx.status(Stage::Download)
    );
}
