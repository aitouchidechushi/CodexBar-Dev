import { expect, it } from "vitest";
import type { ProviderUsageSnapshot, RateWindowSnapshot } from "../types/bridge";
import {
  classifyRateWindow,
  formatMonthlyQuotaLabel,
  selectProviderQuotaWindows,
} from "./rateWindowKind";

function rate(windowMinutes: number | null): RateWindowSnapshot {
  return {
    usedPercent: 30,
    remainingPercent: 70,
    windowMinutes,
    resetsAt: null,
    resetDescription: null,
    isExhausted: false,
    reservePercent: null,
    reserveDescription: null,
  };
}

it("classifies five-hour, seven-day, and monthly quota windows", () => {
  expect(classifyRateWindow(undefined, rate(5 * 60))).toBe("short");
  expect(classifyRateWindow(undefined, rate(7 * 24 * 60))).toBe("weekly");
  expect(classifyRateWindow(undefined, rate(28 * 24 * 60))).toBe("monthly");
  expect(classifyRateWindow(undefined, rate(30 * 24 * 60))).toBe("monthly");
  expect(classifyRateWindow(undefined, rate(31 * 24 * 60))).toBe("monthly");
  expect(classifyRateWindow("Rate Limit", rate(null))).toBe("short");
  expect(classifyRateWindow("Five hours", rate(null))).toBe("short");
  expect(classifyRateWindow("5 hours", rate(null))).toBe("short");
  expect(classifyRateWindow("Weekly", rate(null))).toBe("weekly");
  expect(classifyRateWindow("Monthly", rate(null))).toBe("monthly");
  expect(classifyRateWindow("Monthly quota", rate(null))).toBe("monthly");
  expect(classifyRateWindow("30-day limit", rate(null))).toBe("monthly");
  expect(classifyRateWindow("月额度", rate(null))).toBe("monthly");
});

it("retains unusual Kimi timed quotas without relabeling them weekly", () => {
  const provider = {
    providerId: "kimi", primary: rate(1440), primaryLabel: "Quota",
    secondary: null, modelSpecific: null, tertiary: null, cost: null,
    extraRateWindows: [{ id: "kimi-code-limit-0", title: "Kimi quota", window: rate(360) }],
  } as ProviderUsageSnapshot;
  const windows = selectProviderQuotaWindows(provider);
  expect(windows.map((window) => window.kind)).toEqual(["ordinary", "ordinary"]);
  expect(windows.map((window) => window.snapshot.windowMinutes)).toEqual([1440, 360]);
});

it("does not classify monthly spend or statistics as monthly quota", () => {
  expect(classifyRateWindow("Monthly spend", rate(null))).toBeUndefined();
  expect(classifyRateWindow("Tokens (month)", rate(null))).toBeUndefined();
  expect(classifyRateWindow("Last 30 days", rate(null))).toBeUndefined();
  expect(classifyRateWindow("Monthly balance", rate(null))).toBeUndefined();
  expect(classifyRateWindow("Monthly budget", rate(null))).toBeUndefined();
  expect(classifyRateWindow("Monthly quota", {
    ...rate(null),
    isInformational: true,
  })).toBeUndefined();
  expect(classifyRateWindow(undefined, rate(27 * 24 * 60))).toBeUndefined();
  expect(classifyRateWindow(undefined, rate(32 * 24 * 60))).toBeUndefined();
});

it("keeps every monthly quota after the five-hour and weekly quotas", () => {
  const provider = {
    providerId: "doubao",
    displayName: "Doubao",
    primary: rate(5 * 60),
    primaryLabel: "5-hour",
    secondary: rate(7 * 24 * 60),
    secondaryLabel: "Weekly",
    modelSpecific: null,
    tertiary: rate(30 * 24 * 60),
    extraRateWindows: [
      { id: "agent-monthly", title: "Agent Monthly", window: rate(31 * 24 * 60) },
      { id: "monthly-spend", title: "Monthly spend", window: rate(null) },
      { id: "team-monthly", title: "Monthly · Team", window: rate(28 * 24 * 60) },
    ],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "api",
    updatedAt: "2026-08-13T00:00:00Z",
    error: null,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
  } as ProviderUsageSnapshot;

  const windows = selectProviderQuotaWindows(provider);
  expect(windows.map((window) => window.kind)).toEqual([
    "short",
    "weekly",
    "monthly",
    "monthly",
    "monthly",
  ]);
  expect(windows.map((window) => window.id)).toEqual([
    "primary",
    "secondary",
    "tertiary",
    "extra-agent-monthly",
    "extra-team-monthly",
  ]);
});

