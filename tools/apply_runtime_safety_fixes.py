from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def write(path: str, content: str) -> None:
    target = ROOT / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(content, encoding="utf-8")


def replace_once(path: str, old: str, new: str) -> None:
    text = read(path)
    if old not in text:
        raise RuntimeError(f"missing patch anchor in {path}: {old[:160]!r}")
    write(path, text.replace(old, new, 1))


def replace_regex(path: str, pattern: str, replacement: str) -> None:
    text = read(path)
    updated, count = re.subn(pattern, replacement, text, count=1, flags=re.S)
    if count != 1:
        raise RuntimeError(f"regex patch count {count} in {path}: {pattern[:120]!r}")
    write(path, updated)


# 1. Live Replay segments must never be deleted merely because the safe tail is small.
replace_once(
    "crates/biliup-cli/src/server/common/download.rs",
    """    pub fn new(uploader: Sender<UploaderMessage>, ctx: Context) -> Self {
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
""",
    """    pub fn new(uploader: Sender<UploaderMessage>, ctx: Context) -> Self {
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
""",
)

# 2. Validate a recording with ffprobe before its first upload, preserve failures, and merge
# reconnects by streamer/time rather than mutable live title.
replace_once(
    "crates/biliup-cli/src/server/common/replay.rs",
    """    let runtime = initialize_upload_runtime(ctx, upload_config).await?;
    let mut current = load_segment(ctx.pool(), segment.id).await?;

    let remote_filename = if let Some(filename) = current.remote_filename.clone() {
""",
    """    let runtime = initialize_upload_runtime(ctx, upload_config).await?;
    let mut current = load_segment(ctx.pool(), segment.id).await?;

    if current.remote_filename.is_none() {
        validate_media_file(&current.file_path).await?;
    }

    let remote_filename = if let Some(filename) = current.remote_filename.clone() {
""",
)

replace_once(
    "crates/biliup-cli/src/server/common/replay.rs",
    """async fn append_video(
""",
    """fn parse_positive_duration(value: &str) -> Option<f64> {
    let duration = value.trim().parse::<f64>().ok()?;
    (duration.is_finite() && duration > 0.0).then_some(duration)
}

async fn validate_media_file(path: &Path) -> AppResult<()> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .kill_on_drop(true)
        .output()
        .await
        .change_context(AppError::Custom(
            "无法启动 ffprobe；录像已保留并等待重试".to_string(),
        ))?;
    if !output.status.success() {
        return Err(AppError::Custom(format!(
            "录像文件无法正常解析，已保留：{}",
            path.display()
        ))
        .into());
    }
    let duration = String::from_utf8_lossy(&output.stdout);
    if parse_positive_duration(&duration).is_none() {
        return Err(AppError::Custom(format!(
            "录像时长无效，已保留：{}",
            path.display()
        ))
        .into());
    }
    Ok(())
}

async fn append_video(
""",
)

replace_once(
    "crates/biliup-cli/src/server/common/replay.rs",
    """         WHERE live_streamer_id = ? AND live_title = ? \\
           AND (ended_at IS NULL OR ended_at >= datetime('now', ?)) \\
         ORDER BY id DESC LIMIT 1",
    )
    .bind(ctx.worker_id())
    .bind(&ctx.streamer_info().title)
    .bind(&recent_modifier)
""",
    """         WHERE live_streamer_id = ? \\
           AND (ended_at IS NULL OR ended_at >= datetime('now', ?)) \\
         ORDER BY id DESC LIMIT 1",
    )
    .bind(ctx.worker_id())
    .bind(&recent_modifier)
""",
)

