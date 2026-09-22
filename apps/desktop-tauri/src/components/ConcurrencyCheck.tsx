import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { type ProbePreview, type ProbeState, probeStatusLabel, publishProbeState, refreshProbeState, useProbeState } from "../lib/concurrency";

export default function ConcurrencyCheck() {
  const state = useProbeState();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const active = useRef(false);
  useEffect(() => {
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try { await refreshProbeState(); } catch { /* Preserve quota UI on bridge failure. */ }
      if (!stopped) timer = setTimeout(() => { void poll(); }, 1500);
    };
    void poll();
    return () => { stopped = true; clearTimeout(timer); };
  }, []);
  async function start() {
    if (active.current || state.running) return;
    active.current = true;
    setBusy(true); setError("");
    let stage = "preview";
    try {
      // A click authorizes the batch; retain server credential/endpoint binding.
      const preview = await invoke<ProbePreview>("concurrency_preview");
      if (!preview.items.length) { setError("没有可检测的 Key，请启用 Kimi、MiniMax 或 GLM 的 API Key。"); return; }
      stage = "start";
      publishProbeState(await invoke<ProbeState>("concurrency_start", { options: {
        models: { kimi: "kimi-for-coding", minimax: "MiniMax-M2.5", zai: "glm-4.7" },
        confirmationToken: preview.confirmationToken,
        credentialIds: preview.items.map((item) => item.credentialId),
        endpoints: Object.fromEntries(preview.items.map((item) => [item.credentialId, item.endpoint])),
      } }));
    } catch {
      setError(stage === "preview" ? "无法读取检测列表，请检查设置和密钥存储状态。" : "无法开始：已有检测任务或 Key 配置已变化，请稍后重试。");
    } finally { active.current = false; setBusy(false); }
  }
  async function cancel() {
    setBusy(true); setError("");
    try { await invoke("concurrency_cancel"); await refreshProbeState(); }
    catch { setError("取消请求未确认，请稍后重试。"); }
    finally { setBusy(false); }
  }
  const complete = state.rows.filter((row) => !["pending", "running"].includes(row.status)).length;
  return <div role="group" aria-label="批量并发检测" style={{ display: "inline-flex", alignItems: "center", gap: 6, flexWrap: "wrap" }}>
    <button type="button" className="popout-provider-toolbar__action credential-btn" disabled={busy || state.running}
      title="直接检测已启用的 Kimi、MiniMax、GLM 全部 Key，可能消耗额度。每个 Key 最多两条请求，不自动重试。"
      onClick={() => void start()}>{state.running ? `检测中 ${complete}/${state.rows.length}` : busy ? "准备检测…" : "检测并发"}</button>
    {state.running && <button className="credential-btn" disabled={busy} onClick={() => void cancel()}>取消检测</button>}
    {error && <span role="alert" style={{ color: "var(--text-secondary)", fontSize: 12, maxWidth: 280 }}>{error}</span>}
  </div>;
}

export function ConcurrencyBadge({ credentialId }: { credentialId: string }) {
  const state = useProbeState();
  const row = state.rows.find((item) => item.credentialId === credentialId);
  if (!row?.limitedAt) return null;
  return <div role="status" style={{ color: "#ff6868", fontSize: 12, margin: "6px 0" }} title={`最近确认：${new Date(row.limitedAt).toLocaleString()}；${row.model}；${probeStatusLabel[row.status] ?? row.status}`}>
    并发受限{row.status === "limited" ? "" : "（上次结果，本次未确认恢复）"}
  </div>;
}
