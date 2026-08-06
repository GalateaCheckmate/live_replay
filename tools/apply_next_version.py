from pathlib import Path

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
        raise RuntimeError(f"missing patch anchor in {path}: {old[:120]!r}")
    write(path, text.replace(old, new, 1))


# 1. Persist the single per-streamer master switch.
write(
    "crates/biliup-cli/migrations/5_simple_streamer.sql",
    """ALTER TABLE livestreamers ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1;
CREATE INDEX IF NOT EXISTS idx_livestreamers_enabled ON livestreamers(enabled);
""",
)

replace_once(
    "crates/biliup-cli/src/server/infrastructure/models/live_streamer.rs",
    "use serde_json::Value;\n",
    "use serde_json::Value;\n\nfn default_enabled() -> bool {\n    true\n}\n",
)
replace_once(
    "crates/biliup-cli/src/server/infrastructure/models/live_streamer.rs",
    "    /// 直播间URL\n    pub url: String,\n",
    "    /// 直播间URL\n    pub url: String,\n    /// 主开关：开启后持续监控、自动录制并自动上传\n    #[serde(default = \"default_enabled\")]\n    pub enabled: bool,\n",
)
replace_once(
    "crates/biliup-cli/src/server/infrastructure/models/live_streamer.rs",
    "pub struct InsertLiveStreamer {\n    pub url: String,\n",
    "pub struct InsertLiveStreamer {\n    pub url: String,\n    #[serde(default = \"default_enabled\")]\n    pub enabled: bool,\n",
)

# 2. User-confirmed global defaults.
replace_once(
    "crates/biliup-cli/src/server/infrastructure/context.rs",
    'const DEFAULT_DISK_WARNING_GB: u64 = 100;\nconst DEFAULT_DISK_STOP_GB: u64 = 30;',
    'const DEFAULT_DISK_WARNING_GB: u64 = 30;\nconst DEFAULT_DISK_STOP_GB: u64 = 10;',
)
replace_once(
    "crates/biliup-cli/src/server/config.rs",
    "/// 默认延迟：300秒\nfn default_delay() -> u64 {\n    300\n}",
    "/// 默认断流合并窗口：600秒（10分钟）\nfn default_delay() -> u64 {\n    600\n}",
)
replace_once(
    "crates/biliup-cli/src/server/config.rs",
    'impl Default for Config {\n    fn default() -> Self {\n        serde_json::from_value(serde_json::json!({})).expect("default config should deserialize")\n    }\n}',
    'impl Default for Config {\n    fn default() -> Self {\n        serde_json::from_value(serde_json::json!({\n            "segment_time": "01:00:00",\n            "delay": 600,\n            "douyu_danmaku": false,\n            "huya_danmaku": false,\n            "douyin_danmaku": false,\n            "bilibili_danmaku": false,\n            "bilibili_danmaku_detail": false,\n            "bilibili_danmaku_raw": false,\n            "youtube_danmaku": false,\n            "ytb_danmaku": false,\n            "twitch_danmaku": false\n        })).expect("default config should deserialize")\n    }\n}',
)

