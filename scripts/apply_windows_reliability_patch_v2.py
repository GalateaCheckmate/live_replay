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


# 1) FFmpeg hardening against the current main implementation.
ffmpeg = "crates/biliup-cli/src/server/core/downloader/ffmpeg_downloader.rs"
replace_exact(
    ffmpeg,
    "use tokio::io::{AsyncBufReadExt, BufReader};",
    "use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};",
)
replace_exact(
    ffmpeg,
    "use tokio::sync::RwLock;",
    "use tokio::sync::RwLock;\nuse tokio::time::{sleep, Duration};",
)
replace_exact(ffmpeg, ".stdin(Stdio::null())", ".stdin(Stdio::piped())", expected=2)

# External mode: a .part file becomes publishable only after a clean FFmpeg exit.
replace_between(
    ffmpeg,
    '''        let status = spawn_log(child, &self.process_handle).await?;
        // 退出时，重命名文件
''',
    '''    /// 执行内部分段下载
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
    }

''',
)

# Internal segment mode must write into the configured recording directory.
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

# Internal mode: install the child handle before waiting on segment-list output so
# stop() can reach a live FFmpeg process. Each completed segment is promoted once.
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
            if line.trim().is_empty() {
                continue;
            }
            let file_path = PathBuf::from(line.trim());
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
            // Ask FFmpeg to flush the current container before falling back to a hard kill.
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

# Reuse the guarded steps 2-5 from the first script so there is only one copy of
# the large replay/concurrency/disk/credential patch while this temporary task exists.
base = Path("scripts/apply_windows_reliability_patch.py").read_text(encoding="utf-8")
marker = "# 2) Live Replay upload-file concurrency"
position = base.find(marker)
if position < 0:
    raise SystemExit("cannot locate guarded patch steps 2-5")
exec(base[position:], globals(), globals())
