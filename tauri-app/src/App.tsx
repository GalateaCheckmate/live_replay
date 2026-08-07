import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

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

type CoreStatus = {
  active: boolean;
  room_url?: string | null;
  display_name?: string | null;
  current_file?: string | null;
  last_file?: string | null;
  last_error?: string | null;
};

function App() {
  const userAgent = useMemo(() => navigator.userAgent, []);
  const isAndroid = useMemo(
    () => /Android|HarmonyOS|HUAWEI|ANA-|ALN-/i.test(userAgent),
    [userAgent],
  );
  const [url, setUrl] = useState("");
  const [name, setName] = useState("");
  const [message, setMessage] = useState("正在加载 Android 原生内核...");
  const [probe, setProbe] = useState<ProbeResult | null>(null);
  const [core, setCore] = useState<CoreStatus>({ active: false });
  const [busy, setBusy] = useState(false);

  async function refreshCoreStatus() {
    try {
      const next = await invoke<CoreStatus>("mobile_core_status");
      setCore(next);
      if (next.last_error) setMessage(next.last_error);
      else if (next.active) setMessage("正在录制直播源...");
      else setMessage("Android 原生 Rust 内核已就绪");
    } catch (error) {
      setMessage(String(error));
    }
  }

  useEffect(() => {
    if (!isAndroid) {
      window.location.replace("http://localhost:19159");
      return;
    }
    refreshCoreStatus();
  }, [isAndroid]);

  useEffect(() => {
    if (!isAndroid || !core.active) return;
    const timer = window.setInterval(refreshCoreStatus, 2000);
    return () => window.clearInterval(timer);
  }, [isAndroid, core.active]);

  async function probeStream() {
    if (!url.trim()) {
      setMessage("请输入 B站或抖音直播间地址");
      return;
    }
    setBusy(true);
    setProbe(null);
    setMessage("正在解析直播间...");
    try {
      const result = await invoke<ProbeResult>("mobile_probe_stream", {
        url: url.trim(),
        name: name.trim() || null,
        bilibiliCookie: null,
        douyinCookie: null,
      });
      setProbe(result);
      setMessage(
        result.status === "live"
          ? `已解析 ${result.stream.platform} 直播源：${result.stream.title || result.stream.name}`
          : "主播当前未开播",
      );
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function startRecording() {
    if (!url.trim()) {
      setMessage("请输入 B站或抖音直播间地址");
      return;
    }
    setBusy(true);
    setMessage("正在解析直播源并启动录制...");
    try {
      const next = await invoke<CoreStatus>("mobile_start_recording", {
        url: url.trim(),
        name: name.trim() || null,
        bilibiliCookie: null,
        douyinCookie: null,
      });
      setCore(next);
      setMessage("录制已启动，媒体数据正在写入 Android 本地存储");
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  async function stopRecording() {
    setBusy(true);
    setMessage("正在停止录制并收尾文件...");
    try {
      const next = await invoke<CoreStatus>("mobile_stop_recording");
      setCore(next);
      window.setTimeout(refreshCoreStatus, 800);
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  if (!isAndroid) {
    return <main className="desktop-bridge">正在连接 Live Replay 本地服务...</main>;
  }

  return (
    <main className="app-shell">
      <header className="topbar">
        <div>
          <span className="brand">LIVE REPLAY</span>
          <h1>录制中心</h1>
        </div>
      </header>

      <section className="runtime-card">
        <div className={`runtime-indicator ${core.active ? "running" : ""}`} />
        <div className="runtime-copy">
          <strong>{core.active ? "正在录制" : "服务状态"}</strong>
          <span>{message}</span>
        </div>
        <button className="text-button" type="button" onClick={refreshCoreStatus}>
          刷新
        </button>
      </section>

      <section className="stats-grid" aria-label="任务概览">
        <article>
          <span>解析器</span>
          <strong>{probe?.status === "live" ? "✓" : "—"}</strong>
        </article>
        <article>
          <span>录制中</span>
          <strong>{core.active ? 1 : 0}</strong>
        </article>
        <article>
          <span>待上传</span>
          <strong>0</strong>
        </article>
      </section>

      <section className="panel recorder-panel">
        <div className="section-heading">
          <div>
            <h2>直播源测试</h2>
            <p>当前先验证真实解析与录制内核</p>
          </div>
          <span className="status-pill">Rust Core</span>
        </div>

        <label className="field">
          <span>直播间地址</span>
          <input
            value={url}
            onChange={(event) => setUrl(event.target.value)}
            placeholder="https://live.bilibili.com/... 或 抖音直播地址"
            disabled={core.active}
          />
        </label>

        <label className="field">
          <span>主播名称（可选）</span>
          <input
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="用于录制文件名"
            disabled={core.active}
          />
        </label>

        <div className="action-row">
          <button type="button" className="secondary-action" onClick={probeStream} disabled={busy || core.active}>
            检测直播
          </button>
          {!core.active ? (
            <button type="button" className="primary-action inline" onClick={startRecording} disabled={busy}>
              开始录制
            </button>
          ) : (
            <button type="button" className="danger-action" onClick={stopRecording} disabled={busy}>
              停止录制
            </button>
          )}
        </div>
      </section>

      {probe?.status === "live" && (
        <section className="panel result-panel">
          <div className="section-heading">
            <div>
              <h2>{probe.stream.title || probe.stream.name}</h2>
              <p>{probe.stream.platform} · {probe.stream.suffix || "stream"}</p>
            </div>
            <span className="status-pill">已开播</span>
          </div>
        </section>
      )}

      <section className="panel compact-panel">
        <div className="section-heading">
          <div>
            <h2>最近录制文件</h2>
            <p className="path-text">{core.last_file || "暂无"}</p>
          </div>
        </div>
      </section>

      <section className="panel compact-panel">
        <div className="section-heading">
          <div>
            <h2>后台运行</h2>
            <p>Foreground Service + WakeLock 已启用</p>
          </div>
          <span className="status-pill">已配置</span>
        </div>
      </section>

      <nav className="bottom-nav" aria-label="主导航">
        <button className="nav-item active" type="button">
          <span>●</span>
          <small>录制</small>
        </button>
        <button className="nav-item" type="button" disabled>
          <span>⇧</span>
          <small>上传</small>
        </button>
        <button className="nav-item" type="button" disabled>
          <span>≡</span>
          <small>记录</small>
        </button>
        <button className="nav-item" type="button" disabled>
          <span>⚙</span>
          <small>设置</small>
        </button>
      </nav>
    </main>
  );
}

export default App;