# 3. Adaptive upload bandwidth: high normal ceiling, immediate downlink priority on stalls.
replace_once(
    "crates/biliup/src/uploader/line.rs",
    "use std::sync::atomic::{AtomicUsize, Ordering};\nuse std::time::{Duration, Instant};",
    "use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};\nuse std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};",
)
replace_once(
    "crates/biliup/src/uploader/line.rs",
    "static ACTIVE_RECORDINGS: AtomicUsize = AtomicUsize::new(0);\n",
    """static ACTIVE_RECORDINGS: AtomicUsize = AtomicUsize::new(0);
static DOWNLOAD_PRESSURE_UNTIL_MS: AtomicU64 = AtomicU64::new(0);
static HEALTHY_DOWNLOAD_CHUNKS: AtomicUsize = AtomicUsize::new(0);

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 下载流每收到一个网络块就报告一次。连续恢复后自动解除降速。
pub fn report_download_progress(bytes: usize) {
    if bytes == 0 {
        return;
    }
    let healthy = HEALTHY_DOWNLOAD_CHUNKS.fetch_add(1, Ordering::Relaxed) + 1;
    if healthy >= 8 {
        DOWNLOAD_PRESSURE_UNTIL_MS.store(0, Ordering::Relaxed);
        HEALTHY_DOWNLOAD_CHUNKS.store(8, Ordering::Relaxed);
    }
}

/// 下载超时或断流时，立即压低全进程上传速度，把带宽优先让给直播下行。
pub fn report_download_pressure() {
    HEALTHY_DOWNLOAD_CHUNKS.store(0, Ordering::Relaxed);
    DOWNLOAD_PRESSURE_UNTIL_MS.store(unix_millis().saturating_add(15_000), Ordering::Relaxed);
}

fn download_under_pressure() -> bool {
    let until = DOWNLOAD_PRESSURE_UNTIL_MS.load(Ordering::Relaxed);
    until != 0 && unix_millis() < until
}
""",
)
replace_once(
    "crates/biliup/src/uploader/line.rs",
    """fn configured_rate_bytes_per_second() -> Option<u64> {
    let (key, default_mbps) = if is_recording_active() {
        (\"LIVE_REPLAY_RECORDING_UPLOAD_LIMIT_MBPS\", 20.0)
    } else {
        (\"LIVE_REPLAY_UPLOAD_LIMIT_MBPS\", 0.0)
    };
""",
    """fn configured_rate_bytes_per_second() -> Option<u64> {
    let (key, default_mbps) = if is_recording_active() && download_under_pressure() {
        (\"LIVE_REPLAY_PRESSURE_UPLOAD_LIMIT_MBPS\", 5.0)
    } else if is_recording_active() {
        (\"LIVE_REPLAY_RECORDING_UPLOAD_LIMIT_MBPS\", 100.0)
    } else {
        (\"LIVE_REPLAY_UPLOAD_LIMIT_MBPS\", 0.0)
    };
""",
)
replace_once(
    "crates/biliup/src/uploader/line.rs",
    "/// 所有上传文件和所有并发分片共享一个时间表，因此 20 Mbps 是进程总限速，\n/// 不会因三个并发分片膨胀成 60 Mbps。",
    "/// 所有上传文件和并发分片共享总带宽：录制正常时默认最高100 Mbps；\n/// 一旦直播下行出现超时，立即降到5 Mbps，连续恢复后自动升回。",
)
replace_once(
    "crates/biliup/src/downloader/httpflv.rs",
    """            match timeout(Duration::from_secs(30), self.resp.chunk()).await? {
                Ok(Some(chunk)) => {
                    // let n = chunk.len();
                    // println!(\"Chunk: {:?}\", chunk);
                    self.buffer.put(chunk);
                    // self.buffer.put_slice(&buf[..n]);
                }
                _ => {
                    return Ok(self.buffer.split().freeze());
                }
            }
""",
    """            let chunk_result = timeout(Duration::from_secs(30), self.resp.chunk()).await;
            if chunk_result.is_err() {
                crate::uploader::line::report_download_pressure();
            }
            match chunk_result? {
                Ok(Some(chunk)) => {
                    crate::uploader::line::report_download_progress(chunk.len());
                    self.buffer.put(chunk);
                }
                _ => {
                    crate::uploader::line::report_download_pressure();
                    return Ok(self.buffer.split().freeze());
                }
            }
""",
)

