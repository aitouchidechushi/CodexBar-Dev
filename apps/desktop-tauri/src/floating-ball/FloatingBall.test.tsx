import { fireEvent, render, screen, waitFor } from "@testing-library/react";
// @ts-expect-error Vitest provides Node built-ins; the browser app intentionally omits Node types.
import { readFileSync } from "node:fs";
// @ts-expect-error Vitest provides Node built-ins; the browser app intentionally omits Node types.
import { resolve } from "node:path";
import { afterAll, beforeAll, beforeEach, expect, it, vi } from "vitest";

declare const process: { cwd: () => string };

class TestPointerEvent extends MouseEvent {
  readonly pointerId: number;

  constructor(type: string, init: PointerEventInit = {}) {
    super(type, init);
    this.pointerId = init.pointerId ?? 0;
  }
}

beforeAll(() => vi.stubGlobal("PointerEvent", TestPointerEvent));
afterAll(() => vi.unstubAllGlobals());

const windowMocks = vi.hoisted(() => ({
  startDragging: vi.fn().mockResolvedValue(undefined),
}));
const quotaMocks = vi.hoisted(() => ({
  toggleFloatingQuota: vi.fn().mockResolvedValue(true),
}));
const tauriMocks = vi.hoisted(() => ({
  refreshProvidersIfStale: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ startDragging: windowMocks.startDragging }),
}));
vi.mock("../floating-quota/api", () => quotaMocks);
vi.mock("../lib/tauri", () => tauriMocks);

import FloatingBall from "./FloatingBall";

const floatingBallCss = readFileSync(
  resolve(process.cwd(), "src/floating-ball/FloatingBall.css"),
  "utf8",
);

beforeEach(() => {
  windowMocks.startDragging.mockClear();
  quotaMocks.toggleFloatingQuota.mockReset().mockResolvedValue(true);
  tauriMocks.refreshProvidersIfStale.mockReset().mockResolvedValue(undefined);
});

it("使用当前尺寸的 70% 作为窗口和球体尺寸", () => {
  expect(floatingBallCss).toMatch(/width:\s*42px/);
  expect(floatingBallCss).toMatch(/height:\s*42px/);
  expect(floatingBallCss).toMatch(/width:\s*39\.2px/);
  expect(floatingBallCss).toMatch(/height:\s*39\.2px/);
});

it("静止点击切换轻量额度窗口并在显示后检查过期数据", async () => {
  render(<FloatingBall />);
  const ball = screen.getByRole("button", { name: "CodexBar" });
  fireEvent.pointerDown(ball, { pointerId: 1, clientX: 20, clientY: 20 });
  expect(ball).toHaveClass("is-pressed");
  fireEvent.pointerUp(ball, { pointerId: 1, clientX: 20, clientY: 20 });
  expect(windowMocks.startDragging).not.toHaveBeenCalled();
  expect(ball).not.toHaveClass("is-pressed");
  await waitFor(() => {
    expect(quotaMocks.toggleFloatingQuota).toHaveBeenCalledTimes(1);
  });
  expect(tauriMocks.refreshProvidersIfStale).toHaveBeenCalledTimes(1);
});

it("指针移动超过阈值后只启动一次原生拖动", () => {
  render(<FloatingBall />);
  const ball = screen.getByRole("button", { name: "CodexBar" });
  fireEvent.pointerDown(ball, { pointerId: 2, clientX: 10, clientY: 10 });
  fireEvent.pointerMove(ball, { pointerId: 2, clientX: 12, clientY: 12 });
  expect(windowMocks.startDragging).not.toHaveBeenCalled();
  fireEvent.pointerMove(ball, { pointerId: 2, clientX: 20, clientY: 20 });
  expect(windowMocks.startDragging).toHaveBeenCalledTimes(1);
  fireEvent.pointerUp(ball, { pointerId: 2, clientX: 20, clientY: 20 });
  expect(quotaMocks.toggleFloatingQuota).not.toHaveBeenCalled();
});
