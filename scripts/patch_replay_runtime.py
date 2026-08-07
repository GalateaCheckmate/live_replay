from pathlib import Path


def replace_exact(path: str, old: str, new: str, expected: int = 1) -> None:
    file = Path(path)
    text = file.read_text(encoding="utf-8")
    count = text.count(old)
    if count != expected:
        raise SystemExit(f"{path}: expected {expected} matches, found {count}: {old[:100]!r}")
    file.write_text(text.replace(old, new), encoding="utf-8")


replay = "crates/biliup-cli/src/server/common/replay.rs"

# Upload file concurrency: replace the hard-coded global Semaphore(1) with a
# hot-updatable active counter. Waiting uploads read the shared Config through
# Context on every pass, so pool2_size changes take effect without restart.
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

struct UploadSlotPermit;

impl Drop for UploadSlotPermit {
    fn drop(&mut self) {
        ACTIVE_UPLOADS.fetch_sub(1, Ordering::AcqRel);
    }
}

async fn acquire_upload_slot(ctx: &Context) -> UploadSlotPermit {
    loop {
        let limit = ctx.config().pool2_size.max(1) as usize;
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
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
''',
)
replace_exact(
    replay,
    "        let video = upload_single_file(&upload_path, &runtime).await?;",
    "        let video = upload_single_file(&upload_path, &runtime, ctx).await?;",
)
replace_exact(
    replay,
    '''async fn upload_single_file(file_path: &Path, runtime: &UploadRuntime) -> AppResult<Video> {
    let _permit = upload_slots()
        .acquire()
        .await
        .change_context(AppError::Custom("upload semaphore closed".to_string()))?;
''',
    '''async fn upload_single_file(
    file_path: &Path,
    runtime: &UploadRuntime,
    ctx: &Context,
) -> AppResult<Video> {
    let _permit = acquire_upload_slot(ctx).await;
''',
)

# Credential snapshots: constrain generated paths and clean completed sessions
# only after the reconnect window has elapsed and no DB/outbox work remains.
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
        warn!(session_key, "refusing invalid replay credential snapshot path");
        return false;
    };
    let temporary = target.with_extension("json.part");
    let mut removed = false;
    for path in [&target, &temporary] {
        match tokio::fs::remove_file(path).await {
            Ok(()) => removed = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                warn!(file = %path.display(), error = ?error, "failed to remove replay credential snapshot")
            }
        }
    }
    removed
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
replace_exact(
    replay,
    '''async fn mark_session_complete(pool: &ConnectionPool, session_id: i64) -> AppResult<()> {
    sqlx::query(
        "UPDATE live_sessions SET status = 'complete', last_error = NULL, \\
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
        "SELECT id, session_key FROM live_sessions \\
         WHERE status = 'complete' AND ended_at IS NOT NULL AND session_key IS NOT NULL \\
           AND datetime(ended_at) <= datetime('now', ?) \\
           AND NOT EXISTS (SELECT 1 FROM recording_segments r WHERE r.session_id = live_sessions.id \\
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
        if filesystem_outbox_max_part(session_id).await > 0 {
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
        "UPDATE live_sessions SET status = 'complete', last_error = NULL, \\
         ended_at = COALESCE(ended_at, CURRENT_TIMESTAMP), updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(session_id)
    .execute(pool)
    .await
    .change_context(AppError::Unknown)?;

    let cleanup_pool = pool.clone();
    let delay = env_u64("LIVE_REPLAY_RECONNECT_WINDOW_SECONDS", 600).saturating_add(5);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(delay)).await;
        match cleanup_completed_credentials(&cleanup_pool).await {
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
                extraText="控制同时上传的录像文件数量。修改后立即生效，不会中断正在上传的文件。"
              />
              <Form.Select
''',
)

print("Replay runtime patch applied.")
