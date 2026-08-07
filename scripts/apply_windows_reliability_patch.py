from pathlib import Path


def replace_exact(path, old, new, expected=1):
    p = Path(path)
    text = p.read_text(encoding="utf-8")
    actual = text.count(old)
    if actual != expected:
        raise SystemExit(
            f"{path}: expected {expected} matches, found {actual}: {old[:120]!r}"
        )
    p.write_text(text.replace(old, new), encoding="utf-8")


def replace_between(path, start, end, replacement):
    p = Path(path)
    text = p.read_text(encoding="utf-8")
    if text.count(start) != 1:
        raise SystemExit(f"{path}: start marker count != 1")
    start_at = text.index(start)
    end_at = text.index(end, start_at + len(start))
    p.write_text(text[:start_at] + replacement + text[end_at:], encoding="utf-8")


# 1) FFmpeg: only promote completed .part files after a successful exit,
# and make internal/external modes stoppable through a graceful q command.
ffmpeg = "crates/biliup-cli/src/server/core/downloader/ffmpeg_downloader.rs"
replace_exact(
    ffmpeg,
    "use tokio::io::{AsyncBufReadExt, BufReader};",
    "use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};",
)
replace_exact(ffmpeg, ".stdin(Stdio::null())", ".stdin(Stdio::piped())", expected=2)
replace_exact(
    ffmpeg,
    '''        let status = spawn_log(child, &self.process_handle).await?;
        // 退出时，重命名文件
        let part_file = format!("{}.part", output_file.display());
        tokio::fs::rename(&part_file, &output_file)
            .await
            .change_context(AppError::Custom(String::from("退出时，重命名文件")))?;

        callback(SegmentEvent::Segment(SegmentInfo {
            prev_file_path: output_file,
            danmaku_file_path: None,
            segment_index: 0,
            next_file_path: None,
        }));

        match status.code() {
            Some(0) => Ok(DownloadStatus::SegmentCompleted),
            Some(255) => Ok(DownloadStatus::StreamEnded),
            err => Ok(DownloadStatus::Error(format!("FFmpeg error: {err:?}"))),
        }
''',
    '''        let status = spawn_log(child, &self.process_handle).await?;
        let part_file = format!("{}.part", output_file.display());

        match status.code() {
            Some(0) => {
                // Only a clean FFmpeg exit is allowed to turn a temporary file into a
                // publishable recording. Abnormal exits keep the .part file for inspection.
                tokio::fs::rename(&part_file, &output_file)
                    .await
                    .change_context(AppError::Custom(String::from("退出时，重命名文件")))?;
                callback(SegmentEvent::Segment(SegmentInfo {
                    prev_file_path: output_file,
                    danmaku_file_path: None,
                    segment_index: 0,
                    next_file_path: None,
                }));
                Ok(DownloadStatus::SegmentCompleted)
            }
            Some(255) => {
                info!(file = %part_file, "FFmpeg ended abnormally; preserving temporary recording");
                Ok(DownloadStatus::StreamEnded)
            }
            err => {
                info!(file = %part_file, exit_code = ?err, "FFmpeg failed; preserving temporary recording");
                Ok(DownloadStatus::Error(format!("FFmpeg error: {err:?}")))
            }
        }
''',
)
replace_exact(
    ffmpeg,
    '''            .arg(format!(
                "{}.{}.part",
                download_config.recorder.filename_template(),
                download_config.suffix
            ))
''',
    '''            .arg(
                download_config
                    .output_dir
                    .join(format!(
                        "{}.{}.part",
                        download_config.recorder.filename_template(),
                        download_config.suffix
                    ))
                    .display()
                    .to_string(),
            )
''',
)
replace_between(
    ffmpeg,
    '''        info!("FFmpeg cmd: {:?}", cmd);
        let mut child = cmd.spawn().change_context(AppError::Unknown)?;
''',
    '''    }
}

impl FfmpegDownloader {''',
    '''        info!("FFmpeg cmd: {:?}", cmd);
        let mut child = cmd.spawn().change_context(AppError::Unknown)?;
        let stdout = child.stdout.take().ok_or(AppError::Custom(
            "failed to capture stdout pipe".to_string(),
        ))?;
        let stderr = child.stderr.take().ok_or(AppError::Custom(
            "failed to capture stderr pipe".to_string(),
        ))?;

        // Install the child handle before reading the segment list so stop() can reach
        // a long-running internal FFmpeg process while recording is still active.
        {
            let mut handle = self.process_handle.write().await;
            *handle = Some(child);
        }

        let mut stderr_lines = BufReader::new(stderr).lines();
        let stderr_task = tokio::spawn(async move {
            while let Ok(Some(line)) = stderr_lines.next_line().await {
                info!("[ffmpeg] {line}");
            }
        });

        let mut stdout_lines = BufReader::new(stdout).lines();
        let mut segment_index = 0;
        while let Some(line) = stdout_lines
            .next_line()
            .await
            .change_context(AppError::Unknown)?
        {
            if line.is_empty() {
                continue;
            }
            let file_path = PathBuf::from(&line);
            sleep(Duration::from_secs(1)).await;
            let output_file = file_path.with_extension("");
            tokio::fs::rename(&file_path, &output_file)
                .await
                .change_context(AppError::Custom(String::from("退出时，重命名文件")))?;
            callback(SegmentEvent::Segment(SegmentInfo {
                prev_file_path: output_file,
                danmaku_file_path: None,
                segment_index,
                next_file_path: None,
            }));
            segment_index += 1;
        }

        let _ = stderr_task.await;
        let status = {
            let mut handle = self.process_handle.write().await;
            if let Some(mut child) = handle.take() {
                child.wait().await.change_context(AppError::Unknown)?
            } else {
                bail!(AppError::Custom("Process handle not found".to_string()));
            }
        };

        match status.code() {
            Some(0) => Ok(DownloadStatus::SegmentCompleted),
            Some(255) => Ok(DownloadStatus::StreamEnded),
            err => Ok(DownloadStatus::Error(format!("FFmpeg error: {err:?}"))),
        }
''',
)
replace_exact(
    ffmpeg,
    '''    pub(crate) async fn stop(&self) -> AppResult<()> {
        let mut handle = self.process_handle.write().await;
        if let Some(child) = &mut *handle {
            child.kill().await.change_context(AppError::Unknown)?;
            Ok(())
        } else {
            Err(AppError::Custom("Process handle not found".to_string()).into())
        }
    }
''',
    '''    pub(crate) async fn stop(&self) -> AppResult<()> {
        let mut handle = self.process_handle.write().await;
        if let Some(child) = &mut *handle {
            // Ask FFmpeg to finish the current container first. This gives MP4/FLV a
            // chance to flush indexes and trailers. Killing is retained as a fallback.
            if let Some(mut stdin) = child.stdin.take()
                && stdin.write_all(b"q\\n").await.is_ok()
            {
                let _ = stdin.flush().await;
                return Ok(());
            }
            child.kill().await.change_context(AppError::Unknown)?;
            Ok(())
        } else {
            Err(AppError::Custom("Process handle not found".to_string()).into())
        }
    }
''',
)