replace_regex(
    "crates/biliup-cli/src/server/common/replay.rs",
    r"async fn remote_part_playable\(bilibili: &BiliBili, aid: u64, cid: u64\) -> AppResult<bool> \{.*?\n\}\n\nasync fn wait_for_remote_ready",
    """async fn remote_part_playable(bilibili: &BiliBili, aid: u64, cid: u64) -> AppResult<bool> {
    let response = bilibili
        .client
        .get("https://api.bilibili.com/x/player/playurl")
        .query(&[
            ("avid", aid.to_string()),
            ("cid", cid.to_string()),
            ("qn", "16".to_string()),
            ("fnval", "16".to_string()),
        ])
        .send()
        .await
        .change_context(AppError::Unknown)?;
    if !response.status().is_success() {
        return Ok(false);
    }
    let value: serde_json::Value = response.json().await.change_context(AppError::Unknown)?;
    if value.get("code").and_then(|value| value.as_i64()) != Some(0) {
        return Ok(false);
    }
    let data = value.get("data").cloned().unwrap_or(serde_json::Value::Null);
    let media_url = data
        .get("durl")
        .and_then(|value| value.as_array())
        .and_then(|items| items.first())
        .and_then(|item| item.get("url"))
        .and_then(|value| value.as_str())
        .or_else(|| {
            data.pointer("/dash/video/0/baseUrl")
                .and_then(|value| value.as_str())
        })
        .or_else(|| {
            data.pointer("/dash/video/0/base_url")
                .and_then(|value| value.as_str())
        });
    let Some(media_url) = media_url else {
        return Ok(false);
    };
    let media_url = if media_url.starts_with("//") {
        format!("https:{media_url}")
    } else {
        media_url.to_string()
    };

    // 不仅确认播放接口给出地址，还实际读取远端媒体的首个数据块。
    // 只有 CDN 已经能返回有效媒体字节，才允许进入本地删除阶段。
    let request = bilibili
        .client
        .get(media_url)
        .header("Range", "bytes=0-1023")
        .header("Referer", "https://www.bilibili.com/");
    let mut response = match tokio::time::timeout(Duration::from_secs(20), request.send()).await {
        Ok(Ok(response)) if response.status().is_success() => response,
        _ => return Ok(false),
    };
    match tokio::time::timeout(Duration::from_secs(20), response.chunk()).await {
        Ok(Ok(Some(chunk))) => Ok(!chunk.is_empty()),
        _ => Ok(false),
    }
}

async fn wait_for_remote_ready""",
)

# Add a small pure regression test for duration validation.
replay_text = read("crates/biliup-cli/src/server/common/replay.rs")
if "runtime_safety_duration_tests" not in replay_text:
    replay_text += """

#[cfg(test)]
mod runtime_safety_duration_tests {
    use super::parse_positive_duration;

    #[test]
    fn only_positive_finite_durations_are_uploadable() {
        assert_eq!(parse_positive_duration("1.25"), Some(1.25));
        assert_eq!(parse_positive_duration("0"), None);
        assert_eq!(parse_positive_duration("-1"), None);
        assert_eq!(parse_positive_duration("NaN"), None);
        assert_eq!(parse_positive_duration("bad"), None);
    }
}
"""
    write("crates/biliup-cli/src/server/common/replay.rs", replay_text)

# 3. Replace the global eight-chunk heuristic with per-download pressure tracking and gradual recovery.
replace_once(
    "crates/biliup/src/uploader/line.rs",
    """use std::ffi::OsStr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
""",
    """use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex as StdMutex, OnceLock};
""",
)

replace_regex(
    "crates/biliup/src/uploader/line.rs",
    r"static ACTIVE_RECORDINGS: AtomicUsize = AtomicUsize::new\(0\);.*?fn download_under_pressure\(\) -> bool \{.*?\n\}",
    """static ACTIVE_RECORDINGS: AtomicUsize = AtomicUsize::new(0);
static NEXT_DOWNLOAD_STREAM_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, Default)]
struct DownloadHealth {
    last_pressure_ms: u64,
}

fn download_health() -> &'static StdMutex<HashMap<u64, DownloadHealth>> {
    static HEALTH: OnceLock<StdMutex<HashMap<u64, DownloadHealth>>> = OnceLock::new();
    HEALTH.get_or_init(|| StdMutex::new(HashMap::new()))
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 每一路直播拥有独立压力状态；其他正常直播不能清除这一路的降速状态。
pub struct DownloadPressureGuard {
    id: u64,
}

impl DownloadPressureGuard {
    pub fn new() -> Self {
        let id = NEXT_DOWNLOAD_STREAM_ID.fetch_add(1, Ordering::Relaxed);
        download_health()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, DownloadHealth::default());
        Self { id }
    }

    pub fn report_progress(&self, bytes: usize) {
        if bytes == 0 {
            self.report_pressure();
        }
    }

    pub fn report_pressure(&self) {
        if let Some(health) = download_health()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&self.id)
        {
            health.last_pressure_ms = unix_millis();
        }
    }
}

impl Default for DownloadPressureGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DownloadPressureGuard {
    fn drop(&mut self) {
        download_health()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.id);
    }
}

fn pressure_limit_for_age_ms(age_ms: u64) -> Option<f64> {
    match age_ms {
        0..=14_999 => Some(5.0),
        15_000..=29_999 => Some(10.0),
        30_000..=59_999 => Some(25.0),
        60_000..=119_999 => Some(50.0),
        _ => None,
    }
}

fn current_pressure_limit_mbps() -> Option<f64> {
    let latest_pressure = download_health()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .map(|health| health.last_pressure_ms)
        .filter(|value| *value > 0)
        .max()?;
    pressure_limit_for_age_ms(unix_millis().saturating_sub(latest_pressure))
}""",
)

