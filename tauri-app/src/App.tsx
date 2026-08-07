import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

function App() {
  const isAndroid = useMemo(() => /Android/i.test(navigator.userAgent), []);
  const [status, setStatus] = useState("正在初始化 Android 壳...");

  useEffect(() => {
    if (!isAndroid) {
      window.location.replace("http://localhost:19159");
      return;
    }

    setStatus("Android 壳已启动，Windows sidecar 已隔离。下一步接入原生录制核心。");
  }, [isAndroid]);

  async function probeCore() {
    try {
      const message = await invoke<string>("start_sidecar");
      setStatus(message);
    } catch (error) {
      setStatus(String(error));
    }
  }

  if (!isAndroid) {
    return <main className="container">正在连接 Live Replay 本地服务...</main>;
  }

  return (
    <main className="container android-shell">
      <section className="hero-card">
        <span className="eyebrow">LIVE REPLAY · ANDROID</span>
        <h1>手机录制端</h1>
        <p className="subtitle">
          第一阶段先保证 APK 壳可启动、可安装，并彻底去掉 Windows 专属依赖。
        </p>
      </section>

      <section className="status-card">
        <div className="status-dot" />
        <div>
          <strong>当前状态</strong>
          <p>{status}</p>
        </div>
      </section>

      <section className="grid">
        <article>
          <strong>录制核心</strong>
          <span>待接入 Rust / Android 原生运行层</span>
        </article>
        <article>
          <strong>后台常驻</strong>
          <span>下一阶段接 Foreground Service</span>
        </article>
        <article>
          <strong>B 站上传</strong>
          <span>复用现有 Rust 上传逻辑</span>
        </article>
        <article>
          <strong>存储</strong>
          <span>下一阶段适配 Android 私有目录与空间保护</span>
        </article>
      </section>

      <button type="button" onClick={probeCore}>
        检查 Android 核心状态
      </button>
    </main>
  );
}

export default App;
