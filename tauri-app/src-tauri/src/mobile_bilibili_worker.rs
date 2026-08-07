use super::mobile_bilibili::{
    BilibiliSegmentState, BilibiliSegmentTask, BilibiliSessionTask, BilibiliStore, mutate_store,
    snapshot,
};
use super::mobile_bilibili_auth::load_valid_login;
use biliup::uploader::bilibili::{Vid, Video};
use biliup::uploader::credential::{LoginInfo, bilibili_from_info};
use chrono::{Local, TimeZone};
use live_replay_core::bilibili::{
    append_submission_part, build_live_replay_studio, create_submission, upload_segment_file,
};
use std::path::Path;
use tokio::fs;
use tokio::time::{Duration, sleep};

const UPLOAD_CONCURRENCY: usize = 3;
const WORKER_IDLE_SECONDS: u64 = 5;

#[derive(Clone)]
struct WorkItem {
    session: BilibiliSessionTask,
    segment: BilibiliSegmentTask,
}

pub fn start_upload_worker(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(error) = cleanup_verified_files(&app).await {
                eprintln!("[bilibili] cleanup: {error}");
            }
            if let Err(error) = finalize_completed_session_titles(&app).await {
                eprintln!("[bilibili] finalize title: {error}");
            }
            match snapshot(&app).await {
                Ok(store) if store.settings.auto_upload => {
                    if let Some(work) = next_work(&store, chrono::Utc::now().timestamp()) {
                        if let Err(error) = process_work(&app, work).await {
                            eprintln!("[bilibili] worker: {error}");
                        }
                    }
                }
                Ok(_) => {}
                Err(error) => eprintln!("[bilibili] read queue: {error}"),
            }
            sleep(Duration::from_secs(WORKER_IDLE_SECONDS)).await;
        }
    });
}

fn next_work(store: &BilibiliStore, now: i64) -> Option<WorkItem> {
    let mut sessions = store.sessions.clone();
    sessions.sort_by_key(|session| session.session_started_at);
    for session in sessions {
        let mut segments = session.segments.clone();
        segments.sort_by_key(|segment| segment.segment_index);

        // The first not-yet-verified segment is the only segment allowed to advance for this
        // liveSession. This makes P ordering deterministic even after process restarts.
        let Some(segment) = segments
            .into_iter()
            .find(|segment| segment.state != BilibiliSegmentState::RemoteVerified)
        else {
            continue;
        };

        match segment.state {
            BilibiliSegmentState::ReadyToUpload
            | BilibiliSegmentState::UploadingFile
            | BilibiliSegmentState::FileUploaded
            | BilibiliSegmentState::Submitting
            | BilibiliSegmentState::AuthRequired => {
                return Some(WorkItem { session, segment });
            }
            BilibiliSegmentState::RemoteProcessing if segment.next_retry_at <= now => {
                return Some(WorkItem { session, segment });
            }
            BilibiliSegmentState::RetryPending if segment.next_retry_at <= now => {
                return Some(WorkItem { session, segment });
            }
            // Uncertain/conflict deliberately blocks later P parts for this session. Advancing P2
            // while P1 is uncertain is worse than leaving files on disk.
            BilibiliSegmentState::SubmissionUncertain | BilibiliSegmentState::Conflict => {}
            BilibiliSegmentState::RemoteVerified | BilibiliSegmentState::RemoteProcessing
            | BilibiliSegmentState::RetryPending => {}
        }
    }
    None
}

