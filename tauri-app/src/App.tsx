import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

function App() {
  const userAgent = useMemo(() => navigator.userAgent, []);
  const isAndroid = useMemo(() => /Android/i.test(userAgent), [userAgent]);
  const isHuawei = useMemo(() => /HUAWEI|ANA-|HarmonyOS/i.test(userAgent), [userAgent]);
  const [status, setStatus] = useState("正在初始化 Android 壳...");

  useEffect(() => {
    if (!isAndroid) {
      window.location.replace("http://localhost:19159");
      return;
    }

    setStatus(
      isHuawei
        ? "已进入华为 / HarmonyOS 适配模式。当前 APK 为 ARM64，并启用后台录制所需基础权限。"
        : "Android 壳已启动，Windows sidecar 已隔离。下一步接入原生录制核心。",
    );
  }, [isAndroid, isHuawei]);

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
          当前优先适配 Huawei P40 · HarmonyOS 4.2，目标是锁屏后仍能持续录制和上传。
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
          <strong>CPU 架构</strong>
          <span>ARM64 / aarch64，针对 P40 麒麟平台构建</span>
        </article>
        <article>
          <strong>后台常驻</strong>
          <span>已预留 WakeLock 与 Foreground Service 权限</span>
        </article>
        <article>
          <strong>网络</strong>
          <span>允许锁屏持续联网，并兼容仍使用 HTTP 的直播源</span>
        </article>
        <article>
          <strong>系统基线</strong>
          <span>最低 Android API 29，减少旧系统兼容负担</span>
        </article>
      </section>

      <section className="huawei-card">
        <strong>HarmonyOS 4.2 长期录制设置</strong>
        <ol>
          <li>应用启动管理：关闭自动管理，并允许后台活动。</li>
          <li>电池优化：把 Live Replay 设为“不允许优化”。</li>
          <li>更多电池设置：开启“休眠时始终保持网络连接”。</li>
          <li>最近任务中下拉 Live Replay 卡片并加锁。</li>
          <li>录制期间不要开启省电模式或超级省电。</li>
        </ol>
      </section>

      <button type="button" onClick={probeCore}>
        检查 Android 核心状态
      </button>
    </main>
  );
}

export default App;
