import { invoke } from "@tauri-apps/api/core";

export type FloatingQuotaMode = "normal" | "topmost" | "desktop";

export function toggleFloatingQuota(): Promise<boolean> {
  return invoke<boolean>("toggle_floating_quota");
}

export function hideFloatingQuota(): Promise<void> {
  return invoke<void>("hide_floating_quota");
}

export function openMainWindowFromFloatingQuota(): Promise<void> {
  return invoke<void>("open_main_window_from_floating_quota");
}

export function resizeFloatingQuota(height: number): Promise<void> {
  return invoke<void>("resize_floating_quota", { height });
}

export function startFloatingQuotaDrag(): Promise<void> {
  return invoke<void>("start_floating_quota_drag");
}

export function toggleFloatingQuotaTopmost(): Promise<FloatingQuotaMode> {
  return invoke<FloatingQuotaMode>("toggle_floating_quota_topmost");
}

export function toggleFloatingQuotaDesktop(): Promise<FloatingQuotaMode> {
  return invoke<FloatingQuotaMode>("toggle_floating_quota_desktop");
}