async fn process_work(app: &tauri::AppHandle, work: WorkItem) -> Result<(), String> {
    let login = match load_valid_login(app).await {
        Ok(login) => login,
        Err(error) => {
            set_segment_state(
                app,
                &work.session.live_session_id,
                work.segment.segment_index,
                BilibiliSegmentState::AuthRequired,
                Some(error),
                None,
            )
            .await?;
            return Ok(());
        }
    };

    match work.segment.state {
        BilibiliSegmentState::AuthRequired => {
            let next = if work.segment.remote_filename.is_some() {
                BilibiliSegmentState::FileUploaded
            } else {
                BilibiliSegmentState::ReadyToUpload
            };
            set_segment_state(
                app,
                &work.session.live_session_id,
                work.segment.segment_index,
                next,
                None,
                Some(0),
            )
            .await
        }
        BilibiliSegmentState::ReadyToUpload
        | BilibiliSegmentState::UploadingFile
        | BilibiliSegmentState::RetryPending => upload_file_stage(app, &login, &work).await,
        BilibiliSegmentState::FileUploaded => submit_part_stage(app, &login, &work).await,
        BilibiliSegmentState::Submitting => recover_submitting_stage(app, &login, &work).await,
        BilibiliSegmentState::RemoteProcessing => verify_remote_stage(app, &login, &work).await,
        BilibiliSegmentState::RemoteVerified
        | BilibiliSegmentState::SubmissionUncertain
        | BilibiliSegmentState::Conflict => Ok(()),
    }
}

async fn upload_file_stage(
    app: &tauri::AppHandle,
    login: &LoginInfo,
    work: &WorkItem,
) -> Result<(), String> {
    let metadata = match fs::metadata(&work.segment.local_path).await {
        Ok(metadata) if metadata.is_file() && metadata.len() > 0 => metadata,
        Ok(_) => {
            return mark_conflict(app, work, "待上传录像不是有效文件或为空。".to_string()).await;
        }
        Err(error) => {
            return mark_conflict(app, work, format!("待上传录像不存在: {error}")).await;
        }
    };
    if metadata.len() != work.segment.file_size {
        return mark_conflict(
            app,
            work,
            format!(
                "录像大小发生变化，拒绝上传（入队 {} bytes，当前 {} bytes）。",
                work.segment.file_size,
                metadata.len()
            ),
        )
        .await;
    }

    set_segment_state(
        app,
        &work.session.live_session_id,
        work.segment.segment_index,
        BilibiliSegmentState::UploadingFile,
        None,
        None,
    )
    .await?;

    match upload_segment_file(login.clone(), Path::new(&work.segment.local_path), UPLOAD_CONCURRENCY)
        .await
    {
        Ok(mut video) => {
            video.title = Some(part_title(&work.segment));
            let filename = video.filename.clone();
            mutate_segment(app, work, |_, segment| {
                segment.remote_filename = Some(filename);
                segment.state = BilibiliSegmentState::FileUploaded;
                segment.retry_count = 0;
                segment.next_retry_at = 0;
                segment.last_error = None;
                Ok(())
            })
            .await
        }
        Err(error) => schedule_file_retry(app, work, error).await,
    }
}

async fn submit_part_stage(
    app: &tauri::AppHandle,
    login: &LoginInfo,
    work: &WorkItem,
) -> Result<(), String> {
    let filename = work
        .segment
        .remote_filename
        .clone()
        .ok_or_else(|| "FILE_UPLOADED 状态缺少 remote_filename。".to_string())?;
    let mut video = Video::new(&filename);
    video.title = Some(part_title(&work.segment));

    // Persist SUBMITTING before mutating a remote manuscript. If the process dies after Bilibili
    // accepted the request but before we persist its response, restart logic never blindly repeats
    // the mutation.
    set_segment_state(
        app,
        &work.session.live_session_id,
        work.segment.segment_index,
        BilibiliSegmentState::Submitting,
        None,
        None,
    )
    .await?;

    if let Some(aid) = work.session.aid {
        match append_submission_part(login.clone(), aid, video).await {
            Ok(()) => mark_remote_processing(app, work, 3).await,
            Err(error) => {
                // Known aid lets us safely inspect whether the append actually landed before
                // declaring an uncertain result.
                match remote_contains_filename(login, aid, &filename).await {
                    Ok(true) => mark_remote_verified(app, work).await,
                    Ok(false) | Err(_) => mark_submission_uncertain(app, work, error).await,
                }
            }
        }
    } else {
        let studio = build_live_replay_studio(
            &session_title(&work.session, Some(work.segment.ended_at)),
            &work.session.room_url,
            &format!("自动直播回放\n直播间：{}", work.session.room_url),
            vec![video],
            true,
        )?;
        match create_submission(login.clone(), &studio).await {
            Ok(remote) => {
                mutate_segment(app, work, |session, segment| {
                    session.aid = Some(remote.aid);
                    session.bvid = remote.bvid;
                    session.submission_state = "ACTIVE".to_string();
                    segment.state = BilibiliSegmentState::RemoteProcessing;
                    segment.retry_count = 0;
                    segment.next_retry_at = chrono::Utc::now().timestamp() + 3;
                    segment.last_error = None;
                    Ok(())
                })
                .await
            }
            Err(error) => {
                // No aid means a lost create-submission response cannot be safely reconstructed.
                // Never auto-create a second manuscript in this state.
                mark_submission_uncertain(app, work, error).await
            }
        }
    }
}

