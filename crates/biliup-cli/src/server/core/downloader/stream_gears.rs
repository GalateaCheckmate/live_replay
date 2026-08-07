use crate::server::common::construct_headers;
use crate::server::common::util::parse_time;
use crate::server::core::downloader::{DownloadConfig, DownloadStatus, SegmentEvent, SegmentInfo};
use crate::server::errors::{AppError, AppResult};
use biliup::client::StatelessClient;
use biliup::downloader::flv_parser::header;
use biliup::downloader::httpflv::Connection;
use biliup::downloader::util::{LifecycleFile, Segmentable};
use biliup::downloader::{hls, httpflv};
use error_stack::{ResultExt, bail};
use nom::Err;
use std::path::PathBuf;
use std::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

/// Stream-gears下载器实现
/// 使用stream-gears库进行直播流下载
pub struct StreamGears {
    /// 代理设置（可选）
    proxy: Option<String>,

    token: RwLock<CancellationToken>,
}

impl StreamGears {
    pub fn new(proxy: Option<String>) -> Self {
        Self {
            proxy,
            token: RwLock::new(CancellationToken::new()),
        }
    }

    async fn start_download<'a>(
        &self,
        mut callback: Box<dyn FnMut(SegmentEvent) + Send + Sync + 'a>,
        download_config: DownloadConfig,
    ) -> AppResult<DownloadStatus> {
        let url = download_config.url.clone();
        let file_name = download_config
            .output_dir
            .join(download_config.recorder.filename_template())
            .to_string_lossy()
            .into_owned();
        let headers_in = construct_headers(&download_config.headers).map_err(AppError::Custom)?;
        let proxy = self.proxy.clone();
        let segment = Segmentable::new(
            download_config.segment_time.as_deref().map(parse_time),
            download_config.file_size,
        );

        let client = StatelessClient::new(headers_in, proxy.as_deref());
        let response = client
            .retryable(&url)
            .await
            .change_context(AppError::Unknown)?;
        let mut connection = Connection::new(response);
        let bytes = connection
            .read_frame(9)
            .await
            .change_context(AppError::Unknown)?;

        let hook = {
            let mut i = 0;
            move |s: &str| {
                let file_path = PathBuf::from(s);
                let event = SegmentInfo {
                    prev_file_path: file_path,
                    danmaku_file_path: None,
                    next_file_path: None,
                    segment_index: i,
                };
                callback(SegmentEvent::Segment(event));
                i += 1;
            }
        };

        match header(&bytes) {
            Ok((_i, header)) => {
                debug!("header: {header:#?}");
                info!("Downloading {}...", url);
                let file = LifecycleFile::with_hook(&file_name, "flv", hook);
                httpflv::download(connection, file, segment.clone()).await;
            }
            Err(Err::Incomplete(needed)) => {
                return Err(AppError::Custom(format!(
                    "直播流头部数据不完整：{needed:?}"
                ))
                .into());
            }
            Err(e) => {
                error!("{e}");
                let file = LifecycleFile::with_hook(&file_name, "ts", hook);
                hls::download(&url, &client, file, segment.clone())
                    .await
                    .change_context(AppError::Unknown)?;
            }
        }
        Ok(DownloadStatus::StreamEnded)
    }
}

impl StreamGears {
    pub(crate) async fn download<'a>(
        &self,
        callback: Box<dyn FnMut(SegmentEvent) + Send + Sync + 'a>,
        download_config: DownloadConfig,
    ) -> AppResult<DownloadStatus> {
        *self.token.write().unwrap() = CancellationToken::new();
        let token = self.token.read().unwrap().clone();
        tokio::select! {
            _ = token.cancelled() => {
                bail!(AppError::Custom("StreamGears token cancelled".into()))
            }
            res = self.start_download(callback, download_config) => {res}
        }
    }

    pub(crate) async fn stop(&self) -> AppResult<()> {
        self.token.read().unwrap().cancel();
        Ok(())
    }
}