# 4. Wait until Bilibili can actually return a playable stream before deleting locally.
replace_once(
    "crates/biliup-cli/src/server/common/replay.rs",
    "const VERIFY_ATTEMPTS: usize = 30;",
    "const VERIFY_ATTEMPTS: usize = 180;",
)
replace_once(
    "crates/biliup-cli/src/server/common/replay.rs",
    """    let reconnect_window = env_u64(
        \"LIVE_REPLAY_RECONNECT_WINDOW_SECONDS\",
        ctx.config().delay.max(1),
    );
""",
    """    let reconnect_window = env_u64(\"LIVE_REPLAY_RECONNECT_WINDOW_SECONDS\", 600);
""",
)
replace_once(
    "crates/biliup-cli/src/server/common/replay.rs",
    '    let delete_after_success = env_bool("LIVE_REPLAY_DELETE_AFTER_SUCCESS", false);\n    let preserve_danmaku = env_bool("LIVE_REPLAY_PRESERVE_DANMAKU", true);',
    '    let delete_after_success = env_bool("LIVE_REPLAY_DELETE_AFTER_SUCCESS", true);\n    let preserve_danmaku = env_bool("LIVE_REPLAY_PRESERVE_DANMAKU", false);',
)
replace_once(
    "crates/biliup-cli/src/server/common/replay.rs",
    """    let duration = part
        .get(\"duration\")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    if duration > 0 {
        Ok(RemotePartState::MatchingReady)
    } else {
        Ok(RemotePartState::MatchingProcessing)
    }
}

async fn wait_for_remote_ready(
""",
    """    let duration = part
        .get(\"duration\")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let cid = part
        .get(\"cid\")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    if duration == 0 || cid == 0 {
        return Ok(RemotePartState::MatchingProcessing);
    }
    if remote_part_playable(bilibili, aid, cid).await? {
        Ok(RemotePartState::MatchingReady)
    } else {
        Ok(RemotePartState::MatchingProcessing)
    }
}

async fn remote_part_playable(bilibili: &BiliBili, aid: u64, cid: u64) -> AppResult<bool> {
    let response = bilibili
        .client
        .get(\"https://api.bilibili.com/x/player/playurl\")
        .query(&[
            (\"avid\", aid.to_string()),
            (\"cid\", cid.to_string()),
            (\"qn\", \"16\".to_string()),
            (\"fnval\", \"16\".to_string()),
        ])
        .send()
        .await
        .change_context(AppError::Unknown)?;
    if !response.status().is_success() {
        return Ok(false);
    }
    let value: serde_json::Value = response.json().await.change_context(AppError::Unknown)?;
    if value.get(\"code\").and_then(|value| value.as_i64()) != Some(0) {
        return Ok(false);
    }
    let data = value.get(\"data\").unwrap_or(&serde_json::Value::Null);
    let has_durl = data
        .get(\"durl\")
        .and_then(|value| value.as_array())
        .is_some_and(|items| !items.is_empty());
    let has_dash = data
        .pointer(\"/dash/video\")
        .and_then(|value| value.as_array())
        .is_some_and(|items| !items.is_empty());
    Ok(has_durl || has_dash)
}

async fn wait_for_remote_ready(
""",
)
replace_once(
    "crates/biliup-cli/src/server/common/replay.rs",
    """    match wait_for_remote_ready(
        &runtime.bilibili,
        aid,
        current.part_number as usize,
        &remote_filename,
    )
""",
    """    mark_remote_processing(ctx.pool(), current.id).await?;
    match wait_for_remote_ready(
        &runtime.bilibili,
        aid,
        current.part_number as usize,
        &remote_filename,
    )
""",
)
replace_once(
    "crates/biliup-cli/src/server/common/replay.rs",
    """async fn mark_remote_verified(pool: &ConnectionPool, segment: &SegmentRecord) -> AppResult<()> {
""",
    """async fn mark_remote_processing(pool: &ConnectionPool, segment_id: i64) -> AppResult<()> {
    let mut tx = pool.begin().await.change_context(AppError::Unknown)?;
    sqlx::query(
        \"UPDATE recording_segments SET status = 'remote_processing', last_error = NULL, updated_at = CURRENT_TIMESTAMP WHERE id = ?\",
    )
    .bind(segment_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    sqlx::query(
        \"UPDATE upload_jobs SET status = 'remote_processing', last_error = NULL, updated_at = CURRENT_TIMESTAMP WHERE segment_id = ?\",
    )
    .bind(segment_id)
    .execute(&mut *tx)
    .await
    .change_context(AppError::Unknown)?;
    tx.commit().await.change_context(AppError::Unknown)?;
    Ok(())
}

async fn mark_remote_verified(pool: &ConnectionPool, segment: &SegmentRecord) -> AppResult<()> {
""",
)

