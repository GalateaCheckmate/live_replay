import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

function App() {
  const userAgent = useMemo(() => navigator.userAgent, []);
  const isAndroid = useMemo(
    () => /Android|HarmonyOS|HUAWEI|ANA-|ALN-/i.test(userAgent),
    [userAgent],
  );
  const [status, setStatus] = useState("正在初始化移动端运行环境...");

  useEffect(() => {
    if (!isAndroid) {
      window.location.replace("http://localhost:19159");
      return;
    }

    setStatus("Android 运行环境已就绪，录制核心正在接入。 ");
  }, [isAndroid]);

  async function refreshCoreStatus() {
    try {
      const message = await invoke<string>("start_sidecar");
      setStatus(message);
    } catch (error) {
      setStatus(String(error));
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
        <button className="icon-button" type="button" aria-label="设置" disabled>
          ⚙
        </button>
      </header>

      <section className="runtime-card">
        <div className="runtime-indicator" />
        <div className="runtime-copy">
          <strong>服务状态</strong>
          <span>{status}</span>
        </div>
        <button className="text-button" type="button" onClick={refreshCoreStatus}>
          刷新
        </button>
      </section>

      <section className="stats-grid" aria-label="任务概览">
        <article>
          <span>监控中</span>
          <strong>0</strong>
        </article>
        <article>
          <span>录制中</span>
          <strong>0</strong>
        </article>
        <article>
          <span>待上传</span>
          <strong>0</strong>
        </article>
      </section>

      <button className="primary-action" type="button" disabled>
        ＋ 添加主播
      </button>

      <section className="panel">
        <div className="section-heading">
          <div>
            <h2>录制任务</h2>
            <p>已添加的主播会显示在这里</p>
          </div>
        </div>
        <div className="empty-state">
          <div className="empty-icon">◉</div>
          <strong>暂无任务</strong>
          <span>移动端录制核心接入完成后，即可在这里添加并管理直播录制。</span>
        </div>
      </section>

      <section className="panel compact-panel">
        <div className="section-heading">
          <div>
            <h2>后台运行</h2>
            <p>前台服务与 WakeLock 基础支持已启用</p>
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