it("keeps GLM five-hour and weekly quotas in the standard provider slots", () => {
  const provider = {
    providerId: "zai",
    displayName: "GLM（智谱 BigModel / z.ai）",
    credentialId: "glm-key-1",
    credentialDisplayLabel: "Key 1",
    credentialDisplayOrdinal: 1,
    primary: { ...rate(5 * 60), usedPercent: 0, remainingPercent: 100 },
    primaryLabel: "Rate Limit",
    secondary: { ...rate(7 * 24 * 60), usedPercent: 92, remainingPercent: 8 },
    secondaryLabel: "Weekly",
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "api-key",
    updatedAt: "2026-08-25T00:00:00Z",
    error: null,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
  } as ProviderUsageSnapshot;

  expect(
    selectProviderQuotaWindows(provider).map(({ id, kind, snapshot }) => ({
      id,
      kind,
      usedPercent: snapshot.usedPercent,
    })),
  ).toEqual([
    { id: "primary", kind: "short", usedPercent: 0 },
    { id: "secondary", kind: "weekly", usedPercent: 92 },
  ]);
});

it("keeps real Grok and Chutes unknown-cycle slots ordinary without promoting them to monthly", () => {
  const grok = {
    providerId: "grok",
    displayName: "Grok",
    primary: { ...rate(null), usedPercent: 26, remainingPercent: 74 },
    primaryLabel: "Monthly",
    secondary: null,
    secondaryLabel: "On-demand",
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "api",
    updatedAt: "2026-08-24T00:00:00Z",
    error: null,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
  } as ProviderUsageSnapshot;
  const chutes = {
    ...grok,
    providerId: "chutes",
    displayName: "Chutes",
    primary: { ...rate(null), usedPercent: 14, remainingPercent: 86 },
    primaryLabel: "4-hour quota",
    secondary: { ...rate(null), usedPercent: 37, remainingPercent: 63 },
    secondaryLabel: "Monthly quota",
  } as ProviderUsageSnapshot;

  expect(selectProviderQuotaWindows(grok).map(({ id, kind }) => ({ id, kind }))).toEqual([
    { id: "primary", kind: "ordinary" },
  ]);
  expect(selectProviderQuotaWindows(chutes).map(({ id, kind }) => ({ id, kind }))).toEqual([
    { id: "primary", kind: "ordinary" },
    { id: "secondary", kind: "ordinary" },
  ]);

  grok.extraRateWindows = [
    { id: "parser-monthly", title: "Monthly quota", window: rate(null) },
  ];
  expect(selectProviderQuotaWindows(grok).map(({ id, kind }) => ({ id, kind }))).toEqual([
    { id: "primary", kind: "ordinary" },
    { id: "extra-parser-monthly", kind: "monthly" },
  ]);

  grok.primary = rate(30 * 24 * 60);
  expect(selectProviderQuotaWindows(grok).map(({ id, kind }) => ({ id, kind }))).toEqual([
    { id: "primary", kind: "monthly" },
    { id: "extra-parser-monthly", kind: "monthly" },
  ]);

  grok.primary = rate(null);
  grok.primaryLabel = "Rate Limit";
  grok.secondary = rate(null);
  grok.secondaryLabel = "Weekly";
  expect(selectProviderQuotaWindows(grok).map(({ id, kind }) => ({ id, kind }))).toEqual([
    { id: "primary", kind: "short" },
    { id: "secondary", kind: "weekly" },
    { id: "extra-parser-monthly", kind: "monthly" },
  ]);
});

it("rejects non-quota, informational, and unknown static slots instead of making them ordinary", () => {
  const base = {
    providerId: "openai",
    displayName: "OpenAI",
    primary: rate(5 * 60),
    primaryLabel: "Spend",
    secondary: null,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "api",
    updatedAt: "2026-08-24T00:00:00Z",
    error: null,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
  } as ProviderUsageSnapshot;
  const rejected = [
    base,
    {
      ...base,
      providerId: "bedrock",
      primaryLabel: "Budget",
      primary: rate(30 * 24 * 60),
    },
    { ...base, providerId: "openai", primaryLabel: "Token quota" },
    { ...base, providerId: "openai", primaryLabel: "Request limit" },
    { ...base, primaryLabel: "Monthly statistic", primary: rate(null) },
    { ...base, primaryLabel: "Monthly statistics", primary: rate(null) },
    { ...base, primaryLabel: "Monthly stat", primary: rate(null) },
    { ...base, primaryLabel: "Monthly stats", primary: rate(null) },
    { ...base, primaryLabel: "Monthly cost", primary: rate(null) },
    { ...base, primaryLabel: "Monthly costs", primary: rate(null) },
    { ...base, primaryLabel: "Quota activity", primary: rate(null) },
    { ...base, primaryLabel: "Quota activities", primary: rate(null) },
    { ...base, primaryLabel: "Monthly history", primary: rate(null) },
    { ...base, primaryLabel: "Monthly histories", primary: rate(null) },
    { ...base, primaryLabel: "Capacity", primary: rate(null) },
    {
      ...base,
      primaryLabel: "Monthly quota",
      primary: { ...rate(7 * 24 * 60), isInformational: true },
    },
  ] as ProviderUsageSnapshot[];

  for (const provider of rejected) {
    expect(
      selectProviderQuotaWindows(provider),
      `${provider.providerId}:${provider.primaryLabel}`,
    ).toEqual([]);
  }
});

