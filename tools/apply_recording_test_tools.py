from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def replace_once(path: str, old: str, new: str) -> None:
    file = ROOT / path
    text = file.read_text(encoding="utf-8")
    if old not in text:
        raise SystemExit(f"pattern not found in {path}: {old[:160]!r}")
    file.write_text(text.replace(old, new, 1), encoding="utf-8")


# Runtime recording metrics kept on each Worker.
replace_once(
    "crates/biliup-cli/src/server/infrastructure/context.rs",
    "use std::time::Duration;",
    "use std::time::{Duration, SystemTime};",
)

replace_once(
    "crates/biliup-cli/src/server/infrastructure/context.rs",
    """    pub upload_streamer: Option<UploadStreamer>,\n    config: Arc<RwLock<Config>>,\n    pub client: StatelessClient,\n}""",
    """    pub upload_streamer: Option<UploadStreamer>,\n    config: Arc<RwLock<Config>>,\n    pub client: StatelessClient,\n    recording_started_at: RwLock<Option<SystemTime>>,\n    recording_filename_prefix: RwLock<Option<String>>,\n}""",
)

replace_once(
    "crates/biliup-cli/src/server/infrastructure/context.rs",
    """            upload_streamer,\n            config,\n            client,\n        }""",
    """            upload_streamer,\n            config,\n            client,\n            recording_started_at: RwLock::new(None),\n            recording_filename_prefix: RwLock::new(None),\n        }""",
)

replace_once(
    "crates/biliup-cli/src/server/infrastructure/context.rs",
    """    pub fn get_upload_config(&self) -> &Option<UploadStreamer> {\n        &self.upload_streamer\n    }\n""",
    """    pub fn mark_recording_started(&self, filename_template: String) {\n        let prefix = filename_template\n            .split('%')\n            .next()\n            .unwrap_or(&filename_template)\n            .to_string();\n        *self.recording_started_at.write().unwrap() = Some(SystemTime::now());\n        *self.recording_filename_prefix.write().unwrap() = Some(prefix);\n    }\n\n    pub fn mark_recording_finished(&self) {\n        *self.recording_started_at.write().unwrap() = None;\n        *self.recording_filename_prefix.write().unwrap() = None;\n    }\n\n    pub fn recording_elapsed_seconds(&self) -> Option<u64> {\n        let started = *self.recording_started_at.read().unwrap();\n        started.and_then(|value| value.elapsed().ok().map(|elapsed| elapsed.as_secs()))\n    }\n\n    pub fn recording_local_bytes(&self) -> Option<u64> {\n        let started = *self.recording_started_at.read().unwrap();\n        let prefix = self.recording_filename_prefix.read().unwrap().clone();\n        let (Some(started), Some(prefix)) = (started, prefix) else {\n            return None;\n        };\n        if prefix.is_empty() {\n            return Some(0);\n        }\n        Some(sum_current_recording_bytes(\n            &default_recording_output_dir(),\n            &prefix,\n            started,\n        ))\n    }\n\n    pub fn get_upload_config(&self) -> &Option<UploadStreamer> {\n        &self.upload_streamer\n    }\n""",
)