# 2) Live Replay upload-file concurrency: make pool2_size a real, hot-updatable
# global limit, separate from per-file upload threads.
replay = "crates/biliup-cli/src/server/common/replay.rs"
replace_exact(
    replay,
    "use std::sync::{Arc, OnceLock};",
    "use std::sync::atomic::{AtomicUsize, Ordering};\nuse std::sync::{Arc, OnceLock};",
)
replace_exact(
    replay,
    "use tokio::sync::{Mutex, Notify, Semaphore};",
    "use tokio::sync::{Mutex, Notify};",
)
replace_exact(
    replay,
    '''fn upload_slots() -> &'static Semaphore {
    static SLOTS: OnceLock<Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| Semaphore::new(1))
}
''',
    '''static ACTIVE_UPLOADS: AtomicUsize = AtomicUsize::new(0);
static UPLOAD_LIMIT: AtomicUsize = AtomicUsize::new(1);

fn upload_limit_notify() -> &'static Notify {
    static NOTIFY: OnceLock<Notify> = OnceLock::new();
    NOTIFY.get_or_init(Notify::new)
}

pub fn set_upload_limit(limit: u32) {
    UPLOAD_LIMIT.store(limit.max(1) as usize, Ordering::Release);
    upload_limit_notify().notify_waiters();
}

struct UploadSlotPermit;

impl Drop for UploadSlotPermit {
    fn drop(&mut self) {
        ACTIVE_UPLOADS.fetch_sub(1, Ordering::AcqRel);
        upload_limit_notify().notify_one();
    }
}

async fn acquire_upload_slot() -> UploadSlotPermit {
    loop {
        let limit = UPLOAD_LIMIT.load(Ordering::Acquire).max(1);
        let active = ACTIVE_UPLOADS.load(Ordering::Acquire);
        if active < limit
            && ACTIVE_UPLOADS
                .compare_exchange_weak(
                    active,
                    active.saturating_add(1),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        {
            return UploadSlotPermit;
        }

        let notified = upload_limit_notify().notified();
        // Re-check after registering the waiter to avoid losing a wake-up between
        // observing a full pool and awaiting the notification.
        if ACTIVE_UPLOADS.load(Ordering::Acquire)
            < UPLOAD_LIMIT.load(Ordering::Acquire).max(1)
        {
            continue;
        }
        notified.await;
    }
}
''',
)
replace_exact(
    replay,
    '''    let _permit = upload_slots()
        .acquire()
        .await
        .change_context(AppError::Custom("upload semaphore closed".to_string()))?;
''',
    '''    let _permit = acquire_upload_slot().await;
''',
)

