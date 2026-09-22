import { invoke } from "@tauri-apps/api/core";
import { useSyncExternalStore } from "react";
export interface ProbeItem { providerId: string; credentialId: string; label: string; endpoint: string }
export interface ProbeRow extends ProbeItem { status: string; checkedAt?: string; limitedAt?: string; model: string }
export interface ProbeState { running: boolean; runId: number; rows: ProbeRow[]; warnings: string[] }
export interface ProbePreview { items: ProbeItem[]; skipped: string[]; confirmationToken: string }
let snapshot: ProbeState = { running: false, runId: 0, rows: [], warnings: [] };
const subscribers = new Set<() => void>();
export function publishProbeState(value: ProbeState) {
  if (!value || !Array.isArray(value.rows)) return;
  snapshot = value; subscribers.forEach((fn) => fn());
}
export function useProbeState() {
  return useSyncExternalStore((fn) => { subscribers.add(fn); return () => { subscribers.delete(fn); }; }, () => snapshot);
}
export async function refreshProbeState() { publishProbeState(await invoke<ProbeState>("concurrency_status")); }
export const probeStatusLabel: Record<string, string> = {
  pending: "等待检测", running: "检测中", passed: "本次观察到双请求响应重叠", limited: "并发受限",
  inconclusive: "无法确认双请求重叠", quota: "额度不足", auth: "密钥或调用权限异常",
  rateLimited: "请求限流（未确认并发限制）", busy: "服务端繁忙", failed: "请求失败或响应不符合预期",
  cancelled: "已取消", changed: "配置或 Key 已变化，请重新检测", unsupported: "暂未适配",
};
