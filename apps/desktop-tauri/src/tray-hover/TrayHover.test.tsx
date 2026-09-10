import { render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const tauriMocks = vi.hoisted(() => ({
  getCachedProviders: vi.fn(),
  refreshProviders: vi.fn(),
  refreshProvidersIfStale: vi.fn(),
  getLocaleStrings: vi.fn(),
  setUiLanguage: vi.fn(),
  listKimiAccounts: vi.fn(),
}));
const eventMocks = vi.hoisted(() => ({ listen: vi.fn() }));
const coreMocks = vi.hoisted(() => ({ invoke: vi.fn().mockResolvedValue(undefined) }));

vi.mock("../lib/tauri", () => tauriMocks);
vi.mock("@tauri-apps/api/event", () => eventMocks);
vi.mock("@tauri-apps/api/core", () => coreMocks);

import TrayHover from "./TrayHover";
import { LocaleProvider } from "../i18n/LocaleProvider";
import { buildBundle } from "../test/localeHarness";
import type { BootstrapState, ProviderUsageSnapshot } from "../types/bridge";

function rate(usedPercent: number) {
  return {
    usedPercent,
    remainingPercent: 100 - usedPercent,
    windowMinutes: null,
    resetsAt: null,
    resetDescription: null,
    isExhausted: false,
    reservePercent: null,
    reserveDescription: null,
  };
}

function credential(label: string, ordinal: number, used: number, weekly: number): ProviderUsageSnapshot {
  return {
    providerId: "kimi",
    displayName: "Kimi",
    credentialId: `00000000-0000-0000-0000-00000000000${ordinal}`,
    credentialDisplayLabel: label,
    credentialDisplayOrdinal: ordinal,
    primary: rate(used),
    primaryLabel: "Five hours",
    secondary: rate(weekly),
    secondaryLabel: "Weekly",
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "api-key",
    updatedAt: "2026-08-09T00:00:00Z",
    error: null,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
  };
}

const state = {
  contractVersion: "v1",
  providers: [],
  settings: { enabledProviders: ["kimi"], providerOrder: ["kimi"] },
} as unknown as BootstrapState;

describe("TrayHover", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    eventMocks.listen.mockResolvedValue(() => {});
    tauriMocks.refreshProviders.mockResolvedValue(undefined);
    tauriMocks.refreshProvidersIfStale.mockResolvedValue(undefined);
    tauriMocks.listKimiAccounts.mockResolvedValue([]);
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        PanelUsedSuffix: "used",
        ProviderMonthlyLabel: "月额度",
        KimiAccountMonthlyQuota: "月额度",
        KimiAccountLinkedApiKeys: "此账号的 API Key",
        KimiAccountUnlinkedApiKeys: "未关联 Kimi 月额度",
        TrayStatusRowError: "Error",
      }),
    );
  });

  it("waits for cached credentials before requesting the first window fit", async () => {
    let resolveCache!: (providers: ProviderUsageSnapshot[]) => void;
    tauriMocks.getCachedProviders.mockReturnValue(
      new Promise<ProviderUsageSnapshot[]>((resolve) => {
        resolveCache = resolve;
      }),
    );

    render(
      <LocaleProvider>
        <TrayHover state={state} />
      </LocaleProvider>,
    );

    await waitFor(() => expect(tauriMocks.getCachedProviders).toHaveBeenCalled());
    expect(coreMocks.invoke).not.toHaveBeenCalled();

    resolveCache([credential("Work", 1, 12, 25)]);

    await waitFor(() =>
      expect(coreMocks.invoke).toHaveBeenCalledWith(
        "resize_tray_hover",
        expect.objectContaining({ width: 360 }),
      ),
    );
  });

  it("measures intrinsic content width instead of echoing the current narrow viewport", async () => {
    const scrollWidth = vi
      .spyOn(HTMLElement.prototype, "scrollWidth", "get")
      .mockImplementation(function intrinsicWidth(this: HTMLElement) {
        return this.style.width === "max-content" ? 487 : 320;
      });
    const clientWidth = vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(320);
    try {
      tauriMocks.getCachedProviders.mockResolvedValue([credential("Work", 1, 12, 25)]);

      const { container } = render(
        <LocaleProvider>
          <TrayHover state={state} />
        </LocaleProvider>,
      );

      await waitFor(() =>
        expect(coreMocks.invoke).toHaveBeenCalledWith(
          "resize_tray_hover",
          expect.objectContaining({ width: 487 }),
        ),
      );
      expect(container.querySelector<HTMLElement>(".tray-hover")?.style.width).toBe("");
    } finally {
      scrollWidth.mockRestore();
      clientWidth.mockRestore();
    }
  });

  it("shows every credential and its actual primary and secondary quota labels", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      credential("Work", 1, 12, 25),
      credential("Personal", 2, 34, 48),
    ]);

    const { container } = render(
      <LocaleProvider>
        <TrayHover state={state} />
      </LocaleProvider>,
    );

    await waitFor(() => expect(container.querySelectorAll(".tray-hover__credential")).toHaveLength(2));
    expect(container.textContent).toContain("Work");
    expect(container.textContent).toContain("Personal");
    expect(container.textContent).toContain("Five hours");
    expect(container.textContent).toContain("Weekly");
    expect(container.textContent).toContain("12%");
    expect(container.textContent).toContain("48%");
    expect(tauriMocks.refreshProvidersIfStale).not.toHaveBeenCalled();
  });

  it("shows GLM five-hour and weekly quotas for its API key", async () => {
    const glm = {
      ...credential("Key 1", 1, 0, 92),
      providerId: "zai",
      displayName: "GLM（智谱 BigModel / z.ai）",
      primary: { ...rate(0), windowMinutes: 5 * 60 },
      primaryLabel: "Rate Limit",
      secondary: { ...rate(92), windowMinutes: 7 * 24 * 60 },
      secondaryLabel: "Weekly",
    } satisfies ProviderUsageSnapshot;
    tauriMocks.getCachedProviders.mockResolvedValue([glm]);
    const providerState = {
      ...state,
      settings: {
        ...state.settings,
        enabledProviders: ["zai"],
        providerOrder: ["zai"],
        showAsUsed: true,
      },
    } as BootstrapState;

    const { container } = render(
      <LocaleProvider>
        <TrayHover state={providerState} />
      </LocaleProvider>,
    );

    await waitFor(() => expect(container.textContent).toContain("GLM"));
    expect(container.textContent).toContain("Key 1");
    expect(container.textContent).toContain("Rate Limit");
    expect(container.textContent).toContain("0%");
    expect(container.textContent).toContain("Weekly");
    expect(container.textContent).toContain("92%");
  });

  it("keeps real Grok and Chutes unknown-cycle percentages ordinary", async () => {
    const grok = {
      ...credential("Grok", 1, 26, 0),
      providerId: "grok",
      displayName: "Grok",
      credentialId: undefined,
      credentialDisplayLabel: undefined,
      credentialDisplayOrdinal: undefined,
      primaryLabel: "Monthly",
      secondary: null,
      secondaryLabel: "On-demand",
    } satisfies ProviderUsageSnapshot;
    const chutes = {
      ...credential("Chutes", 1, 14, 37),
      providerId: "chutes",
      displayName: "Chutes",
      credentialId: undefined,
      credentialDisplayLabel: undefined,
      credentialDisplayOrdinal: undefined,
      primaryLabel: "4-hour quota",
      secondaryLabel: "Monthly quota",
    } satisfies ProviderUsageSnapshot;
    tauriMocks.getCachedProviders.mockResolvedValue([grok, chutes]);
    const providerState = {
      ...state,
      settings: {
        ...state.settings,
        enabledProviders: ["grok", "chutes"],
        providerOrder: ["grok", "chutes"],
        showAsUsed: true,
      },
    } as BootstrapState;

    const { container } = render(
      <LocaleProvider>
        <TrayHover state={providerState} />
      </LocaleProvider>,
    );

    await waitFor(() => expect(container.textContent).toContain("26%"));
    expect(container.textContent).toContain("14%");
    expect(container.textContent).toContain("37%");
    expect(container.querySelectorAll(".tray-hover__quota")).toHaveLength(3);
    expect(container.querySelector(".tray-hover__quota--monthly")).not.toBeInTheDocument();
    expect(container.querySelector(".monthly-quota-badge")).not.toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
  });

  it("shows all monthly quotas and reset times without monthly spend", async () => {
    const snapshot = credential("Work", 1, 12, 25);
    snapshot.tertiary = {
      ...rate(45),
      windowMinutes: 30 * 24 * 60,
      resetDescription: "Account reset",
    };
    snapshot.extraRateWindows = [
      {
        id: "agent-monthly",
        title: "Agent Monthly",
        window: {
          ...rate(65),
          windowMinutes: 31 * 24 * 60,
          resetDescription: "Agent reset",
        },
      },
      {
        id: "monthly-spend",
        title: "Monthly spend",
        window: rate(80),
      },
    ];
    tauriMocks.getCachedProviders.mockResolvedValue([snapshot]);

    const { container } = render(
      <LocaleProvider>
        <TrayHover state={state} />
      </LocaleProvider>,
    );

    await waitFor(() => expect(container.textContent).toContain("月额度 · Agent"));
    expect(container.textContent).toContain("月额度");
    expect(container.textContent).toContain("Account reset");
    expect(container.textContent).toContain("Agent reset");
    expect(container.textContent).not.toContain("Monthly spend");
  });

  it("groups stable provider accounts, masks email, and deduplicates only audited monthly quota", async () => {
    const groupedCredential = (label: string, ordinal: number, monthly: number) => ({
      ...credential(label, ordinal, 10 + ordinal, 20 + ordinal),
      providerId: "clinepass",
      displayName: "ClinePass",
      accountGroupId: "clinepass:account-42",
      accountEmail: "owner@example.com",
      primary: { ...rate(10 + ordinal), windowMinutes: 5 * 60 },
      secondary: { ...rate(20 + ordinal), windowMinutes: 7 * 24 * 60 },
      tertiary: { ...rate(45), windowMinutes: 30 * 24 * 60 },
      extraRateWindows: [{
        id: `member-${ordinal}-monthly`,
        title: `Agent ${ordinal} Monthly`,
        window: { ...rate(monthly), windowMinutes: 31 * 24 * 60 },
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
    const groupedState = {
      ...state,
      settings: {
        ...state.settings,
        enabledProviders: ["clinepass"],
        providerOrder: ["clinepass"],
        hidePersonalInfo: true,
        showAsUsed: false,
        resetTimeRelative: true,
      },
    } as BootstrapState;

    const { container } = render(
      <LocaleProvider>
        <TrayHover state={groupedState} />
      </LocaleProvider>,
    );

    const account = await screen.findByRole("region", { name: "o••••@example.com" });
    expect(account).toHaveClass("tray-hover__account-group");
    expect(within(account).getByText("Work")).toBeInTheDocument();
    expect(within(account).getByText("Personal")).toBeInTheDocument();
    expect(within(account).getAllByText("Five hours")).toHaveLength(2);
    expect(within(account).getAllByText("Weekly")).toHaveLength(2);
    expect(within(account).getAllByText("月额度")).toHaveLength(1);
    expect(within(account).getByText("月额度 · Agent 1")).toHaveClass("monthly-quota-badge");
    expect(within(account).getByText("月额度 · Agent 2")).toHaveClass("monthly-quota-badge");
    expect(within(account).getAllByRole("progressbar")).toHaveLength(3);
    expect(within(account).getByRole("progressbar", { name: "月额度 · Agent 1" }))
      .toHaveAttribute("aria-valuenow", "39");
    expect(account).not.toHaveTextContent("Unmatched Key");
    expect(screen.getByText("Unmatched Key")).toBeInTheDocument();
    expect(container).not.toHaveTextContent("clinepass:account-42");
    expect(container).not.toHaveTextContent("owner@example.com");
  });

  it("keeps failed credentials visible with a safe error label", async () => {
    const failed = credential("Backup", 2, 0, 0);
    failed.error = "raw secret failure";
    tauriMocks.getCachedProviders.mockResolvedValue([failed]);

    const { container } = render(
      <LocaleProvider>
        <TrayHover state={state} />
      </LocaleProvider>,
    );

    await waitFor(() => expect(container.textContent).toContain("Backup"));
    expect(container.textContent).toContain("Error");
    expect(container.textContent).not.toContain("raw secret failure");
  });

  it("groups browser monthly quota with the matching Kimi keys and isolates unlinked keys", async () => {
    tauriMocks.getCachedProviders.mockResolvedValue([
      credential("Work", 1, 12, 25),
      credential("Personal", 2, 34, 48),
      credential("Unlinked", 3, 20, 30),
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
    const kimiState = {
      ...state,
      settings: {
        ...state.settings,
        kimiMonthlyQuotaEnabled: true,
        showAsUsed: true,
        resetTimeRelative: true,
      },
    } as BootstrapState;

    render(
      <LocaleProvider>
        <TrayHover state={kimiState} />
      </LocaleProvider>,
    );

    const accountA = await screen.findByRole("region", { name: "账号 A" });
    const accountB = screen.getByRole("region", { name: "账号 B" });
    const unlinked = screen.getByRole("region", { name: "未关联 Kimi 月额度" });
    expect(within(accountA).getByText("Work")).toBeInTheDocument();
    expect(within(accountA).queryByText("Personal")).not.toBeInTheDocument();
    expect(within(accountA).getByText("Edge · Profile 1")).toBeInTheDocument();
    expect(within(accountA).getByRole("progressbar", { name: "账号 A 月额度" })).toHaveAttribute(
      "aria-valuenow",
      "35",
    );
    expect(within(accountB).getByText("Personal")).toBeInTheDocument();
    expect(within(unlinked).getByText("Unlinked")).toBeInTheDocument();
  });
});
