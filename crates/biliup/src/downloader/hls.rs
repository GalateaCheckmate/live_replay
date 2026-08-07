use crate::downloader::error::{Error, Result};
use crate::downloader::util::{LifecycleFile, Segmentable};
use crate::uploader::line::DownloadPressureGuard;
use m3u8_rs::Playlist;

use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::Duration;
use tokio::time::{sleep, timeout};
use tracing::{debug, info, warn};
use url::Url;

use crate::client::StatelessClient;

pub async fn download(
    url: &str,
    client: &StatelessClient,
    file: LifecycleFile<'_>,
    mut splitting: Segmentable,
) -> Result<()> {
    info!("Downloading {}...", url);
    let pressure = DownloadPressureGuard::new();
    let resp = client.retryable(url).await.map_err(|error| {
        pressure.report_pressure();
        error
    })?;
    info!("{}", resp.status());
    let bytes = resp.bytes().await.map_err(|error| {
        pressure.report_pressure();
        error
    })?;
    pressure.report_progress(bytes.len());
    let mut ts_file = TsFile::new(file)?;

    let mut media_url = Url::parse(url)?;
    let mut pl = match m3u8_rs::parse_playlist(&bytes) {
        Ok((_i, Playlist::MasterPlaylist(pl))) => {
            info!("Master playlist:\n{:#?}", pl);
            let best = pl
                .variants
                .iter()
                .filter(|v| !v.is_i_frame && v.resolution.is_some())
                .max_by_key(|v| v.bandwidth)
                .or_else(|| {
                    pl.variants
                        .iter()
                        .filter(|v| !v.is_i_frame)
                        .max_by_key(|v| v.bandwidth)
                })
                .ok_or_else(|| Error::Custom("HLS master playlist 没有可用视频变体。".to_string()))?;
            info!(
                "Selected variant: bandwidth={}, resolution={:?}, video={:?}",
                best.bandwidth, best.resolution, best.video
            );
            media_url = media_url.join(&best.uri)?;
            info!("media url: {media_url}");
            let resp = client.retryable(media_url.as_str()).await?;
            let bs = resp.bytes().await.map_err(|error| {
                pressure.report_pressure();
                error
            })?;
            pressure.report_progress(bs.len());
            match m3u8_rs::parse_media_playlist(&bs) {
                Ok((_, pl)) => pl,
                Err(e) => {
                    return Err(Error::Custom(format!(
                        "Unable to parse media playlist content: {e}"
                    )));
                }
            }
        }
        Ok((_i, Playlist::MediaPlaylist(pl))) => {
            info!("Media playlist:\n{:#?}", pl);
            info!("index {}", pl.media_sequence);
            pl
        }
        Err(e) => return Err(Error::Custom(format!("Parsing playlist error: {e}"))),
    };
    let mut previous_last_segment = 0;
    loop {
        if pl.segments.is_empty() {
            if pl.end_list {
                info!("HLS playlist ended with no remaining segments");
                break;
            }
            sleep(Duration::from_secs(1)).await;
        }

        let mut seq = pl.media_sequence;
        for segment in &pl.segments {
            if seq > previous_last_segment {
                if (previous_last_segment > 0) && (seq > (previous_last_segment + 1)) {
                    warn!("SEGMENT INFO SKIPPED");
                }
                debug!("Yield segment");
                if segment.discontinuity {
                    warn!("#EXT-X-DISCONTINUITY");
                    ts_file.create_new()?;
                    splitting.reset();
                }
                let length = download_to_file(
                    media_url.join(&segment.uri)?,
                    client,
                    &mut ts_file.buf_writer,
                    &pressure,
                )
                .await?;
                splitting.increase_size(length);
                splitting.increase_time(Duration::from_secs(segment.duration as u64));
                if splitting.needed() {
                    ts_file.create_new()?;
                    splitting.reset();
                }
                previous_last_segment = seq;
            }
            seq += 1;
        }

        if pl.end_list {
            info!("HLS #EXT-X-ENDLIST reached");
            break;
        }

        // Poll at roughly half of the latest segment duration instead of hammering the playlist in
        // a tight loop. Clamp the interval so very short/long segment durations remain practical.
        let poll_ms = pl
            .segments
            .last()
            .map(|segment| ((segment.duration as f64 * 500.0).clamp(500.0, 5_000.0)) as u64)
            .unwrap_or(1_000);
        sleep(Duration::from_millis(poll_ms)).await;

        let resp = client
            .retryable(media_url.as_str())
            .await
            .map_err(|error| {
                pressure.report_pressure();
                error
            })?;
        let bs = resp.bytes().await.map_err(|error| {
            pressure.report_pressure();
            error
        })?;
        pressure.report_progress(bs.len());
        pl = m3u8_rs::parse_media_playlist(&bs)
            .map(|(_, playlist)| playlist)
            .map_err(|error| Error::Custom(format!("Unable to refresh HLS playlist: {error}")))?;
    }
    info!("Done...");
    Ok(())
}

