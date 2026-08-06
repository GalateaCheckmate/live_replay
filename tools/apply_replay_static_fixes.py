from pathlib import Path

path = Path("crates/biliup-cli/src/server/common/replay.rs")
text = path.read_text(encoding="utf-8")


def replace_exact(old: str, new: str) -> None:
    global text
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"expected one exact match, found {count}: {old[:80]!r}")
    text = text.replace(old, new, 1)


def replace_between(start: str, end: str, replacement: str) -> None:
    global text
    start_index = text.index(start)
    end_index = text.index(end, start_index)
    text = text[:start_index] + replacement.rstrip() + "\n\n" + text[end_index:]


replace_exact(
    '''#[derive(Debug, Clone, Serialize, Deserialize)]
struct OutboxRecord {
    session_id: i64,
    part_number: i64,
    file_path: PathBuf,
    original_file_path: PathBuf,
    danmaku_file_path: Option<PathBuf>,
    file_size: i64,
    file_mtime_ns: i64,
    file_identity: String,
}''',
    '''#[derive(Debug, Clone, Serialize, Deserialize)]
struct OutboxRecord {
    session_id: i64,
    part_number: i64,
    file_path: PathBuf,
    original_file_path: PathBuf,
    #[serde(default)]
    original_danmaku_file_path: Option<PathBuf>,
    danmaku_file_path: Option<PathBuf>,
    file_size: i64,
    file_mtime_ns: i64,
    file_identity: String,
}''',
)

replace_between(
    "async fn register_segment(",
    "async fn next_part_number(",
    r'''async fn register_segment(
    ctx: &Context,
    session_id: i64,
    event: &SegmentInfo,
) -> AppResult<SegmentRecord> {
    let part_number = next_part_number(ctx.pool(), session_id).await?;
    let record = prepare_outbox_record(session_id, part_number, event).await?;

    // 先持久化意图清单，再移动录像。这样任意时刻断电，恢复器都能根据清单
    // 判断文件仍在原位置还是已经进入安全队列，不会出现“文件已移动但无清单”的窗口。
    let outbox_path = write_outbox(&record).await?;
    if let Err(error) = stage_completed_segment(&record).await {
        rollback_staged_segment(&record).await;
        let _ = tokio::fs::remove_file(&outbox_path).await;
        return Err(error);
    }

    let mut last_error = None;
    for attempt in 0..6u64 {
        match persist_outbox_record(ctx.pool(), ctx.id(), &record).await {
            Ok(segment) => {
                let _ = tokio::fs::remove_file(&outbox_path).await;
                if let Err(error) = mark_session_recording(ctx.pool(), session_id).await {
                    warn!(error = ?error, session_id, "segment persisted but session status could not be refreshed");
                }
                return Ok(segment);
            }
            Err(error) => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(200 * (attempt + 1))).await;
            }
        }
    }
    Err(last_error
        .unwrap_or_else(|| AppError::Custom("failed to persist segment".to_string()).into()))
}''',
)

