import { ActionIcon, Alert, Badge, Button, Checkbox, NativeSelect, Progress } from "@mantine/core";
import { invoke } from "@tauri-apps/api/core";
import { Check, Copy, Link2, RefreshCw, Square, Unplug } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { toUiError } from "./uiError";

type AssetType = "model" | "motion" | "scene";
export type AgentLinkScope = { rootId: string | null; assetType: AssetType | null };
type Root = { id: string; displayName: string; enabled: boolean; assetType: AssetType };
export type AgentLinkEvent = { id: string; time: string; operation: string; phase: string; message: string; percent: number | null; details: Record<string, unknown> };
export type AgentLinkSnapshot = {
  sessionId: string; status: "waiting" | "active" | "finished" | "cancelled" | "disconnected";
  agentName: string; percent: number | null; scope: AgentLinkScope; prompt: string;
  cliAvailable: boolean; skillAvailable: boolean; events: AgentLinkEvent[];
  revision: number; libraryRevision: number; error: string | null;
};
const statusLabels = { waiting: "等待连接", active: "正在接管", finished: "已完成", cancelled: "已取消", disconnected: "连接已断开" };
const operationLabels: Record<string, string> = { "agent-identify": "Agent 连接", "agent-inspect": "资产检查", "agent-tags": "追加标签", "agent-sync-cards": "资源卡同步", "agent-log": "进度", "agent-finish": "完成", "agent-cancel": "取消接管", "agent-session": "新会话" };
const summaryKeys: Record<string, string> = { assets: "资产", items: "读取", changed: "新增", blockedByUser: "移除覆盖", completed: "完成", failed: "失败" };

async function copyText(value: string): Promise<void> {
  try { await navigator.clipboard.writeText(value); return; } catch { /* Use the WebView clipboard fallback. */ }
  const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const input = document.createElement("textarea"); input.value = value; input.style.position = "fixed"; input.style.opacity = "0";
  document.body.appendChild(input); input.select();
  try { if (!document.execCommand("copy")) throw new Error("复制失败，请选择文字后手动复制。"); }
  finally { input.remove(); previous?.focus({ preventScroll: true }); }
}

