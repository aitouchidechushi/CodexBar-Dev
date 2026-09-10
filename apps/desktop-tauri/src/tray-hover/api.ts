import { invoke } from "@tauri-apps/api/core";

export const TRAY_HOVER_WINDOW_LABEL = "tray-hover";

export function setTrayHoverPointerInside(inside: boolean): Promise<void> {
  return invoke<void>("set_tray_hover_pointer_inside", { inside });
}

export function resizeTrayHover(width: number, height: number): Promise<void> {
  return invoke<void>("resize_tray_hover", { width, height });
}