replace_between(
    "async fn stage_completed_segment(",
    "async fn rollback_staged_segment(",
    r'''async fn prepare_outbox_record(
    session_id: i64,
    part_number: i64,
    event: &SegmentInfo,
) -> AppResult<OutboxRecord> {
    let source = &event.prev_file_path;
    let metadata = tokio::fs::metadata(source)
        .await
        .change_context(AppError::Unknown)?;
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    let queue_dir = parent
        .join(".live-replay-queue")
        .join(session_id.to_string());
    let original_name = source
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("segment.bin");
    let staged_path = queue_dir.join(format!(
        "{:06}-{}-{original_name}",
        part_number,
        unix_nanos()
    ));
    let original_danmaku_file_path = event
        .danmaku_file_path
        .clone()
        .filter(|path| path.exists());
    let staged_danmaku = original_danmaku_file_path
        .as_ref()
        .map(|_| staged_path.with_extension("xml"));
    let mtime_ns = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_nanos() as i64)
        .unwrap_or(0);
    let identity = format!("{}:{}:{}", metadata.len(), mtime_ns, staged_path.display());
    Ok(OutboxRecord {
        session_id,
        part_number,
        file_path: staged_path,
        original_file_path: source.clone(),
        original_danmaku_file_path,
        danmaku_file_path: staged_danmaku,
        file_size: metadata.len() as i64,
        file_mtime_ns: mtime_ns,
        file_identity: identity,
    })
}

async fn stage_completed_segment(record: &OutboxRecord) -> AppResult<()> {
    if !record.file_path.exists() {
        if !record.original_file_path.exists() {
            return Err(AppError::Custom(format!(
                "replay segment is missing from both source and queue: {}",
                record.original_file_path.display()
            ))
            .into());
        }
        move_file(&record.original_file_path, &record.file_path).await?;
    }

    let staged_size = tokio::fs::metadata(&record.file_path)
        .await
        .change_context(AppError::Unknown)?
        .len();
    if staged_size != record.file_size as u64 {
        return Err(AppError::Custom(format!(
            "staged replay segment size mismatch: expected={}, actual={}, file={}",
            record.file_size,
            staged_size,
            record.file_path.display()
        ))
        .into());
    }

    if let (Some(original), Some(staged)) = (
        record.original_danmaku_file_path.as_ref(),
        record.danmaku_file_path.as_ref(),
    ) && !staged.exists()
        && original.exists()
    {
        move_file(original, staged).await?;
    }
    Ok(())
}''',
)

replace_between(
    "async fn rollback_staged_segment(",
    "async fn move_file(",
    r'''async fn rollback_staged_segment(record: &OutboxRecord) {
    if record.file_path.exists() && !record.original_file_path.exists() {
        let _ = move_file(&record.file_path, &record.original_file_path).await;
    }
    if let (Some(staged_xml), Some(original_xml)) = (
        record.danmaku_file_path.as_ref(),
        record.original_danmaku_file_path.as_ref(),
    ) && staged_xml.exists()
        && !original_xml.exists()
    {
        let _ = move_file(staged_xml, original_xml).await;
    }
}''',
)

replace_between(
    "pub async fn recover_filesystem_outbox(",
    "async fn persist_outbox_record(",
    r'''pub async fn recover_filesystem_outbox(pool: &ConnectionPool) -> AppResult<usize> {
    let mut recovered = 0usize;
    let mut entries = match tokio::fs::read_dir(OUTBOX_DIR).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).change_context(AppError::Unknown),
    };
    while let Some(entry) = entries
        .next_entry()
        .await
        .change_context(AppError::Unknown)?
    {
        let path = entry.path();
        let filename = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if path.extension().and_then(|value| value.to_str()) != Some("json")
            && !filename.ends_with(".json.tmp")
        {
            continue;
        }
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) => {
                warn!(file = ?path, error = ?error, "cannot read replay outbox file");
                continue;
            }
        };
        let record: OutboxRecord = match serde_json::from_slice(&bytes) {
            Ok(record) => record,
            Err(error) => {
                warn!(file = ?path, error = ?error, "quarantining malformed replay outbox file");
                let _ = tokio::fs::rename(&path, path.with_extension("bad")).await;
                continue;
            }
        };
        let source_info_id: i64 = match sqlx::query_scalar(
            "SELECT source_streamer_info_id FROM live_sessions WHERE id = ?",
        )
        .bind(record.session_id)
        .fetch_optional(pool)
        .await
        .change_context(AppError::Unknown)?
        {
            Some(id) => id,
            None => {
                warn!(session_id = record.session_id, file = ?record.file_path, "outbox session is missing; preserving files");
                continue;
            }
        };
        if let Err(error) = stage_completed_segment(&record).await {
            warn!(error = ?error, session_id = record.session_id, file = ?record.file_path, "replay outbox staging is incomplete; preserving manifest for retry");
            continue;
        }
        persist_outbox_record(pool, source_info_id, &record).await?;
        let _ = tokio::fs::remove_file(&path).await;
        wake_session(record.session_id).await;
        recovered += 1;
    }
    Ok(recovered)
}''',
)