replace_once(
    "crates/biliup-cli/src/server/infrastructure/context.rs",
    """fn ensure_directory(path: PathBuf) -> PathBuf {\n    if let Err(error) = std::fs::create_dir_all(&path) {\n        warn!(directory = ?path, error = ?error, \"failed to create recording directory; using current directory\");\n        PathBuf::from(\".\")\n    } else {\n        path\n    }\n}\n\n#[derive(Debug)]\npub struct Worker {""",
    """fn ensure_directory(path: PathBuf) -> PathBuf {\n    if let Err(error) = std::fs::create_dir_all(&path) {\n        warn!(directory = ?path, error = ?error, \"failed to create recording directory; using current directory\");\n        PathBuf::from(\".\")\n    } else {\n        path\n    }\n}\n\nfn sum_current_recording_bytes(root: &Path, prefix: &str, started: SystemTime) -> u64 {\n    let cutoff = started\n        .checked_sub(Duration::from_secs(5))\n        .unwrap_or(started);\n    let mut total = 0u64;\n    let mut stack = vec![(root.to_path_buf(), 0usize)];\n\n    while let Some((directory, depth)) = stack.pop() {\n        let Ok(entries) = std::fs::read_dir(&directory) else {\n            continue;\n        };\n        for entry in entries.flatten() {\n            let path = entry.path();\n            let Ok(metadata) = entry.metadata() else {\n                continue;\n            };\n            if metadata.is_dir() {\n                let name = entry.file_name();\n                let name = name.to_string_lossy();\n                if (depth == 0 && name == \".live-replay-queue\") || (depth > 0 && depth < 4) {\n                    stack.push((path, depth + 1));\n                }\n                continue;\n            }\n\n            let name = entry.file_name();\n            let name = name.to_string_lossy();\n            if !name.contains(prefix) {\n                continue;\n            }\n            if metadata.modified().is_ok_and(|modified| modified < cutoff) {\n                continue;\n            }\n            total = total.saturating_add(metadata.len());\n        }\n    }\n    total\n}\n\n#[derive(Debug)]\npub struct Worker {""",
)

# Start/stop the runtime metrics with the actual recording workflow.
replace_once(
    "crates/biliup-cli/src/server/common/download.rs",
    """    let task = Arc::new(DownloadTask::new(downloader_runtime(\n        ctx.config().downloader,\n        ctx.live_stream(),\n    )));\n    ctx.change_status(Stage::Download, WorkerStatus::Working(task.clone()))\n        .await;\n""",
    """    let task = Arc::new(DownloadTask::new(downloader_runtime(\n        ctx.config().downloader,\n        ctx.live_stream(),\n    )));\n    let recording_template = ctx.recorder(ctx.streamer_info().clone()).filename_template();\n    ctx.worker().mark_recording_started(recording_template);\n    ctx.change_status(Stage::Download, WorkerStatus::Working(task.clone()))\n        .await;\n""",
)

replace_once(
    "crates/biliup-cli/src/server/common/download.rs",
    """    let _ = task.execute(&ctx, sender, downloader, rooms_handle).await;\n    process(&[], &ctx.live_streamer().downloaded_processor).await;\n""",
    """    let _ = task.execute(&ctx, sender, downloader, rooms_handle).await;\n    ctx.worker().mark_recording_finished();\n    process(&[], &ctx.live_streamer().downloaded_processor).await;\n""",
)

# Return the runtime metrics in /v1/streamers.
replace_once(
    "crates/biliup-cli/src/server/infrastructure/dto.rs",
    """    /// 上传状态\n    pub upload_status: String,\n}""",
    """    /// 上传状态\n    pub upload_status: String,\n    /// 当前连续录制时长（秒），只有正在录制时存在\n    pub recording_elapsed_seconds: Option<u64>,\n    /// 当前这场录制仍占用的本地空间（含当前文件与安全队列）\n    pub recording_bytes: Option<u64>,\n}""",
)

replace_once(
    "crates/biliup-cli/src/server/api/endpoints.rs",
    """        results.push(LiveStreamerResponse {\n            status,\n            inner: x,\n            upload_status: option\n                .map(|t| format!(\"{:?}\", *t.uploader_status.read().unwrap()))\n                .unwrap_or_default(),\n        });\n""",
    """        let is_recording = status == \"Working\";\n        let recording_elapsed_seconds = option\n            .as_ref()\n            .filter(|_| is_recording)\n            .and_then(|worker| worker.recording_elapsed_seconds());\n        let recording_bytes = option\n            .as_ref()\n            .filter(|_| is_recording)\n            .and_then(|worker| worker.recording_local_bytes());\n        let upload_status = option\n            .as_ref()\n            .map(|worker| format!(\"{:?}\", *worker.uploader_status.read().unwrap()))\n            .unwrap_or_default();\n\n        results.push(LiveStreamerResponse {\n            status,\n            inner: x,\n            upload_status,\n            recording_elapsed_seconds,\n            recording_bytes,\n        });\n""",
)