async fn download_to_file(
    url: Url,
    client: &StatelessClient,
    out: &mut impl Write,
    pressure: &DownloadPressureGuard,
) -> Result<u64> {
    debug!("url: {url}");
    let mut response = client.retryable(url.as_str()).await.map_err(|error| {
        pressure.report_pressure();
        error
    })?;
    let mut length: u64 = 0;
    loop {
        match timeout(Duration::from_secs(30), response.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                pressure.report_progress(chunk.len());
                length += chunk.len() as u64;
                out.write_all(&chunk)?;
            }
            Ok(Ok(None)) => break,
            Ok(Err(error)) => {
                pressure.report_pressure();
                return Err(error.into());
            }
            Err(_) => {
                pressure.report_pressure();
                return Err(Error::Custom(format!("HLS segment read timed out: {url}")));
            }
        }
    }
    Ok(length)
}

pub struct TsFile<'a> {
    pub buf_writer: BufWriter<File>,
    pub file: LifecycleFile<'a>,
}

impl<'a> TsFile<'a> {
    pub fn new(mut file: LifecycleFile<'a>) -> std::io::Result<Self> {
        let path = file.create()?;
        Ok(Self {
            buf_writer: Self::create(path)?,
            file,
        })
    }

    /// Ensure a completed TS segment is durable before LifecycleFile fires its completion hook.
    pub fn create_new(&mut self) -> std::io::Result<()> {
        self.sync_current()?;
        self.file.rename();
        let path = self.file.create()?;
        self.buf_writer = Self::create(path)?;
        Ok(())
    }

    fn sync_current(&mut self) -> std::io::Result<()> {
        self.buf_writer.flush()?;
        self.buf_writer.get_ref().sync_all()
    }

    fn create<P: AsRef<std::path::Path>>(path: P) -> std::io::Result<BufWriter<File>> {
        let path = path.as_ref();
        let out = match File::create(path) {
            Ok(o) => o,
            Err(e) => {
                return Err(std::io::Error::new(
                    e.kind(),
                    format!("Unable to create file {}", path.display()),
                ));
            }
        };
        info!("create file {}", path.display());
        Ok(BufWriter::new(out))
    }
}

impl Drop for TsFile<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.sync_current() {
            warn!("failed to sync TS before final rename: {error}");
            return;
        }
        self.file.rename()
    }
}

#[cfg(test)]
mod tests {
    use reqwest::Url;

    #[test]
    fn test_url() -> Result<(), Box<dyn std::error::Error>> {
        let url = Url::parse("h://host.path/to/remote/resource.m3u8")?;
        let scheme = url.scheme();
        let new_url = url.join("http://path.host/remote/resource.ts")?;
        println!("{url}, {scheme}");
        println!("{new_url}, {scheme}");
        Ok(())
    }

    #[test]
    fn it_works() -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
}