replace_once(
    "crates/biliup/src/uploader/line.rs",
    """fn configured_rate_bytes_per_second() -> Option<u64> {
    let (key, default_mbps) = if is_recording_active() && download_under_pressure() {
        ("LIVE_REPLAY_PRESSURE_UPLOAD_LIMIT_MBPS", 5.0)
    } else if is_recording_active() {
        ("LIVE_REPLAY_RECORDING_UPLOAD_LIMIT_MBPS", 100.0)
    } else {
        ("LIVE_REPLAY_UPLOAD_LIMIT_MBPS", 0.0)
    };
""",
    """fn configured_rate_bytes_per_second() -> Option<u64> {
    let (key, default_mbps) = if is_recording_active() {
        if let Some(pressure_limit) = current_pressure_limit_mbps() {
            ("LIVE_REPLAY_PRESSURE_UPLOAD_LIMIT_MBPS", pressure_limit)
        } else {
            ("LIVE_REPLAY_RECORDING_UPLOAD_LIMIT_MBPS", 100.0)
        }
    } else {
        ("LIVE_REPLAY_UPLOAD_LIMIT_MBPS", 0.0)
    };
""",
)

line_text = read("crates/biliup/src/uploader/line.rs")
if "pressure_recovery_is_gradual" not in line_text:
    line_text += """

#[cfg(test)]
mod pressure_policy_tests {
    use super::pressure_limit_for_age_ms;

    #[test]
    fn pressure_recovery_is_gradual() {
        assert_eq!(pressure_limit_for_age_ms(0), Some(5.0));
        assert_eq!(pressure_limit_for_age_ms(15_000), Some(10.0));
        assert_eq!(pressure_limit_for_age_ms(30_000), Some(25.0));
        assert_eq!(pressure_limit_for_age_ms(60_000), Some(50.0));
        assert_eq!(pressure_limit_for_age_ms(120_000), None);
    }
}
"""
    write("crates/biliup/src/uploader/line.rs", line_text)

# HTTP-FLV owns one pressure guard for its entire connection lifetime.
replace_once(
    "crates/biliup/src/downloader/httpflv.rs",
    """pub struct Connection {
    resp: Response,
    buffer: BytesMut,
}
""",
    """pub struct Connection {
    resp: Response,
    buffer: BytesMut,
    pressure: crate::uploader::line::DownloadPressureGuard,
}
""",
)
replace_once(
    "crates/biliup/src/downloader/httpflv.rs",
    """        Connection {
            resp,
            buffer: BytesMut::with_capacity(8 * 1024),
        }
""",
    """        Connection {
            resp,
            buffer: BytesMut::with_capacity(8 * 1024),
            pressure: crate::uploader::line::DownloadPressureGuard::new(),
        }
""",
)
replace_once(
    "crates/biliup/src/downloader/httpflv.rs",
    """            if chunk_result.is_err() {
                crate::uploader::line::report_download_pressure();
            }
""",
    """            if chunk_result.is_err() {
                self.pressure.report_pressure();
            }
""",
)
replace_once(
    "crates/biliup/src/downloader/httpflv.rs",
    """                    crate::uploader::line::report_download_progress(chunk.len());
""",
    """                    self.pressure.report_progress(chunk.len());
""",
)
replace_once(
    "crates/biliup/src/downloader/httpflv.rs",
    """                    crate::uploader::line::report_download_pressure();
""",
    """                    self.pressure.report_pressure();
""",
)