# 5. Backend endpoint that creates the hidden per-streamer upload settings in one action.
replace_once(
    "crates/biliup-cli/src/server/api/endpoints.rs",
    """pub async fn post_streamers_endpoint(
""",
    """#[derive(Deserialize)]
pub struct SimpleStreamerRequest {
    pub url: String,
    pub remark: String,
    pub user_cookie: String,
    pub tid: Option<u16>,
}

pub async fn post_simple_streamer_endpoint(
    State(service_register): State<ServiceRegister>,
    State(managers): State<Arc<DownloadManager>>,
    State(pool): State<ConnectionPool>,
    Json(payload): Json<SimpleStreamerRequest>,
) -> Result<Json<LiveStreamer>, Response> {
    let url = payload.url.trim().to_string();
    let remark = payload.remark.trim().to_string();
    let user_cookie = payload.user_cookie.trim().to_string();
    if url.is_empty() || remark.is_empty() || user_cookie.is_empty() {
        return Err((StatusCode::BAD_REQUEST, \"直播间、主播名称和投稿账号不能为空\").into_response());
    }

    let upload = InsertUploadStreamer {
        id: None,
        template_name: format!(\"live-replay:{}:{}\", remark, Utc::now().timestamp_millis()),
        title: Some(\"{streamer} 直播回放 %Y-%m-%d %H-%M\".to_string()),
        tid: Some(payload.tid.unwrap_or(65)),
        copyright: Some(2),
        copyright_source: Some(url.clone()),
        cover_path: None,
        description: Some(String::new()),
        dynamic: None,
        dtime: None,
        dolby: None,
        hires: None,
        charging_pay: None,
        no_reprint: None,
        uploader: Some(\"biliup-rs\".to_string()),
        user_cookie: Some(user_cookie),
        tags: vec![\"三角洲行动\".to_string(), \"游戏\".to_string()],
        credits: None,
        up_selection_reply: None,
        up_close_reply: None,
        up_close_danmu: None,
        extra_fields: None,
        is_only_self: Some(1),
    }
    .insert(&pool)
    .await
    .change_context(AppError::Unknown)
    .map_err(report_to_response)?;

    let streamer = InsertLiveStreamer {
        url: url.clone(),
        enabled: true,
        remark,
        filename_prefix: Some(\"{streamer}%Y-%m-%dT%H_%M_%S\".to_string()),
        time_range: None,
        upload_streamers_id: Some(upload.id),
        format: None,
        override_cfg: None,
        preprocessor: None,
        segment_processor: None,
        downloaded_processor: None,
        postprocessor: None,
        opt_args: None,
        excluded_keywords: None,
    }
    .insert(&pool)
    .await
    .change_context(AppError::Unknown)
    .map_err(report_to_response)?;

    let upload_config = get_upload_config(&pool, streamer.id)
        .await
        .map_err(report_to_response)?;
    if managers
        .add_room(service_register.worker(streamer.clone(), upload_config))
        .await
        .is_none()
    {
        let _ = del_streamer(&pool, streamer.id).await;
        let _ = sqlx::query(\"DELETE FROM uploadstreamers WHERE id = ?\")
            .bind(upload.id)
            .execute(&pool)
            .await;
        return Err((StatusCode::BAD_REQUEST, \"不支持这个直播间链接\").into_response());
    }

    info!(url, \"created simple Live Replay streamer\");
    Ok(Json(streamer))
}

pub async fn post_streamers_endpoint(
""",
)
replace_once(
    "crates/biliup-cli/src/server/api/endpoints.rs",
    """    let upload_config = get_upload_config(&pool, live_streamers.id)
        .await
        .map_err(report_to_response)?;
    let Some(_) = managers
        .add_room(service_register.worker(live_streamers.clone(), upload_config))
        .await
    else {
        info!(\"not supported url: {}\", url);
        return Err((StatusCode::BAD_REQUEST, \"Not supported url\").into_response());
    };
""",
    """    if live_streamers.enabled {
        let upload_config = get_upload_config(&pool, live_streamers.id)
            .await
            .map_err(report_to_response)?;
        let Some(_) = managers
            .add_room(service_register.worker(live_streamers.clone(), upload_config))
            .await
        else {
            info!(\"not supported url: {}\", url);
            return Err((StatusCode::BAD_REQUEST, \"Not supported url\").into_response());
        };
    }
""",
)
replace_once(
    "crates/biliup-cli/src/server/api/endpoints.rs",
    """    managers
        .add_room(service_register.worker(streamer.clone(), upload_config))
        .await
        .ok_or(AppError::Unknown)
        .map_err(report_to_response)?;
""",
    """    if streamer.enabled {
        managers
            .add_room(service_register.worker(streamer.clone(), upload_config))
            .await
            .ok_or(AppError::Unknown)
            .map_err(report_to_response)?;
    }
""",
)
start = read("crates/biliup-cli/src/server/api/endpoints.rs")
old_pause_start = start.index("pub async fn pause_streamers_endpoint(")
old_pause_end = start.index("\npub async fn get_configuration", old_pause_start)
new_pause = """pub async fn pause_streamers_endpoint(
    State(service_register): State<ServiceRegister>,
    State(managers): State<Arc<DownloadManager>>,
    State(pool): State<ConnectionPool>,
    Path(id): Path<i64>,
) -> Result<Json<()>, Response> {
    let enabled: Option<i64> = sqlx::query_scalar(\"SELECT enabled FROM livestreamers WHERE id = ?\")
        .bind(id)
        .fetch_optional(&pool)
        .await
        .change_context(AppError::Unknown)
        .map_err(report_to_response)?;
    let Some(enabled) = enabled else {
        return Err((StatusCode::NOT_FOUND, \"主播不存在\").into_response());
    };
    let next_enabled = enabled == 0;
    sqlx::query(\"UPDATE livestreamers SET enabled = ? WHERE id = ?\")
        .bind(next_enabled)
        .bind(id)
        .execute(&pool)
        .await
        .change_context(AppError::Unknown)
        .map_err(report_to_response)?;

    if next_enabled {
        if let Some(worker) = managers.get_room_by_id(id).await {
            worker.change_status(Stage::Download, WorkerStatus::Idle).await;
            managers.wake_waker(id).await;
        } else {
            let streamer = get_all_streamer(&pool)
                .await
                .map_err(report_to_response)?
                .into_iter()
                .find(|item| item.id == id)
                .ok_or_else(|| (StatusCode::NOT_FOUND, \"主播不存在\").into_response())?;
            let upload_config = get_upload_config(&pool, id)
                .await
                .map_err(report_to_response)?;
            managers
                .add_room(service_register.worker(streamer, upload_config))
                .await
                .ok_or_else(|| (StatusCode::BAD_REQUEST, \"不支持这个直播间链接\").into_response())?;
        }
        info!(id, \"streamer master switch enabled\");
    } else if let Some(worker) = managers.get_room_by_id(id).await {
        worker.change_status(Stage::Download, WorkerStatus::Pause).await;
        managers.make_waker(id).await;
        info!(id, \"streamer master switch disabled; current segment will be finalized\");
    }

    Ok(Json(()))
}
"""
write(
    "crates/biliup-cli/src/server/api/endpoints.rs",
    start[:old_pause_start] + new_pause + start[old_pause_end:],
)
replace_once(
    "crates/biliup-cli/src/server/api/endpoints.rs",
    """        let status = match option.as_ref() {
            Some(t) => format!(\"{:?}\", *t.downloader_status.read().unwrap()),
            None => String::new(),
        };
""",
    """        let status = if !x.enabled {
            \"Pause\".to_string()
        } else {
            match option.as_ref() {
                Some(t) => format!(\"{:?}\", *t.downloader_status.read().unwrap()),
                None => String::new(),
            }
        };
""",
)

