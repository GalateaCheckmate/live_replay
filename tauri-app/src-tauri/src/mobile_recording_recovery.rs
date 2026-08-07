use super::mobile_recording_journal::{
    RecordingJournalSession, complete_segment_handoff, finish_session, register_recovered_segment,
    snapshot,
};
use chrono::{Local, TimeZone};
use live_replay_core::recording::{LIVE_OFFLINE_GRACE, RecordingSegment};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::Manager;
use tokio::fs;

pub async fn recover_startup(app: &tauri::AppHandle) -> Result<(), String> {
    let mut errors = Vec::new();

    if let Err(error) = recover_pending_segments(app).await {
        errors.push(error);
    }
    if let Err(error) = recover_discoverable_sources(app).await {
        errors.push(error);
    }
    if let Err(error) = recover_raw_files(app).await {
        errors.push(error);
    }
    if let Err(error) = close_stale_sessions(app).await {
        errors.push(error);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn recordings_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("recordings"))
        .map_err(|error| format!("无法获取录像恢复目录: {error}"))
}

async fn recover_pending_segments(app: &tauri::AppHandle) -> Result<(), String> {
    let mut pending = snapshot(app).await?.pending_segments;
    pending.sort_by(|a, b| {
        a.live_session_id
            .cmp(&b.live_session_id)
            .then(a.segment_index.cmp(&b.segment_index))
    });
    let mut errors = Vec::new();
    for segment in pending {
        if let Err(error) = finalize_enqueue_and_clear(app, &segment).await {
            errors.push(format!(
                "恢复 {} P{} 失败: {error}",
                segment.live_session_id, segment.segment_index
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

async fn recover_discoverable_sources(app: &tauri::AppHandle) -> Result<(), String> {
    let dir = recordings_dir(app)?;
    fs::create_dir_all(&dir)
        .await
        .map_err(|error| format!("创建录像恢复目录失败: {error}"))?;
    let mut entries = fs::read_dir(&dir)
        .await
        .map_err(|error| format!("扫描录像恢复目录失败: {error}"))?;
    let mut sources = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("扫描录像恢复文件失败: {error}"))?
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some((session_id, index, extension)) = parse_source_name(&name) {
            sources.push((session_id, index, extension, entry.path()));
        }
    }
    sources.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    let mut errors = Vec::new();
    for (session_id, index, extension, source_path) in sources {
        let store = snapshot(app).await?;
        if store.pending_segments.iter().any(|segment| {
            segment.live_session_id == session_id && segment.segment_index == index
        }) {
            continue;
        }
        let Some(session) = store
            .sessions
            .iter()
            .find(|session| session.live_session_id == session_id)
            .cloned()
        else {
            errors.push(format!(
                "发现无法归属的恢复源文件，已保留: {}",
                source_path.display()
            ));
            continue;
        };
        match segment_from_recovered_source(&session, index, &extension, &source_path).await {
            Ok(segment) => {
                if let Err(error) = register_recovered_segment(
                    app,
                    &session.room_url,
                    &segment,
                    session.last_heartbeat_at,
                )
                .await
                {
                    errors.push(format!("恢复 P{index} 写 journal 失败: {error}"));
                    continue;
                }
                if let Err(error) = finalize_enqueue_and_clear(app, &segment).await {
                    errors.push(format!("恢复 P{index} handoff 失败: {error}"));
                }
            }
            Err(error) => errors.push(error),
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

async fn recover_raw_files(app: &tauri::AppHandle) -> Result<(), String> {
    let dir = recordings_dir(app)?;
    fs::create_dir_all(&dir)
        .await
        .map_err(|error| format!("创建录像恢复目录失败: {error}"))?;
    let mut entries = fs::read_dir(&dir)
        .await
        .map_err(|error| format!("扫描未完成 raw 录像失败: {error}"))?;
    let sessions = snapshot(app).await?.sessions;
    let mut raws: Vec<(String, String, PathBuf)> = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("读取未完成 raw 录像失败: {error}"))?
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(extension) = raw_source_extension(&name) else {
            continue;
        };
        for session in &sessions {
            let prefix = format!(".lr-{}-raw-", session.live_session_id);
            if name.starts_with(&prefix) {
                raws.push((
                    session.live_session_id.clone(),
                    extension.to_string(),
                    entry.path(),
                ));
                break;
            }
        }
    }
    raws.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| modified_seconds_sync(&a.2).cmp(&modified_seconds_sync(&b.2)))
    });

    let mut errors = Vec::new();
    for (session_id, extension, raw_path) in raws {
        let store = snapshot(app).await?;
        let Some(session) = store
            .sessions
            .iter()
            .find(|session| session.live_session_id == session_id)
            .cloned()
        else {
            continue;
        };
        let index = session.next_segment_index.max(1);
        let metadata = match fs::metadata(&raw_path).await {
            Ok(metadata) => metadata,
            Err(error) => {
                errors.push(format!("读取 raw 录像失败 {}: {error}", raw_path.display()));
                continue;
            }
        };
        let minimum = if extension == "flv" { 14 } else { 1 };
        if metadata.len() < minimum {
            let _ = fs::remove_file(&raw_path).await;
            continue;
        }

        // A process kill leaves LifecycleFile at *.flv.part / *.ts.part. At startup no writer can
        // still own it, so fsync it first and then assign the durable session/P source identity.
        match fs::OpenOptions::new().read(true).open(&raw_path).await {
            Ok(file) => {
                if let Err(error) = file.sync_all().await {
                    errors.push(format!("同步 raw 录像失败 {}: {error}", raw_path.display()));
                    continue;
                }
            }
            Err(error) => {
                errors.push(format!("打开 raw 录像失败 {}: {error}", raw_path.display()));
                continue;
            }
        }

        let source_path = dir.join(format!(
            ".lr-{}-P{}-source.{}",
            session.live_session_id, index, extension
        ));
        if fs::metadata(&source_path).await.is_ok() {
            errors.push(format!(
                "恢复目标 P{index} 已存在，raw 文件保留避免覆盖: {}",
                raw_path.display()
            ));
            continue;
        }
        if let Err(error) = fs::rename(&raw_path, &source_path).await {
            errors.push(format!("提交 raw 恢复文件失败: {error}"));
            continue;
        }
        sync_parent_dir(&source_path);

        match segment_from_recovered_source(&session, index, &extension, &source_path).await {
            Ok(segment) => {
                if let Err(error) = register_recovered_segment(
                    app,
                    &session.room_url,
                    &segment,
                    session.last_heartbeat_at,
                )
                .await
                {
                    errors.push(format!("恢复 raw P{index} 写 journal 失败: {error}"));
                    continue;
                }
                if let Err(error) = finalize_enqueue_and_clear(app, &segment).await {
                    errors.push(format!("恢复 raw P{index} handoff 失败: {error}"));
                }
            }
            Err(error) => errors.push(error),
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn raw_source_extension(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".flv.part") || lower.ends_with(".flv") {
        Some("flv")
    } else if lower.ends_with(".ts.part") || lower.ends_with(".ts") {
        Some("ts")
    } else {
        None
    }
}

async fn segment_from_recovered_source(
    session: &RecordingJournalSession,
    index: u32,
    extension: &str,
    source_path: &Path,
) -> Result<RecordingSegment, String> {
    let metadata = fs::metadata(source_path)
        .await
        .map_err(|error| format!("读取恢复源文件失败 {}: {error}", source_path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(format!("恢复源文件为空: {}", source_path.display()));
    }
    let ended_at = metadata
        .modified()
        .ok()
        .and_then(system_time_seconds)
        .unwrap_or_else(|| chrono::Utc::now().timestamp())
        .max(session.current_segment_started_at);
    let started_at = session.current_segment_started_at;
    let base = safe_file_component(&session.streamer_name);
    let start = Local
        .timestamp_opt(started_at, 0)
        .single()
        .unwrap_or_else(Local::now);
    let end = Local
        .timestamp_opt(ended_at, 0)
        .single()
        .unwrap_or_else(Local::now);
    let local_stem = format!(
        "{base}｜{}｜{}-{}",
        start.format("%Y-%m-%d"),
        start.format("%H：%M"),
        end.format("%H：%M")
    );
    let mut final_mp4_path = source_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{local_stem}.mp4"));
    if fs::metadata(&final_mp4_path).await.is_ok() {
        final_mp4_path = source_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{local_stem}｜P{index}.mp4"));
    }
    let youtube_title = format!(
        "{base}｜{}｜{}-{}",
        start.format("%Y-%m-%d"),
        start.format("%H:%M"),
        end.format("%H:%M")
    );
    Ok(RecordingSegment {
        live_session_id: session.live_session_id.clone(),
        segment_index: index,
        streamer_name: session.streamer_name.clone(),
        platform: "restored".to_string(),
        room_url: session.room_url.clone(),
        source_path: source_path.to_string_lossy().into_owned(),
        final_mp4_path: final_mp4_path.to_string_lossy().into_owned(),
        local_file_name: final_mp4_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string(),
        youtube_title,
        started_at,
        ended_at,
        bytes_written: metadata.len(),
    })
}

async fn finalize_enqueue_and_clear(
    app: &tauri::AppHandle,
    segment: &RecordingSegment,
) -> Result<(), String> {
    let final_mp4 = super::mobile_recordings::finalize_segment_mp4(app, segment).await?;
    super::mobile_bilibili::enqueue_finalized_segment(app, segment, &final_mp4).await?;
    complete_segment_handoff(app, &segment.live_session_id, segment.segment_index).await
}

async fn close_stale_sessions(app: &tauri::AppHandle) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp();
    let grace = LIVE_OFFLINE_GRACE.as_secs() as i64;
    let store = snapshot(app).await?;
    let mut errors = Vec::new();
    for session in store.sessions {
        if now.saturating_sub(session.last_heartbeat_at) <= grace {
            continue;
        }
        let end = session
            .last_heartbeat_at
            .max(session.current_segment_started_at);
        if let Err(error) = super::mobile_bilibili::mark_session_recording_complete(
            app,
            &session.live_session_id,
            end,
        )
        .await
        {
            errors.push(format!(
                "关闭过期 session {} 失败: {error}",
                session.live_session_id
            ));
            continue;
        }
        if let Err(error) = finish_session(app, &session.room_url).await {
            errors.push(format!(
                "清理过期 journal {} 失败: {error}",
                session.live_session_id
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn parse_source_name(name: &str) -> Option<(String, u32, String)> {
    let rest = name.strip_prefix(".lr-")?;
    let (session_id, p_rest) = rest.rsplit_once("-P")?;
    let (index, extension) = p_rest.split_once("-source.")?;
    let index = index.parse::<u32>().ok()?;
    if !matches!(extension, "flv" | "ts") {
        return None;
    }
    Some((session_id.to_string(), index, extension.to_string()))
}

fn system_time_seconds(value: SystemTime) -> Option<i64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
}

fn modified_seconds_sync(path: &Path) -> i64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(system_time_seconds)
        .unwrap_or(0)
}

fn sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
    }
}

fn safe_file_component(input: &str) -> String {
    let cleaned: String = input
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            control if control.is_control() => '_',
            other => other,
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.');
    if cleaned.is_empty() {
        "live-replay".to_string()
    } else {
        cleaned.chars().take(80).collect()
    }
}
