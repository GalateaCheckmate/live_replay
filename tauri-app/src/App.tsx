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

type BilibiliSegmentTask = {
  live_session_id: string;
  segment_index: number;
  local_path: string;
  file_size: number;
  started_at: number;
  ended_at: number;
  state: string;
  remote_filename?: string | null;
  retry_count: number;
  next_retry_at: number;
  last_error?: string | null;
  local_deleted: boolean;
};

type BilibiliSessionTask = {
  live_session_id: string;
  streamer_name: string;
  room_url: string;
  platform: string;
  session_started_at: number;
  session_ended_at?: number | null;
  recording_complete: boolean;
  aid?: number | null;
  bvid?: string | null;
  submission_state: string;
  segments: BilibiliSegmentTask[];
};

type BilibiliStore = {
  settings: {
    auto_upload: boolean;
    delete_after_success: boolean;
    account_label?: string | null;
  };
  sessions: BilibiliSessionTask[];
};

type BilibiliAuthStatus = {
  logged_in: boolean;
  mid?: number | null;
  name?: string | null;
  last_error?: string | null;
};

type BilibiliQrStart = { url: string; svg: string };

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

const emptyBilibili: BilibiliStore = {
  settings: { auto_upload: false, delete_after_success: true, account_label: null },
  sessions: [],
};

const emptyBilibiliAuth: BilibiliAuthStatus = { logged_in: false, mid: null, name: null, last_error: null };

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