# Credential snapshot paths are constrained to generated session keys.
replace_exact(
    replay,
    '''async fn freeze_upload_config(
    upload_config: &UploadStreamer,
    session_key: &str,
) -> AppResult<UploadStreamer> {
''',
    '''fn credential_snapshot_path(session_key: &str) -> Option<PathBuf> {
    if session_key.is_empty()
        || session_key.len() > 200
        || !session_key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return None;
    }
    Some(Path::new(CREDENTIAL_DIR).join(format!("{session_key}.json")))
}

async fn remove_credential_snapshot(session_key: &str) -> bool {
    let Some(target) = credential_snapshot_path(session_key) else {
        warn!(session_key, "refusing to remove invalid replay credential snapshot path");
        return false;
    };
    let temporary = target.with_extension("json.part");
    let mut removed = false;
    for path in [&target, &temporary] {
        match tokio::fs::remove_file(path).await {
            Ok(()) => removed = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => warn!(file = %path.display(), error = ?error, "failed to remove replay credential snapshot"),
        }
    }
    removed
}

async fn session_has_filesystem_outbox(session_id: i64) -> bool {
    let prefix = format!("{session_id}-");
    let mut entries = match tokio::fs::read_dir(OUTBOX_DIR).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(error) => {
            warn!(error = ?error, session_id, "cannot inspect replay outbox before credential cleanup");
            return true;
        }
    };
    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with(&prefix) {
                    return true;
                }
            }
            Ok(None) => return false,
            Err(error) => {
                warn!(error = ?error, session_id, "cannot enumerate replay outbox before credential cleanup");
                return true;
            }
        }
    }
}

async fn freeze_upload_config(
    upload_config: &UploadStreamer,
    session_key: &str,
) -> AppResult<UploadStreamer> {
''',
)
replace_exact(
    replay,
    '''        let target = Path::new(CREDENTIAL_DIR).join(format!("{session_key}.json"));
''',
    '''        let target = credential_snapshot_path(session_key).ok_or_else(|| {
            AppError::Custom("invalid Live Replay session key for credential snapshot".to_string())
        })?;
''',
)

