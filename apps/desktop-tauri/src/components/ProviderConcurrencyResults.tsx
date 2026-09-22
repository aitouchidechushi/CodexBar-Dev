import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { probeStatusLabel, useProbeState } from "../lib/concurrency";
import "./ProviderConcurrencyResults.css";

export default function ProviderConcurrencyResults({ providerId, providerName }: { providerId: string; providerName: string }) {
  const state = useProbeState();
  const rows = state.rows.filter((row) => row.providerId === providerId);
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState({ left: 8, top: 8, width: 360, height: 400 });
  const trigger = useRef<HTMLButtonElement>(null);
  const card = useRef<HTMLDivElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>();
  const focusCard = useRef(false);
  const id = useId();
  function clearClose() { clearTimeout(timer.current); }
  function show() { clearClose(); setOpen(true); }
  function closeSoon() {
    clearClose();
    timer.current = setTimeout(() => setOpen(false), 180);
  }
  function escape(event: React.KeyboardEvent) {
    if (event.key === "Escape") { trigger.current?.focus(); clearClose(); setOpen(false); event.stopPropagation(); }
  }
  function triggerKey(event: React.KeyboardEvent) {
    if (["ArrowDown", "Enter", " "].includes(event.key)) {
      event.preventDefault();
      show();
      if (card.current) card.current.focus();
      else focusCard.current = true;
    } else escape(event);
  }
  function blur(event: React.FocusEvent) {
    const next = event.relatedTarget as Node | null;
    if (!trigger.current?.contains(next) && !card.current?.contains(next)) closeSoon();
  }
  useEffect(() => () => clearTimeout(timer.current), []);
  useLayoutEffect(() => {
    if (!open) return;
    if (focusCard.current) { card.current?.focus(); focusCard.current = false; }
    function place() {
      const rect = trigger.current?.getBoundingClientRect();
      if (!rect) return;
      const width = Math.min(380, Math.max(0, window.innerWidth - 16));
      const below = window.innerHeight - rect.bottom - 16;
      const above = rect.top - 16;
      const down = below >= Math.min(320, above);
      const height = Math.max(0, Math.min(420, down ? below : above));
      const actualHeight = Math.min(card.current?.scrollHeight ?? height, height);
      setPosition({ width, height, left: Math.max(8, Math.min(rect.left, window.innerWidth - width - 8)), top: down ? rect.bottom + 6 : Math.max(8, rect.top - actualHeight - 6) });
    }
    place();
    const onScroll = (event: Event) => { if (!card.current?.contains(event.target as Node)) { clearClose(); setOpen(false); } };
    const onWindowBlur = () => { clearClose(); setOpen(false); };
    window.addEventListener("resize", place);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("blur", onWindowBlur);
    return () => { window.removeEventListener("resize", place); window.removeEventListener("scroll", onScroll, true); window.removeEventListener("blur", onWindowBlur); };
  }, [open, rows.length]);
  if (!rows.length) return null;
  const limited = rows.filter((row) => row.status === "limited").length;
  const passed = rows.filter((row) => row.status === "passed").length;
  const pending = rows.filter((row) => ["pending", "running"].includes(row.status)).length;
  return <>
    <button ref={trigger} type="button" className="concurrency-results-trigger" aria-haspopup="dialog" aria-expanded={open} aria-controls={open ? id : undefined}
      onMouseEnter={show} onMouseLeave={closeSoon} onFocus={show} onBlur={blur} onKeyDown={triggerKey} onClick={show}>并发检测结果</button>
    {open && createPortal(<div ref={card} id={id} role="dialog" aria-label={`${providerName} 并发检测结果`} tabIndex={0}
      className="concurrency-results-card" style={{ left: position.left, top: position.top, width: position.width, maxHeight: position.height }}
      onMouseEnter={clearClose} onMouseLeave={closeSoon} onFocus={clearClose} onBlur={blur} onKeyDown={escape}>
      <div className="concurrency-results-heading"><strong>{providerName} · 并发检测结果</strong><span>{rows.length} 个 Key</span></div>
      <div className="concurrency-results-summary">响应重叠 {passed} · 并发受限 {limited} · 未判定 {rows.length - passed - limited - pending}{pending > 0 ? ` · 检测中/等待 ${pending}` : ""}</div>
      <div className="concurrency-results-list">{rows.map((row) => {
        const tone = row.status === "limited" ? "danger" : row.status === "passed" ? "success" : ["running", "pending"].includes(row.status) ? "pending" : "warning";
        const label = row.status === "passed" ? "响应重叠" : ["limited", "pending", "running"].includes(row.status) ? probeStatusLabel[row.status] : `未判定 · ${probeStatusLabel[row.status] ?? "未知状态"}`;
        return <div className="concurrency-result-row" key={row.credentialId}>
          <div className="concurrency-result-title"><strong>{row.label}</strong><span className={`concurrency-result-status concurrency-result-status--${tone}`}>{tone === "success" ? "✓ " : tone === "pending" ? "◷ " : "⚠ "}{label}</span></div>
          <div className="concurrency-result-meta">{row.model}{row.checkedAt ? ` · ${new Date(row.checkedAt).toLocaleTimeString()}` : ""}</div>
          {row.limitedAt && row.status !== "limited" && <div className="concurrency-result-history">上次检测受限，本次未确认恢复</div>}
        </div>;
      })}</div>
      <p className="concurrency-results-note">响应重叠仅为本次观察，不代表账号真实并发上限。结果在退出程序后清除。</p>
    </div>, document.body)}
  </>;
}