replace_once(
    "crates/biliup-cli/src/server/router.rs",
    """    login_by_qrcode, pause_streamers_endpoint, post_streamers_endpoint, post_uploads,
""",
    """    login_by_qrcode, pause_streamers_endpoint, post_simple_streamer_endpoint,
    post_streamers_endpoint, post_uploads,
""",
)
replace_once(
    "crates/biliup-cli/src/server/router.rs",
    """        .route(\"/v1/streamers/{id}\", delete(delete_streamers_endpoint))
""",
    """        .route(\"/v1/streamers/simple\", post(post_simple_streamer_endpoint))
        .route(\"/v1/streamers/{id}\", delete(delete_streamers_endpoint))
""",
)

# 6. Restore only enabled streamers after restart and create config-imported streamers enabled.
replace_once(
    "crates/biliup-cli/src/lib.rs",
    """    for live_streamer in streamers {
        let upload_config =
""",
    """    for live_streamer in streamers {
        if !live_streamer.enabled {
            continue;
        }
        let upload_config =
""",
)
replace_once(
    "crates/biliup-cli/src/lib.rs",
    """        url: url.to_string(),
        remark: remark.to_string(),
""",
    """        url: url.to_string(),
        enabled: true,
        remark: remark.to_string(),
""",
)

# 7. Frontend types and a real home page for adding/toggling streamers.
replace_once(
    "app/lib/api-streamer.ts",
    """export interface LiveStreamerEntity {
\tid: number;
\turl: string;
""",
    """export interface LiveStreamerEntity {
\tid: number;
\turl: string;
\tenabled: boolean;
\tupload_streamers_id?: number;
""",
)

