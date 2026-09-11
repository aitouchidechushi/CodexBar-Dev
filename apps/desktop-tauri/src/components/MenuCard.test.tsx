import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const tauriMocks = vi.hoisted(() => ({
  getProviderChartData: vi.fn(),
  getLocaleStrings: vi.fn(),
  setUiLanguage: vi.fn(),
}));

const eventMocks = vi.hoisted(() => ({
  listen: vi.fn(),
}));

vi.mock("../lib/tauri", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/tauri")>()),
  ...tauriMocks,
}));
vi.mock("@tauri-apps/api/event", () => eventMocks);

import { LocaleProvider } from "../i18n/LocaleProvider";
import { buildBundle } from "../test/localeHarness";
import type { ProviderPresentationGroup } from "../lib/providerGroups";
import type { ProviderUsageSnapshot } from "../types/bridge";
import MenuCard from "./MenuCard";

function rateWindow(
  usedPercent = 0,
  opts: {
    exhausted?: boolean;
    resetDescription?: string | null;
    reservePercent?: number | null;
    reserveDescription?: string | null;
    reserveWillLastToReset?: boolean;
    reserveEtaSeconds?: number | null;
    windowMinutes?: number | null;
    resetsAt?: string | null;
  } = {},
) {
  return {
    usedPercent,
    remainingPercent: 100 - usedPercent,
    windowMinutes: opts.windowMinutes ?? null,
    resetsAt: opts.resetsAt ?? null,
    resetDescription: opts.resetDescription ?? null,
    isExhausted: opts.exhausted ?? false,
    reservePercent: opts.reservePercent ?? null,
    reserveDescription: opts.reserveDescription ?? null,
    reserveWillLastToReset: opts.reserveWillLastToReset ?? false,
    reserveEtaSeconds: opts.reserveEtaSeconds ?? null,
  };
}

function provider(
  error: string | null,
  usedPercent = 0,
  opts: { exhausted?: boolean; resetDescription?: string | null } = {},
): ProviderUsageSnapshot {
  return {
    providerId: "claude",
    displayName: "Claude",
    primary: rateWindow(usedPercent, opts),
    primaryLabel: "Session",
    secondary: null,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "oauth",
    updatedAt: "2026-05-24T00:00:00Z",
    error,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
    fetchDurationMs: null,
  };
}

function renderCard(
  snapshot: ProviderUsageSnapshot,
  opts: {
    compactMetrics?: boolean;
    showAsUsed?: boolean;
    showResetWhenExhausted?: boolean;
    onLayoutChange?: () => void;
  } = {},
) {
  return render(
    <LocaleProvider>
      <MenuCard
        providerGroup={providerGroup([snapshot])}
        compactMetrics={opts.compactMetrics}
        hideEmail={false}
        resetTimeRelative={true}
        showAsUsed={opts.showAsUsed}
        showResetWhenExhausted={opts.showResetWhenExhausted}
        onLayoutChange={opts.onLayoutChange}
      />
    </LocaleProvider>,
  );
}

function providerGroup(
  snapshots: ProviderUsageSnapshot[],
): ProviderPresentationGroup {
  const successfulSnapshots = snapshots.filter((snapshot) => !snapshot.error);
  const credentialSnapshots = snapshots.filter((snapshot) => snapshot.credentialId);
  const failedCredentialCount = credentialSnapshots.filter(
    (snapshot) => snapshot.error,
  ).length;
  return {
    providerId: snapshots[0].providerId,
    displayName: snapshots[0].displayName,
    snapshots,
    successfulSnapshots,
    failedCredentialCount,
    credentialCount: credentialSnapshots.length,
    hasMultipleCredentials: credentialSnapshots.length > 1,
    isAllFailed: successfulSnapshots.length === 0,
    isPartialFailure:
      successfulSnapshots.length > 0 && successfulSnapshots.length < snapshots.length,
  };
}

function credential(
  credentialId: string,
  label: string,
  ordinal: number,
  usedPercent: number,
  error: string | null = null,
): ProviderUsageSnapshot {
  return {
    ...provider(error, usedPercent),
    providerId: "openrouter",
    displayName: "OpenRouter",
    credentialId,
    credentialDisplayLabel: label,
    credentialDisplayOrdinal: ordinal,
    accountEmail: `${credentialId}@private.example`,
    accountOrganization: "Private Org",
    planName: "Private Plan",
  };
}

function renderGroup(
  group: ProviderPresentationGroup,
  options: { groupKimiAccounts?: boolean; hideEmail?: boolean; showAsUsed?: boolean } = {},
) {
  return render(
    <LocaleProvider>
      <MenuCard
        providerGroup={group}
        hideEmail={options.hideEmail ?? false}
        resetTimeRelative={true}
        showAsUsed={options.showAsUsed ?? true}
        groupKimiAccounts={options.groupKimiAccounts}
      />
    </LocaleProvider>,
  );
}