# Safe test cut: finalize the active segment, enqueue it, then keep monitoring enabled.
insert_after = """pub async fn pause_streamers_endpoint(\n    State(service_register): State<ServiceRegister>,\n    State(managers): State<Arc<DownloadManager>>,\n    State(pool): State<ConnectionPool>,\n    Path(id): Path<i64>,\n) -> Result<Json<()>, Response> {"""
end_marker = """\n    Ok(Json(()))\n}\n\npub async fn get_configuration("""
endpoints = ROOT / "crates/biliup-cli/src/server/api/endpoints.rs"
text = endpoints.read_text(encoding="utf-8")
start = text.find(insert_after)
if start < 0:
    raise SystemExit("pause endpoint not found")
end = text.find(end_marker, start)
if end < 0:
    raise SystemExit("pause endpoint end not found")
end += len("\n    Ok(Json(()))\n}\n")
addition = r'''

pub async fn upload_now_streamer_endpoint(
    State(managers): State<Arc<DownloadManager>>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, Response> {
    let worker = managers
        .get_room_by_id(id)
        .await
        .ok_or_else(|| (StatusCode::NOT_FOUND, "主播不存在").into_response())?;
    let is_working = {
        let status = worker.downloader_status.read().unwrap();
        matches!(&*status, WorkerStatus::Working(_))
    };
    if !is_working {
        return Err((StatusCode::CONFLICT, "当前主播没有正在录制的分段").into_response());
    }

    info!(id, "manual test upload requested; finalizing current segment");
    // 从 Working 切到 Idle 会调用 DownloadTask::stop() 并等待当前文件安全封段。
    // 下载流程结束时会自动把完整分段交给 Live Replay 安全上传队列并重新放回监控队列。
    worker
        .change_status(Stage::Download, WorkerStatus::Idle)
        .await;
    managers.wake_waker(id).await;

    Ok(Json(json!({
        "message": "当前录像已安全封段并进入自动上传队列；主播仍保持开启，会继续录制"
    })))
}
'''
text = text[:end] + addition + text[end:]
endpoints.write_text(text, encoding="utf-8")

replace_once(
    "crates/biliup-cli/src/server/router.rs",
    """    login_by_qrcode, pause_streamers_endpoint, post_simple_streamer_endpoint,\n    post_streamers_endpoint, post_uploads, put_configuration, put_streamers_endpoint,\n""",
    """    login_by_qrcode, pause_streamers_endpoint, post_simple_streamer_endpoint,\n    post_streamers_endpoint, post_uploads, put_configuration, put_streamers_endpoint,\n    upload_now_streamer_endpoint,\n""",
)

replace_once(
    "crates/biliup-cli/src/server/router.rs",
    """        .route(\"/v1/streamers/{id}/pause\", put(pause_streamers_endpoint))\n""",
    """        .route(\"/v1/streamers/{id}/pause\", put(pause_streamers_endpoint))\n        .route(\"/v1/streamers/{id}/upload-now\", post(upload_now_streamer_endpoint))\n""",
)

# Prefer a bundled ffprobe next to live-replay.exe, then fall back to PATH for development.
replace_once(
    "crates/biliup-cli/src/server/common/replay.rs",
    """async fn validate_media_file(path: &Path) -> AppResult<()> {\n    let output = Command::new(\"ffprobe\")\n""",
    """fn ffprobe_program() -> PathBuf {\n    if let Ok(exe) = std::env::current_exe()\n        && let Some(directory) = exe.parent()\n    {\n        let bundled = directory.join(if cfg!(windows) { \"ffprobe.exe\" } else { \"ffprobe\" });\n        if bundled.exists() {\n            return bundled;\n        }\n    }\n    PathBuf::from(if cfg!(windows) { \"ffprobe.exe\" } else { \"ffprobe\" })\n}\n\nasync fn validate_media_file(path: &Path) -> AppResult<()> {\n    let probe = ffprobe_program();\n    let output = Command::new(&probe)\n""",
)

replace_once(
    "crates/biliup-cli/src/server/common/replay.rs",
    """        .change_context(AppError::Custom(\n            \"无法启动 ffprobe；录像已保留并等待重试\".to_string(),\n        ))?;\n""",
    """        .change_context(AppError::Custom(format!(\n            \"无法启动 ffprobe（{}）；录像已保留并等待重试\",\n            probe.display()\n        )))?;\n""",
)

