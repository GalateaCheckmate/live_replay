use crate::error::Result;
use crate::uploader::{Uploader, VideoFile, VideoStream};
use futures::{Stream, StreamExt, TryStreamExt};
use reqwest::{Body, RequestBuilder};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::ffi::OsStr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::info;

use crate::client::StatelessClient;
use crate::error::Kind::{Custom, RateLimit};
use crate::uploader::bilibili::{BiliBili, Video};
use crate::uploader::line::upos::Upos;

pub mod upos;

static ACTIVE_RECORDINGS: AtomicUsize = AtomicUsize::new(0);

pub fn recording_started() {
    ACTIVE_RECORDINGS.fetch_add(1, Ordering::Relaxed);
}

pub fn recording_finished() {
    let _ = ACTIVE_RECORDINGS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_sub(1))
    });
}

pub fn is_recording_active() -> bool {
    ACTIVE_RECORDINGS.load(Ordering::Relaxed) > 0
}

fn configured_rate_bytes_per_second() -> Option<u64> {
    let (key, default_mbps) = if is_recording_active() {
        ("LIVE_REPLAY_RECORDING_UPLOAD_LIMIT_MBPS", 20.0)
    } else {
        ("LIVE_REPLAY_UPLOAD_LIMIT_MBPS", 0.0)
    };
    let mbps = std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .unwrap_or(default_mbps);
    if !mbps.is_finite() || mbps <= 0.0 {
        None
    } else {
        Some((mbps * 1_000_000.0 / 8.0).max(1.0) as u64)
    }
}

fn global_schedule() -> &'static Mutex<Instant> {
    static SCHEDULE: OnceLock<Mutex<Instant>> = OnceLock::new();
    SCHEDULE.get_or_init(|| Mutex::new(Instant::now()))
}

/// 所有上传文件和所有并发分片共享一个时间表，因此 20 Mbps 是进程总限速，
/// 不会因三个并发分片膨胀成 60 Mbps。
fn throttle_stream<S, B>(stream: S) -> impl Stream<Item = Result<(B, usize)>>
where
    S: Stream<Item = Result<(B, usize)>>,
    B: Into<Body> + Clone,
{
    stream.then(|item| async move {
        let (body, len) = item?;
        if let Some(rate) = configured_rate_bytes_per_second() {
            let spacing = Duration::from_secs_f64(len as f64 / rate as f64);
            let wait = {
                let mut next = global_schedule().lock().await;
                let now = Instant::now();
                if *next < now {
                    *next = now;
                }
                let wait = (*next).saturating_duration_since(now);
                *next += spacing;
                wait
            };
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
        } else {
            // 解除限速后清空旧时间债务，避免空闲时仍等待先前录制限速的排期。
            *global_schedule().lock().await = Instant::now();
        }
        Ok((body, len))
    })
}

pub struct Parcel {
    line: Bucket,
    video_file: VideoFile,
}

impl Parcel {
    pub async fn upload<F, S, B>(
        self,
        client: StatelessClient,
        limit: usize,
        progress: F,
    ) -> Result<Video>
    where
        F: FnOnce(VideoStream) -> S,
        S: Stream<Item = Result<(B, usize)>>,
        B: Into<Body> + Clone,
    {
        let mut video = match self.line {
            Bucket::Upos(bucket) => {
                let chunk_size = bucket.chunk_size;
                let upos = Upos::from(client, bucket).await?;
                let mut parts = Vec::new();
                let source = progress(self.video_file.get_stream(chunk_size)?);
                let source = throttle_stream(source);
                let stream = upos
                    .upload_stream(source, self.video_file.total_size, limit)
                    .await?;
                tokio::pin!(stream);
                while let Some((part, _size)) = stream.try_next().await? {
                    parts.push(part);
                }
                upos.get_ret_video_info(&parts, &self.video_file.filepath)
                    .await?
            }
        };

        if video.title.is_none()
            && let Some(filename) = self.video_file.filepath.file_stem().and_then(OsStr::to_str)
        {
            video.title = Some(if filename.chars().count() >= 80 {
                Video::truncate_title(filename, 80)
            } else {
                filename.to_string()
            });
        }
        Ok(video)
    }
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Probe {
    #[serde(rename = "OK")]
    ok: u8,
    lines: Vec<Line>,
    probe: serde_json::Value,
}

impl Probe {
    pub async fn probe(client: &reqwest::Client) -> Result<Line> {
        let res: Self = client
            .get("https://member.bilibili.com/preupload?r=probe")
            .send()
            .await?
            .json()
            .await?;
        let mut choice_line: Line = Default::default();
        for mut line in res.lines {
            let instant = Instant::now();
            if Probe::ping(&res.probe, &format!("https:{}", line.probe_url), client)
                .send()
                .await?
                .status()
                .is_success()
            {
                line.cost = instant.elapsed().as_millis();
                info!("{}: {}", line.query, line.cost);
                if choice_line.cost > line.cost {
                    choice_line = line
                }
            };
        }
        Ok(choice_line)
    }