# HLS gets the same independent pressure tracking and timeout feedback.
replace_once(
    "crates/biliup/src/downloader/hls.rs",
    """use crate::downloader::util::{LifecycleFile, Segmentable};
""",
    """use crate::downloader::util::{LifecycleFile, Segmentable};
use crate::uploader::line::DownloadPressureGuard;
""",
)
replace_once(
    "crates/biliup/src/downloader/hls.rs",
    """use std::time::Duration;
""",
    """use std::time::Duration;
use tokio::time::timeout;
""",
)
replace_once(
    "crates/biliup/src/downloader/hls.rs",
    """    info!("Downloading {}...", url);
    let resp = client.retryable(url).await?;
""",
    """    info!("Downloading {}...", url);
    let pressure = DownloadPressureGuard::new();
    let resp = client.retryable(url).await.map_err(|error| {
        pressure.report_pressure();
        error
    })?;
""",
)
replace_once(
    "crates/biliup/src/downloader/hls.rs",
    """    let bytes = resp.bytes().await?;
""",
    """    let bytes = resp.bytes().await.map_err(|error| {
        pressure.report_pressure();
        error
    })?;
    pressure.report_progress(bytes.len());
""",
)
replace_once(
    "crates/biliup/src/downloader/hls.rs",
    """            let bs = resp.bytes().await?;
            // println!("{:?}", bs);
""",
    """            let bs = resp.bytes().await.map_err(|error| {
                pressure.report_pressure();
                error
            })?;
            pressure.report_progress(bs.len());
            // println!("{:?}", bs);
""",
)
replace_once(
    "crates/biliup/src/downloader/hls.rs",
    """                    &mut ts_file.buf_writer,
                )
""",
    """                    &mut ts_file.buf_writer,
                    &pressure,
                )
""",
)
replace_once(
    "crates/biliup/src/downloader/hls.rs",
    """        let resp = client.retryable(media_url.as_str()).await?;
        let bs = resp.bytes().await?;
""",
    """        let resp = client.retryable(media_url.as_str()).await.map_err(|error| {
            pressure.report_pressure();
            error
        })?;
        let bs = resp.bytes().await.map_err(|error| {
            pressure.report_pressure();
            error
        })?;
        pressure.report_progress(bs.len());
""",
)
replace_regex(
    "crates/biliup/src/downloader/hls.rs",
    r"async fn download_to_file\(url: Url, client: &StatelessClient, out: &mut impl Write\) -> Result<u64> \{.*?\n\}",
    """async fn download_to_file(
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
}""",
)

# 4. Persistently and forcibly disable all danmaku settings, including old databases and overrides.
replace_once(
    "crates/biliup-cli/src/server/config.rs",
    """    pub fn normalize_segment_limits(&mut self) {
        if self
            .segment_time
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            self.segment_time = None;
        }
    }
""",
    """    pub fn normalize_segment_limits(&mut self) {
        if self
            .segment_time
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            self.segment_time = None;
        }
        // Live Replay 当前完全不录制弹幕。读取旧数据库、导入配置和保存设置时
        // 都会归一化为关闭，避免旧值在升级后重新生效。
        self.douyu_danmaku = Some(false);
        self.huya_danmaku = Some(false);
        self.douyin_danmaku = Some(false);
        self.bilibili_danmaku = Some(false);
        self.bilibili_danmaku_detail = Some(false);
        self.bilibili_danmaku_raw = Some(false);
        self.youtube_danmaku = Some(false);
        self.ytb_danmaku = Some(false);
        self.twitch_danmaku = Some(false);
        self.twitcasting_danmaku = Some(false);
    }
""",
)

# Worker overrides are applied after global normalization, so normalize once more afterwards.
replace_once(
    "crates/biliup-cli/src/server/infrastructure/context.rs",
    """        if let Some(cfg_p) = self.live_streamer.override_cfg.clone() {
            cfg.apply(cfg_p)
        }
        cfg
""",
    """        if let Some(cfg_p) = self.live_streamer.override_cfg.clone() {
            cfg.apply(cfg_p)
        }
        cfg.normalize_segment_limits();
        cfg
""",
)

