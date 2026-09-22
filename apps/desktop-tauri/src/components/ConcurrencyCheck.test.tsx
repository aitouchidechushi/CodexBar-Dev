import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
const bridge = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => bridge);
import ConcurrencyCheck, { ConcurrencyBadge } from "./ConcurrencyCheck";
import { publishProbeState } from "../lib/concurrency";

beforeEach(() => {
  publishProbeState({ running: false, runId: 0, rows: [], warnings: [] });
  bridge.invoke.mockReset();
  bridge.invoke.mockImplementation(async (command: string) => {
    if (command === "concurrency_preview") return { items: [
      { providerId: "kimi", credentialId: "a", label: "Kimi A", endpoint: "https://api.kimi.com/coding/v1/chat/completions" },
      { providerId: "minimax", credentialId: "b", label: "MiniMax B", endpoint: "https://api.minimaxi.com/v1/chat/completions" },
      { providerId: "zai", credentialId: "c", label: "GLM C", endpoint: "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions" },
    ], skipped: ["codex"], confirmationToken: "test-confirmation" };
    return { running: false, rows: [], warnings: [], runId: 0 };
  });
});

it("only restricted evidence shows on its own key card", () => {
  const row = { providerId: "kimi", credentialId: "a", label: "A", endpoint: "https://api.kimi.com", model: "kimi-for-coding", status: "limited", limitedAt: "2026-09-21T12:00:00Z" };
  publishProbeState({ running: false, runId: 1, rows: [row], warnings: [] });
  const view = render(<ConcurrencyBadge credentialId="b" />);
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  view.rerender(<ConcurrencyBadge credentialId="a" />);
  expect(screen.getByText("并发受限")).toBeInTheDocument();
  view.unmount();
  publishProbeState({ running: false, runId: 2, rows: [{ ...row, status: "passed", limitedAt: undefined }], warnings: [] });
  render(<ConcurrencyBadge credentialId="a" />);
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
});

it("preserves a restriction warning after an inconclusive recheck", () => {
  publishProbeState({ running: false, runId: 1, warnings: [], rows: [{ providerId: "kimi", credentialId: "a", label: "A", endpoint: "https://api.kimi.com", model: "kimi-for-coding", status: "inconclusive", limitedAt: "2026-09-21T12:00:00Z" }] });
  render(<ConcurrencyBadge credentialId="a" />);
  expect(screen.getByText(/上次结果，本次未确认恢复/)).toBeInTheDocument();
});

it("preview failure never starts a diagnostic", async () => {
  bridge.invoke.mockImplementation(async (command: string) => {
    if (command === "concurrency_preview") throw new Error("store unavailable");
    return { running: false, rows: [], warnings: [], runId: 0 };
  });
  render(<ConcurrencyCheck />);
  fireEvent.click(screen.getByRole("button", { name: "检测并发" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("无法读取检测列表");
  expect(bridge.invoke.mock.calls.some(([name]) => name === "concurrency_start")).toBe(false);
});

it("does not start automatically and a single click starts without a confirmation dialog", async () => {
  render(<ConcurrencyCheck />);
  expect(bridge.invoke.mock.calls.some(([name]) => name === "concurrency_start")).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "检测并发" }));
  await waitFor(() => expect(bridge.invoke.mock.calls.filter(([name]) => name === "concurrency_start")).toHaveLength(1));
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  expect(screen.getAllByRole("button", { name: "检测并发" })).toHaveLength(1);
});

it("a click sends one batch with default models, never API keys", async () => {
  render(<ConcurrencyCheck />);
  fireEvent.click(screen.getByRole("button", { name: "检测并发" }));
  await waitFor(() => expect(bridge.invoke.mock.calls.filter(([name]) => name === "concurrency_start")).toHaveLength(1));
  const args = bridge.invoke.mock.calls.find(([name]) => name === "concurrency_start")?.[1];
  expect(args.options.models.kimi).toBe("kimi-for-coding");
  expect(args.options.credentialIds).toEqual(["a", "b", "c"]);
  expect(args.options.confirmationToken).toBe("test-confirmation");
  expect(JSON.stringify(args)).not.toMatch(/secret|apiKey/);
});

it("does not start an empty batch", async () => {
  bridge.invoke.mockImplementation(async (command: string) => command === "concurrency_preview"
    ? { items: [], skipped: ["codex"], confirmationToken: "empty" }
    : { running: false, rows: [], warnings: [], runId: 0 });
  render(<ConcurrencyCheck />);
  fireEvent.click(screen.getByRole("button", { name: "检测并发" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("没有可检测的 Key");
  expect(bridge.invoke.mock.calls.some(([name]) => name === "concurrency_start")).toBe(false);
});

it("ignores repeated clicks while preparing the batch", async () => {
  const original = bridge.invoke.getMockImplementation()!;
  let finish: (value: unknown) => void = () => {};
  bridge.invoke.mockImplementation((command: string) => command === "concurrency_preview"
    ? new Promise((resolve) => { finish = resolve; }) : original(command));
  render(<ConcurrencyCheck />);
  const button = screen.getByRole("button", { name: "检测并发" });
  fireEvent.click(button);
  fireEvent.click(button);
  finish({ items: [{ providerId: "kimi", credentialId: "a", label: "A", endpoint: "https://api.kimi.com" }], skipped: [], confirmationToken: "test" });
  await waitFor(() => expect(bridge.invoke.mock.calls.filter(([name]) => name === "concurrency_start")).toHaveLength(1));
});