describe("MenuCard", () => {
  it("shows a safe Kimi parsing category instead of hiding the cause or exposing raw errors", async () => {
    tauriMocks.getLocaleStrings.mockResolvedValue(buildBundle({
      ProviderErrorQuotaFormat: "额度数据格式不兼容或未提供有效额度",
    }));
    const snapshot = { ...credential("key-failed", "Failed Kimi", 9, 0,
      "Parse error: Kimi quota response contains no usable quota windows secret-private"),
      providerId: "kimi", displayName: "Kimi" };
    renderGroup(providerGroup([snapshot]));
    expect(await screen.findByText("额度数据格式不兼容或未提供有效额度")).toBeInTheDocument();
    expect(document.body.textContent).not.toContain("secret-private");
  });

  it("shows only the real five-hour quota for a Kimi key without weekly usage", async () => {
    tauriMocks.getLocaleStrings.mockResolvedValue(buildBundle({
      ProviderRateLimitLabel: "五小时额度", ProviderWeeklyLabel: "本周",
    }));
    const snapshot = { ...credential("key-short", "Partial Kimi", 9, 45),
      providerId: "kimi", displayName: "Kimi", primaryLabel: "Weekly",
      primary: rateWindow(45, { windowMinutes: 300 }), secondary: null };
    renderGroup(providerGroup([snapshot]));
    const card = await screen.findByRole("region", { name: "Partial Kimi" });
    expect(within(card).getByText("五小时额度")).toBeInTheDocument();
    expect(card.textContent).not.toContain("本周");
    expect(card.textContent).toContain("45%");
  });

  beforeEach(() => {
    vi.clearAllMocks();
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        ActionCopyError: "Copy error",
        DetailPaceRunsOutIn: "Runs out in",
        PanelEstimatedFromLocalLogs: "Estimated from local logs",
        PanelLeftSuffix: "left",
        PanelNow: "now",
        PanelOneHour: "1h",
        PanelFiveHours: "5h",
        PanelOnPaceBudget: "On-pace budget",
        PanelReserveSuffix: "in reserve",
        PanelThirtyDayCost: "30d cost",
        PanelThirtyDayTokens: "30d tokens",
        PanelTodayBudget: "today",
        PanelUsedSuffix: "used",
        ResetsInHoursMinutes: "Resets in {}h {}m",
        ResetsInMinutes: "Resets in {}m",
        WayfinderGatewayStatus: "Gateway",
        WayfinderModels: "Models",
        WayfinderRequests: "Requests",
        WayfinderTokens: "Tokens",
        WayfinderSaved: "Saved",
        WayfinderOffline: "Gateway offline",
        WayfinderDryRun: "Dry run",
        WayfinderMissingKeys: "Missing keys",
        ApiKeyFailureCount: "{} de {} fallaron",
        ApiKeyFailureDetails: "{} — {}",
      }),
    );
    tauriMocks.getProviderChartData.mockResolvedValue({
      providerId: "claude",
      costHistory: [{ date: "2026-05-24", value: 1.23 }],
      creditsHistory: [],
      usageBreakdown: [],
      localUsage: {
        todayCost: null,
        thirtyDayCost: 1.23,
        thirtyDayTokens: 584_000,
        latestTokens: null,
        topModel: "glim-4.6",
        estimateNote: "Estimated from local logs",
        tokenCostUpdatedAtMs: 1234,
      },
    });
    eventMocks.listen.mockResolvedValue(() => {});
  });

  it("renders one provider header with stable ordered credential quota regions", async () => {
    const first = credential("credential-a", "Work", 1, 25);
    const second = credential("credential-b", "Key 2", 2, 80);
    const { container } = renderGroup(providerGroup([first, second]));

    expect(await screen.findByRole("region", { name: "Work" })).toBeInTheDocument();
    expect(container.querySelectorAll(".menu-card")).toHaveLength(1);
    expect(screen.getAllByText("OpenRouter")).toHaveLength(1);
    const regions = screen.getAllByRole("region");
    expect(regions.map((region) => region.getAttribute("aria-label"))).toEqual([
      "Work",
      "Key 2",
    ]);
    expect(await screen.findByText("25% used")).toBeInTheDocument();
    expect(screen.getByText("80% used")).toBeInTheDocument();
    expect(screen.queryByText("Private Org")).not.toBeInTheDocument();
    expect(screen.queryByText("Private Plan")).not.toBeInTheDocument();
    expect(screen.queryByText("credential-a@private.example")).not.toBeInTheDocument();
    expect(screen.queryByText(/52%/)).not.toBeInTheDocument();
  });

  it("groups stable provider accounts, deduplicates only audited monthly quota, and masks email headings", async () => {
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        ProviderMonthlyLabel: "月额度",
        ProviderRateLimitLabel: "五小时额度",
        ProviderWeeklyLabel: "本周",
      }),
    );
    const groupedCredential = (credentialId: string, label: string, ordinal: number, monthly: number) => ({
      ...credential(credentialId, label, ordinal, 10 + ordinal),
      providerId: "clinepass",
      displayName: "ClinePass",
      accountGroupId: "clinepass:account-42",
      accountEmail: "owner@example.com",
      primaryLabel: "Rate Limit",
      primary: rateWindow(10 + ordinal, { windowMinutes: 5 * 60 }),
      secondaryLabel: "Weekly",
      secondary: rateWindow(20 + ordinal, { windowMinutes: 7 * 24 * 60 }),
      tertiary: rateWindow(45, { windowMinutes: 30 * 24 * 60 }),
      extraRateWindows: [{
        id: `member-${ordinal}-monthly`,
        title: `Agent ${ordinal} Monthly`,
        window: rateWindow(monthly, { windowMinutes: 31 * 24 * 60 }),
      }],
    }) satisfies ProviderUsageSnapshot;
    const first = groupedCredential("credential-a", "Work", 1, 61);
    const second = groupedCredential("credential-b", "Personal", 2, 72);
    const unmatched = {
      ...groupedCredential("credential-c", "Unmatched Key", 3, 83),
      accountGroupId: undefined,
      accountEmail: null,
      tertiary: null,
    } satisfies ProviderUsageSnapshot;

    const { container } = renderGroup(providerGroup([first, second, unmatched]), {
      hideEmail: true,
      groupKimiAccounts: false,
      showAsUsed: false,
    });

    const account = await screen.findByRole("region", { name: "o••••@example.com" });
    expect(account).toHaveClass("menu-card__provider-account-group");
    expect(within(account).getAllByRole("region").map((region) => region.getAttribute("aria-label")))
      .toEqual(["Work", "Personal"]);
    expect(within(account).getAllByText("五小时额度")).toHaveLength(2);
    expect(within(account).getAllByText("本周")).toHaveLength(2);
    expect(within(account).getAllByText("月额度")).toHaveLength(1);
    expect(within(account).getByText("月额度 · Agent 1")).toHaveClass("monthly-quota-badge");
    expect(within(account).getByText("月额度 · Agent 2")).toHaveClass("monthly-quota-badge");
    expect(within(account).getAllByRole("progressbar")).toHaveLength(3);
    expect(within(account).getByRole("progressbar", { name: "月额度 · Agent 1" }))
      .toHaveAttribute("aria-valuenow", "39");
    expect(container).not.toHaveTextContent("clinepass:account-42");
    expect(container).not.toHaveTextContent("owner@example.com");

    const unmatchedCard = screen.getByRole("region", { name: "Unmatched Key" });
    expect(account).not.toContainElement(unmatchedCard);
    expect(unmatchedCard).toHaveTextContent("月额度 · Agent 3");
    expect(within(unmatchedCard).getByRole("progressbar", { name: "月额度 · Agent 3" }))
      .toHaveAttribute("aria-valuenow", "17");
  });

  it("materializes a selected provider-account monthly cost quota without hiding cost details", async () => {
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        ProviderMonthlyLabel: "月额度",
        ProviderRateLimitLabel: "五小时额度",
        DetailCostTitle: "Cost",
        DetailCostUsed: "Spent",
      }),
    );
    const snapshot = {
      ...credential("credential-a", "Work", 1, 10),
      providerId: "commandcode",
      displayName: "Command Code",
      accountGroupId: "commandcode:account-1",
      accountEmail: "group@example.com",
      primaryLabel: "Rate Limit",
      primary: rateWindow(10, { windowMinutes: 5 * 60 }),
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
    } satisfies ProviderUsageSnapshot;

    renderGroup(providerGroup([snapshot]));

    const account = await screen.findByRole("region", { name: "group@example.com" });
    expect(within(account).getByText("月额度")).toHaveClass("monthly-quota-badge");
    expect(within(account).getByRole("progressbar", { name: "月额度" }))
      .toHaveAttribute("aria-valuenow", "25");
    expect(within(account).getByText(/Spent: \$25\.00 \/ \$100\.00/)).toBeInTheDocument();
  });

  it("uses distinct React keys for provider accounts without credential ids", async () => {
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    try {
      const account = (accountGroupId: string, accountEmail: string) => ({
        ...provider(null, 20),
        providerId: "clinepass",
        displayName: "ClinePass",
        accountGroupId,
        accountEmail,
      }) satisfies ProviderUsageSnapshot;

      renderGroup(providerGroup([
        account("clinepass:account-a", "a@example.com"),
        account("clinepass:account-b", "b@example.com"),
      ]));

      expect(await screen.findByRole("region", { name: "a@example.com" })).toBeInTheDocument();
      expect(screen.getByRole("region", { name: "b@example.com" })).toBeInTheDocument();
      expect(consoleError.mock.calls.flat().join(" ")).not.toMatch(/same key/i);
    } finally {
      consoleError.mockRestore();
    }
  });

  it("renders each API key in a glass card with direct management actions", async () => {
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        ApiKeyEditLabel: "Edit label",
        ApiKeyReplaceSecret: "Replace key",
        ApiKeyDelete: "Delete",
      }),
    );
    const onEditApiKey = vi.fn();
    const onReplaceApiKey = vi.fn();
    const onDeleteApiKey = vi.fn();
    render(
      <LocaleProvider>
        <MenuCard
          providerGroup={providerGroup([
            credential("credential-a", "Work", 1, 25),
            credential("credential-b", "Key 2", 2, 80),
          ])}
          hideEmail={false}
          resetTimeRelative={true}
          showAsUsed
          onEditApiKey={onEditApiKey}
          onReplaceApiKey={onReplaceApiKey}
          onDeleteApiKey={onDeleteApiKey}
        />
      </LocaleProvider>,
    );

    expect(await screen.findAllByRole("button", { name: "Edit label" })).toHaveLength(2);
    expect(screen.getAllByRole("button", { name: "Replace key" })).toHaveLength(2);
    expect(screen.getAllByRole("button", { name: "Delete" })).toHaveLength(2);
    expect(document.querySelectorAll(".menu-card__credential-card")).toHaveLength(2);
    fireEvent.click(screen.getAllByRole("button", { name: "Replace key" })[1]);
    expect(onReplaceApiKey).toHaveBeenCalledWith("openrouter", "credential-b", "Key 2");
  });

  it("keeps successful quota blocks visible and announces a partial failure count", async () => {
    renderGroup(
      providerGroup([
        credential("credential-a", "Work", 1, 30),
        credential("credential-b", "Key 2", 2, 0, "secret backend failure"),
      ]),
    );

    expect(await screen.findByRole("region", { name: "Work" })).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("1 de 2 fallaron");
    expect(screen.queryByText("secret backend failure")).not.toBeInTheDocument();
  });

  it("keeps a temporarily stale API-key quota visible and labels the failed refresh", async () => {
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        PanelUsedSuffix: "used",
        ProviderStatusStale: "Last update failed",
      }),
    );
    const stale = {
      ...credential("credential-a", "Work", 1, 42),
      refreshError: "Timeout",
    } as ProviderUsageSnapshot;

    renderGroup(providerGroup([stale]));

    expect(await screen.findByRole("region", { name: "Work" })).toHaveTextContent("42% used");
    expect(screen.getByRole("status")).toHaveTextContent("Last update failed: Timeout");
  });

  it("renders one safe provider error treatment when all credentials fail", async () => {
    const { container } = renderGroup(
      providerGroup([
        credential("credential-a", "Work", 1, 0, "first private error"),
        credential("credential-b", "Key 2", 2, 0, "second private error"),
      ]),
    );

    expect(await screen.findByRole("alert")).toBeInTheDocument();
    expect(container.querySelectorAll(".menu-card--error")).toHaveLength(1);
    expect(screen.getByRole("alert")).toHaveTextContent("2 de 2 fallaron");
    expect(screen.getByRole("alert")).toHaveTextContent(
      "2 de 2 fallaron — Work, Key 2",
    );
    expect(screen.getByRole("alert")).toHaveTextContent("Work");
    expect(screen.getByRole("alert")).toHaveTextContent("Key 2");
    expect(screen.queryByText("first private error")).not.toBeInTheDocument();
    expect(screen.queryByText("second private error")).not.toBeInTheDocument();
  });

  it("keeps the original single default-provider presentation", async () => {
    const { container } = renderCard(provider(null, 35), { showAsUsed: true });

    expect(await screen.findByText("35% used")).toBeInTheDocument();
    expect(screen.getAllByText("Claude")).toHaveLength(1);
    expect(container.querySelector(".menu-card__credential-heading")).toBeNull();
    expect(container.querySelectorAll(".menu-card")).toHaveLength(1);
  });

  it("does not mix stale local usage into an error card", async () => {
    const { container } = renderCard(
      provider("OAuth error: Claude OAuth credentials not found."),
    );

    expect(
      await screen.findByText("OAuth error: Claude OAuth credentials not found."),
    ).toBeInTheDocument();
    expect(container.querySelector(".menu-card--header-only")).toBeInTheDocument();
    expect(container.querySelector(".menu-card--with-details")).not.toBeInTheDocument();

    await waitFor(() => {
      expect(tauriMocks.getProviderChartData).toHaveBeenCalled();
    });

    expect(screen.queryByText("30d cost")).not.toBeInTheDocument();
    expect(screen.queryByText("30d tokens")).not.toBeInTheDocument();
    expect(screen.queryByText("Estimated from local logs")).not.toBeInTheDocument();
  });

  it("can render metric bars as used instead of remaining", async () => {
    renderCard(provider(null, 35), { showAsUsed: true });

    expect(await screen.findByText("35% used")).toBeInTheDocument();
    expect(screen.queryByText("65% left")).not.toBeInTheDocument();

    const fill = document.querySelector<HTMLElement>(".menu-metric__bar-fill");
    expect(fill?.style.width).toBe("35%");
  });

  it("displays over-quota usage without overflowing the bar", async () => {
    renderCard(provider(null, 115, { exhausted: true, resetDescription: "115% used" }), {
      showAsUsed: true,
    });

    expect(await screen.findAllByText("115% used")).not.toHaveLength(0);
    const fill = document.querySelector<HTMLElement>(".menu-metric__bar-fill");
    expect(fill?.style.width).toBe("100%");
  });

  it("replaces an exhausted percentage with a future reset countdown", async () => {
    const snapshot = provider(null, 100, { exhausted: true });
    snapshot.primary.resetsAt = new Date(Date.now() + 60 * 60 * 1000).toISOString();

    renderCard(snapshot, { showResetWhenExhausted: true });

    expect(await screen.findByText(/Resets in \d+m/)).toBeInTheDocument();
    expect(screen.queryByText("0% left")).not.toBeInTheDocument();
  });

  it("keeps an exhausted percentage without a concrete future reset", async () => {
    renderCard(provider(null, 100, { exhausted: true, resetDescription: "in 2h" }), {
      showResetWhenExhausted: true,
    });

    expect(await screen.findByText("0% left")).toBeInTheDocument();
  });

  it("renders additional Copilot budget windows", async () => {
    const snapshot = provider(null, 20);
    snapshot.providerId = "copilot";
    snapshot.displayName = "GitHub Copilot";
    snapshot.extraRateWindows = [
      {
        id: "additional_budget",
        title: "Additional Budget",
        window: rateWindow(42),
      },
    ];

    renderCard(snapshot);

    expect(await screen.findByText("Additional Budget")).toBeInTheDocument();
    expect(screen.getByText("58% left")).toBeInTheDocument();
  });

  it("localizes primary and secondary raw quota labels", async () => {
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        ProviderWeeklyLabel: "本周",
        ProviderRateLimitLabel: "五小时额度",
      }),
    );
    const snapshot = provider(null, 20);
    snapshot.primaryLabel = "Rate Limit";
    snapshot.secondary = rateWindow(40);
    snapshot.secondaryLabel = "Weekly";

    renderCard(snapshot);

    expect(await screen.findByText("五小时额度")).toBeInTheDocument();
    expect(screen.getByText("本周")).toBeInTheDocument();
    expect(screen.queryByText("Rate Limit")).not.toBeInTheDocument();
    expect(screen.queryByText("Weekly")).not.toBeInTheDocument();
  });

  it("localizes monthly and usage quota labels", async () => {
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        ProviderMonthlyLabel: "每月",
        ProviderUsageLabel: "用量",
      }),
    );
    const snapshot = provider(null, 20);
    snapshot.primaryLabel = "Usage";
    snapshot.secondary = rateWindow(40);
    snapshot.secondaryLabel = "Monthly";

    renderCard(snapshot);

    expect(await screen.findByText("用量")).toBeInTheDocument();
    expect(screen.getByText("每月")).toBeInTheDocument();
  });

  it.each([
    {
      providerId: "grok",
      primaryLabel: "Monthly",
      primaryUsed: 26,
      secondaryLabel: undefined,
      secondaryUsed: undefined,
      expectedPercents: [26],
    },
    {
      providerId: "chutes",
      primaryLabel: "4-hour quota",
      primaryUsed: 14,
      secondaryLabel: "Monthly quota",
      secondaryUsed: 37,
      expectedPercents: [14, 37],
    },
  ])(
    "keeps real $providerId unknown-cycle slots as ordinary percentages without monthly styling",
    async ({
      providerId,
      primaryLabel,
      primaryUsed,
      secondaryLabel,
      secondaryUsed,
      expectedPercents,
    }) => {
      tauriMocks.getLocaleStrings.mockResolvedValue(
        buildBundle({ ProviderMonthlyLabel: "月额度" }),
      );
      const snapshot = provider(null, primaryUsed);
      snapshot.providerId = providerId;
      snapshot.displayName = providerId;
      snapshot.primaryLabel = primaryLabel;
      snapshot.primary = rateWindow(primaryUsed);
      snapshot.secondaryLabel = secondaryLabel;
      snapshot.secondary = secondaryUsed == null ? null : rateWindow(secondaryUsed);

      const { container } = renderCard(snapshot, { showAsUsed: true });

      await waitFor(() => {
        for (const percent of expectedPercents) {
          expect(screen.getByText(new RegExp(`^${percent}%`))).toBeInTheDocument();
        }
      });
      expect(container.querySelectorAll(".menu-metric__bar")).toHaveLength(
        expectedPercents.length,
      );
      expect(container.querySelector(".menu-metric--monthly")).not.toBeInTheDocument();
      expect(container.querySelector(".monthly-quota-badge")).not.toBeInTheDocument();
      expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
    },
  );

  it("keeps all monthly quotas visible in compact provider cards", async () => {
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        ProviderMonthlyLabel: "月额度",
        ProviderRateLimitLabel: "五小时额度",
        ProviderWeeklyLabel: "七天额度",
      }),
    );
    const snapshot = provider(null, 20);
    snapshot.primaryLabel = "Rate Limit";
    snapshot.primary = rateWindow(20, { windowMinutes: 5 * 60 });
    snapshot.secondaryLabel = "Weekly";
    snapshot.secondary = rateWindow(30, { windowMinutes: 7 * 24 * 60 });
    snapshot.tertiary = rateWindow(40, {
      windowMinutes: 30 * 24 * 60,
      resetDescription: "Account reset",
    });
    snapshot.extraRateWindows = [
      {
        id: "agent-monthly",
        title: "Agent Monthly",
        window: rateWindow(50, {
          windowMinutes: 31 * 24 * 60,
          resetDescription: "Agent reset",
        }),
      },
      {
        id: "monthly-spend",
        title: "Monthly spend",
        window: rateWindow(60),
      },
    ];

    renderCard(snapshot, { compactMetrics: true });

    expect(await screen.findByText("月额度")).toBeInTheDocument();
    expect(screen.getByText("月额度 · Agent")).toBeInTheDocument();
    expect(screen.getByText("Account reset")).toBeInTheDocument();
    expect(screen.getByText("Agent reset")).toBeInTheDocument();
    expect(screen.queryByText("Monthly spend")).not.toBeInTheDocument();
  });

  it("shows a friendly MiniMax configuration summary while preserving raw copy details", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        ActionCopyError: "复制原始错误",
        ProviderErrorMiniMaxNotConfigured: "请添加 MiniMax Token Plan Key",
      }),
    );
    const raw =
      "Provider not installed: MiniMax API not configured. Set MINIMAX_API_KEY and MINIMAX_GROUP_ID environment variables";
    const snapshot = provider(raw);
    snapshot.providerId = "minimax";
    snapshot.displayName = "MiniMax";

    renderCard(snapshot);

    expect(
      await screen.findByText("请添加 MiniMax Token Plan Key"),
    ).toBeInTheDocument();
    expect(screen.queryByText(raw)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "复制原始错误" }));
    expect(writeText).toHaveBeenCalledWith(raw);
  });

  it("localizes a MiniMax Token Plan availability error", async () => {
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        ProviderErrorMiniMaxPlanUnavailable: "MiniMax 文本模型不在当前套餐中",
      }),
    );
    const snapshot = provider(
      "MiniMax text models are not included in the current Token Plan",
    );
    snapshot.providerId = "minimax";
    snapshot.displayName = "MiniMax";

    renderCard(snapshot);

    expect(
      await screen.findByText("MiniMax 文本模型不在当前套餐中"),
    ).toBeInTheDocument();
  });

  it("turns the MiniMax unlimited-week marker into one badge instead of a metric", async () => {
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({ ProviderUnlimitedWeekly: "无周限额" }),
    );
    const snapshot = provider(null, 20);
    snapshot.providerId = "minimax";
    snapshot.displayName = "MiniMax";
    snapshot.extraRateWindows = [
      {
        id: "minimax-weekly-unlimited",
        title: "internal marker",
        window: rateWindow(0),
      },
    ];

    const { container } = renderCard(snapshot);

    expect(await screen.findByText("无周限额")).toBeInTheDocument();
    expect(screen.queryByText("internal marker")).not.toBeInTheDocument();
    expect(container.querySelectorAll(".menu-metric")).toHaveLength(1);
  });

  it("renders a full add-key action without bubbling into the card", async () => {
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({ ApiKeyAddCard: "＋ 添加 API Key" }),
    );
    const onAddApiKey = vi.fn();
    const onCardClick = vi.fn();
    render(
      <div onClick={onCardClick}>
        <LocaleProvider>
          <MenuCard
            providerGroup={providerGroup([provider(null)])}
            hideEmail={false}
            resetTimeRelative={true}
            canAddApiKey
            onAddApiKey={onAddApiKey}
          />
        </LocaleProvider>
      </div>,
    );

    fireEvent.click(await screen.findByRole("button", { name: "＋ 添加 API Key" }));

    expect(onAddApiKey).toHaveBeenCalledTimes(1);
    expect(onCardClick).not.toHaveBeenCalled();
  });

  it("renders informational metrics without quota percentages", async () => {
    const snapshot = provider(null, 20);
    snapshot.extraRateWindows = [
      {
        id: "requests",
        title: "Requests",
        window: {
          ...rateWindow(0),
          isInformational: true,
          resetDescription: "7 requests",
        },
      },
    ];

    renderCard(snapshot);

    const title = await screen.findByText("Requests");
    expect(title.parentElement).not.toHaveTextContent("100% left");
    expect(title.parentElement?.querySelector(".menu-metric__bar")).toBeNull();
    expect(screen.getByText("7 requests")).toBeInTheDocument();
  });

  it("renders Wayfinder telemetry without quota or identity rows", async () => {
    const snapshot = provider(null);
    snapshot.providerId = "wayfinder";
    snapshot.displayName = "Wayfinder";
    snapshot.accountEmail = "should-not-render@example.test";
    snapshot.planName = "should-not-render";
    snapshot.wayfinderUsage = {
      gatewayStatus: "ok",
      offline: false,
      dryRun: false,
      missingKeys: [],
      modelCount: 2,
      models: ["model-a", "model-b"],
      requests: 14,
      estimatedRequests: 0,
      tokens: 1028,
      realized: 0.004,
      baseline: 0.01,
      saved: 0.006,
      savedPercent: 60,
      periodDays: 30,
      unit: "usd",
      priced: true,
      routes: [],
    };

    renderCard(snapshot);

    expect(await screen.findByText("ok")).toBeInTheDocument();
    expect(screen.getByText("2")).toBeInTheDocument();
    expect(screen.getByText("1K")).toBeInTheDocument();
    expect(screen.queryByText("should-not-render@example.test")).not.toBeInTheDocument();
    expect(screen.queryByText("should-not-render")).not.toBeInTheDocument();
    expect(screen.queryByText("Session")).not.toBeInTheDocument();
  });

  it("notifies the tray panel after async local usage data loads", async () => {
    const onLayoutChange = vi.fn();

    renderCard(provider(null), { onLayoutChange });

    await waitFor(() => {
      expect(onLayoutChange).toHaveBeenCalled();
    });
  });

  it("shows the formatted predicted exhaustion time", async () => {
    const snapshot = provider(null, 40);
    snapshot.pace = {
      stage: "far_ahead",
      deltaPercent: 20,
      expectedUsedPercent: 20,
      actualUsedPercent: 40,
      etaSeconds: 90 * 60,
      willLastToReset: false,
    };

    const { container } = renderCard(snapshot);

    await waitFor(() => {
      expect(container.querySelector(".menu-card__pace-eta")).toHaveTextContent(
        "⚠ Runs out in 2h",
      );
    });
  });

  it("renders local token and cost totals after chart data loads", async () => {
    const { container } = renderCard(provider(null));

    expect(await screen.findByText("30d cost")).toBeInTheDocument();
    expect(container.querySelector(".menu-card--with-details")).toBeInTheDocument();
    expect(container.querySelector(".menu-card--header-only")).not.toBeInTheDocument();
    expect(screen.getAllByText("$1.23").length).toBeGreaterThan(0);
    expect(screen.getByText("30d tokens")).toBeInTheDocument();
    expect(screen.getByText("584K")).toBeInTheDocument();
    expect(screen.getByText("Estimated from local logs")).toBeInTheDocument();
  });

  it("shows on-pace budgets and expands projection details", async () => {
    const onLayoutChange = vi.fn();
    const resetAt = new Date(
      Date.now() + 0.6 * 7 * 24 * 60 * 60 * 1000,
    );
    const snapshot = provider(null, 20);
    snapshot.primary = rateWindow(20, {
      reservePercent: 20,
      reserveWillLastToReset: true,
      windowMinutes: 7 * 24 * 60,
      resetsAt: resetAt.toISOString(),
    });

    renderCard(snapshot, { onLayoutChange });

    const toggle = await screen.findByRole("button", { name: /On-pace budget/ });
    expect(screen.getByText("now 20%")).toBeInTheDocument();
    expect(screen.getByText("1h 21%")).toBeInTheDocument();
    expect(screen.queryByRole("img", { name: /PaceChartAriaLabel/i })).not.toBeInTheDocument();

    fireEvent.click(toggle);

    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("img", { name: /PaceChartAriaLabel/i })).toBeInTheDocument();
    await waitFor(() => {
      expect(onLayoutChange).toHaveBeenCalled();
    });
  });

  it("shows on-pace budgets when timing exists without reserve metadata", async () => {
    const resetAt = new Date(Date.now() + 6 * 24 * 60 * 60 * 1000);
    const snapshot = provider(null, 31);
    snapshot.primary = rateWindow(31, {
      windowMinutes: 7 * 24 * 60,
      resetsAt: resetAt.toISOString(),
    });

    renderCard(snapshot);

    expect(
      await screen.findByRole("button", { name: /On-pace budget/ }),
    ).toBeInTheDocument();
      expect(screen.getByText("now 0%")).toBeInTheDocument();
      expect(screen.queryByText(/in reserve/)).not.toBeInTheDocument();
      expect(screen.queryByText("Lasts until reset")).not.toBeInTheDocument();
  });

  it("does not show pace budgets for a five-hour session window", async () => {
    const resetAt = new Date(Date.now() + 4 * 60 * 60 * 1000);
    const snapshot = provider(null, 31);
    snapshot.primary = rateWindow(31, {
      windowMinutes: 5 * 60,
      resetsAt: resetAt.toISOString(),
    });

    renderCard(snapshot);

    expect(await screen.findByText("69% left")).toBeInTheDocument();
    expect(screen.queryByText("On-pace budget")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("img", { name: /PaceChartAriaLabel/i }),
    ).not.toBeInTheDocument();
  });

  it("marks weekly and five-hour metric rows with distinct visual kinds", async () => {
    const snapshot = provider(null, 20);
    snapshot.primaryLabel = "Weekly";
    snapshot.primary = rateWindow(20, { windowMinutes: 7 * 24 * 60 });
    snapshot.secondaryLabel = "Rate Limit";
    snapshot.secondary = rateWindow(40, { windowMinutes: 5 * 60 });

    const { container } = renderCard(snapshot);

    await waitFor(() => {
      expect(container.querySelector(".menu-metric--weekly")).not.toBeNull();
      expect(container.querySelector(".menu-metric--short-window")).not.toBeNull();
    });
  });

  it("uses the provider window labels when legacy quota snapshots omit their duration", async () => {
    const snapshot = provider(null, 20);
    snapshot.primaryLabel = "Weekly";
    snapshot.primary = rateWindow(20);
    snapshot.secondaryLabel = "Rate Limit";
    snapshot.secondary = rateWindow(40);

    const { container } = renderCard(snapshot);

    await waitFor(() => {
      expect(container.querySelector(".menu-metric--weekly")).not.toBeNull();
      expect(container.querySelector(".menu-metric--short-window")).not.toBeNull();
    });
  });

  it("keeps the reserve row when timing data is incomplete", async () => {
    const snapshot = provider(null, 20);
    snapshot.primary = rateWindow(20, {
        reservePercent: 12,
        reserveWillLastToReset: true,
      });

    renderCard(snapshot);

    expect(await screen.findByText("12% in reserve")).toBeInTheDocument();
    expect(screen.queryByText("On-pace budget")).not.toBeInTheDocument();
  });

  it("localizes the relative updated-at time in Japanese without duplicated prefix", async () => {
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        UpdatedJustNow: "たった今",
        UpdatedMinutesAgo: "{}分前",
        UpdatedHoursAgo: "{}時間前",
        UpdatedDaysAgo: "{}日前",
      }),
    );

    const snapshot = provider(null, 20);
    snapshot.updatedAt = new Date(Date.now() - 3 * 60 * 1000).toISOString();
    renderCard(snapshot);

    expect(await screen.findByText("3分前")).toBeInTheDocument();
  });

  it("renders a Kimi account monthly quota even when the account has no API key", async () => {
    renderGroup({
      providerId: "kimi",
      displayName: "Kimi",
      snapshots: [],
      successfulSnapshots: [],
      failedCredentialCount: 0,
      credentialCount: 0,
      hasMultipleCredentials: false,
      isAllFailed: false,
      isPartialFailure: false,
      kimiAccounts: [
        {
          accountId: "account-a",
          displayName: "账号 A",
          usedPercent: 35,
          resetsAt: null,
          updatedAt: "2026-08-14T00:00:00Z",
          status: "ok",
          matchedCredentialIds: [],
        },
      ],
    });

    expect(await screen.findByText("账号 A")).toBeInTheDocument();
    expect(screen.getByText(/35%/)).toBeInTheDocument();
  });

  it("groups Kimi monthly quotas with their matched API keys only when requested", async () => {
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        KimiAccountMonthlyQuota: "月额度",
        KimiAccountLinkedApiKeys: "此账号的 API Key",
        KimiAccountUnlinkedApiKeys: "未关联 Kimi 月额度",
      }),
    );
    const accountAKey = {
      ...credential("key-a", "账号 A Key", 1, 20),
      providerId: "kimi",
      displayName: "Kimi",
    };
    const accountBKey = {
      ...credential("key-b", "账号 B Key", 2, 30),
      providerId: "kimi",
      displayName: "Kimi",
    };
    const unmatchedKey = {
      ...credential("key-unmatched", "未关联 Key", 3, 40),
      providerId: "kimi",
      displayName: "Kimi",
    };
    const group: ProviderPresentationGroup = {
      providerId: "kimi",
      displayName: "Kimi",
      snapshots: [accountAKey, accountBKey, unmatchedKey],
      successfulSnapshots: [accountAKey, accountBKey, unmatchedKey],
      failedCredentialCount: 0,
      credentialCount: 3,
      hasMultipleCredentials: true,
      isAllFailed: false,
      isPartialFailure: false,
      kimiAccounts: [
        {
          accountId: "account-a",
          displayName: "账号 A",
          sourceLabels: ["Edge · Profile 1"],
          usedPercent: 35,
          resetsAt: null,
          updatedAt: "2026-08-14T00:00:00Z",
          status: "ok",
          matchedCredentialIds: ["key-a"],
        },
        {
          accountId: "account-b",
          displayName: "账号 B",
          sourceLabels: ["Chrome · Default"],
          usedPercent: 50,
          resetsAt: null,
          updatedAt: "2026-08-14T00:00:00Z",
          status: "ok",
          matchedCredentialIds: ["key-b"],
        },
      ],
    };

    const ungrouped = renderGroup(group);
    expect(await screen.findByText("账号 A")).toBeInTheDocument();
    expect(ungrouped.container.querySelector(".menu-card__kimi-account-group")).toBeNull();
    ungrouped.unmount();

    renderGroup(group, { groupKimiAccounts: true });
    const accountA = await screen.findByRole("region", { name: "账号 A" });
    const accountB = screen.getByRole("region", { name: "账号 B" });
    const unmatched = screen.getByRole("region", { name: "未关联 Kimi 月额度" });

    expect(within(accountA).getByText("Edge · Profile 1")).toBeInTheDocument();
    expect(within(accountA).getByRole("region", { name: "账号 A Key" })).toBeInTheDocument();
    expect(within(accountA).queryByText("账号 B Key")).not.toBeInTheDocument();
    expect(within(accountA).getByRole("progressbar", { name: "账号 A 月额度" })).toHaveAttribute(
      "aria-valuenow",
      "35",
    );
    expect(within(accountB).getByRole("region", { name: "账号 B Key" })).toBeInTheDocument();
    expect(within(unmatched).getByRole("region", { name: "未关联 Key" })).toBeInTheDocument();
  });
});
