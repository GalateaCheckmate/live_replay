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
use error_stack::ResultExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

// Configuration and retry policy
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

/// 分段事件处理器
pub struct SegmentEventProcessor {
    channel: Option<Sender<SegmentInfo>>,
    uploader: Sender<UploaderMessage>,
    ctx: Context,
    file_validator: FileValidator,
}

impl SegmentEventProcessor {
    /// 创建处理器
    pub fn new(uploader: Sender<UploaderMessage>, ctx: Context) -> Self {
        Self {
            channel: None,
            uploader,
            file_validator: FileValidator::new(
                ctx.config().filtering_threshold * 1000 * 1000,
                true,
            ),
            ctx,
        }
    }

    /// 处理分段事件
    pub fn process(&mut self, event: SegmentInfo) -> AppResult<()> {
        // 只有下载器已关闭并通过文件验证的完整分段，才允许进入上传队列。
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
                // 无界通道只负责快速登记分段。上传重试在独立数据库队列中执行，
                // 网络或账号故障不会反向阻塞录制线程。
                let (tx, rx) = async_channel::unbounded();

                match self.ctx.upload_config().clone() {
                    Some(config) if !config.is_noop_uploader() => {
                        let ctx = self.ctx.clone();
                        tokio::spawn(async move {
                            replay::process_session(rx, ctx, config).await;
                        });
                    }
                    _ => {
                        // Noop 或无上传配置继续沿用原版后处理链路。
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

                let res = tx
                    .force_send(event)
                    .change_context(AppError::Custom("Failed to persist segment event".to_string()))?;
                if let Some(prev) = res {
                    warn!(SegmentEvent = ?prev, "replace an existing segment event");
                }
                self.channel = Some(tx);
            }
            Some(tx) => {
                let res = tx
                    .force_send(event)
                    .change_context(AppError::Custom("Failed to persist segment event".to_string()))?;
                if let Some(prev) = res {
                    warn!(SegmentEvent = ?prev, "replace an existing segment event");
                }
            }
        }

        Ok(())
    }
}

/// 下载任务
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
        // 重试配置
        let mut retry_count = 0;
        let max_retries = 3; // 最大重试次数
        let base_delay = Duration::from_secs(2); // 基础延迟时间（2秒）
        let max_delay = Duration::from_secs(ctx.config().delay); // 最大延迟时间
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
        // 启动弹幕客户端
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
            // 检查流状态
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

            info!(
                url = url,
                "preparing to retry. Attempt: {}/{}",
                retry_count + 1,
                max_retries
            );

            let delay = if retry_count != 0 {
                base_delay * 2_u32.pow(retry_count)
            } else {
                Duration::ZERO
            };
            let delay = delay.min(max_delay);

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

        let hook = |event| {
            match event {
                SegmentEvent::Start { .. } => {
                    warn!("Ignoring unexpected segment start event");
                }
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

/// 启动完整下载流程。
///
/// 只能由 `Monitor` 在取得下载池许可后调用；调用方必须把许可移动到同一个任务中，
/// 并持有到本函数返回，保证 `pool1_size` 是下载并发的唯一限制。
pub async fn start_download_workflow(
    downloader: Arc<dyn LivePlugin + Send + Sync>,
    ctx: Context,
    sender: Sender<UploaderMessage>,
    rooms_handle: Arc<Monitor>,
) {
    let task = Arc::new(DownloadTask::new(downloader_runtime(
        ctx.config().downloader,
        ctx.live_stream(),
    )));
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

    process(&[], &ctx.live_streamer().downloaded_processor).await;

    info!(
        "Download workflow completed {} => {:?}",
        ctx.live_streamer().url,
        ctx.status(Stage::Download)
    );
}