it("synthesizes and clamps a finite monthly cost quota while preserving reset", () => {
  const provider = {
    providerId: "factory",
    displayName: "Factory",
    primary: rate(5 * 60),
    primaryLabel: "Five hours",
    secondary: null,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: {
      used: 125,
      limit: 100,
      remaining: 0,
      currencyCode: "USD",
      period: "Monthly",
      resetsAt: "2026-09-01T00:00:00Z",
      formattedUsed: "$125.00",
      formattedLimit: "$100.00",
    },
    planName: null,
    accountEmail: null,
    sourceLabel: "api",
    updatedAt: "2026-08-24T00:00:00Z",
    error: null,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
  } as ProviderUsageSnapshot;

  expect(selectProviderQuotaWindows(provider)).toEqual([
    {
      id: "primary",
      label: "Five hours",
      snapshot: rate(5 * 60),
      source: "primary",
      kind: "short",
    },
    {
      id: "cost-monthly",
      label: "Monthly",
      snapshot: {
        usedPercent: 100,
        remainingPercent: 0,
        windowMinutes: 30 * 24 * 60,
        resetsAt: "2026-09-01T00:00:00Z",
        resetDescription: null,
        isExhausted: true,
        isInformational: false,
        reservePercent: null,
        reserveDescription: null,
      },
      source: "cost",
      kind: "monthly",
    },
  ]);

  provider.cost!.used = -25;
  const synthesized = selectProviderQuotaWindows(provider)[1];
  expect(synthesized.snapshot.usedPercent).toBe(0);
  expect(synthesized.snapshot.remainingPercent).toBe(100);
  expect(synthesized.snapshot.isExhausted).toBe(false);
});

it("rejects invalid limits and monthly-looking cost statistics", () => {
  const provider = {
    providerId: "openrouter",
    displayName: "OpenRouter",
    primary: rate(5 * 60),
    primaryLabel: "Five hours",
    secondary: null,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: {
      used: 25,
      limit: 100,
      remaining: 75,
      currencyCode: "USD",
      period: "Monthly",
      resetsAt: null,
      formattedUsed: "$25.00",
      formattedLimit: "$100.00",
    },
    planName: null,
    accountEmail: null,
    sourceLabel: "api",
    updatedAt: "2026-08-24T00:00:00Z",
    error: null,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
  } as ProviderUsageSnapshot;
  const rejected: Array<[number | null, number, string]> = [
    [null, 25, "Monthly"],
    [0, 25, "Monthly"],
    [-1, 25, "Monthly"],
    [Number.NaN, 25, "Monthly"],
    [Number.POSITIVE_INFINITY, 25, "Monthly"],
    [100, Number.NaN, "Monthly"],
    [100, Number.POSITIVE_INFINITY, "Monthly"],
    [100, 25, "Last 30 days spend"],
    [100, 25, "Current month cost"],
    [100, 25, "Monthly history"],
    [100, 25, "Monthly tokens"],
    [100, 25, "Monthly requests"],
    [100, 25, "Monthly balance"],
    [100, 25, "Local monthly budget"],
  ];

  for (const [limit, used, period] of rejected) {
    provider.cost = { ...provider.cost!, limit, used, period };
    expect(
      selectProviderQuotaWindows(provider).map((window) => window.id),
      `${period} limit=${String(limit)} used=${String(used)}`,
    ).toEqual(["primary"]);
  }

  provider.providerId = "bedrock";
  provider.cost = { ...provider.cost!, limit: 100, used: 25, period: "Monthly" };
  expect(selectProviderQuotaWindows(provider).map((window) => window.id)).toEqual([
    "primary",
  ]);
});

it("formats account and scoped monthly quota labels consistently", () => {
  expect(formatMonthlyQuotaLabel("Monthly", "月额度")).toBe("月额度");
  expect(formatMonthlyQuotaLabel("30-day quota", "月额度")).toBe("月额度");
  expect(formatMonthlyQuotaLabel("Agent Monthly", "月额度")).toBe("月额度 · Agent");
  expect(formatMonthlyQuotaLabel("Monthly · GPT-5", "月额度")).toBe("月额度 · GPT-5");
  expect(formatMonthlyQuotaLabel(undefined, "月额度", "模型专属")).toBe(
    "月额度 · 模型专属",
  );
});
