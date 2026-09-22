import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { publishProbeState } from "../lib/concurrency";
import ProviderConcurrencyResults from "./ProviderConcurrencyResults";

beforeEach(() => {
  vi.useFakeTimers();
  publishProbeState({ running: false, runId: 1, warnings: [], rows: [
    { providerId: "kimi", credentialId: "a", label: "Kimi A", endpoint: "https://api.kimi.com", model: "kimi-for-coding", status: "quota" },
    { providerId: "minimax", credentialId: "b", label: "MiniMax B", endpoint: "https://api.minimaxi.com", model: "MiniMax-M2.5", status: "passed" },
  ] });
});
afterEach(() => { vi.useRealTimers(); });

it("shows only this provider's results on hover, not a permanent result list", () => {
  render(<ProviderConcurrencyResults providerId="kimi" providerName="Kimi" />);
  expect(screen.queryByText("Kimi A")).not.toBeInTheDocument();
  fireEvent.mouseEnter(screen.getByRole("button", { name: "并发检测结果" }));
  expect(screen.getByText("Kimi A")).toBeInTheDocument();
  expect(screen.queryByText("MiniMax B")).not.toBeInTheDocument();
  expect(screen.getByText(/额度不足/)).toBeInTheDocument();
});

it("keeps the card open across the gap and closes after leaving both regions", () => {
  render(<ProviderConcurrencyResults providerId="kimi" providerName="Kimi" />);
  const trigger = screen.getByRole("button", { name: "并发检测结果" });
  fireEvent.mouseEnter(trigger);
  fireEvent.mouseLeave(trigger);
  act(() => vi.advanceTimersByTime(100));
  const card = screen.getByRole("dialog", { name: "Kimi 并发检测结果" });
  fireEvent.mouseEnter(card);
  act(() => vi.advanceTimersByTime(500));
  expect(card).toBeInTheDocument();
  fireEvent.mouseLeave(card);
  act(() => vi.advanceTimersByTime(500));
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

it("supports focus and Escape without starting a new detection", () => {
  render(<ProviderConcurrencyResults providerId="kimi" providerName="Kimi" />);
  const trigger = screen.getByRole("button", { name: "并发检测结果" });
  fireEvent.focus(trigger);
  expect(screen.getByRole("dialog")).toBeInTheDocument();
  fireEvent.keyDown(trigger, { key: "Escape" });
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

it("does not show another provider's entry when this provider has no results", () => {
  render(<ProviderConcurrencyResults providerId="zai" providerName="GLM" />);
  expect(screen.queryByRole("button")).not.toBeInTheDocument();
});

it("lets keyboard users enter the scrollable card and return with Escape", () => {
  render(<ProviderConcurrencyResults providerId="kimi" providerName="Kimi" />);
  const trigger = screen.getByRole("button", { name: "并发检测结果" });
  act(() => trigger.focus());
  fireEvent.keyDown(trigger, { key: "ArrowDown" });
  const card = screen.getByRole("dialog");
  expect(card).toHaveFocus();
  fireEvent.keyDown(card, { key: "Escape" });
  expect(trigger).toHaveFocus();
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});