write(
    "app/(app)/page.tsx",
    r"""'use client'

import { useMemo, useState } from 'react'
import Link from 'next/link'
import { Button, Card, Col, Form, Layout, Modal, Notification, Row, Select, Switch, Tag, Typography } from '@douyinfe/semi-ui'
import { IconPlusCircle, IconRefresh } from '@douyinfe/semi-icons'
import { useSWRConfig } from 'swr'
import useStreamers, { useBiliUsers, useTypeTree } from '../lib/use-streamers'
import { API_BASE } from '../lib/api-streamer'

const statusText: Record<string, string> = {
  Working: '正在录制',
  Pending: '检测直播状态',
  Idle: '等待开播',
  Pause: '已关闭',
}

const statusColor: Record<string, 'red' | 'blue' | 'green' | 'grey'> = {
  Working: 'red',
  Pending: 'blue',
  Idle: 'green',
  Pause: 'grey',
}

export default function Home() {
  const { Header, Content } = Layout
  const { Title, Text } = Typography
  const { streamers, isLoading } = useStreamers()
  const { biliUsers } = useBiliUsers()
  const { typeTree } = useTypeTree()
  const { mutate } = useSWRConfig()
  const [visible, setVisible] = useState(false)
  const [saving, setSaving] = useState(false)
  const [formApi, setFormApi] = useState<any>()

  const defaultTid = useMemo(() => {
    const children = (typeTree ?? []).flatMap((item: any) => item.children ?? [])
    return children.find((item: any) => item.name?.includes('三角洲'))?.id
      ?? children.find((item: any) => item.name === '网络游戏')?.id
      ?? 65
  }, [typeTree])

  const accountOptions = (biliUsers ?? []).map(item => ({ label: item.name, value: item.value }))

  const createStreamer = async () => {
    const values = await formApi?.validate()
    if (!values) return
    setSaving(true)
    try {
      const response = await fetch(`${API_BASE}/v1/streamers/simple`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ...values, tid: defaultTid }),
      })
      if (!response.ok) throw new Error(await response.text())
      Notification.success({ title: '添加成功', content: '已持续关注；开播后会自动录制、上传并在可播放后删除本地视频。' })
      setVisible(false)
      formApi?.reset()
      await mutate('/v1/streamers')
    } catch (error: any) {
      Notification.error({ title: '添加失败', content: error.message })
      throw error
    } finally {
      setSaving(false)
    }
  }

  const toggleStreamer = async (id: number) => {
    const response = await fetch(`${API_BASE}/v1/streamers/${id}/pause`, { method: 'PUT' })
    if (!response.ok) {
      Notification.error({ title: '切换失败', content: await response.text() })
      return
    }
    await mutate('/v1/streamers')
  }

  return (
    <>
      <Header style={{ backgroundColor: 'var(--semi-color-bg-1)', padding: '0 24px' }}>
        <div style={{ height: 64, display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
          <div>
            <Title heading={4}>Live Replay</Title>
            <Text type="tertiary">一个开关完成持续关注、自动录制和自动上传</Text>
          </div>
          <div style={{ display: 'flex', gap: 8 }}>
            <Button icon={<IconRefresh />} onClick={() => mutate('/v1/streamers')}>刷新</Button>
            <Button theme="solid" icon={<IconPlusCircle />} onClick={() => setVisible(true)}>添加主播</Button>
          </div>
        </div>
      </Header>
      <Content style={{ padding: 24, backgroundColor: 'var(--semi-color-bg-0)' }}>
        {!isLoading && (streamers?.length ?? 0) === 0 && (
          <Card style={{ maxWidth: 720, margin: '48px auto', textAlign: 'center' }}>
            <Title heading={4}>还没有关注主播</Title>
            <Text type="tertiary">粘贴直播间链接后，软件会一直等待开播并自动完成后续流程。</Text>
            <div style={{ marginTop: 20 }}><Button theme="solid" onClick={() => setVisible(true)}>添加第一个主播</Button></div>
          </Card>
        )}
        <Row gutter={[16, 16]}>
          {(streamers ?? []).map(streamer => {
            const status = streamer.enabled === false ? 'Pause' : (streamer.status || 'Idle')
            return (
              <Col key={streamer.id} xs={24} sm={24} md={12} lg={8} xl={6}>
                <Card shadows="hover" title={streamer.remark} headerExtraContent={
                  <Switch checked={streamer.enabled !== false} onChange={() => toggleStreamer(streamer.id)} />
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
        </Form>
      </Modal>
    </>
  )
}
""",
)

