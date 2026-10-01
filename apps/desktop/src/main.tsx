import React from "react";
import ReactDOM from "react-dom/client";
import { Alert, Loader, MantineProvider, Progress } from "@mantine/core";
import { setNonce } from "get-nonce";
import "@mantine/core/styles.css";
import "./styles.css";
import "./mantine-layout.css";
import { libraryTheme } from "./theme";
import { invoke } from "@tauri-apps/api/core";
import { toUiError } from "./uiError";

const App = React.lazy(() => import("./App"));
// Reuse Tauri's document nonce for Mantine's theme, responsive layout and portals.
// Read the property: browsers deliberately hide nonce values from getAttribute.
const getStyleNonce = () => document.querySelector<HTMLStyleElement>("#library-style-nonce")?.nonce || "";
// Mantine's modal scroll lock uses react-style-singleton, which has its own nonce API.
setNonce(getStyleNonce());

function Startup() {
  const [ready, setReady] = React.useState(false);
  const [phase, setPhase] = React.useState("正在打开本地资产库…");
  const [error, setError] = React.useState("");
  const [progress, setProgress] = React.useState<{ step: number; detail: string; completed: number | null; total: number | null; elapsed_ms: number; phase_elapsed_ms: number; idle_ms: number } | null>(null);
  React.useEffect(() => {
    let active = true;
    let timer: number | undefined;
    const poll = async () => {
      try {
        const status = await invoke<{ ready: boolean; phase: string; error: string | null; step: number; detail: string; completed: number | null; total: number | null; elapsed_ms: number; phase_elapsed_ms: number; idle_ms: number }>("startup_status");
        if (!active) return;
        setPhase(status.phase);
        setProgress(status);
        if (status.error) { setError(status.error); return; }
        if (status.ready) { setReady(true); return; }
        timer = window.setTimeout(poll, 250);
      } catch (reason) { if (active) setError(toUiError(reason)); }
    };
    void poll();
    return () => { active = false; window.clearTimeout(timer); };
  }, []);
  if (ready) return <React.Suspense fallback={<div className="startup-screen" role="status"><Loader size="sm" /><p>正在载入 Library 界面…</p></div>}><App /></React.Suspense>;
  const determinate = progress?.completed !== null && progress?.completed !== undefined && (progress.total ?? 0) > 0;
  const percent = determinate ? Math.min(100, progress!.completed! / progress!.total! * 100) : null;
  return <div className="startup-screen"><div className="startup-brand">◇</div><span>ASSET LIBRARY</span><h1>MMDbridgeLib</h1>
    {!error && <p role="status">{phase}</p>}
    {progress && <div className="startup-status-detail"><div>{progress.detail}</div>
      <div>初始化阶段 {Math.max(1, progress.step)} / 6 · 已用 {Math.floor(progress.elapsed_ms / 1000)} 秒 · 本阶段 {Math.floor(progress.phase_elapsed_ms / 1000)} 秒</div>
      {progress.completed !== null && progress.total !== null && <strong>{progress.completed.toLocaleString()} / {progress.total.toLocaleString()}{percent !== null ? ` · ${Math.round(percent)}%` : ""}</strong>}
      {progress.idle_ms >= 15000 && !error && <div className="startup-waiting">本阶段已 {Math.floor(progress.idle_ms / 1000)} 秒未返回新进度；可能正在等待磁盘或数据库锁。</div>}
    </div>}
    {!error && (percent !== null ? <Progress value={percent} aria-label={phase} w={280} size="sm" mt="lg" mb="xl" /> : <Loader type="bars" size="sm" mt="lg" mb="xl" aria-label={phase} />)}
    {error && <Alert color="red" title="初始化未完成" maw={640} mx="lg" mb="lg">{error}</Alert>}
    <small>{error ? "请关闭程序后重试；错误记录：data/startup-error.txt" : "当前只初始化资产库；尚未扫描素材或生成缩略图"}</small>
    <small className="startup-log-path">阶段记录：data/startup-progress.log</small>
  </div>;
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <MantineProvider theme={libraryTheme} forceColorScheme="dark" getStyleNonce={getStyleNonce}>
      <Startup />
    </MantineProvider>
  </React.StrictMode>,
);
