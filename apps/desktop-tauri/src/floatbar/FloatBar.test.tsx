import { act, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const tauriMocks = vi.hoisted(() => ({
  getCachedProviders: vi.fn(),
  getProviderChartData: vi.fn(),
  getProviderLocalUsageSummary: vi.fn(),
  refreshProviders: vi.fn(),
  refreshProvidersIfStale: vi.fn(),
  getSettingsSnapshot: vi.fn(),
  updateSettings: vi.fn(),
  getLocaleStrings: vi.fn(),
  setUiLanguage: vi.fn(),
  listKimiAccounts: vi.fn(),
}));

const eventMocks = vi.hoisted(() => ({
  listen: vi.fn(),
}));

const windowMocks = vi.hoisted(() => ({
  getCurrentWindow: vi.fn(() => ({
    startDragging: vi.fn().mockResolvedValue(undefined),
  })),
}));

const coreMocks = vi.hoisted(() => ({
  invoke: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("../lib/tauri", () => tauriMocks);
vi.mock("@tauri-apps/api/event", () => eventMocks);
vi.mock("@tauri-apps/api/window", () => windowMocks);
vi.mock("@tauri-apps/api/core", () => coreMocks);

import FloatBar from "./FloatBar";
import { LocaleProvider } from "../i18n/LocaleProvider";
import { buildBundle } from "../test/localeHarness";
import type { BootstrapState, ProviderUsageSnapshot, SettingsSnapshot } from "../types/bridge";

function rateWindow(
  used: number,
  opts: {
    exhausted?: boolean;
    resetsAt?: string | null;
    resetDescription?: string | null;
  } = {},
) {
  return {
    usedPercent: used,
    remainingPercent: 100 - used,
    windowMinutes: null,
    resetsAt: opts.resetsAt ?? null,
    resetDescription: opts.resetDescription ?? null,
    isExhausted: opts.exhausted ?? false,
    reservePercent: null,
    reserveDescription: null,
  };
}

function snapshot(
  id: string,
  display: string,
  used: number,
  opts: {
    exhausted?: boolean;
    error?: string | null;
    resetsAt?: string | null;
    resetDescription?: string | null;
    credentialId?: string;
    credentialDisplayLabel?: string;
    credentialDisplayOrdinal?: number;
  } = {},
): ProviderUsageSnapshot {
  return {
    providerId: id,
    displayName: display,
    credentialId: opts.credentialId,
    credentialDisplayLabel: opts.credentialDisplayLabel,
    credentialDisplayOrdinal: opts.credentialDisplayOrdinal,
    primary: rateWindow(used, opts),
    secondary: null,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "auto",
    updatedAt: "2026-05-15T00:00:00Z",
    error: opts.error ?? null,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
  };
}

function settings(overrides: Partial<SettingsSnapshot> = {}): SettingsSnapshot {
  return {
    enabledProviders: ["claude", "codex"],
    refreshIntervalSecs: 300,
    adaptiveRefresh: false,
    refreshAllProvidersOnMenuOpen: false,
    startAtLogin: false,
    startMinimized: false,
    showNotifications: true,
    soundEnabled: true,
    soundVolume: 100,
    highUsageThreshold: 70,
    criticalUsageThreshold: 90,
    predictivePaceWarningEnabled: false,
    trayIconMode: "single",
    switcherShowsIcons: true,
    menuBarShowsHighestUsage: false,
    menuBarShowsPercent: false,
    showAsUsed: true,
    showAllTokenAccountsInMenu: false,
    enableAnimations: true,
    resetTimeRelative: true,
    showResetWhenExhausted: false,
    menuBarDisplayMode: "detailed",
    hidePersonalInfo: false,
    updateChannel: "stable",
    autoDownloadUpdates: false,
    installUpdatesOnQuit: false,
    globalShortcut: "Ctrl+Shift+U",
    codexCustomSessionsDirs: [],
    uiLanguage: "english",
    theme: "dark",
    windowScalePercent: 125,
    trayScalePercent: 100,
    powertoysStatusPipeEnabled: false,
    claudeAvoidKeychainPrompts: false,
    codexSparkUsageVisible: true,
    disableKeychainAccess: false,
    providerMetrics: {},
    floatBarEnabled: true,
    floatBarOpacity: 80,
    floatBarScale: 100,
    floatBarOrientation: "horizontal",
    floatBarStyle: "floating",
    floatBarClickThrough: false,
    floatBarProviderIds: [],
    floatBarDarkText: false,
    floatBarShowResetInline: false,
    floatBarShowCost: false,
    ...overrides,
  };
}

function bootstrap(settingsOverrides: Partial<SettingsSnapshot> = {}): BootstrapState {
  return {
    contractVersion: "v1",
    providers: [],
    settings: settings(settingsOverrides),
  };
}

function renderFloatBar(state: BootstrapState) {
  return render(
    <LocaleProvider>
      <FloatBar state={state} />
    </LocaleProvider>,
  );
}

describe("FloatBar", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    coreMocks.invoke.mockResolvedValue(undefined);
    tauriMocks.refreshProviders.mockResolvedValue(undefined);
    tauriMocks.refreshProvidersIfStale.mockResolvedValue(undefined);
    tauriMocks.getProviderLocalUsageSummary.mockResolvedValue(null);
    tauriMocks.listKimiAccounts.mockResolvedValue([]);
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        ResetsInHoursMinutes: "Resets in {}h {}m",
        ResetsInDaysHours: "Resets in {}d {}h",
        TrayResetsDueNow: "Resetting",
        PanelToday: "Today",
        PanelUsedSuffix: "used",
        FloatBarThirtyDayShort: "30d",
        FloatBarNoProviders: "No providers",
        FloatBarRemainingSuffix: "remaining",
        ApiKeyCount: "{} keys",
        TrayStatusRowError: "Error",
        KimiAccountMonthlyQuota: "月额度",
        ProviderMonthlyLabel: "月额度",
        KimiAccountUnlinkedApiKeys: "未关联 Kimi 月额度",
      }),
    );
    eventMocks.listen.mockResolvedValue(() => {});
  });

  it("renders a pill per enabled provider, sorted by usage descending", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 20),
      snapshot("codex", "Codex", 75),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ floatBarShowCost: true }),
    );

    const { container } = renderFloatBar(bootstrap());
    await waitFor(() => {
      const pills = container.querySelectorAll(".floatbar__pill");
      expect(pills.length).toBe(2);
    });

    const titles = Array.from(container.querySelectorAll(".floatbar__pill")).map(
      (el) => el.getAttribute("title") ?? "",
    );
    // Highest used (codex, 75%) shows first; display follows showAsUsed.
    expect(titles[0]).toMatch(/Codex: 75% used/);
    expect(titles[1]).toMatch(/Claude: 20% used/);
  });

  it("renders an ungrouped primary monthly quota as one monthly pill", async () => {
    const primaryMonthly = {
      ...snapshot("commandcode", "Command Code", 25),
      primaryLabel: "Monthly",
      primary: { ...rateWindow(25), windowMinutes: 30 * 24 * 60 },
    } satisfies ProviderUsageSnapshot;
    const overrides = {
      enabledProviders: ["commandcode"],
      providerOrder: ["commandcode"],
      floatBarProviderIds: ["commandcode"],
    } satisfies Partial<SettingsSnapshot>;
    tauriMocks.getCachedProviders.mockResolvedValue([primaryMonthly]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings(overrides));

    const { container } = renderFloatBar(bootstrap(overrides));

    await waitFor(() => expect(container.querySelectorAll(".floatbar__pill")).toHaveLength(1));
    const pill = container.querySelector(".floatbar__pill");
    expect(pill).toHaveClass("floatbar__monthly-pill");
    expect(within(pill as HTMLElement).getByRole("progressbar", { name: "月额度" }))
      .toHaveAttribute("aria-valuenow", "25");
  });

  it("keeps real Grok and Chutes unknown-cycle percentages as ordinary pills", async () => {
    const grok = {
      ...snapshot("grok", "Grok", 26),
      primaryLabel: "Monthly",
    } satisfies ProviderUsageSnapshot;
    const chutes = {
      ...snapshot("chutes", "Chutes", 14),
      primaryLabel: "4-hour quota",
      secondary: rateWindow(37),
      secondaryLabel: "Monthly quota",
    } satisfies ProviderUsageSnapshot;
    const overrides = {
      enabledProviders: ["grok", "chutes"],
      providerOrder: ["grok", "chutes"],
      floatBarProviderIds: ["grok", "chutes"],
    } satisfies Partial<SettingsSnapshot>;
    tauriMocks.getCachedProviders.mockResolvedValue([grok, chutes]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings(overrides));

    const { container } = renderFloatBar(bootstrap(overrides));

    await waitFor(() => expect(container.querySelectorAll(".floatbar__pill")).toHaveLength(2));
    expect(screen.getByText("26%")).toBeInTheDocument();
    expect(screen.getByText("14%")).toBeInTheDocument();
    expect(screen.getByText("37%")).toBeInTheDocument();
    expect(container.querySelector(".floatbar__monthly-pill")).not.toBeInTheDocument();
    expect(container.querySelector(".monthly-quota-badge")).not.toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
  });

  it("keeps short and weekly capacity in one pill and uses the most severe tone", async () => {
    const provider = {
      ...snapshot("codex", "Codex", 20),
      primaryLabel: "5-hour limit",
      primary: { ...rateWindow(20), windowMinutes: 5 * 60 },
      secondaryLabel: "Weekly",
      secondary: { ...rateWindow(95), windowMinutes: 7 * 24 * 60 },
    } satisfies ProviderUsageSnapshot;
    const overrides = {
      enabledProviders: ["codex"],
      providerOrder: ["codex"],
      floatBarProviderIds: ["codex"],
    } satisfies Partial<SettingsSnapshot>;
    tauriMocks.getCachedProviders.mockResolvedValue([provider]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings(overrides));

    const { container } = renderFloatBar(bootstrap(overrides));

    await waitFor(() => expect(container.querySelectorAll(".floatbar__pill")).toHaveLength(1));
    const capacity = container.querySelector(".floatbar__capacity-pill") as HTMLElement;
    expect(capacity).not.toBeNull();
    expect(within(capacity).getByText("5-hour limit")).toBeInTheDocument();
    expect(within(capacity).getByText("20%")).toBeInTheDocument();
    expect(within(capacity).getByText("Weekly")).toBeInTheDocument();
    expect(within(capacity).getByText("95%")).toBeInTheDocument();
    expect(capacity).toHaveClass("floatbar__pill--crit");
  });

  it("shows GLM five-hour and weekly quotas in its credential pill", async () => {
    const provider = {
      ...snapshot("zai", "GLM（智谱 BigModel / z.ai）", 0, {
        credentialId: "glm-key-1",
        credentialDisplayLabel: "Key 1",
        credentialDisplayOrdinal: 1,
      }),
      primaryLabel: "Rate Limit",
      primary: { ...rateWindow(0), windowMinutes: 5 * 60 },
      secondaryLabel: "Weekly",
      secondary: { ...rateWindow(92), windowMinutes: 7 * 24 * 60 },
    } satisfies ProviderUsageSnapshot;
    const overrides = {
      enabledProviders: ["zai"],
      providerOrder: ["zai"],
      floatBarProviderIds: ["zai"],
    } satisfies Partial<SettingsSnapshot>;
    tauriMocks.getCachedProviders.mockResolvedValue([provider]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings(overrides));

    const { container } = renderFloatBar(bootstrap(overrides));

    await waitFor(() => expect(container.querySelectorAll(".floatbar__pill")).toHaveLength(1));
    const capacity = container.querySelector(".floatbar__capacity-pill") as HTMLElement;
    expect(capacity).not.toBeNull();
    expect(capacity).toHaveTextContent("Key 1");
    expect(capacity).toHaveTextContent("Rate Limit");
    expect(capacity).toHaveTextContent("0%");
    expect(capacity).toHaveTextContent("Weekly");
    expect(capacity).toHaveTextContent("92%");
  });

  it("shows weekly capacity beside a primary monthly meter", async () => {
    const provider = {
      ...snapshot("commandcode", "Command Code", 40),
      primaryLabel: "Monthly",
      primary: { ...rateWindow(40), windowMinutes: 30 * 24 * 60 },
      secondaryLabel: "Weekly",
      secondary: { ...rateWindow(55), windowMinutes: 7 * 24 * 60 },
    } satisfies ProviderUsageSnapshot;
    const overrides = {
      enabledProviders: ["commandcode"],
      providerOrder: ["commandcode"],
      floatBarProviderIds: ["commandcode"],
    } satisfies Partial<SettingsSnapshot>;
    tauriMocks.getCachedProviders.mockResolvedValue([provider]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings(overrides));

    const { container } = renderFloatBar(bootstrap(overrides));

    await waitFor(() => expect(container.querySelectorAll(".floatbar__pill")).toHaveLength(2));
    const capacity = container.querySelector(".floatbar__capacity-pill") as HTMLElement;
    expect(within(capacity).getByText("Weekly")).toBeInTheDocument();
    expect(within(capacity).getByText("55%")).toBeInTheDocument();
    expect(within(container).getByRole("progressbar", { name: "月额度" }))
      .toHaveAttribute("aria-valuenow", "40");
  });

  it("renders a provider-account child primary monthly quota as one monthly pill", async () => {
    const groupedPrimaryMonthly = {
      ...snapshot("commandcode", "Command Code", 40, {
        credentialId: "00000000-0000-0000-0000-000000000001",
        credentialDisplayLabel: "Work",
        credentialDisplayOrdinal: 1,
      }),
      accountGroupId: "commandcode:account-1",
      accountEmail: "group@example.com",
      primaryLabel: "Monthly",
      primary: { ...rateWindow(40), windowMinutes: 30 * 24 * 60 },
    } satisfies ProviderUsageSnapshot;
    const overrides = {
      enabledProviders: ["commandcode"],
      providerOrder: ["commandcode"],
      floatBarProviderIds: ["commandcode"],
    } satisfies Partial<SettingsSnapshot>;
    tauriMocks.getCachedProviders.mockResolvedValue([groupedPrimaryMonthly]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings(overrides));

    renderFloatBar(bootstrap(overrides));

    const account = await screen.findByRole("region", { name: "group@example.com" });
    expect(account.querySelectorAll(".floatbar__pill")).toHaveLength(1);
    expect(account.querySelector(".floatbar__pill")).toHaveClass("floatbar__monthly-pill");
    expect(within(account).getByRole("progressbar", { name: "月额度 · Work" }))
      .toHaveAttribute("aria-valuenow", "40");
  });

  it("loads local cost summaries without using the foreground chart endpoint", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("codex", "Codex", 75),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());
    tauriMocks.getProviderLocalUsageSummary.mockResolvedValue({
      todayCost: 1.25,
      thirtyDayCost: 12.5,
      thirtyDayTokens: 1000,
      latestTokens: 200,
      topModel: "gpt-5",
      estimateNote: "Estimated from local logs",
      tokenCostUpdatedAtMs: 1234,
    });

    renderFloatBar(bootstrap({ floatBarShowCost: true }));

    await waitFor(() => {
      expect(tauriMocks.getProviderLocalUsageSummary).toHaveBeenCalledWith("codex");
    });
    expect(tauriMocks.getProviderChartData).not.toHaveBeenCalled();
  });

  it("renders every successful credential as an independent labeled pill and loads local cost once", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("openrouter", "OpenRouter", 91, {
        credentialId: "00000000-0000-0000-0000-000000000001",
        credentialDisplayLabel: "Work",
        credentialDisplayOrdinal: 1,
        resetDescription: "5m",
      }),
      snapshot("openrouter", "OpenRouter", 12, {
        credentialId: "00000000-0000-0000-0000-000000000002",
        credentialDisplayLabel: "Personal",
        credentialDisplayOrdinal: 2,
        resetDescription: "2h",
      }),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ enabledProviders: ["openrouter"], floatBarShowCost: true, floatBarShowResetInline: true }),
    );

    const { container } = renderFloatBar(
      bootstrap({
        enabledProviders: ["openrouter"],
        floatBarShowCost: true,
        floatBarShowResetInline: true,
      }),
    );

    await waitFor(() => {
      expect(container.querySelectorAll(".floatbar__pill")).toHaveLength(2);
    });
    const pills = Array.from(container.querySelectorAll(".floatbar__pill"));
    expect(pills.map((pill) => pill.textContent)).toEqual([
      expect.stringContaining("Work91%"),
      expect.stringContaining("Personal12%"),
    ]);
    expect(pills.map((pill) => pill.getAttribute("title"))).toEqual([
      expect.stringContaining("OpenRouter — Work: 91% used"),
      expect.stringContaining("OpenRouter — Personal: 12% used"),
    ]);
    expect(pills.every((pill) => pill.querySelector(".floatbar__reset") != null)).toBe(true);
    await waitFor(() => {
      expect(tauriMocks.getProviderLocalUsageSummary).toHaveBeenCalledTimes(1);
      expect(tauriMocks.getProviderLocalUsageSummary).toHaveBeenCalledWith("openrouter");
    });
  });

  it("visually groups Kimi account monthly quota with matched key pills and isolates unlinked keys", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("kimi", "Kimi", 20, {
        credentialId: "00000000-0000-0000-0000-000000000001",
        credentialDisplayLabel: "Work",
        credentialDisplayOrdinal: 1,
      }),
      snapshot("kimi", "Kimi", 30, {
        credentialId: "00000000-0000-0000-0000-000000000002",
        credentialDisplayLabel: "Personal",
        credentialDisplayOrdinal: 2,
      }),
      snapshot("kimi", "Kimi", 40, {
        credentialId: "00000000-0000-0000-0000-000000000003",
        credentialDisplayLabel: "Unlinked",
        credentialDisplayOrdinal: 3,
      }),
    ]);
    tauriMocks.listKimiAccounts.mockResolvedValue([
      {
        accountId: "account-a",
        displayName: "账号 A",
        sourceLabels: ["Edge · Profile 1"],
        usedPercent: 35,
        resetsAt: null,
        updatedAt: "2026-08-23T00:00:00Z",
        status: "ok",
        matchedCredentialIds: ["00000000-0000-0000-0000-000000000001"],
      },
      {
        accountId: "account-b",
        displayName: "账号 B",
        sourceLabels: ["Chrome · Default"],
        usedPercent: 50,
        resetsAt: null,
        updatedAt: "2026-08-23T00:00:00Z",
        status: "ok",
        matchedCredentialIds: ["00000000-0000-0000-0000-000000000002"],
      },
    ]);
    const overrides = {
      enabledProviders: ["kimi"],
      providerOrder: ["kimi"],
      kimiMonthlyQuotaEnabled: true,
      floatBarProviderIds: ["kimi"],
    } satisfies Partial<SettingsSnapshot>;
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings(overrides));

    renderFloatBar(bootstrap(overrides));

    const accountA = await screen.findByRole("region", { name: "账号 A" });
    const accountB = screen.getByRole("region", { name: "账号 B" });
    const unlinked = screen.getByRole("region", { name: "未关联 Kimi 月额度" });
    expect(within(accountA).getByText("Work")).toBeInTheDocument();
    expect(within(accountA).queryByText("Personal")).not.toBeInTheDocument();
    expect(within(accountA).getByLabelText("月额度")).toHaveTextContent("M");
    expect(within(accountA).getByRole("progressbar", { name: "账号 A 月额度" })).toHaveAttribute(
      "aria-valuenow",
      "35",
    );
    expect(within(accountB).getByText("Personal")).toBeInTheDocument();
    expect(within(unlinked).getByText("Unlinked")).toBeInTheDocument();
  });

  it("groups stable provider accounts, masks email, and deduplicates only audited monthly quota", async () => {
    const groupedCredential = (label: string, ordinal: number, monthly: number) => ({
      ...snapshot("clinepass", "ClinePass", 10 + ordinal, {
        credentialId: `00000000-0000-0000-0000-00000000000${ordinal}`,
        credentialDisplayLabel: label,
        credentialDisplayOrdinal: ordinal,
      }),
      accountGroupId: "clinepass:account-42",
      accountEmail: "owner@example.com",
      primaryLabel: "Rate Limit",
      primary: { ...rateWindow(10 + ordinal), windowMinutes: 5 * 60 },
      secondaryLabel: "Weekly",
      secondary: { ...rateWindow(20 + ordinal), windowMinutes: 7 * 24 * 60 },
      tertiary: { ...rateWindow(45), windowMinutes: 30 * 24 * 60 },
      extraRateWindows: [{
        id: `member-${ordinal}-monthly`,
        title: `Agent ${ordinal} Monthly`,
        window: { ...rateWindow(monthly), windowMinutes: 31 * 24 * 60 },
      }],
    }) satisfies ProviderUsageSnapshot;
    const first = groupedCredential("Work", 1, 61);
    const second = groupedCredential("Personal", 2, 72);
    const unmatched = {
      ...groupedCredential("Unmatched Key", 3, 83),
      accountGroupId: undefined,
      accountEmail: null,
      tertiary: null,
    } satisfies ProviderUsageSnapshot;
    tauriMocks.getCachedProviders.mockResolvedValue([first, second, unmatched]);
    const overrides = {
      enabledProviders: ["clinepass"],
      providerOrder: ["clinepass"],
      floatBarProviderIds: ["clinepass"],
      hidePersonalInfo: true,
      showAsUsed: false,
    } satisfies Partial<SettingsSnapshot>;
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings(overrides));

    const { container } = renderFloatBar(bootstrap(overrides));

    const account = await screen.findByRole("region", { name: "o••••@example.com" });
    expect(account).toHaveClass("floatbar__provider-account-group");
    expect(within(account).getByText("Work")).toBeInTheDocument();
    expect(within(account).getByText("Personal")).toBeInTheDocument();
    expect(within(account).getByText("月额度")).toHaveClass("monthly-quota-badge");
    expect(within(account).getByText("月额度 · Agent 1")).toHaveClass("monthly-quota-badge");
    expect(within(account).getByText("月额度 · Agent 2")).toHaveClass("monthly-quota-badge");
    expect(within(account).getAllByRole("progressbar")).toHaveLength(3);
    const capacityPills = account.querySelectorAll(".floatbar__capacity-pill");
    expect(capacityPills).toHaveLength(2);
    expect(capacityPills[0]).toHaveTextContent("Work");
    expect(capacityPills[0]).toHaveTextContent("Rate Limit");
    expect(capacityPills[0]).toHaveTextContent("89%");
    expect(capacityPills[0]).toHaveTextContent("Weekly");
    expect(capacityPills[0]).toHaveTextContent("79%");
    expect(capacityPills[1]).toHaveTextContent("Personal");
    expect(capacityPills[1]).toHaveTextContent("Rate Limit");
    expect(capacityPills[1]).toHaveTextContent("88%");
    expect(capacityPills[1]).toHaveTextContent("Weekly");
    expect(capacityPills[1]).toHaveTextContent("78%");
    expect(within(account).getAllByText("月额度", { exact: true })).toHaveLength(1);
    expect(within(account).getByRole("progressbar", { name: "月额度 · Agent 1" }))
      .toHaveAttribute("aria-valuenow", "39");
    expect(account).not.toHaveTextContent("Unmatched Key");
    expect(screen.getByText("Unmatched Key")).toBeInTheDocument();
    expect(container).not.toHaveTextContent("clinepass:account-42");
    expect(container).not.toHaveTextContent("owner@example.com");
  });

  it("resizes before initial completion and completes exactly once across observer callbacks", async () => {
    let observerCallback: ResizeObserverCallback | null = null;
    class TestResizeObserver {
      constructor(callback: ResizeObserverCallback) {
        observerCallback = callback;
      }
      observe() {}
      disconnect() {}
      unobserve() {}
    }
    vi.stubGlobal("ResizeObserver", TestResizeObserver);
    try {
      tauriMocks.getCachedProviders.mockResolvedValue([snapshot("claude", "Claude", 20)]);
      tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());

      renderFloatBar(bootstrap());

      await waitFor(() => {
        expect(coreMocks.invoke).toHaveBeenCalledWith("complete_float_bar_initial_show");
      });
      const commands = coreMocks.invoke.mock.calls.map(([command]) => command);
      expect(commands.indexOf("resize_float_bar")).toBeGreaterThanOrEqual(0);
      expect(commands.indexOf("complete_float_bar_initial_show"))
        .toBeGreaterThan(commands.indexOf("resize_float_bar"));
      expect(commands).not.toContain("show_float_bar");

      act(() => {
        observerCallback?.([], {} as ResizeObserver);
        observerCallback?.([], {} as ResizeObserver);
      });
      await act(async () => Promise.resolve());
      expect(coreMocks.invoke.mock.calls.filter(
        ([command]) => command === "complete_float_bar_initial_show",
      ))
        .toHaveLength(1);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("completes once after the first resize rejects", async () => {
    coreMocks.invoke.mockImplementation((command: string) =>
      command === "resize_float_bar"
        ? Promise.reject(new Error("resize failed"))
        : Promise.resolve(undefined),
    );
    tauriMocks.getCachedProviders.mockResolvedValue([snapshot("claude", "Claude", 20)]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());

    renderFloatBar(bootstrap());

    await waitFor(() => {
      expect(coreMocks.invoke).toHaveBeenCalledWith("complete_float_bar_initial_show");
    });
    const commands = coreMocks.invoke.mock.calls.map(([command]) => command);
    expect(commands.indexOf("complete_float_bar_initial_show"))
      .toBeGreaterThan(commands.indexOf("resize_float_bar"));
    expect(commands.filter((command) => command === "complete_float_bar_initial_show"))
      .toHaveLength(1);
    expect(commands).not.toContain("show_float_bar");
  });

  it("reports a rejected initial completion without retrying or changing command order", async () => {
    const showError = new Error("show failed");
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    coreMocks.invoke.mockImplementation((command: string) =>
      command === "complete_float_bar_initial_show"
        ? Promise.reject(showError)
        : Promise.resolve(undefined),
    );
    tauriMocks.getCachedProviders.mockResolvedValue([snapshot("claude", "Claude", 20)]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());

    try {
      renderFloatBar(bootstrap());

      await waitFor(() => {
        expect(consoleError).toHaveBeenCalledWith(
          "FloatBar failed to complete its initial show",
          showError,
        );
      });
      const commands = coreMocks.invoke.mock.calls.map(([command]) => command);
      expect(commands.indexOf("resize_float_bar")).toBeGreaterThanOrEqual(0);
      expect(commands.indexOf("complete_float_bar_initial_show"))
        .toBeGreaterThan(commands.indexOf("resize_float_bar"));
      expect(commands.filter((command) => command === "complete_float_bar_initial_show"))
        .toHaveLength(1);
      expect(commands).not.toContain("show_float_bar");
    } finally {
      consoleError.mockRestore();
    }
  });

  it("keeps healthy and failed credentials independently visible without exposing raw errors", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("codex", "Codex", 84, {
        credentialId: "00000000-0000-0000-0000-000000000001",
        credentialDisplayOrdinal: 1,
        resetDescription: "5m",
      }),
      snapshot("codex", "Codex", 0, {
        credentialId: "00000000-0000-0000-0000-000000000002",
        credentialDisplayOrdinal: 2,
        error: "secret backend failure",
      }),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ enabledProviders: ["codex"], floatBarShowResetInline: true }),
    );

    const { container } = renderFloatBar(
      bootstrap({ enabledProviders: ["codex"], floatBarShowResetInline: true }),
    );

    await waitFor(() => {
      expect(container.querySelectorAll(".floatbar__pill")).toHaveLength(2);
    });
    const pills = Array.from(container.querySelectorAll(".floatbar__pill"));
    expect(pills[0].textContent).toContain("Key 1");
    expect(pills[0].textContent).toContain("84%");
    expect(pills[0].querySelector(".floatbar__reset")).not.toBeNull();
    expect(pills[1].textContent).toContain("Key 2");
    expect(pills[1].textContent).toContain("Error");
    expect(pills[1].classList.contains("floatbar__pill--crit")).toBe(true);
    expect(container.textContent).not.toContain("secret backend failure");
  });

  it("renders every failed credential as its own safe error pill", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("codex", "Codex", 0, {
        credentialId: "00000000-0000-0000-0000-000000000001",
        credentialDisplayOrdinal: 1,
        error: "first secret failure",
      }),
      snapshot("codex", "Codex", 0, {
        credentialId: "00000000-0000-0000-0000-000000000002",
        credentialDisplayOrdinal: 2,
        error: "second secret failure",
      }),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ enabledProviders: ["codex"] }),
    );

    const { container } = renderFloatBar(bootstrap({ enabledProviders: ["codex"] }));

    await waitFor(() => {
      expect(container.querySelectorAll(".floatbar__pill")).toHaveLength(2);
    });
    const pills = Array.from(container.querySelectorAll(".floatbar__pill"));
    expect(pills.every((pill) => pill.classList.contains("floatbar__pill--crit"))).toBe(true);
    expect(pills.every((pill) => pill.textContent?.includes("Error"))).toBe(true);
    expect(container.textContent).not.toContain("first secret failure");
    expect(container.textContent).not.toContain("second secret failure");
  });

  it("shows only a safe error for a failed credential with weekly and monthly data", async () => {
    const failed = {
      ...snapshot("commandcode", "Command Code", 40, {
        credentialId: "00000000-0000-0000-0000-000000000001",
        credentialDisplayLabel: "Work",
        credentialDisplayOrdinal: 1,
        error: "secret monthly backend failure",
      }),
      accountGroupId: "commandcode:raw-account-id",
      accountEmail: "private@example.com",
      primaryLabel: "Monthly",
      primary: { ...rateWindow(40), windowMinutes: 30 * 24 * 60 },
      secondaryLabel: "Weekly",
      secondary: { ...rateWindow(55), windowMinutes: 7 * 24 * 60 },
    } satisfies ProviderUsageSnapshot;
    const overrides = {
      enabledProviders: ["commandcode"],
      providerOrder: ["commandcode"],
      floatBarProviderIds: ["commandcode"],
    } satisfies Partial<SettingsSnapshot>;
    tauriMocks.getCachedProviders.mockResolvedValue([failed]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings(overrides));

    const { container } = renderFloatBar(bootstrap(overrides));

    await waitFor(() => expect(container.querySelectorAll(".floatbar__pill")).toHaveLength(1));
    expect(container.querySelector(".floatbar__pill")).toHaveTextContent("WorkError");
    expect(container.querySelector(".floatbar__pill")).toHaveClass("floatbar__pill--crit");
    expect(container.querySelector(".floatbar__monthly-pill")).not.toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
    expect(container).not.toHaveTextContent("Weekly");
    expect(container).not.toHaveTextContent("secret monthly backend failure");
    expect(container).not.toHaveTextContent("commandcode:raw-account-id");
    expect(container).not.toHaveTextContent("private@example.com");
  });

  it("does not scan local costs by default", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("codex", "Codex", 75),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());

    renderFloatBar(bootstrap());

    await waitFor(() => {
      expect(tauriMocks.getCachedProviders).toHaveBeenCalled();
    });
    expect(tauriMocks.getProviderLocalUsageSummary).not.toHaveBeenCalled();
  });

  it("can show remaining percentages when configured", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 20),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings({ showAsUsed: false }));

    const { container } = renderFloatBar(bootstrap({ showAsUsed: false }));

    await waitFor(() => {
      const title = container
        .querySelector(".floatbar__pill")
        ?.getAttribute("title");
      expect(title).toContain("Claude: 80% remaining");
    });
  });

  it("applies warning tone when remaining drops below the high threshold", async () => {
    // highUsageThreshold = 70 → high-remaining cutoff = 30%.
    // claude at 80% used → 20% remaining → critical (also below crit cutoff 10).
    // Use 75% used → 25% remaining → warn (between 10 and 30).
    tauriMocks.getCachedProviders.mockResolvedValue([snapshot("claude", "Claude", 75)]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());

    const { container } = renderFloatBar(bootstrap());
    await waitFor(() => {
      expect(container.querySelector(".floatbar__pill--warn")).not.toBeNull();
    });
  });

  it("applies critical tone when the provider is exhausted", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 100, { exhausted: true }),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());

    const { container } = renderFloatBar(bootstrap());
    await waitFor(() => {
      expect(container.querySelector(".floatbar__pill--crit")).not.toBeNull();
    });
  });

  it("filters to the floatBarProviderIds allowlist when non-empty", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 30),
      snapshot("codex", "Codex", 50),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ floatBarProviderIds: ["codex"] }),
    );

    const { container } = renderFloatBar(
      bootstrap({ floatBarProviderIds: ["codex"] }),
    );
    await waitFor(() => {
      const pills = container.querySelectorAll(".floatbar__pill");
      expect(pills.length).toBe(1);
      expect(pills[0].getAttribute("title")).toMatch(/Codex/);
    });
  });

  it("does not show stale cached providers when all providers are disabled", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 30),
      snapshot("codex", "Codex", 50),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ enabledProviders: [] }),
    );

    const { container } = renderFloatBar(bootstrap({ enabledProviders: [] }));
    await waitFor(() => {
      expect(container.querySelectorAll(".floatbar__pill").length).toBe(0);
      expect(container.querySelector(".floatbar__empty")).not.toBeNull();
    });
  });

  it("shows an empty state when no providers match", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());

    const { container } = renderFloatBar(bootstrap());
    await waitFor(() => {
      expect(container.querySelector(".floatbar__empty")).not.toBeNull();
    });
  });

  it("applies the light-background class and CSS opacity", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ floatBarDarkText: true, floatBarOpacity: 45 }),
    );

    const { container } = renderFloatBar(
      bootstrap({ floatBarDarkText: true, floatBarOpacity: 45 }),
    );

    await waitFor(() => {
      const bar = container.querySelector<HTMLElement>(".floatbar");
      expect(bar).not.toBeNull();
      expect(bar?.classList.contains("floatbar--light-bg")).toBe(true);
      expect(bar?.style.opacity).toBe("0.45");
    });
  });

  it("applies the configured scale as a CSS variable", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings({ floatBarScale: 150 }));

    const { container } = renderFloatBar(bootstrap({ floatBarScale: 150 }));

    await waitFor(() => {
      const bar = container.querySelector<HTMLElement>(".floatbar");
      expect(bar).not.toBeNull();
      expect(bar?.style.getPropertyValue("--floatbar-scale")).toBe("1.5");
    });
  });

  it("uses the localized reset formatter in pill tooltips", async () => {
    const resetsAt = new Date(Date.now() + 3 * 60 * 60_000 + 42 * 60_000).toISOString();
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 20, { resetsAt }),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());

    const { container } = renderFloatBar(bootstrap());

    await waitFor(() => {
      const title = container
        .querySelector(".floatbar__pill")
        ?.getAttribute("title");
      expect(title).toContain("Claude: 20% used");
      expect(title).toMatch(/Resets in 3h 4[12]m/);
      expect(title).not.toContain("Resets in due now");
    });
  });

  it("can render a next reset icon and time in provider pills", async () => {
    const resetsAt = new Date(Date.now() + 2 * 60 * 60_000 + 5 * 60_000).toISOString();
    tauriMocks.getCachedProviders.mockResolvedValue([
      snapshot("claude", "Claude", 20, { resetsAt }),
    ]);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(
      settings({ floatBarShowResetInline: true }),
    );

    const { container } = renderFloatBar(
      bootstrap({ floatBarShowResetInline: true }),
    );

    await waitFor(() => {
      const reset = container.querySelector(".floatbar__reset");
      expect(reset).not.toBeNull();
      expect(reset?.getAttribute("aria-label")).toMatch(/Resets in 2h [45]m/);
      expect(reset?.textContent).toMatch(/2h [45]m/);
      expect(reset?.textContent).not.toContain("Resets in");
    });
  });

  it("polls refreshProvidersIfStale on the configured interval", async () => {
    vi.useFakeTimers();
    try {
      tauriMocks.getCachedProviders.mockResolvedValue([]);
      tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());
      // 60s minimum is enforced in FloatBar.tsx; use the floor here.
      await act(async () => {
        renderFloatBar(bootstrap({ refreshIntervalSecs: 60 }));
      });

      // Initial tick fires synchronously on mount; useProviders is passive here
      // so the floatbar does not double-request stale refreshes at startup.
      await vi.waitFor(() => {
        expect(tauriMocks.refreshProvidersIfStale).toHaveBeenCalledTimes(1);
      });
      const initialCalls = tauriMocks.refreshProvidersIfStale.mock.calls.length;

      // Advance the timer past the 60-second interval — the floatbar tick
      // should fire again.
      await vi.advanceTimersByTimeAsync(60_000);
      expect(tauriMocks.refreshProvidersIfStale.mock.calls.length).toBeGreaterThan(
        initialCalls,
      );
    } finally {
      vi.useRealTimers();
    }
  });
});