export function AgentLinkPage({ active, initialScope, roots, onLibraryChanged }: {
  active: boolean; initialScope: AgentLinkScope; roots: Root[]; onLibraryChanged: () => Promise<void>;
}) {
  const [snapshot, setSnapshot] = useState<AgentLinkSnapshot | null>(null);
  const [scope, setScope] = useState(initialScope);
  const [prompt, setPrompt] = useState("");
  const [dirty, setDirty] = useState(false);
  const [stalePrompt, setStalePrompt] = useState(false);
  const [error, setError] = useState("");
  const [connectionError, setConnectionError] = useState("");
  const [busy, setBusy] = useState(false);
  const [followLog, setFollowLog] = useState(true);
  const [copied, setCopied] = useState<"prompt" | "log" | null>(null);
  const current = useRef<AgentLinkSnapshot | null>(null);
  const dirtyRef = useRef(false);
  const scopeApplied = useRef(false);
  const initial = useRef(initialScope);
  const onChanged = useRef(onLibraryChanged);
  const logBody = useRef<HTMLDivElement | null>(null);
  const inFlight = useRef(false);
  const pollVersion = useRef(0);
  const copyTimer = useRef<number | null>(null);
  onChanged.current = onLibraryChanged;

  const accept = useCallback((next: AgentLinkSnapshot, replacePrompt = false) => {
    const previous = current.current;
    if (previous?.sessionId === next.sessionId && next.revision < previous.revision) return;
    if (!replacePrompt && previous?.sessionId === next.sessionId && next.revision === previous.revision
      && next.status === previous.status && next.cliAvailable === previous.cliAvailable
      && next.skillAvailable === previous.skillAvailable && next.error === previous.error) return;
    current.current = next; setSnapshot(next); setScope(next.scope);
    if (replacePrompt || !dirtyRef.current) { setPrompt(next.prompt); setDirty(false); dirtyRef.current = false; setStalePrompt(false); }
    if (previous && (next.libraryRevision !== previous.libraryRevision || (next.status === "finished" && previous.status !== "finished"))) {
      void onChanged.current().catch((reason) => setError(toUiError(reason)));
    }
  }, []);

  useEffect(() => {
    let disposed = false; let timer = 0;
    ++pollVersion.current;
    const poll = async () => {
      const version = pollVersion.current;
      try {
        if (!inFlight.current) {
          let next = await invoke<AgentLinkSnapshot>("agentlink_open");
          if (disposed || version !== pollVersion.current) return;
          if (!scopeApplied.current && next.status !== "active") {
            const selectedRoot = roots.find((root) => root.id === initial.current.rootId && root.enabled);
            const initialValue = { rootId: selectedRoot?.id ?? null, assetType: selectedRoot?.assetType ?? initial.current.assetType };
            next = await invoke<AgentLinkSnapshot>("agentlink_scope_set", { scope: initialValue });
            if (disposed || version !== pollVersion.current) return;
            scopeApplied.current = true;
          }
          accept(next); setConnectionError("");
        }
      } catch (reason) { if (!disposed) setConnectionError(toUiError(reason)); }
      finally { if (!disposed) timer = window.setTimeout(poll, active ? 800 : 2500); }
    };
    void poll();
    return () => { disposed = true; window.clearTimeout(timer); };
  }, [active, accept]);

  useEffect(() => {
    if (active && followLog && logBody.current) logBody.current.scrollTop = logBody.current.scrollHeight;
  }, [active, followLog, snapshot?.revision]);
  useEffect(() => () => { if (copyTimer.current !== null) window.clearTimeout(copyTimer.current); }, []);

  async function action(command: string, args: Record<string, unknown> = {}, replacePrompt = false) {
    inFlight.current = true; ++pollVersion.current; setBusy(true); setError("");
    try { accept(await invoke<AgentLinkSnapshot>(command, args), replacePrompt); }
    catch (reason) { setError(toUiError(reason)); }
    finally { inFlight.current = false; setBusy(false); }
  }
  async function changeScope(next: AgentLinkScope) {
    await action("agentlink_scope_set", { scope: next });
    if (dirtyRef.current && JSON.stringify(current.current?.scope) === JSON.stringify(next)) setStalePrompt(true);
  }
  async function copy(which: "prompt" | "log") {
    try {
      const value = which === "prompt" ? prompt : (snapshot?.events ?? []).map((event) => `${event.time} [${operationLabels[event.operation] ?? event.operation}] ${event.message}`).join("\n");
      await copyText(value); setCopied(which);
      if (copyTimer.current !== null) window.clearTimeout(copyTimer.current);
      copyTimer.current = window.setTimeout(() => setCopied(null), 1800);
    } catch (reason) { setError(toUiError(reason)); }
  }
  const connected = snapshot?.status === "active";
  const events = snapshot?.events ?? [];
  const progress = snapshot?.percent ?? null;
  return <section className="agentlink-page" hidden={!active} aria-label="AgentLink">
    <div className="agentlink-heading"><div><h1>AgentLink</h1><p>在外部 Agent 中使用 Prompt，进度与操作记录会显示在 Log。</p></div>
      <div className="agentlink-session-controls">
        <Badge variant="light" color={connected ? "blue" : snapshot?.status === "finished" ? "green" : "gray"} className={`agentlink-status ${connected ? "is-active" : ""}`}><span className="agentlink-status-dot" />{snapshot ? statusLabels[snapshot.status] : "正在连接"}{snapshot?.agentName ? ` · ${snapshot.agentName}` : ""}</Badge>
        {connected ? <Button variant="light" color="red" disabled={busy} leftSection={<Square size={13} />} onClick={() => void action("agentlink_cancel")}>取消接管</Button>
          : <Button variant="subtle" disabled={busy || !snapshot || snapshot.status === "disconnected"} leftSection={<Link2 size={15} />} onClick={() => void action("agentlink_new_session")}>新会话</Button>}
      </div>
    </div>
    {(error || connectionError || snapshot?.error) && <Alert role="alert" color="red" title="AgentLink">{error || connectionError || snapshot?.error}</Alert>}
    {snapshot && (!snapshot.cliAvailable || !snapshot.skillAvailable) && <Alert role="alert" color="yellow">便携目录缺少 CLI 或标签 Skill，请使用完整便携包。</Alert>}
    <div className="agentlink-scope">
      <NativeSelect label="资产类型" aria-label="AgentLink 资产类型" value={scope.assetType ?? "all"} disabled={busy || connected || !snapshot} data={[{value:"all",label:"全部资产"},{value:"model",label:"模型"},{value:"motion",label:"动作"},{value:"scene",label:"场景"}]} onChange={(event) => void changeScope({assetType:event.target.value === "all" ? null : event.target.value as AssetType,rootId:null})} />
      <NativeSelect label="资产目录" aria-label="AgentLink 资产目录" value={scope.rootId ?? "all"} disabled={busy || connected || !snapshot} data={[{value:"all",label:"全部已启用目录"},...roots.filter((root) => root.enabled && (!scope.assetType || root.assetType === scope.assetType)).map((root) => ({value:root.id,label:root.displayName}))]} onChange={(event) => void changeScope({...scope,rootId:event.target.value === "all" ? null : event.target.value})} />
      {connected && <span className="agentlink-scope-note">接管期间范围已固定</span>}
    </div>
    <div className="agentlink-panels">
      <div className="agentlink-panel prompt-panel">
        <div className="agentlink-panel-header"><h2>Prompt</h2><div><Button size="compact-xs" variant="subtle" leftSection={<RefreshCw size={14} />} disabled={busy || !snapshot} onClick={() => void action("agentlink_open", {}, true)}>重新生成</Button><Button size="compact-xs" variant="light" leftSection={copied === "prompt" ? <Check size={14} /> : <Copy size={14} />} disabled={!prompt || stalePrompt} onClick={() => void copy("prompt")}>{copied === "prompt" ? "已复制" : "复制"}</Button></div></div>
        {stalePrompt && <div className="agentlink-prompt-warning" role="status">范围已变化，请重新生成 Prompt。</div>}
        <textarea className="agentlink-prompt" aria-label="AgentLink Prompt" spellCheck={false} value={prompt} placeholder="正在生成当前资产范围的 Prompt…" onChange={(event) => {setPrompt(event.target.value);setDirty(true);dirtyRef.current=true;}} />
        <div className="agentlink-panel-footer"><span>{dirty ? "已编辑" : "当前资产范围"}</span><span>{prompt.length.toLocaleString()} 字符</span></div>
      </div>
      <div className="agentlink-panel log-panel">
        <div className="agentlink-panel-header"><h2>Log</h2><div><ActionIcon variant="subtle" aria-label="复制 AgentLink 日志" title="复制日志" disabled={!events.length} onClick={() => void copy("log")}>{copied === "log" ? <Check size={15} /> : <Copy size={15} />}</ActionIcon><Button size="compact-xs" variant="subtle" leftSection={<RefreshCw size={14} />} disabled={busy} onClick={() => void onChanged.current().catch((reason) => setError(toUiError(reason)))}>刷新资产</Button></div></div>
        {progress !== null && <div className="agentlink-progress"><Progress value={progress} size={3} aria-label="Agent 接管进度" /><span>{progress}%</span></div>}
        <div className="agentlink-log" ref={logBody} role="log" aria-label="AgentLink 操作日志" aria-live="polite" aria-relevant="additions">
          {!events.length && <div className="agentlink-log-empty"><Unplug size={25} strokeWidth={1.3} /><strong>暂无操作记录</strong><span>Agent 连接后会显示检查、标签与资源卡同步记录。</span></div>}
          {events.map((event) => <article key={event.id} className={`agentlink-event phase-${event.phase}`}><div className="agentlink-event-heading"><strong>{operationLabels[event.operation] ?? event.operation}</strong><time dateTime={event.time}>{new Date(event.time).toLocaleTimeString("zh-CN",{hour12:false})}</time></div><p>{event.message}</p>{Object.entries(event.details).some(([key,value]) => summaryKeys[key] && typeof value === "number") && <dl>{Object.entries(event.details).filter(([key,value]) => summaryKeys[key] && typeof value === "number").map(([key,value]) => <div key={key}><dt>{summaryKeys[key]}</dt><dd>{String(value)}</dd></div>)}</dl>}</article>)}
        </div>
        <div className="agentlink-panel-footer"><Checkbox size="xs" label="跟随最新日志" checked={followLog} onChange={(event) => setFollowLog(event.target.checked)} /><span>{events.length} 条记录</span></div>
      </div>
    </div>
  </section>;
}