# Frontend API fields.
replace_once(
    "app/lib/api-streamer.ts",
    """\tupload_status?: string;\n\tstatusTag?: React.ReactNode;\n""",
    """\tupload_status?: string;\n\trecording_elapsed_seconds?: number;\n\trecording_bytes?: number;\n\tstatusTag?: React.ReactNode;\n""",
)

# Poll runtime recording stats every second.
replace_once(
    "app/lib/use-streamers.ts",
    """  const { data, error, isLoading } = useSWR<LiveStreamerEntity[]>(\"/v1/streamers\", fetcher);\n""",
    """  const { data, error, isLoading } = useSWR<LiveStreamerEntity[]>(\"/v1/streamers\", fetcher, { refreshInterval: 1000 });\n""",
)

# Recording management UI: metrics and test upload button.
streamers = ROOT / "app/(app)/streamers/page.tsx"
text = streamers.read_text(encoding="utf-8")
text = text.replace(
    "import React, { useState } from 'react'",
    "import React, { useState } from 'react'\nimport { useSWRConfig } from 'swr'",
    1,
)
text = text.replace(
    "import { LiveStreamerEntity, put, requestDelete, sendRequest } from '../../lib/api-streamer'",
    "import { API_BASE, LiveStreamerEntity, put, requestDelete, sendRequest } from '../../lib/api-streamer'",
    1,
)
needle = """export default function Home() {\n  const { Header, Content } = Layout\n  const { Text } = Typography\n  const { streamers, isLoading } = useStreamers()\n"""
replacement = """const formatDuration = (seconds?: number) => {\n  const value = Math.max(0, Math.floor(seconds ?? 0))\n  const h = Math.floor(value / 3600)\n  const m = Math.floor((value % 3600) / 60)\n  const s = value % 60\n  return [h, m, s].map(item => String(item).padStart(2, '0')).join(':')\n}\n\nconst formatBytes = (bytes?: number) => {\n  const value = Math.max(0, bytes ?? 0)\n  if (value < 1024) return `${value} B`\n  const units = ['KB', 'MB', 'GB', 'TB']\n  let current = value / 1024\n  let index = 0\n  while (current >= 1024 && index < units.length - 1) {\n    current /= 1024\n    index += 1\n  }\n  return `${current.toFixed(index >= 2 ? 2 : 1)} ${units[index]}`\n}\n\nexport default function Home() {\n  const { Header, Content } = Layout\n  const { Text } = Typography\n  const { streamers, isLoading } = useStreamers()\n  const { mutate } = useSWRConfig()\n  const [forcingUpload, setForcingUpload] = useState<Set<number>>(new Set())\n"""
if needle not in text:
    raise SystemExit("streamers home header not found")
text = text.replace(needle, replacement, 1)

needle = """  const onConfirm = async (id: number) => {\n    await deleteStreamers(id)\n  }\n"""
replacement = """  const onConfirm = async (id: number) => {\n    await deleteStreamers(id)\n  }\n\n  const uploadNow = async (id: number) => {\n    setForcingUpload(previous => new Set(previous).add(id))\n    try {\n      const response = await fetch(`${API_BASE}/v1/streamers/${id}/upload-now`, { method: 'POST' })\n      const payload = await response.json().catch(() => ({}))\n      if (!response.ok) throw new Error(payload?.message ?? payload?.error ?? '立即上传失败')\n      Notification.success({\n        title: '已开始立即上传',\n        content: payload?.message ?? '当前分段已封存并进入自动上传队列。',\n      })\n      await mutate('/v1/streamers')\n      await mutate('/v1/replay/sessions')\n      await mutate('/v1/replay/jobs')\n    } catch (error: any) {\n      Notification.error({ title: '立即上传失败', content: error?.message ?? String(error), duration: 0 })\n    } finally {\n      setForcingUpload(previous => {\n        const next = new Set(previous)\n        next.delete(id)\n        return next\n      })\n    }\n  }\n"""
if needle not in text:
    raise SystemExit("onConfirm block not found")