# Runtime options also hard-code danmaku off as a final guard.
live_path = "crates/biliup-cli/src/server/core/live.rs"
live_text = read(live_path)
live_text = live_text.replace(
    """            danmaku: config.bilibili_danmaku.unwrap_or(false),
            danmaku_raw: config.bilibili_danmaku_raw.unwrap_or(false),
            danmaku_detail: config.bilibili_danmaku_detail.unwrap_or(false),
""",
    """            danmaku: false,
            danmaku_raw: false,
            danmaku_detail: false,
""",
)
live_text = live_text.replace("danmaku: config.douyin_danmaku.unwrap_or(false),", "danmaku: false,")
live_text = live_text.replace("danmaku: config.douyu_danmaku.unwrap_or(false),", "danmaku: false,")
live_text = live_text.replace("danmaku: config.huya_danmaku.unwrap_or(false),", "danmaku: false,")
live_text = live_text.replace("danmaku: config.twitcasting_danmaku.unwrap_or(false),", "danmaku: false,")
live_text = live_text.replace("danmaku: config.twitch_danmaku.unwrap_or(false),", "danmaku: false,")
live_text = re.sub(
    r"danmaku: config\s*\.youtube_danmaku\s*\.or\(config\.ytb_danmaku\)\s*\.unwrap_or\(false\),",
    "danmaku: false,",
    live_text,
)
write(live_path, live_text)

# 5. Add disk status reporting; unknown disk capacity blocks new recordings instead of writing blindly.
replace_once(
    "crates/biliup-cli/src/server/infrastructure/context.rs",
    """use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, RwLock};
""",
    """use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
""",
)
replace_once(
    "crates/biliup-cli/src/server/infrastructure/context.rs",
    """#[derive(Debug, Clone)]
pub struct Context {
""",
    """#[derive(Debug, Clone, Serialize)]
pub struct RecordingDiskStatus {
    pub directory: String,
    pub free_bytes: Option<u64>,
    pub free_gb: Option<f64>,
    pub warning_gb: u64,
    pub stop_gb: u64,
    pub state: String,
    pub message: String,
}

pub fn default_recording_output_dir() -> PathBuf {
    if let Ok(value) = std::env::var("LIVE_REPLAY_OUTPUT_DIR")
        && !value.trim().is_empty()
    {
        return ensure_directory(PathBuf::from(value));
    }

    #[cfg(windows)]
    {
        let d_drive = Path::new(r"D:\\");
        if d_drive.exists() {
            return ensure_directory(PathBuf::from(r"D:\\LiveReplay\\Recordings"));
        }
    }

    ensure_directory(PathBuf::from("recordings"))
}

pub fn recording_disk_status() -> RecordingDiskStatus {
    let directory = default_recording_output_dir();
    let stop_gb = env_u64("LIVE_REPLAY_DISK_STOP_GB", DEFAULT_DISK_STOP_GB).max(1);
    let warning_gb = env_u64("LIVE_REPLAY_DISK_WARNING_GB", DEFAULT_DISK_WARNING_GB).max(stop_gb);
    let mut free_bytes = None;
    for attempt in 0..3 {
        free_bytes = free_space_bytes(&directory);
        if free_bytes.is_some() {
            break;
        }
        if attempt < 2 {
            thread::sleep(Duration::from_millis(200));
        }
    }
    let free_gb = free_bytes.map(|value| value as f64 / GIB as f64);
    let (state, message) = match free_bytes {
        None => (
            "unknown",
            format!(
                "无法确认录像目录 {} 的剩余空间；已暂停开始新录制",
                directory.display()
            ),
        ),
        Some(value) if value < stop_gb.saturating_mul(GIB) => (
            "blocked",
            format!(
                "录像目录仅剩 {:.1} GB，低于停止阈值 {} GB",
                free_gb.unwrap_or_default(),
                stop_gb
            ),
        ),
        Some(value) if value < warning_gb.saturating_mul(GIB) => (
            "warning",
            format!(
                "录像目录仅剩 {:.1} GB，低于提醒阈值 {} GB",
                free_gb.unwrap_or_default(),
                warning_gb
            ),
        ),
        Some(_) => (
            "ok",
            format!("录像目录剩余 {:.1} GB", free_gb.unwrap_or_default()),
        ),
    };
    RecordingDiskStatus {
        directory: directory.display().to_string(),
        free_bytes,
        free_gb,
        warning_gb,
        stop_gb,
        state: state.to_string(),
        message,
    }
}

#[derive(Debug, Clone)]
pub struct Context {
""",
)
replace_regex(
    "crates/biliup-cli/src/server/infrastructure/context.rs",
    r"    pub fn recording_output_dir\(&self\) -> PathBuf \{.*?\n    \}\n\n    pub fn ensure_recording_space\(&self\) -> AppResult<\(\)> \{.*?\n    \}\n\n    pub fn download_config",
    """    pub fn recording_output_dir(&self) -> PathBuf {
        default_recording_output_dir()
    }

    pub fn ensure_recording_space(&self) -> AppResult<()> {
        let status = recording_disk_status();
        match status.state.as_str() {
            "ok" => Ok(()),
            "warning" => {
                warn!(
                    directory = status.directory,
                    free_gb = status.free_gb,
                    warning_gb = status.warning_gb,
                    "recording disk space is below warning threshold"
                );
                Ok(())
            }
            _ => Err(AppError::Custom(status.message).into()),
        }
    }

    pub fn download_config""",
)