# Keep snapshots through the reconnect window, then remove only completed sessions
# that have no pending DB segment and no filesystem outbox.
replace_exact(
    replay,
    '''async fn mark_session_complete(pool: &ConnectionPool, session_id: i64) -> AppResult<()> {
    sqlx::query(
        "UPDATE live_sessions SET status = 'complete', last_error = NULL, \
         ended_at = COALESCE(ended_at, CURRENT_TIMESTAMP), updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(session_id)
    .execute(pool)
    .await
    .change_context(AppError::Unknown)?;
    Ok(())
}
''',
    '''pub async fn cleanup_completed_credentials(pool: &ConnectionPool) -> AppResult<usize> {
    let reconnect_window = env_u64("LIVE_REPLAY_RECONNECT_WINDOW_SECONDS", 600);
    let modifier = format!("-{reconnect_window} seconds");
    let rows = sqlx::query(
        "SELECT s.id, s.session_key FROM live_sessions s \
         WHERE s.status = 'complete' AND s.ended_at IS NOT NULL AND s.session_key IS NOT NULL \
           AND datetime(s.ended_at) <= datetime('now', ?) \
           AND NOT EXISTS (SELECT 1 FROM recording_segments r WHERE r.session_id = s.id \
                           AND r.status NOT IN ('deleted', 'retained'))",
    )
    .bind(&modifier)
    .fetch_all(pool)
    .await
    .change_context(AppError::Unknown)?;

    let mut removed = 0usize;
    for row in rows {
        let session_id: i64 = row.try_get("id").change_context(AppError::Unknown)?;
        let session_key: String = row
            .try_get("session_key")
            .change_context(AppError::Unknown)?;
        if session_has_filesystem_outbox(session_id).await {
            continue;
        }
        if remove_credential_snapshot(&session_key).await {
            removed += 1;
        }
    }
    Ok(removed)
}

async fn mark_session_complete(pool: &ConnectionPool, session_id: i64) -> AppResult<()> {
    sqlx::query(
        "UPDATE live_sessions SET status = 'complete', last_error = NULL, \
         ended_at = COALESCE(ended_at, CURRENT_TIMESTAMP), updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(session_id)
    .execute(pool)
    .await
    .change_context(AppError::Unknown)?;

    // A completed session can still reconnect into the same logical live session for a
    // short window. Delay cleanup until that window has elapsed; startup recovery also
    // performs the same sweep so a restart cannot leave snapshots accumulating forever.
    let pool = pool.clone();
    let delay = env_u64("LIVE_REPLAY_RECONNECT_WINDOW_SECONDS", 600).saturating_add(5);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(delay)).await;
        match cleanup_completed_credentials(&pool).await {
            Ok(removed) if removed > 0 => {
                info!(removed, "cleaned completed replay credential snapshots")
            }
            Ok(_) => {}
            Err(error) => warn!(error = ?error, "delayed replay credential cleanup failed"),
        }
    });
    Ok(())
}
''',
)

# 3) Downloader enum mapping: every variant is explicit; context-dependent
# downloaders can never silently fall back to Stream Gears.
downloader = "crates/biliup-cli/src/server/core/downloader.rs"
replace_exact(
    downloader,
    '''    pub fn from_type(downloader_type: DownloaderType) -> Self {
        match downloader_type {
            DownloaderType::Ffmpeg => Self::Ffmpeg(FfmpegDownloader::new(
                vec![],
                DownloaderType::FfmpegExternal,
            )),
            _ => Self::StreamGears(StreamGearsDownloader::default()),
        }
    }
''',
    '''    pub fn from_type(downloader_type: DownloaderType) -> Self {
        match downloader_type {
            DownloaderType::Ffmpeg | DownloaderType::FfmpegExternal => Self::Ffmpeg(
                FfmpegDownloader::new(vec![], DownloaderType::FfmpegExternal),
            ),
            DownloaderType::FfmpegInternal => Self::Ffmpeg(FfmpegDownloader::new(
                vec![],
                DownloaderType::FfmpegInternal,
            )),
            // Legacy sync-downloader is the historical name for the built-in
            // synchronous recording path now provided by Stream Gears.
            DownloaderType::SyncDownloader | DownloaderType::StreamGears => {
                Self::StreamGears(StreamGearsDownloader::default())
            }
            DownloaderType::Streamlink | DownloaderType::YtDlp | DownloaderType::Ytarchive => {
                unreachable!(
                    "context-dependent downloader {downloader_type:?} must be constructed with stream context"
                )
            }
        }
    }
''',
)
live = "crates/biliup-cli/src/server/core/live.rs"
replace_exact(
    live,
    '''            DownloaderType::YtDlp | DownloaderType::Ytarchive => {
                ytdlp_runtime(stream, downloader_type)
            }
            _ => DownloaderRuntime::from_type(downloader_type),
''',
    '''            DownloaderType::YtDlp | DownloaderType::Ytarchive => {
                ytdlp_runtime(stream, downloader_type)
            }
            DownloaderType::Ffmpeg
            | DownloaderType::FfmpegExternal
            | DownloaderType::FfmpegInternal
            | DownloaderType::StreamGears
            | DownloaderType::SyncDownloader => DownloaderRuntime::from_type(downloader_type),
''',
)