async fn recover_submitting_stage(
    app: &tauri::AppHandle,
    login: &LoginInfo,
    work: &WorkItem,
) -> Result<(), String> {
    let Some(filename) = work.segment.remote_filename.as_deref() else {
        return mark_conflict(app, work, "SUBMITTING 状态缺少 remote_filename。".to_string()).await;
    };
    let Some(aid) = work.session.aid else {
        return mark_submission_uncertain(
            app,
            work,
            "App 在首次投稿请求期间中断且 aid 尚未持久化；为避免重复投稿，已停止自动重试。"
                .to_string(),
        )
        .await;
    };
    match remote_contains_filename(login, aid, filename).await {
        Ok(true) => mark_remote_verified(app, work).await,
        Ok(false) => {
            mark_submission_uncertain(
                app,
                work,
                "App 在追加分P请求期间中断，远端暂未确认该 P；为避免重复分P，已停止自动追加。"
                    .to_string(),
            )
            .await
        }
        Err(error) => mark_remote_processing_with_error(app, work, error).await,
    }
}

async fn verify_remote_stage(
    app: &tauri::AppHandle,
    login: &LoginInfo,
    work: &WorkItem,
) -> Result<(), String> {
    let aid = work
        .session
        .aid
        .ok_or_else(|| "REMOTE_PROCESSING 状态缺少 aid。".to_string())?;
    let filename = work
        .segment
        .remote_filename
        .as_deref()
        .ok_or_else(|| "REMOTE_PROCESSING 状态缺少 remote_filename。".to_string())?;
    match remote_contains_filename(login, aid, filename).await {
        Ok(true) => mark_remote_verified(app, work).await,
        Ok(false) => {
            let attempts = work.segment.retry_count.saturating_add(1);
            if attempts >= 8 {
                mark_submission_uncertain(
                    app,
                    work,
                    "B站已接受提交，但多次检查仍无法确认该分P；本地录像保留，停止自动重复提交。"
                        .to_string(),
                )
                .await
            } else {
                let delay = verification_backoff(attempts);
                mutate_segment(app, work, |_, segment| {
                    segment.state = BilibiliSegmentState::RemoteProcessing;
                    segment.retry_count = attempts;
                    segment.next_retry_at = chrono::Utc::now().timestamp() + delay;
                    segment.last_error = Some("等待 B站远端确认分P。".to_string());
                    Ok(())
                })
                .await
            }
        }
        Err(error) => mark_remote_processing_with_error(app, work, error).await,
    }
}

async fn remote_contains_filename(
    login: &LoginInfo,
    aid: u64,
    filename: &str,
) -> Result<bool, String> {
    let bili = bilibili_from_info(login.clone(), None)
        .map_err(|error| format!("创建 B站验证客户端失败: {error}"))?;
    let studio = bili
        .studio_data(&Vid::Aid(aid), None)
        .await
        .map_err(|error| format!("读取 B站远端稿件失败: {error}"))?;
    Ok(studio.videos.iter().any(|video| video.filename == filename))
}