# 6. Require an exact Delta Force category and expose disk status through the API.
replace_once(
    "crates/biliup-cli/src/server/api/endpoints.rs",
    """use crate::server::infrastructure::context::{Stage, WorkerStatus};
""",
    """use crate::server::infrastructure::context::{
    RecordingDiskStatus, Stage, WorkerStatus, recording_disk_status,
};
""",
)
replace_once(
    "crates/biliup-cli/src/server/api/endpoints.rs",
    """pub struct SimpleStreamerRequest {
    pub url: String,
    pub remark: String,
    pub user_cookie: String,
    pub tid: Option<u16>,
}
""",
    """pub struct SimpleStreamerRequest {
    pub url: String,
    pub remark: String,
    pub user_cookie: String,
    pub tid: u16,
}
""",
)
replace_once(
    "crates/biliup-cli/src/server/api/endpoints.rs",
    """            tid: Some(payload.tid.unwrap_or(65)),
""",
    """            tid: Some(payload.tid),
""",
)
replace_once(
    "crates/biliup-cli/src/server/api/endpoints.rs",
    """pub async fn get_streamers_endpoint(
""",
    """pub async fn get_disk_status_endpoint() -> Json<RecordingDiskStatus> {
    Json(recording_disk_status())
}

pub async fn get_streamers_endpoint(
""",
)
replace_once(
    "crates/biliup-cli/src/server/router.rs",
    """    delete_template_endpoint, delete_user_endpoint, get_configuration, get_qrcode, get_status,
""",
    """    delete_template_endpoint, delete_user_endpoint, get_configuration, get_disk_status_endpoint,
    get_qrcode, get_status,
""",
)
replace_once(
    "crates/biliup-cli/src/server/router.rs",
    """        .route("/v1/streamers/simple", post(post_simple_streamer_endpoint))
""",
    """        .route("/v1/streamers/simple", post(post_simple_streamer_endpoint))
        .route("/v1/disk-status", get(get_disk_status_endpoint))
""",
)

