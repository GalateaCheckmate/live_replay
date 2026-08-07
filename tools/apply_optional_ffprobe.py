from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
path = ROOT / "crates/biliup-cli/src/server/common/replay.rs"
text = path.read_text(encoding="utf-8")
old = '''async fn validate_media_file(path: &Path) -> AppResult<()> {
    let probe = ffprobe_program();
    let output = Command::new(&probe)
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
        .change_context(AppError::Custom(format!(
            "无法启动 ffprobe（{}）；录像已保留并等待重试",
            probe.display()
        )))?;
    if !output.status.success() {
        return Err(
            AppError::Custom(format!("录像文件无法正常解析，已保留：{}", path.display())).into(),
        );
    }
    let duration = String::from_utf8_lossy(&output.stdout);
    if parse_positive_duration(&duration).is_none() {
        return Err(AppError::Custom(format!("录像时长无效，已保留：{}", path.display())).into());
    }
    Ok(())
}
'''
new = '''async fn validate_media_file(path: &Path) -> AppResult<()> {
    // ffprobe 是增强校验，不再是 Live Replay 的硬依赖。
    // 便携环境没有安装 FFmpeg 时，先做内置容器头检查；上传/远端验证失败仍会保留本地文件。
    basic_media_validation(path).await?;

    let probe = ffprobe_program();
    let output = Command::new(&probe)
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
        .await;

    let output = match output {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            warn!(
                probe = %probe.display(),
                file = %path.display(),
                "ffprobe not installed; built-in media validation passed, continuing upload"
            );
            return Ok(());
        }
        Err(error) => {
            warn!(
                probe = %probe.display(),
                file = %path.display(),
                error = ?error,
                "ffprobe could not be started; built-in media validation passed, continuing upload"
            );
            return Ok(());
        }
    };

    if !output.status.success() {
        return Err(
            AppError::Custom(format!("录像文件无法正常解析，已保留：{}", path.display())).into(),
        );
    }
    let duration = String::from_utf8_lossy(&output.stdout);
    if parse_positive_duration(&duration).is_none() {
        return Err(AppError::Custom(format!("录像时长无效，已保留：{}", path.display())).into());
    }
    Ok(())
}

async fn basic_media_validation(path: &Path) -> AppResult<()> {
    let metadata = tokio::fs::metadata(path)
        .await
        .change_context(AppError::Unknown)?;
    if metadata.len() < 13 {
        return Err(AppError::Custom(format!(
            "录像文件过小或未完整封装，已保留：{}",
            path.display()
        ))
        .into());
    }

    let bytes = tokio::fs::read(path)
        .await
        .change_context(AppError::Unknown)?;
    let head = &bytes[..bytes.len().min(16)];
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let valid = match extension.as_str() {
        "flv" => head.starts_with(b"FLV"),
        "mp4" | "3gp" => head.len() >= 8 && &head[4..8] == b"ftyp",
        "ts" => head.first() == Some(&0x47),
        "mkv" | "webm" => head.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]),
        _ => true,
    };
    if !valid {
        return Err(AppError::Custom(format!(
            "录像容器头校验失败，已保留：{}",
            path.display()
        ))
        .into());
    }
    Ok(())
}
'''
if old not in text:
    raise SystemExit("validate_media_file block not found")
path.write_text(text.replace(old, new, 1), encoding="utf-8")