async fn mutate_segment<F>(app: &tauri::AppHandle, work: &WorkItem, mutate: F) -> Result<(), String>
where
    F: FnOnce(&mut BilibiliSessionTask, &mut BilibiliSegmentTask) -> Result<(), String>,
{
    mutate_store(app, |store| {
        let session = store
            .sessions
            .iter_mut()
            .find(|session| session.live_session_id == work.session.live_session_id)
            .ok_or_else(|| "B站 liveSession 不存在。".to_string())?;
        let pos = session
            .segments
            .iter()
            .position(|segment| segment.segment_index == work.segment.segment_index)
            .ok_or_else(|| "B站 segment 不存在。".to_string())?;
        let mut segment = session.segments.remove(pos);
        let result = mutate(session, &mut segment);
        session.segments.insert(pos, segment);
        result
    })
    .await
}

async fn set_segment_state(
    app: &tauri::AppHandle,
    session_id: &str,
    segment_index: u32,
    state: BilibiliSegmentState,
    error: Option<String>,
    next_retry_at: Option<i64>,
) -> Result<(), String> {
    mutate_store(app, |store| {
        let segment = store
            .sessions
            .iter_mut()
            .find(|session| session.live_session_id == session_id)
            .and_then(|session| {
                session
                    .segments
                    .iter_mut()
                    .find(|segment| segment.segment_index == segment_index)
            })
            .ok_or_else(|| "B站 segment 不存在。".to_string())?;
        segment.state = state;
        segment.last_error = error;
        if let Some(next_retry_at) = next_retry_at {
            segment.next_retry_at = next_retry_at;
        }
        Ok(())
    })
    .await
}

async fn schedule_file_retry(
    app: &tauri::AppHandle,
    work: &WorkItem,
    error: String,
) -> Result<(), String> {
    let attempts = work.segment.retry_count.saturating_add(1);
    let delay = upload_backoff(attempts);
    mutate_segment(app, work, |_, segment| {
        segment.state = BilibiliSegmentState::RetryPending;
        segment.retry_count = attempts;
        segment.next_retry_at = chrono::Utc::now().timestamp() + delay;
        segment.last_error = Some(error);
        Ok(())
    })
    .await
}

async fn mark_remote_processing(
    app: &tauri::AppHandle,
    work: &WorkItem,
    delay_seconds: i64,
) -> Result<(), String> {
    mutate_segment(app, work, |_, segment| {
        segment.state = BilibiliSegmentState::RemoteProcessing;
        segment.retry_count = 0;
        segment.next_retry_at = chrono::Utc::now().timestamp() + delay_seconds;
        segment.last_error = None;
        Ok(())
    })
    .await
}

async fn mark_remote_processing_with_error(
    app: &tauri::AppHandle,
    work: &WorkItem,
    error: String,
) -> Result<(), String> {
    let attempts = work.segment.retry_count.saturating_add(1);
    let delay = verification_backoff(attempts);
    mutate_segment(app, work, |_, segment| {
        segment.state = BilibiliSegmentState::RemoteProcessing;
        segment.retry_count = attempts;
        segment.next_retry_at = chrono::Utc::now().timestamp() + delay;
        segment.last_error = Some(error);
        Ok(())
    })
    .await
}

async fn mark_remote_verified(app: &tauri::AppHandle, work: &WorkItem) -> Result<(), String> {
    mutate_segment(app, work, |_, segment| {
        segment.state = BilibiliSegmentState::RemoteVerified;
        segment.retry_count = 0;
        segment.next_retry_at = 0;
        segment.last_error = None;
        Ok(())
    })
    .await
}

async fn mark_submission_uncertain(
    app: &tauri::AppHandle,
    work: &WorkItem,
    error: String,
) -> Result<(), String> {
    mutate_segment(app, work, |session, segment| {
        session.submission_state = "UNCERTAIN".to_string();
        segment.state = BilibiliSegmentState::SubmissionUncertain;
        segment.last_error = Some(error);
        segment.next_retry_at = 0;
        Ok(())
    })
    .await
}

async fn mark_conflict(app: &tauri::AppHandle, work: &WorkItem, error: String) -> Result<(), String> {
    mutate_segment(app, work, |_, segment| {
        segment.state = BilibiliSegmentState::Conflict;
        segment.last_error = Some(error);
        segment.next_retry_at = 0;
        Ok(())
    })
    .await
}