function youtubeStateLabel(state: string) {
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

function bilibiliStateLabel(state: string) {
  const labels: Record<string, string> = {
    READY_TO_UPLOAD: "等待上传",
    UPLOADING_FILE: "上传文件中",
    FILE_UPLOADED: "文件已上传",
    SUBMITTING: "正在提交分P",
    REMOTE_PROCESSING: "等待远端确认",
    REMOTE_VERIFIED: "分P已确认",
    RETRY_PENDING: "等待重试",
    AUTH_REQUIRED: "需要登录",
    SUBMISSION_UNCERTAIN: "提交结果待确认",
    CONFLICT: "状态冲突",
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
  const [bilibili, setBilibili] = useState<BilibiliStore>(emptyBilibili);
  const [bilibiliAuth, setBilibiliAuth] = useState<BilibiliAuthStatus>(emptyBilibiliAuth);
  const [bilibiliQr, setBilibiliQr] = useState<BilibiliQrStart | null>(null);
  const [youtube, setYoutube] = useState<YoutubeStatus>(emptyYoutube);
  const [busy, setBusy] = useState(false);

  async function refreshCoreStatus() {
    try {
      const next = await invoke<CoreStatus>("mobile_recordings_status");
      setCore(next);
      if (next.last_error) setMessage(next.last_error);
      else if (next.active) setMessage(`正在同时录制 ${next.active_count} 路直播 · 单段约 15GB 自动安全切换`);
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

  async function refreshBilibili() {
    try {
      setBilibili(await invoke<BilibiliStore>("mobile_bilibili_status"));
    } catch (error) {
      setMessage(String(error));
    }
  }

  async function refreshBilibiliAuth() {
    try {
      setBilibiliAuth(await invoke<BilibiliAuthStatus>("mobile_bilibili_auth_status"));
    } catch (error) {
      setBilibiliAuth({ logged_in: false, last_error: String(error) });
    }
  }

  async function refreshYoutube() {
    try {
      setYoutube(await invoke<YoutubeStatus>("mobile_youtube_status"));
    } catch (error) {
      setMessage(String(error));
    }
  }

  async function refreshLocalState() {
    await Promise.allSettled([refreshCoreStatus(), refreshMonitor(), refreshBilibili(), refreshYoutube()]);
  }

  async function refreshAll() {
    await Promise.allSettled([refreshLocalState(), refreshBilibiliAuth()]);
  }

  useEffect(() => {
    if (!isAndroid) {
      window.location.replace("http://localhost:19159");
      return;
    }
    refreshAll();
    const timer = window.setInterval(refreshLocalState, 3000);
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
      setMessage("已加入监控。检测到开播后自动录制，并在单段接近 15GB 时安全切换下一段。");
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
    setMessage(roomUrl ? "正在停止该路录制并安全收尾当前分段..." : "正在停止全部录制并安全收尾当前分段...");
    try {
      setCore(await invoke<CoreStatus>("mobile_stop_recording_multi", { roomUrl: roomUrl || null }));
      window.setTimeout(refreshLocalState, 1000);
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function startBilibiliLogin() {
    setBusy(true);
    try {
      const qr = await invoke<BilibiliQrStart>("mobile_bilibili_auth_start");
      setBilibiliQr(qr);
      setMessage("请使用哔哩哔哩 App 扫描二维码并确认登录，然后点击“已扫码，等待确认”。");
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function completeBilibiliLogin() {
    setBusy(true);
    setMessage("正在等待 B站扫码确认...");
    try {
      const auth = await withTimeout(
        invoke<BilibiliAuthStatus>("mobile_bilibili_auth_complete"),
        185_000,
        "B站扫码等待超时，请重新生成二维码。",
      );
      setBilibiliAuth(auth);
      setBilibiliQr(null);
      setMessage(`B站已登录：${auth.name || `UID ${auth.mid || ""}`}`);
      await refreshBilibili();
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function logoutBilibili() {
    setBusy(true);
    try {
      await invoke("mobile_bilibili_logout");
      setBilibiliAuth(emptyBilibiliAuth);
      setBilibiliQr(null);
      setMessage("B站已退出；录像和上传队列全部保留。重新登录后可继续。");
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function updateBilibiliSettings(autoUpload: boolean, deleteAfterSuccess: boolean) {
    try {
      setBilibili(await invoke<BilibiliStore>("mobile_bilibili_set_settings", { autoUpload, deleteAfterSuccess }));
    } catch (error) {
      setMessage(String(error));
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
      setMessage("YouTube 已退出；已有录像和未完成上传任务均会保留。");
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
      setMessage("已安排重新检查现有 YouTube resumable session。");
    } catch (error) {
      setMessage(String(error));
    }
  }

  if (!isAndroid) return <main className="desktop-bridge">正在连接 Live Replay 本地服务...</main>;

  const pendingBilibiliParts = bilibili.sessions.reduce(
    (count, session) => count + session.segments.filter((segment) => segment.state !== "REMOTE_VERIFIED").length,
    0,
  );
  const pendingYoutube = youtube.store.tasks.filter((task) => task.state !== "UPLOAD_SUCCESS").length;
  const successfulYoutube = youtube.store.tasks.filter((task) => task.state === "UPLOAD_SUCCESS");
  const completedBilibili = bilibili.sessions.filter((session) => session.submission_state === "FINALIZED");

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
            <article><span>B站待传P</span><strong>{pendingBilibiliParts}</strong></article>
          </section>

          <section className="panel recorder-panel">
            <div className="section-heading"><div><h2>添加主播</h2><p>开播自动录制 · 单段接近 15GB 安全切换</p></div><span className="status-pill">自动监控</span></div>
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
              <div className="section-heading"><div><h2>正在录制</h2><p>每个直播间独立 session · 约15GB自动切段</p></div><span className="status-pill">{core.active_count} 路</span></div>
              <div className="task-list">
                {core.recordings.map((recording) => (
                  <article className="task-item" key={recording.room_url}>
                    <div className="task-main"><strong>{recording.display_name}</strong><span>录制中 · 达到阈值后安全进入下一段</span><small>{recording.room_url}</small></div>
                    <button className="mini-button danger" type="button" onClick={() => stopRecording(recording.room_url)}>停止</button>
                  </article>
                ))}
              </div>
            </section>
          )}

          <section className="panel">
            <div className="section-heading"><div><h2>监控列表</h2><p>后台定时检查，开播自动录制</p></div></div>
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
        <>
          <section className="panel">
            <div className="section-heading"><div><h2>B站多P上传</h2><p>同一 liveSession 严格按 P1 → P2 → P3 顺序追加</p></div><span className="status-pill">{bilibiliAuth.logged_in ? "已登录" : "未登录"}</span></div>
            <div className="task-list">
              {bilibili.sessions.length === 0 && <p className="empty-text">暂无 B站上传任务。15GB 分段收尾完成后会进入这里。</p>}
              {bilibili.sessions.slice().reverse().map((session) => (
                <article className="task-item stacked" key={session.live_session_id}>
                  <div className="task-main">
                    <strong>{session.streamer_name}</strong>
                    <span>{session.submission_state} · {session.recording_complete ? "直播已结束" : "直播进行中"}</span>
                    {(session.bvid || session.aid) && <small>{session.bvid ? `BV: ${session.bvid}` : `aid: ${session.aid}`}</small>}
                  </div>
                  <div className="task-list nested-list">
                    {session.segments.map((segment) => (
                      <div className="segment-row" key={`${session.live_session_id}-${segment.segment_index}`}>
                        <div><strong>P{segment.segment_index}</strong><span>{bilibiliStateLabel(segment.state)} · {formatBytes(segment.file_size)}</span></div>
                        <small>{segment.local_deleted ? "远端已确认 · 本地已删除" : segment.last_error || "本地录像保留"}</small>
                      </div>
                    ))}
                  </div>
                </article>
              ))}
            </div>
          </section>

          <section className="panel compact-panel">
            <div className="section-heading"><div><h2>YouTube</h2><p>现有 OAuth / Resumable Upload / 状态持久化全部保留，当前等待后续无损合并多段再继续完善</p></div><span className="status-pill">{youtube.authorized ? "已保留登录" : "冻结开发"}</span></div>
            {pendingYoutube > 0 && <p className="empty-text">已有旧任务：{pendingYoutube} 个，现有 worker 仍会安全处理。</p>}
          </section>
        </>
      )}

      {tab === "history" && (
        <>
          <section className="panel">
            <div className="section-heading"><div><h2>B站已完成</h2><p>同场多P全部远端确认并完成最终标题更新</p></div><span className="status-pill">{completedBilibili.length}</span></div>
            <div className="task-list">
              {completedBilibili.length === 0 && <p className="empty-text">暂无已完成 B站稿件。</p>}
              {completedBilibili.slice().reverse().map((session) => (
                <article className="task-item" key={session.live_session_id}>
                  <div className="task-main"><strong>{session.streamer_name}</strong><span>{session.segments.length} 个分P · {session.segments.every((part) => part.local_deleted) ? "本地已安全清理" : "仍有本地文件"}</span>{session.bvid && <small>{session.bvid}</small>}</div>
                </article>
              ))}
            </div>
          </section>
          <section className="panel compact-panel"><div className="section-heading"><div><h2>最近完成分段</h2><p className="path-text">{core.last_file || "暂无"}</p></div></div></section>
          {successfulYoutube.length > 0 && (
            <section className="panel">
              <div className="section-heading"><div><h2>YouTube 历史任务</h2><p>路线调整前已确认成功的任务仍保留</p></div><span className="status-pill">{successfulYoutube.length}</span></div>
              <div className="task-list">
                {successfulYoutube.slice().reverse().map((task) => <article className="task-item" key={task.id}><div className="task-main"><strong>{task.youtube_title}</strong><span>{task.local_deleted ? "YouTube 成功 · 本地已安全删除" : "YouTube 成功 · 本地保留"}</span>{task.youtube_video_id && <small>videoId: {task.youtube_video_id}</small>}</div></article>)}
              </div>
            </section>
          )}
        </>
      )}

      {tab === "settings" && (
        <>
          <section className="panel">
            <div className="section-heading"><div><h2>哔哩哔哩</h2><p>{bilibiliAuth.logged_in ? bilibiliAuth.name || `UID ${bilibiliAuth.mid}` : "尚未登录上传账号"}</p></div><span className="status-pill">当前优先</span></div>
            {!bilibiliAuth.logged_in && !bilibiliQr && <button className="primary-action" type="button" onClick={startBilibiliLogin} disabled={busy}>扫码登录 B站</button>}
            {bilibiliQr && (
              <div className="qr-login">
                <div className="qr-image" dangerouslySetInnerHTML={{ __html: bilibiliQr.svg }} />
                <p>使用哔哩哔哩 App 扫码确认。二维码仅在本机生成。</p>
                <button className="primary-action" type="button" onClick={completeBilibiliLogin} disabled={busy}>已扫码，等待确认</button>
                <button className="text-button" type="button" onClick={startBilibiliLogin} disabled={busy}>重新生成</button>
              </div>
            )}
            {bilibiliAuth.logged_in && <button className="secondary-action" type="button" onClick={logoutBilibili} disabled={busy}>退出 B站账号</button>}
            {!bilibiliAuth.logged_in && bilibiliAuth.last_error && <p className="error-text">{bilibiliAuth.last_error}</p>}
          </section>

          <section className="panel settings-list">
            <label className="setting-row"><div><strong>B站自动上传</strong><span>15GB 分段完成后按同一 liveSession 依次追加为同稿多P</span></div><input type="checkbox" checked={bilibili.settings.auto_upload} onChange={(event) => updateBilibiliSettings(event.target.checked, bilibili.settings.delete_after_success)} /></label>
            <label className="setting-row"><div><strong>B站成功后删除本地</strong><span>仅删除已经远端反查确认的对应分段，不会连带删除其它 P</span></div><input type="checkbox" checked={bilibili.settings.delete_after_success} onChange={(event) => updateBilibiliSettings(bilibili.settings.auto_upload, event.target.checked)} /></label>
            <div className="setting-row static"><div><strong>单段大小</strong><span>按文件大小安全切段，不按固定时长</span></div><b>约 15GB</b></div>
            <div className="setting-row static"><div><strong>B站可见性</strong><span>开发测试阶段先保持仅自己可见</span></div><b>仅自己</b></div>
          </section>

          <section className="panel">
            <div className="section-heading"><div><h2>YouTube（保留）</h2><p>{youtube.store.settings.account_label || "现有实现保留，当前暂不扩展"}</p></div><span className="status-pill">Private</span></div>
            <div className="settings-actions">{!youtube.authorized ? <button className="secondary-action" type="button" onClick={authorizeYoutube} disabled={busy}>登录 YouTube</button> : <button className="secondary-action" type="button" onClick={logoutYoutube} disabled={busy}>退出 YouTube</button>}</div>
          </section>
          <section className="panel settings-list">
            <label className="setting-row"><div><strong>旧 YouTube 自动上传任务</strong><span>仅保留现有整文件任务处理；新的 15GB 分段不会直接上传 YouTube</span></div><input type="checkbox" checked={youtube.store.settings.auto_upload} onChange={(event) => updateYoutubeSettings(event.target.checked, youtube.store.settings.delete_after_success)} /></label>
            <label className="setting-row"><div><strong>YouTube 成功后删除本地</strong><span>仍使用既有 videoId + SUCCESS 持久化安全屏障</span></div><input type="checkbox" checked={youtube.store.settings.delete_after_success} onChange={(event) => updateYoutubeSettings(youtube.store.settings.auto_upload, event.target.checked)} /></label>
          </section>

          <section className="panel compact-panel"><div className="section-heading"><div><h2>后台运行</h2><p>Foreground Service + WakeLock · 录制与上传互不阻塞</p></div><span className="status-pill">已启用</span></div></section>
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