# 8. Make queue states understandable and show deletion confirmation in the page.
replace_once(
    "app/(app)/replay/page.tsx",
    """  if (['uploading', 'recording', 'uploaded_to_storage'].includes(status)) return 'blue'
  if (['queued', 'recording_complete', 'remote_verified', 'cleanup_pending', 'postprocessing'].includes(status)) return 'cyan'
""",
    """  if (['uploading', 'recording', 'uploaded_to_storage'].includes(status)) return 'blue'
  if (['queued', 'recording_complete', 'remote_processing', 'remote_verified', 'cleanup_pending', 'postprocessing'].includes(status)) return 'cyan'
""",
)
replace_once(
    "app/(app)/replay/page.tsx",
    """const formatBytes = (value: number) => {
""",
    """const statusLabel: Record<string, string> = {
  queued: '等待上传',
  uploading: '上传文件中',
  uploaded_to_storage: '文件已上传',
  submitting: '正在创建投稿',
  remote_processing: 'B站转码/可播放检查中',
  remote_verified: '已可播放',
  cleanup_pending: '等待删除本地文件',
  deleted: '本地文件已删除',
  retained: '本地文件已保留',
  retry_wait: '等待重试',
  conflict: '需要处理冲突',
  submission_uncertain: '投稿结果待确认',
  complete: '已完成',
}

const formatBytes = (value: number) => {
""",
)
replace_once(
    "app/(app)/replay/page.tsx",
    "render: (value: string) => <Tag color={statusColor(value)}>{value}</Tag>,",
    "render: (value: string) => <Tag color={statusColor(value)}>{statusLabel[value] ?? value}</Tag>,",
)
replace_once(
    "app/(app)/replay/page.tsx",
    "render: (value: string) => <Tag color={statusColor(value)}>{value}</Tag>,",
    "render: (value: string) => <Tag color={statusColor(value)}>{statusLabel[value] ?? value}</Tag>,",
)
replace_once(
    "app/(app)/replay/page.tsx",
    """    {
      title: '失败原因',
""",
    """    {
      title: '本地文件',
      width: 170,
      render: (_: unknown, record: ReplayJob) => record.deleted_at
        ? <Tag color=\"green\">已删除 {new Date(`${record.deleted_at}Z`).toLocaleString()}</Tag>
        : <Tag color=\"orange\">仍保留</Tag>,
    },
    {
      title: '失败原因',
""",
)

# Update docs with the new default behavior.
append = """

## 简化版默认行为（2026-08）

- 首页直接添加主播；每个主播只有一个总开关。
- 总开关开启：持续监控、开播自动录制、每60分钟分段并自动上传。
- 总开关关闭：立即安全封段，已录内容继续上传，完成后停止关注。
- 默认标题：`{streamer} 直播回放 %Y-%m-%d %H-%M`。
- 默认投稿：三角洲行动/游戏标签、转载、来源为完整直播间链接、仅自己可见、简介为空。
- 默认不录制弹幕。
- 10分钟内恢复直播视为同一场，超过10分钟创建新稿件。
- 录制正常时上传总上限默认100 Mbps；直播下行超时会自动降至5 Mbps，恢复后自动升回。
- 磁盘低于30GB警告，低于10GB不再开始新录制。
- B站分P必须已经转码并能通过播放接口取得视频流，页面显示“已可播放”后才删除本地视频。
"""
doc = read("LIVE_REPLAY.md")
if "## 简化版默认行为（2026-08）" not in doc:
    write("LIVE_REPLAY.md", doc.rstrip() + append + "\n")