# 4) Windows disk space: query the actual path/UNC share through Win32 instead
# of extracting a drive letter and launching PowerShell.
context = "crates/biliup-cli/src/server/infrastructure/context.rs"
replace_exact(
    context,
    "use std::process::Command;",
    "#[cfg(not(windows))]\nuse std::process::Command;",
)
replace_exact(
    context,
    '''    #[cfg(windows)]
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
''',
    '''    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "Kernel32")]
        unsafe extern "system" {
            fn GetDiskFreeSpaceExW(
                directory_name: *const u16,
                free_bytes_available: *mut u64,
                total_number_of_bytes: *mut u64,
                total_number_of_free_bytes: *mut u64,
            ) -> i32;
        }

        let query_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let wide: Vec<u16> = query_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut available = 0u64;
        let mut total = 0u64;
        let mut total_free = 0u64;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut available,
                &mut total,
                &mut total_free,
            )
        };
        return (ok != 0).then_some(available);
    }
''',
)

# Wire the hot upload-file limit into startup and settings saves.
manager = "crates/biliup-cli/src/server/core/download_manager.rs"
replace_exact(
    manager,
    "use crate::server::common::upload::UActor;",
    "use crate::server::common::replay;\nuse crate::server::common::upload::UActor;",
)
replace_exact(
    manager,
    '''    pub fn new(download_semaphore: u32, update_semaphore: u32, pool: ConnectionPool) -> Self {
        // 创建消息通道
''',
    '''    pub fn new(download_semaphore: u32, update_semaphore: u32, pool: ConnectionPool) -> Self {
        replay::set_upload_limit(update_semaphore);
        // 创建消息通道
''',
)

endpoints = "crates/biliup-cli/src/server/api/endpoints.rs"
replace_exact(
    endpoints,
    "use crate::server::common::upload::{build_studio, submit_to_bilibili, upload};",
    "use crate::server::common::replay;\nuse crate::server::common::upload::{build_studio, submit_to_bilibili, upload};",
)
replace_exact(
    endpoints,
    '''    *config.write().unwrap() = saved_config;
    let guard = config.read().unwrap();
    if let Some(loggers_level) = &guard.loggers_level {
''',
    '''    *config.write().unwrap() = saved_config;
    let guard = config.read().unwrap();
    replay::set_upload_limit(guard.pool2_size);
    if let Some(loggers_level) = &guard.loggers_level {
''',
)

dashboard = "app/(app)/dashboard/page.tsx"
replace_exact(
    dashboard,
    '''            <Card title="后台上传" style={{ marginBottom: 16 }}>
              <Form.Select
''',
    '''            <Card title="后台上传" style={{ marginBottom: 16 }}>
              <Form.InputNumber
                field="pool2_size"
                label="最大同时上传文件数"
                min={1}
                max={16}
                style={{ width: '100%' }}
                extraText="控制同时上传的录像文件数量；修改后立即生效，不会中断正在上传的文件。"
              />
              <Form.Select
''',
)

# 5) Sweep old safe-to-delete snapshots during startup recovery after filesystem
# outbox manifests have been imported.
recovery = "crates/biliup-cli/src/server/common/replay_recovery.rs"
replace_exact(
    recovery,
    '''    if outbox_count > 0 {
        info!(outbox_count, "restored filesystem replay outbox records");
    }

    let rows = sqlx::query(
''',
    '''    if outbox_count > 0 {
        info!(outbox_count, "restored filesystem replay outbox records");
    }
    let cleaned_credentials = replay::cleanup_completed_credentials(&pool).await?;
    if cleaned_credentials > 0 {
        info!(cleaned_credentials, "cleaned completed replay credential snapshots");
    }

    let rows = sqlx::query(
''',
)

print("Guarded Windows reliability patch applied successfully.")
