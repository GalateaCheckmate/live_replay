import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

type Tab = "record" | "upload" | "history" | "settings";

type ProbeResult =
  | { status: "offline" }
  | {
      status: "live";
      stream: {
        name: string;
        title: string;
        platform: string;
        room_url: string;
        stream_url: string;
        suffix: string;
      };
    };

type RecordingStatusItem = {
  room_url: string;
  display_name: string;
  current_file: string;
  started_at: number;
};

type CoreStatus = {
  active: boolean;
  active_count: number;
  recordings: RecordingStatusItem[];
  last_file?: string | null;
  last_error?: string | null;
  available_bytes?: number | null;
  low_space_warning: boolean;
};

type MonitorTarget = {
  id: string;
  url: string;
  name: string;
  enabled: boolean;
  last_state: string;
  last_error?: string | null;
  last_checked_at?: number | null;
};

type MonitorStore = { targets: MonitorTarget[] };

type UploadTask = {
  id: string;
  streamer_name: string;
  local_path: string;
  youtube_title: string;
  file_size: number;
  state: string;
  confirmed_bytes: number;
  youtube_video_id?: string | null;
  attempts: number;
  last_error?: string | null;
  local_deleted: boolean;
  started_at: number;
  ended_at: number;
};

type YoutubeSettings = {
  auto_upload: boolean;
  privacy_status: string;
  delete_after_success: boolean;
  account_label?: string | null;
};

type YoutubeStore = { settings: YoutubeSettings; tasks: UploadTask[] };
type YoutubeStatus = { store: YoutubeStore; authorized: boolean };
type YoutubeAuthResult = { authorized: boolean; access_token?: string | null; account_label?: string | null };

const emptyCore: CoreStatus = {
  active: false,
  active_count: 0,
  recordings: [],
  low_space_warning: false,
  available_bytes: null,
};

const emptyYoutube: YoutubeStatus = {
  authorized: false,
  store: {
    settings: {
      auto_upload: false,
      privacy_status: "private",
      delete_after_success: true,
      account_label: null,
    },
    tasks: [],
  },
};

function withTimeout<T>(promise: Promise<T>, timeoutMs: number, message: string): Promise<T> {
  return Promise.race([
    promise,
    new Promise<T>((_, reject) => window.setTimeout(() => reject(new Error(message)), timeoutMs)),
  ]);
}