    fn ping(probe: &serde_json::Value, url: &str, client: &reqwest::Client) -> RequestBuilder {
        if !probe["get"].is_null() {
            client.get(url)
        } else {
            client
                .post(url)
                .body(vec![0; (1024. * 1024. * 10.) as usize])
        }
    }
}

enum Bucket {
    Upos(upos::Bucket),
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Line {
    os: Uploader,
    probe_url: String,
    query: String,
    #[serde(skip)]
    cost: u128,
}

impl Line {
    pub async fn pre_upload(&self, bili: &BiliBili, video_file: VideoFile) -> Result<Parcel> {
        let total_size = video_file.total_size;
        let file_name = video_file.file_name.clone();
        let profile = "ugcupos/bup";
        let params = json!({
            "name": file_name,
            "r": self.os,
            "profile": profile,
            "ssl": 0,
            "version": "2.14.0",
            "build": 2140000,
            "size": total_size,
        });
        info!("pre_upload: {}", params);

        let response = bili
            .client
            .get(format!(
                "https://member.bilibili.com/preupload?{}",
                self.query
            ))
            .query(&params)
            .send()
            .await?;

        if !response.status().is_success() {
            let response_text = response.text().await?;
            if let Ok(error_json) = serde_json::from_str::<serde_json::Value>(&response_text)
                && let Some(code) = error_json.get("code").and_then(|c| c.as_i64())
                && code == 601
            {
                let message = error_json
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("上传过快")
                    .to_string();
                return Err(RateLimit { code, message });
            }
            return Err(Custom(format!(
                "Failed to pre_upload from {}",
                response_text
            )));
        }

        match self.os {
            Uploader::Upos => Ok(Parcel {
                line: Bucket::Upos(response.json().await?),
                video_file,
            }),
        }
    }
}

impl Default for Line {
    fn default() -> Self {
        Line {
            cost: u128::MAX,
            ..bldsa()
        }
    }
}

pub fn bldsa() -> Line {
    Line {
        os: Uploader::Upos,
        query: "zone=cs&upcdn=bldsa&probe_version=20221109".into(),
        probe_url: "//upos-cs-upcdnbldsa.bilivideo.com/OK".into(),
        cost: 0,
    }
}

pub fn cnbldsa() -> Line {
    Line {
        os: Uploader::Upos,
        query: "zone=cs&upcdn=cnbldsa&probe_version=20221109".into(),
        probe_url: "//upos-cs-upcdnbldsa.bilivideo.cn/OK".into(),
        cost: 0,
    }
}

pub fn andsa() -> Line {
    Line {
        os: Uploader::Upos,
        query: "zone=cs&upcdn=andsa&probe_version=20221109".into(),
        probe_url: "//c3350892csdsa.anitama.cn/OK".into(),
        cost: 0,
    }
}

pub fn atdsa() -> Line {
    Line {
        os: Uploader::Upos,
        query: "zone=cs&upcdn=atdsa&probe_version=20221109".into(),
        probe_url: "//c3350892csdsa.anitama.net/OK".into(),
        cost: 0,
    }
}

pub fn bda2() -> Line {
    Line {
        os: Uploader::Upos,
        query: "probe_version=20221109&upcdn=bda2&zone=cs".into(),
        probe_url: "//upos-cs-upcdnbda2.bilivideo.com/OK".into(),
        cost: 0,
    }
}

pub fn cnbd() -> Line {
    Line {
        os: Uploader::Upos,
        query: "probe_version=20221109&upcdn=cnbd&zone=cs".into(),
        probe_url: "//upos-cs-upcdnbd.bilivideo.cn/OK".into(),
        cost: 0,
    }
}

pub fn anbd() -> Line {
    Line {
        os: Uploader::Upos,
        query: "probe_version=20221109&upcdn=anbd&zone=cs".into(),
        probe_url: "//c3350892csbd.anitama.cn/OK".into(),
        cost: 0,
    }
}

pub fn atbd() -> Line {
    Line {
        os: Uploader::Upos,
        query: "probe_version=20221109&upcdn=atbd&zone=cs".into(),
        probe_url: "//c3350892csbd.anitama.net/OK".into(),
        cost: 0,
    }
}

pub fn tx() -> Line {
    Line {
        os: Uploader::Upos,
        query: "zone=cs&upcdn=tx&probe_version=20221109".into(),
        probe_url: "//upos-cs-upcdntx.bilivideo.com/OK".into(),
        cost: 0,
    }
}

pub fn cntx() -> Line {
    Line {
        os: Uploader::Upos,
        query: "zone=cs&upcdn=cntx&probe_version=20221109".into(),
        probe_url: "//upos-cs-upcdntx.bilivideo.com/OK".into(),
        cost: 0,
    }
}

pub fn antx() -> Line {
    Line {
        os: Uploader::Upos,
        query: "zone=cs&upcdn=antx&probe_version=20221109".into(),
        probe_url: "//c3350892cstx.anitama.cn/OK".into(),
        cost: 0,
    }
}

pub fn attx() -> Line {
    Line {
        os: Uploader::Upos,
        query: "zone=cs&upcdn=attx&probe_version=20221109".into(),
        probe_url: "//c3350892cstx.anitama.net/OK".into(),
        cost: 0,
    }
}

pub fn bda() -> Line {
    Line {
        os: Uploader::Upos,
        query: "zone=cs&upcdn=bda&probe_version=20221109".into(),
        probe_url: "//upos-cs-upcdnbda.bilivideo.com/OK".into(),
        cost: 0,
    }
}

pub fn txa() -> Line {
    Line {
        os: Uploader::Upos,
        query: "zone=cs&upcdn=txa&probe_version=20221109".into(),
        probe_url: "//upos-cs-upcdntxa.bilivideo.com/OK".into(),
        cost: 0,
    }
}

pub fn alia() -> Line {
    Line {
        os: Uploader::Upos,
        query: "zone=cs&upcdn=alia&probe_version=20221109".into(),
        probe_url: "//upos-cs-upcdnalia.bilivideo.com/OK".into(),
        cost: 0,
    }
}