replace_between(
    "async fn prepare_paths(",
    "async fn remux_to_mp4(",
    r'''async fn prepare_paths(
    pool: &ConnectionPool,
    segment: &SegmentRecord,
    processors: &[HookStep],
) -> AppResult<Vec<PathBuf>> {
    if segment.processed_file_path.is_some() {
        return Ok(prepared_paths(segment));
    }
    let mut paths = vec![segment.file_path.clone()];
    if segment
        .file_path
        .extension()
        .and_then(|value| value.to_str())
        .is_none_or(|ext| !ext.eq_ignore_ascii_case("mp4"))
    {
        paths[0] = remux_to_mp4(&segment.file_path).await?;
    }
    if let Some(path) = &segment.danmaku_file_path {
        paths.push(path.clone());
    }

    let mut remove_after_commit = Vec::new();
    for processor in processors {
        match processor {
            HookStep::Move { mv } => {
                let target = Path::new(mv);
                tokio::fs::create_dir_all(target)
                    .await
                    .change_context(AppError::Unknown)?;
                let mut copied = Vec::with_capacity(paths.len());
                for source in &paths {
                    let destination = target.join(source.file_name().ok_or_else(|| {
                        AppError::Custom("invalid processor path".to_string())
                    })?);
                    copy_file_atomic(source, &destination).await?;
                    if source != &destination {
                        remove_after_commit.push(source.clone());
                    }
                    copied.push(destination);
                }
                paths = copied;
            }
            HookStep::Remux { .. } => {}
            HookStep::Run { .. } | HookStep::Remove(_) => {
                return Err(AppError::Custom(
                    "上传前处理器不允许 run/rm；它们无法保证路径和重试幂等，请改到上传后手动处理"
                        .to_string(),
                )
                .into());
            }
        }
    }

    let processed = paths
        .first()
        .cloned()
        .ok_or_else(|| AppError::Custom("processor removed upload path".to_string()))?;
    let danmaku = paths
        .iter()
        .skip(1)
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("xml"))
        .cloned();

    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE recording_segments SET processed_file_path = ?, danmaku_file_path = ?, \
         updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(processed.display().to_string())
    .bind(danmaku.as_ref().map(|path| path.display().to_string()))
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    if processed != segment.file_path {
        sqlx::query("UPDATE filelist SET file = ? WHERE file = ?")
            .bind(processed.display().to_string())
            .bind(segment.file_path.display().to_string())
            .execute(&mut *tx)
            .await
            .change_context(AppError::Unknown)?;
    }
    tx.commit().await.change_context(AppError::Unknown)?;

    // 数据库已经指向新路径后再删旧文件。这里失败只会留下安全的重复副本，
    // 不会让队列失去唯一可用文件。
    for source in remove_after_commit {
        if let Err(error) = tokio::fs::remove_file(&source).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!(file = ?source, error = ?error, "failed to remove old processor source; duplicate copy retained");
        }
    }
    Ok(paths)
}''',
)

move_end = '''async fn write_outbox(record: &OutboxRecord) -> AppResult<PathBuf> {'''
insert_at = text.index(move_end)
copy_helper = r'''async fn copy_file_atomic(source: &Path, destination: &Path) -> AppResult<()> {
    if source == destination {
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .change_context(AppError::Unknown)?;
    }
    let source_size = tokio::fs::metadata(source)
        .await
        .change_context(AppError::Unknown)?
        .len();
    if let Ok(metadata) = tokio::fs::metadata(destination).await {
        if metadata.len() == source_size {
            return Ok(());
        }
        return Err(AppError::Custom(format!(
            "processor destination already exists with different size: {}",
            destination.display()
        ))
        .into());
    }
    let temporary = destination.with_extension(format!(
        "{}.copy-part",
        destination
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("file")
    ));
    let _ = tokio::fs::remove_file(&temporary).await;
    tokio::fs::copy(source, &temporary)
        .await
        .change_context(AppError::Unknown)?;
    let copied_size = tokio::fs::metadata(&temporary)
        .await
        .change_context(AppError::Unknown)?
        .len();
    if copied_size != source_size {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(AppError::Custom(format!(
            "processor copy size mismatch: source={source_size}, copied={copied_size}"
        ))
        .into());
    }
    tokio::fs::rename(&temporary, destination)
        .await
        .change_context(AppError::Unknown)?;
    Ok(())
}

'''
text = text[:insert_at] + copy_helper + text[insert_at:]