text = text.replace(needle, replacement, 1)

text = text.replace(
    """    delete values.upload_status\n""",
    """    delete values.upload_status\n    delete values.recording_elapsed_seconds\n    delete values.recording_bytes\n""",
    1,
)

needle = """                  <Text style={{ width: '101%' }} ellipsis={{ showTooltip: true }} type=\"tertiary\">\n                    {item.url}\n                  </Text>\n\n                  <div\n"""
replacement = """                  <Text style={{ width: '101%' }} ellipsis={{ showTooltip: true }} type=\"tertiary\">\n                    {item.url}\n                  </Text>\n\n                  {item.status === 'Working' && (\n                    <div style={{ marginTop: 14, display: 'flex', gap: 16, alignItems: 'center', flexWrap: 'wrap' }}>\n                      <Text strong>已录制 {formatDuration(item.recording_elapsed_seconds)}</Text>\n                      <Text>本地占用 {formatBytes(item.recording_bytes)}</Text>\n                      <Button\n                        size=\"small\"\n                        theme=\"solid\"\n                        icon={<IconUpload />}\n                        loading={forcingUpload.has(item.id)}\n                        onClick={() => uploadNow(item.id)}\n                      >\n                        立即上传（测试）\n                      </Button>\n                    </div>\n                  )}\n\n                  <div\n"""
if needle not in text:
    raise SystemExit("streamer card url block not found")
text = text.replace(needle, replacement, 1)
streamers.write_text(text, encoding="utf-8")

# Full portable package now includes a verified ffprobe.exe.
full = ROOT / ".github/workflows/full.yml"
text = full.read_text(encoding="utf-8")
needle = """      - name: Assemble portable package\n        shell: pwsh\n        run: |\n          New-Item -ItemType Directory -Force -Path dist/live-replay | Out-Null\n          Copy-Item target/release/biliup.exe dist/live-replay/live-replay.exe\n"""
replacement = """      - name: Download bundled ffprobe\n        shell: pwsh\n        run: |\n          $zip = Join-Path $PWD 'dist/ffmpeg-release-essentials.zip'\n          $extract = Join-Path $PWD 'dist/ffmpeg-tools'\n          New-Item -ItemType Directory -Force -Path dist | Out-Null\n          Invoke-WebRequest 'https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip' -OutFile $zip\n          Expand-Archive -Path $zip -DestinationPath $extract -Force\n          $ffprobe = Get-ChildItem -Path $extract -Recurse -Filter 'ffprobe.exe' | Select-Object -First 1\n          if (-not $ffprobe) { throw 'ffprobe.exe was not found in the downloaded FFmpeg package' }\n          Copy-Item $ffprobe.FullName 'dist/ffprobe.exe'\n\n      - name: Assemble portable package\n        shell: pwsh\n        run: |\n          New-Item -ItemType Directory -Force -Path dist/live-replay | Out-Null\n          Copy-Item target/release/biliup.exe dist/live-replay/live-replay.exe\n          Copy-Item dist/ffprobe.exe dist/live-replay/ffprobe.exe\n"""
if needle not in text:
    raise SystemExit("full assemble block not found")
text = text.replace(needle, replacement, 1)
needle = """          Copy-Item public/config.toml dist/live-replay/config.example.toml\n\n      - name: Upload Windows package\n"""
replacement = """          Copy-Item public/config.toml dist/live-replay/config.example.toml\n          @'\nFFmpeg/ffprobe is distributed with this portable package for media validation.\nBuild source: https://www.gyan.dev/ffmpeg/builds/\nFFmpeg project: https://ffmpeg.org/\n'@ | Set-Content -Encoding UTF8 dist/live-replay/FFMPEG-NOTICE.txt\n\n      - name: Smoke test bundled ffprobe\n        shell: pwsh\n        run: |\n          & 'dist/live-replay/ffprobe.exe' -version\n          if ($LASTEXITCODE -ne 0) { throw 'bundled ffprobe.exe failed to start' }\n\n      - name: Upload Windows package\n"""
if needle not in text:
    raise SystemExit("full upload block not found")
full.write_text(text.replace(needle, replacement, 1), encoding="utf-8")