function formatBytes(bytes?: number | null) {
  if (!Number.isFinite(bytes) || !bytes || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 10 || unit === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
}

function uploadStateLabel(state: string) {
  const labels: Record<string, string> = {
    RECORDING: "录制中",
    READY_TO_UPLOAD: "等待上传",
    UPLOADING: "上传中",
    WAITING_FOR_NETWORK: "等待网络",
    RETRY_PENDING: "等待重试",
    AUTH_REQUIRED: "需要登录",
    UPLOAD_SUCCESS: "上传成功",
    UPLOAD_RESULT_UNKNOWN: "结果待确认",
  };
  return labels[state] || state;
}

function App() {
  const userAgent = useMemo(() => navigator.userAgent, []);
  const isAndroid = useMemo(() => /Android|HarmonyOS|HUAWEI|ANA-|ALN-/i.test(userAgent), [userAgent]);

  const [tab, setTab] = useState<Tab>("record");
  const [url, setUrl] = useState("");
  const [name, setName] = useState("");
  const [message, setMessage] = useState("正在加载 Android 原生内核...");
  const [probe, setProbe] = useState<ProbeResult | null>(null);
  const [core, setCore] = useState<CoreStatus>(emptyCore);
  const [monitor, setMonitor] = useState<MonitorStore>({ targets: [] });
  const [youtube, setYoutube] = useState<YoutubeStatus>(emptyYoutube);
  const [busy, setBusy] = useState(false);

  async function refreshCoreStatus() {
    try {
      const next = await invoke<CoreStatus>("mobile_recordings_status");
      setCore(next);
      if (next.last_error) setMessage(next.last_error);
      else if (next.active) setMessage(`正在同时录制 ${next.active_count} 路直播`);
      else if (next.low_space_warning) setMessage(`可用存储空间较低：${formatBytes(next.available_bytes)}`);
      else setMessage("后台监控与录制服务已就绪");
    } catch (error) {
      setMessage(String(error));
    }
  }

  async function refreshMonitor() {
    try {
      setMonitor(await invoke<MonitorStore>("mobile_monitor_status"));
    } catch (error) {
      setMessage(String(error));
    }
  }

  async function refreshYoutube() {
    try {
      setYoutube(await invoke<YoutubeStatus>("mobile_youtube_status"));
    } catch (error) {
      setMessage(String(error));
    }
  }

  async function refreshAll() {
    await Promise.allSettled([refreshCoreStatus(), refreshMonitor(), refreshYoutube()]);
  }

  useEffect(() => {
    if (!isAndroid) {
      window.location.replace("http://localhost:19159");
      return;
    }
    refreshAll();
    const timer = window.setInterval(refreshAll, 3000);
    return () => window.clearInterval(timer);
  }, [isAndroid]);

  async function probeStream() {
    if (!url.trim()) {
      setMessage("请输入 B站或抖音直播间地址");
      return;
    }
    setBusy(true);
    setProbe(null);
    setMessage("正在解析直播间...");
    try {
      const result = await withTimeout(
        invoke<ProbeResult>("mobile_probe_stream", {
          url: url.trim(),
          name: name.trim() || null,
          bilibiliCookie: null,
          douyinCookie: null,
        }),
        35_000,
        "直播检测超过 35 秒，已恢复界面。请检查模拟器网络后重试。",
      );
      setProbe(result);
      setMessage(result.status === "live" ? `已解析 ${result.stream.platform}：${result.stream.title || result.stream.name}` : "主播当前未开播");
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function addMonitor() {
    if (!url.trim()) {
      setMessage("请输入直播间地址");
      return;
    }
    setBusy(true);
    try {
      const next = await invoke<MonitorStore>("mobile_monitor_add", { url: url.trim(), name: name.trim() || null });
      setMonitor(next);
      setMessage("已加入监控。检测到开播后会自动开始整场录制。")
      setUrl("");
      setName("");
      setProbe(null);
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function toggleMonitor(target: MonitorTarget) {
    try {
      setMonitor(await invoke<MonitorStore>("mobile_monitor_set_enabled", { targetId: target.id, enabled: !target.enabled }));
    } catch (error) {
      setMessage(String(error));
    }
  }

  async function removeMonitor(target: MonitorTarget) {
    try {
      setMonitor(await invoke<MonitorStore>("mobile_monitor_remove", { targetId: target.id }));
    } catch (error) {
      setMessage(String(error));
    }
  }

  async function stopRecording(roomUrl?: string) {
    setBusy(true);
    setMessage(roomUrl ? "正在停止该路录制并收尾完整录像..." : "正在停止全部录制并收尾完整录像...");
    try {
      setCore(await invoke<CoreStatus>("mobile_stop_recording_multi", { roomUrl: roomUrl || null }));
      window.setTimeout(refreshAll, 1000);
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function authorizeYoutube() {
    setBusy(true);
    try {
      const auth = await invoke<YoutubeAuthResult>("mobile_youtube_authorize");
      if (!auth.authorized) throw new Error("YouTube 授权未完成");
      setMessage(`YouTube 已登录：${auth.account_label || "Google Account"}`);
      await refreshYoutube();
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function logoutYoutube() {
    setBusy(true);
    try {
      await invoke("mobile_youtube_logout");
      setMessage("YouTube 已退出；本地录像和未完成上传任务均会保留。")
      await refreshYoutube();
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function updateYoutubeSettings(autoUpload: boolean, deleteAfterSuccess: boolean) {
    try {
      const store = await invoke<YoutubeStore>("mobile_youtube_set_settings", { autoUpload, deleteAfterSuccess });
      setYoutube((current) => ({ ...current, store }));
    } catch (error) {
      setMessage(String(error));
    }
  }

  async function retryUpload(task: UploadTask) {
    try {
      const store = await invoke<YoutubeStore>("mobile_youtube_retry_task", { taskId: task.id });
      setYoutube((current) => ({ ...current, store }));
      setMessage("已安排重新检查现有 YouTube resumable session。")
    } catch (error) {
      setMessage(String(error));
    }
  }

  if (!isAndroid) return <main className="desktop-bridge">正在连接 Live Replay 本地服务...</main>;

  const pendingUploads = youtube.store.tasks.filter((task) => task.state !== "UPLOAD_SUCCESS").length;
  const successfulUploads = youtube.store.tasks.filter((task) => task.state === "UPLOAD_SUCCESS");

  return (
    <main className="app-shell">
      <header className="topbar">
        <div>
          <span className="brand">LIVE REPLAY</span>
          <h1>{tab === "record" ? "录制" : tab === "upload" ? "上传" : tab === "history" ? "记录" : "设置"}</h1>
        </div>
      </header>

      <section className="runtime-card">
        <div className={`runtime-indicator ${core.active ? "running" : ""}`} />
        <div className="runtime-copy">
          <strong>{core.active ? `正在录制 ${core.active_count} 路` : "服务状态"}</strong>
          <span>{message}</span>
        </div>
        <button className="text-button" type="button" onClick={refreshAll}>刷新</button>
      </section>

      {tab === "record" && (
        <>
          <section className="stats-grid" aria-label="任务概览">
            <article><span>监控主播</span><strong>{monitor.targets.filter((item) => item.enabled).length}</strong></article>
            <article><span>录制中</span><strong>{core.active_count}</strong></article>
            <article><span>待上传</span><strong>{pendingUploads}</strong></article>
          </section>

          <section className="panel recorder-panel">
            <div className="section-heading"><div><h2>添加主播</h2><p>检测到开播后自动整场录制</p></div><span className="status-pill">自动监控</span></div>
            <label className="field"><span>直播间地址</span><input value={url} onChange={(event) => setUrl(event.target.value)} placeholder="B站或抖音直播间地址" /></label>
            <label className="field"><span>主播名称</span><input value={name} onChange={(event) => setName(event.target.value)} placeholder="例如：昊天" /></label>
            <div className="action-row">
              <button type="button" className="secondary-action" onClick={probeStream} disabled={busy}>检测直播</button>
              <button type="button" className="primary-action" onClick={addMonitor} disabled={busy}>加入监控</button>
            </div>
          </section>

          {probe?.status === "live" && <section className="panel result-panel"><div className="section-heading"><div><h2>{probe.stream.title || probe.stream.name}</h2><p>{probe.stream.platform} · {probe.stream.suffix}</p></div><span className="status-pill">已开播</span></div></section>}

          {core.recordings.length > 0 && (
            <section className="panel">
              <div className="section-heading"><div><h2>正在录制</h2><p>每个直播间独立运行</p></div><span className="status-pill">{core.active_count} 路</span></div>
              <div className="task-list">
                {core.recordings.map((recording) => (
                  <article className="task-item" key={recording.room_url}>
                    <div className="task-main"><strong>{recording.display_name}</strong><span>整场连续录制中</span><small>{recording.room_url}</small></div>
                    <button className="mini-button danger" type="button" onClick={() => stopRecording(recording.room_url)}>停止</button>
                  </article>
                ))}
              </div>
            </section>
          )}

          <section className="panel">
            <div className="section-heading"><div><h2>监控列表</h2><p>后台每 20 秒自动检查</p></div></div>
            <div className="task-list">
              {monitor.targets.length === 0 && <p className="empty-text">还没有监控主播。</p>}
              {monitor.targets.map((target) => (
                <article className="task-item" key={target.id}>
                  <div className="task-main"><strong>{target.name}</strong><span>{target.last_error || target.last_state}</span><small>{target.url}</small></div>
                  <div className="task-actions">
                    <button className="mini-button" type="button" onClick={() => toggleMonitor(target)}>{target.enabled ? "暂停" : "启用"}</button>
                    <button className="mini-button danger" type="button" onClick={() => removeMonitor(target)}>删除</button>
                  </div>
                </article>
              ))}
            </div>
          </section>

          <section className="panel compact-panel">
            <div className="section-heading"><div><h2>存储空间</h2><p>{formatBytes(core.available_bytes)} 可用 · 低于 10GB 不启动新录制</p></div><span className="status-pill">{core.low_space_warning ? "注意" : "正常"}</span></div>
          </section>

          {core.active_count > 1 && <button className="danger-action full-action" type="button" onClick={() => stopRecording()} disabled={busy}>停止全部录制</button>}
        </>
      )}

      {tab === "upload" && (
        <section className="panel">
          <div className="section-heading"><div><h2>YouTube 上传队列</h2><p>Private · Resumable Upload</p></div><span className="status-pill">{youtube.authorized ? "已登录" : "未登录"}</span></div>
          <div className="task-list">
            {youtube.store.tasks.length === 0 && <p className="empty-text">暂无上传任务。录像完成后会自动进入这里。</p>}
            {youtube.store.tasks.slice().reverse().map((task) => {
              const percent = task.file_size > 0 ? Math.min(100, Math.round((task.confirmed_bytes / task.file_size) * 100)) : 0;
              return (
                <article className="task-item stacked" key={task.id}>
                  <div className="task-main">
                    <strong>{task.youtube_title}</strong>
                    <span>{uploadStateLabel(task.state)} · {formatBytes(task.confirmed_bytes)} / {formatBytes(task.file_size)} {task.state === "UPLOADING" ? `· ${percent}%` : ""}</span>
                    {task.last_error && <small className="error-text">{task.last_error}</small>}
                    {task.youtube_video_id && <small>videoId: {task.youtube_video_id}</small>}
                  </div>
                  {task.state !== "UPLOAD_SUCCESS" && task.state !== "UPLOAD_RESULT_UNKNOWN" && <button className="mini-button" type="button" onClick={() => retryUpload(task)}>重试</button>}
                </article>
              );
            })}
          </div>
        </section>
      )}

      {tab === "history" && (
        <>
          <section className="panel compact-panel"><div className="section-heading"><div><h2>最近完整录像</h2><p className="path-text">{core.last_file || "暂无"}</p></div></div></section>
          <section className="panel">
            <div className="section-heading"><div><h2>已上传</h2><p>YouTube 已确认成功的录像</p></div><span className="status-pill">{successfulUploads.length}</span></div>
            <div className="task-list">
              {successfulUploads.length === 0 && <p className="empty-text">暂无成功记录。</p>}
              {successfulUploads.slice().reverse().map((task) => <article className="task-item" key={task.id}><div className="task-main"><strong>{task.youtube_title}</strong><span>{task.local_deleted ? "YouTube 成功 · 本地已安全删除" : "YouTube 成功 · 本地保留"}</span>{task.youtube_video_id && <small>videoId: {task.youtube_video_id}</small>}</div></article>)}
            </div>
          </section>
        </>
      )}

      {tab === "settings" && (
        <>
          <section className="panel">
            <div className="section-heading"><div><h2>YouTube</h2><p>{youtube.store.settings.account_label || "尚未授权账号"}</p></div><span className="status-pill">Private</span></div>
            <div className="settings-actions">{!youtube.authorized ? <button className="primary-action" type="button" onClick={authorizeYoutube} disabled={busy}>登录 YouTube</button> : <button className="secondary-action" type="button" onClick={logoutYoutube} disabled={busy}>退出账号</button>}</div>
          </section>
          <section className="panel settings-list">
            <label className="setting-row"><div><strong>自动上传</strong><span>整场录像结束后自动上传 YouTube Private</span></div><input type="checkbox" checked={youtube.store.settings.auto_upload} onChange={(event) => updateYoutubeSettings(event.target.checked, youtube.store.settings.delete_after_success)} /></label>
            <label className="setting-row"><div><strong>上传成功后删除本地</strong><span>仅在 videoId 与成功状态持久化后删除</span></div><input type="checkbox" checked={youtube.store.settings.delete_after_success} onChange={(event) => updateYoutubeSettings(youtube.store.settings.auto_upload, event.target.checked)} /></label>
            <div className="setting-row static"><div><strong>默认可见性</strong><span>第一阶段固定为 Private</span></div><b>Private</b></div>
          </section>
          <section className="panel compact-panel"><div className="section-heading"><div><h2>后台运行</h2><p>Foreground Service + WakeLock · 录制与上传独立运行</p></div><span className="status-pill">已启用</span></div></section>
        </>
      )}

      <nav className="bottom-nav" aria-label="主导航">
        <button className={`nav-item ${tab === "record" ? "active" : ""}`} type="button" onClick={() => setTab("record")}><span>●</span><small>录制</small></button>
        <button className={`nav-item ${tab === "upload" ? "active" : ""}`} type="button" onClick={() => setTab("upload")}><span>⇧</span><small>上传</small></button>
        <button className={`nav-item ${tab === "history" ? "active" : ""}`} type="button" onClick={() => setTab("history")}><span>≡</span><small>记录</small></button>
        <button className={`nav-item ${tab === "settings" ? "active" : ""}`} type="button" onClick={() => setTab("settings")}><span>⚙</span><small>设置</small></button>
      </nav>
    </main>
  );
}

export default App;