replace_between(
    "async fn mark_terminal(",
    "async fn mark_retry(",
    r'''async fn mark_terminal(
    pool: &ConnectionPool,
    segment: &SegmentRecord,
    terminal: &str,
) -> AppResult<()> {
    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;
    let timestamp_column = if terminal == "deleted" {
        "deleted_at"
    } else {
        "verified_at"
    };
    let query = format!(
        "UPDATE recording_segments SET status = ?, cleanup_state = ?, {timestamp_column} = COALESCE({timestamp_column}, CURRENT_TIMESTAMP), \
         last_error = NULL, updated_at = CURRENT_TIMESTAMP WHERE id = ?"
    );
    sqlx::query(&query)
        .bind(terminal)
        .bind(terminal)
        .bind(segment.id)
        .execute(&mut *tx)
        .await
        .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = 'complete', last_error = NULL, locked_at = NULL, \
         updated_at = CURRENT_TIMESTAMP WHERE segment_id = ?",
    )
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    if terminal == "deleted" {
        let processed = segment
            .processed_file_path
            .as_ref()
            .unwrap_or(&segment.file_path);
        sqlx::query("DELETE FROM filelist WHERE file = ? OR file = ?")
            .bind(segment.file_path.display().to_string())
            .bind(processed.display().to_string())
            .execute(&mut *tx)
            .await
            .change_context(AppError::Unknown)?;
    }
    sqlx::query(
        "UPDATE live_sessions SET verified_parts = MAX(verified_parts, ?), \
         next_part_to_upload = CASE WHEN next_part_to_upload = ? THEN ? + 1 ELSE next_part_to_upload END, \
         last_error = NULL, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(segment.part_number)
    .bind(segment.part_number)
    .bind(segment.part_number)
    .bind(segment.session_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    tx.commit().await.change_context(AppError::Unknown)?;
    wake_session(segment.session_id).await;
    Ok(())
}''',
)

replace_between(
    "async fn mark_retry(",
    "async fn mark_recording_complete(",
    r'''async fn mark_retry(
    pool: &ConnectionPool,
    segment: &SegmentRecord,
    attempt: i64,
    delay_seconds: u64,
    error_message: &str,
) -> AppResult<()> {
    let modifier = format!("+{delay_seconds} seconds");
    let cleanup_retry = matches!(
        segment.status.as_str(),
        "remote_verified" | "cleanup_pending"
    );
    let segment_status = if cleanup_retry {
        "cleanup_pending"
    } else {
        "retry_wait"
    };
    let job_status = if cleanup_retry {
        "remote_verified"
    } else {
        "retry_wait"
    };
    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE recording_segments SET status = ?, retry_count = ?, last_error = ?, \
         next_retry_at = datetime('now', ?), updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(segment_status)
    .bind(attempt)
    .bind(error_message)
    .bind(&modifier)
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE upload_jobs SET status = ?, last_error = ?, next_attempt_at = datetime('now', ?), \
         locked_at = NULL, updated_at = CURRENT_TIMESTAMP WHERE segment_id = ?",
    )
    .bind(job_status)
    .bind(error_message)
    .bind(&modifier)
    .bind(segment.id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        "UPDATE live_sessions SET status = CASE WHEN ended_at IS NULL THEN 'retrying' ELSE 'recording_complete' END, \
         last_error = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(error_message)
    .bind(segment.session_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    tx.commit().await.change_context(AppError::Unknown)?;
    Ok(())
}''',
)

path.write_text(text, encoding="utf-8")
print("Applied replay static safety fixes")