# 7. Replace the homepage with exact category validation, disk warnings and a visible finalizing state.
write(
    "app/(app)/page.tsx",
    """'use client'

import { useMemo, useState } from 'react'
import Link from 'next/link'
import useSWR from 'swr'
import { Button, Card, Col, Form, Layout, Modal, Notification, Row, Switch, Tag, Typography } from '@douyinfe/semi-ui'
import { IconPlusCircle, IconRefresh } from '@douyinfe/semi-icons'
import { useSWRConfig } from 'swr'
import useStreamers, { useBiliUsers, useTypeTree } from '../lib/use-streamers'
import { API_BASE, fetcher } from '../lib/api-streamer'

const statusText: Record<string, string> = {
  Working: '正在录制',
  Pending: '检测直播状态',
  Idle: '等待开播',
  Pause: '已关闭',
  Finalizing: '正在收尾并封段',
}

const statusColor: Record<string, 'red' | 'blue' | 'green' | 'grey' | 'orange'> = {
  Working: 'red',
  Pending: 'blue',
  Idle: 'green',
  Pause: 'grey',
  Finalizing: 'orange',
}

interface DiskStatus {
  directory: string
  free_bytes?: number
  free_gb?: number
  warning_gb: number
  stop_gb: number
  state: 'ok' | 'warning' | 'blocked' | 'unknown'
  message: string
}

export default function Home() {
  const { Header, Content } = Layout
  const { Title, Text } = Typography
  const { streamers, isLoading } = useStreamers()
  const { biliUsers } = useBiliUsers()
  const { typeTree } = useTypeTree()
  const { data: diskStatus, mutate: refreshDisk } = useSWR<DiskStatus>('/v1/disk-status', fetcher, { refreshInterval: 10000 })
  const { mutate } = useSWRConfig()
  const [visible, setVisible] = useState(false)
  const [saving, setSaving] = useState(false)
  const [formApi, setFormApi] = useState<any>()
  const [finalizing, setFinalizing] = useState<Set<number>>(new Set())

  const deltaForceTid = useMemo(() => {
    const children = (typeTree ?? []).flatMap((item: any) => item.children ?? [])
    return children.find((item: any) => item.name?.trim() === '三角洲行动')?.id as number | undefined
  }, [typeTree])

  const accountOptions = (biliUsers ?? []).map(item => ({ label: item.name, value: item.value }))

  const createStreamer = async () => {
    if (deltaForceTid === undefined) {
      Notification.error({ title: '无法添加', content: '未能从B站获取“三角洲行动”分区，请刷新后重试。不会自动改投其他分区。' })
      return
    }
    const values = await formApi?.validate()
    if (!values) return
    setSaving(true)
    try {
      const response = await fetch(`${API_BASE}/v1/streamers/simple`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ...values, tid: deltaForceTid }),
      })
      if (!response.ok) throw new Error(await response.text())
      Notification.success({ title: '添加成功', content: '已持续关注；开播后会自动录制、上传并在可播放后删除本地视频。' })
      setVisible(false)
      formApi?.reset()
      await mutate('/v1/streamers')
    } catch (error: any) {
      Notification.error({ title: '添加失败', content: error.message })
    } finally {
      setSaving(false)
    }
  }

  const toggleStreamer = async (id: number) => {
    const streamer = streamers?.find(item => item.id === id)
    const isDisabling = streamer?.enabled !== false
    if (isDisabling) {
      setFinalizing(previous => new Set(previous).add(id))
    }
    try {
      const response = await fetch(`${API_BASE}/v1/streamers/${id}/pause`, { method: 'PUT' })
      if (!response.ok) {
        Notification.error({ title: '切换失败', content: await response.text() })
        return
      }
      await mutate('/v1/streamers')
    } finally {
      setFinalizing(previous => {
        const next = new Set(previous)
        next.delete(id)
        return next
      })
    }
  }

  const diskAttention = diskStatus && diskStatus.state !== 'ok'

  return (
    <>
      <Header style={{ backgroundColor: 'var(--semi-color-bg-1)', padding: '0 24px' }}>
        <div style={{ height: 64, display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
          <div>
            <Title heading={4}>Live Replay</Title>
            <Text type="tertiary">一个开关完成持续关注、自动录制和自动上传</Text>
          </div>
          <div style={{ display: 'flex', gap: 8 }}>
            <Button icon={<IconRefresh />} onClick={() => Promise.all([mutate('/v1/streamers'), refreshDisk()])}>刷新</Button>
            <Button theme="solid" icon={<IconPlusCircle />} onClick={() => setVisible(true)}>添加主播</Button>
          </div>
        </div>
      </Header>
      <Content style={{ padding: 24, backgroundColor: 'var(--semi-color-bg-0)' }}>
        {diskAttention && (
          <Card style={{ marginBottom: 16, borderColor: diskStatus.state === 'warning' ? 'var(--semi-color-warning)' : 'var(--semi-color-danger)' }}>
            <Text type={diskStatus.state === 'warning' ? 'warning' : 'danger'} strong>{diskStatus.message}</Text><br />
            <Text type="tertiary">录像目录：{diskStatus.directory}</Text>
          </Card>
        )}
        {!isLoading && (streamers?.length ?? 0) === 0 && (
          <Card style={{ maxWidth: 720, margin: '48px auto', textAlign: 'center' }}>
            <Title heading={4}>还没有关注主播</Title>
            <Text type="tertiary">粘贴直播间链接后，软件会一直等待开播并自动完成后续流程。</Text>
            <div style={{ marginTop: 20 }}><Button theme="solid" onClick={() => setVisible(true)}>添加第一个主播</Button></div>
          </Card>
        )}
        <Row gutter={[16, 16]}>
          {(streamers ?? []).map(streamer => {
            const isFinalizing = finalizing.has(streamer.id)
            const status = isFinalizing ? 'Finalizing' : (streamer.enabled === false ? 'Pause' : (streamer.status || 'Idle'))
            return (
              <Col key={streamer.id} xs={24} sm={24} md={12} lg={8} xl={6}>
                <Card shadows="hover" title={streamer.remark} headerExtraContent={
                  <Switch checked={streamer.enabled !== false} disabled={isFinalizing} onChange={() => toggleStreamer(streamer.id)} />
                }>
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
                    <div><Tag color={statusColor[status] ?? 'grey'}>{statusText[status] ?? status}</Tag></div>
                    <Text ellipsis={{ showTooltip: true }} type="tertiary">{streamer.url}</Text>
                    <Text>自动行为：录制 → 上传 → B站可播放 → 删除本地视频</Text>
                    <Text>投稿默认：三角洲行动 · 仅自己可见 · 转载</Text>
                    <div style={{ display: 'flex', gap: 8, marginTop: 6 }}>
                      <Link href="/replay"><Button size="small">查看上传队列</Button></Link>
                      <Link href="/streamers"><Button size="small" theme="borderless">详细设置</Button></Link>
                    </div>
                  </div>
                </Card>
              </Col>
            )
          })}
        </Row>
      </Content>

      <Modal
        title="添加主播"
        visible={visible}
        confirmLoading={saving}
        onOk={createStreamer}
        onCancel={() => setVisible(false)}
        okText="开始持续关注"
      >
        <Form getFormApi={setFormApi} initValues={{ user_cookie: accountOptions[0]?.value }}>
          <Form.Input field="url" label="直播间链接" placeholder="粘贴抖音、B站或斗鱼直播间链接" rules={[{ required: true, message: '请填写直播间链接' }]} />
          <Form.Input field="remark" label="主播名称" placeholder="例如：小天才" rules={[{ required: true, message: '请填写主播名称' }]} />
          <Form.Select field="user_cookie" label="投稿账号" optionList={accountOptions} rules={[{ required: true, message: '请先登录B站账号' }]} style={{ width: '100%' }} />
          <Card style={{ marginTop: 12 }}>
            <Text>默认标题：主播名 直播回放 日期 时间</Text><br />
            <Text>分区/标签：三角洲行动 / 游戏</Text><br />
            <Text>可见范围：仅自己可见　类型：转载</Text><br />
            <Text>简介：空　分段：60分钟　弹幕：不录制</Text><br />
            <Text>磁盘：低于30GB提醒，低于10GB停止新录制</Text>
          </Card>
          {accountOptions.length === 0 && <Typography.Text type="danger">当前没有可用B站账号，请先扫码登录。</Typography.Text>}
          {deltaForceTid === undefined && <Typography.Text type="danger">未找到B站“三角洲行动”分区，当前不能添加主播。</Typography.Text>}
        </Form>
      </Modal>
    </>
  )
}
""",
)

# Document the tightened behavior.
doc_path = "LIVE_REPLAY.md"
doc = read(doc_path)
marker = "## 运行安全补强（2026-08-07）"
if marker not in doc:
    doc = doc.rstrip() + """

## 运行安全补强（2026-08-07）

- 主开关关闭产生的短尾段不再按20MB阈值删除；上传前使用 ffprobe 验证，异常文件保留并显示错误。
- 同一主播10分钟内恢复始终续接原场次，不再依赖直播标题相同。
- 每一路 HTTP-FLV/HLS 下载独立记录压力；任一路异常时上传按 5/10/25/50 Mbps 逐级恢复。
- “三角洲行动”分区必须精确匹配，找不到时禁止添加，不静默回退。
- 旧配置与主播覆盖中的弹幕选项都会强制关闭。
- 首页显示30GB空间提醒；低于10GB或无法确认空间时暂停新录制。
- B站播放接口返回后还会读取远端媒体首个数据块，成功后才删除本地录像。
""" + "\n"
    write(doc_path, doc)

print("runtime safety fixes applied")