async fn cleanup_verified_files(app: &tauri::AppHandle) -> Result<(), String> {
    let store = snapshot(app).await?;
    if !store.settings.delete_after_success {
        return Ok(());
    }
    for session in store.sessions {
        for segment in session.segments {
            if segment.state != BilibiliSegmentState::RemoteVerified || segment.local_deleted {
                continue;
            }
            match fs::remove_file(&segment.local_path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    eprintln!(
                        "[bilibili] remote P{} verified but local delete failed; will retry: {error}",
                        segment.segment_index
                    );
                    continue;
                }
            }
            let session_id = session.live_session_id.clone();
            let segment_index = segment.segment_index;
            mutate_store(app, |store| {
                if let Some(segment) = store
                    .sessions
                    .iter_mut()
                    .find(|item| item.live_session_id == session_id)
                    .and_then(|item| {
                        item.segments
                            .iter_mut()
                            .find(|item| item.segment_index == segment_index)
                    })
                {
                    // RemoteVerified was persisted before this file was removed. This second write
                    // merely records cleanup; failure here can only cause another harmless delete.
                    segment.local_deleted = true;
                }
                Ok(())
            })
            .await?;
        }
    }
    Ok(())
}

async fn finalize_completed_session_titles(app: &tauri::AppHandle) -> Result<(), String> {
    let store = snapshot(app).await?;
    if !store.settings.auto_upload {
        return Ok(());
    }
    let Some(session) = store.sessions.into_iter().find(|session| {
        session.recording_complete
            && session.aid.is_some()
            && session.submission_state == "ACTIVE"
            && !session.segments.is_empty()
            && session
                .segments
                .iter()
                .all(|segment| segment.state == BilibiliSegmentState::RemoteVerified)
    }) else {
        return Ok(());
    };

    let login = load_valid_login(app).await?;
    let aid = session.aid.expect("checked above");
    let bili = bilibili_from_info(login, None)
        .map_err(|error| format!("创建 B站标题更新客户端失败: {error}"))?;
    let mut studio = bili
        .studio_data(&Vid::Aid(aid), None)
        .await
        .map_err(|error| format!("读取 B站最终稿件失败: {error}"))?;
    studio.title = session_title(&session, session.session_ended_at);
    studio.aid = Some(aid);
    bili.edit_by_app(&studio, None)
        .await
        .map_err(|error| format!("更新 B站最终标题失败: {error}"))?;
    let session_id = session.live_session_id;
    mutate_store(app, |store| {
        if let Some(session) = store
            .sessions
            .iter_mut()
            .find(|session| session.live_session_id == session_id)
        {
            session.submission_state = "FINALIZED".to_string();
        }
        Ok(())
    })
    .await
}

fn part_title(segment: &BilibiliSegmentTask) -> String {
    format!(
        "{}-{}",
        local_time(segment.started_at, "%H:%M"),
        local_time(segment.ended_at, "%H:%M")
    )
}

fn session_title(session: &BilibiliSessionTask, end: Option<i64>) -> String {
    let end = end
        .or(session.session_ended_at)
        .or_else(|| session.segments.iter().map(|segment| segment.ended_at).max())
        .unwrap_or(session.session_started_at);
    format!(
        "{}｜{}｜{}-{}",
        session.streamer_name,
        local_time(session.session_started_at, "%Y-%m-%d"),
        local_time(session.session_started_at, "%H:%M"),
        local_time(end, "%H:%M")
    )
}

fn local_time(timestamp: i64, format: &str) -> String {
    Local
        .timestamp_opt(timestamp, 0)
        .single()
        .map(|value| value.format(format).to_string())
        .unwrap_or_else(|| timestamp.to_string())
}

fn upload_backoff(attempt: u32) -> i64 {
    let shift = attempt.saturating_sub(1).min(7);
    (30_i64.saturating_mul(1_i64 << shift)).min(3600)
}

fn verification_backoff(attempt: u32) -> i64 {
    let shift = attempt.saturating_sub(1).min(5);
    (5_i64.saturating_mul(1_i64 << shift)).min(300)
}
