import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  BootstrapState,
  KimiAccountSnapshot,
  ProviderUsageSnapshot,
  RateWindowSnapshot,
  SettingsSnapshot,
} from "../types/bridge";

const providerMocks = vi.hoisted(() => ({
  providers: [] as ProviderUsageSnapshot[],
  kimiAccounts: [] as KimiAccountSnapshot[],
}));
const settingsMocks = vi.hoisted(() => ({
  settings: {} as SettingsSnapshot,
}));
const apiMocks = vi.hoisted(() => ({
  hideFloatingQuota: vi.fn().mockResolvedValue(undefined),
  openMainWindowFromFloatingQuota: vi.fn().mockResolvedValue(undefined),
  resizeFloatingQuota: vi.fn().mockResolvedValue(undefined),
  startFloatingQuotaDrag: vi.fn().mockResolvedValue(undefined),
  toggleFloatingQuotaDesktop: vi.fn().mockResolvedValue("desktop"),
  toggleFloatingQuotaTopmost: vi.fn().mockResolvedValue("topmost"),
}));
const themeMocks = vi.hoisted(() => ({ useTheme: vi.fn() }));

vi.mock("../hooks/useProviders", () => ({
  useProviders: () => ({
    providers: providerMocks.providers,
    kimiAccounts: providerMocks.kimiAccounts,
    hasLoadedCache: true,
  }),
}));
vi.mock("../hooks/useSettings", () => ({
  useSettings: () => ({ settings: settingsMocks.settings }),
}));
vi.mock("../hooks/useTheme", () => themeMocks);
vi.mock("./api", () => apiMocks);
vi.mock("../hooks/useLocale", () => ({
  useLocale: () => ({
    t: (key: string) => ({
      CreditsTitle: "额度",
      FloatingQuotaDesktop: "固定在桌面",
      FloatingQuotaUndockDesktop: "退出桌面模式",
      FloatingQuotaOpenMain: "打开主窗口",
      FloatingQuotaHide: "隐藏",
      FloatingQuotaPin: "固定在最前端",
      FloatingQuotaUnpin: "取消固定在最前端",
      FloatingQuotaEmpty: "暂无五小时或本周额度。",
      ProviderRateLimitLabel: "五小时额度",
      ProviderWeeklyLabel: "本周",
      ProviderMonthlyLabel: "月额度",
      ProviderStatusError: "获取失败",
      KimiAccountMonthlyQuota: "月额度",
      KimiAccountLinkedApiKeys: "此账号的 API Key",
      KimiAccountUnlinkedApiKeys: "未关联 Kimi 月额度",
      PanelUsedSuffix: "已使用",
      PanelLeftSuffix: "剩余",
    })[key] ?? key,
  }),
}));

import FloatingQuota from "./FloatingQuota";

class TestResizeObserver {
  constructor(private readonly callback: ResizeObserverCallback) {}

  observe(target: Element) {
    this.callback([{ target } as ResizeObserverEntry], this as unknown as ResizeObserver);
  }

  disconnect() {}
  unobserve() {}
}

beforeAll(() => vi.stubGlobal("ResizeObserver", TestResizeObserver));
afterAll(() => vi.unstubAllGlobals());

function rate(
  usedPercent: number,
  windowMinutes: number | null,
  resetDescription: string,
): RateWindowSnapshot {
  return {
    usedPercent,
    remainingPercent: 100 - usedPercent,
    windowMinutes,
    resetsAt: null,
    resetDescription,
    isExhausted: false,
    reservePercent: null,
    reserveDescription: null,
  };
}

function credential(
  label: string,
  ordinal: number,
  primary: RateWindowSnapshot,
  primaryLabel: string,
  secondary: RateWindowSnapshot | null,
  secondaryLabel: string | undefined,
): ProviderUsageSnapshot {
  return {
    providerId: "kimi",
    displayName: "Kimi",
    credentialId: `credential-${ordinal}`,
    credentialDisplayLabel: label,
    credentialDisplayOrdinal: ordinal,
    credentialGroupSize: 2,
    primary,
    primaryLabel,
    secondary,
    secondaryLabel,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [
      {
        id: "monthly",
        title: "Monthly",
        window: rate(50, null, "下月重置"),
      },
    ],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "api-key",
    updatedAt: "2026-08-11T00:00:00Z",
    error: null,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
  };
}

const state = {
  contractVersion: "v1",
  providers: [],
  settings: {},
} as unknown as BootstrapState;

function baseSettings(overrides: Partial<SettingsSnapshot> = {}): SettingsSnapshot {
  return {
    enabledProviders: ["kimi"],
    providerOrder: ["kimi"],
    showAsUsed: true,
    resetTimeRelative: true,
    theme: "dark",
    ...overrides,
  } as SettingsSnapshot;
}

describe("FloatingQuota", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    settingsMocks.settings = baseSettings();
    providerMocks.kimiAccounts = [];
    providerMocks.providers = [
      credential(
        "Work",
        1,
        rate(25, 7 * 24 * 60, "3 天后"),
        "Weekly",
        rate(10, 5 * 60, "2 小时后"),
        "Rate Limit",
      ),
      credential(
        "Personal",
        2,
        rate(40, 5 * 60, "4 小时后"),
        "Rate Limit",
        rate(60, 7 * 24 * 60, "5 天后"),
        "Weekly",
      ),
    ];
  });

  it("按供应商保留每个 API Key，并只显示五小时、本周和各自重置时间", () => {
    const { container } = render(<FloatingQuota state={state} />);

    expect(screen.getByRole("heading", { level: 2, name: "Kimi" })).toBeInTheDocument();
    expect(screen.getByText("Work")).toBeInTheDocument();
    expect(screen.getByText("Personal")).toBeInTheDocument();
    expect(screen.getAllByText("五小时额度")).toHaveLength(2);
    expect(screen.getAllByText("本周")).toHaveLength(2);
    expect(screen.getByText("2 小时后")).toBeInTheDocument();
    expect(screen.getByText("3 天后")).toBeInTheDocument();
    expect(screen.queryByText("Monthly")).not.toBeInTheDocument();
    expect(screen.getAllByText("下月重置")).toHaveLength(2);
    expect(container.querySelector(".menu-metric__bar")).toBeNull();
  });

  it("显示 GLM API Key 的五小时和本周额度", () => {
    providerMocks.providers = [{
      ...credential(
        "Key 1",
        1,
        rate(0, 5 * 60, "5 小时后"),
        "Rate Limit",
        rate(92, 7 * 24 * 60, "3 天后"),
        "Weekly",
      ),
      providerId: "zai",
      displayName: "GLM（智谱 BigModel / z.ai）",
      credentialGroupSize: 1,
      extraRateWindows: [],
    } satisfies ProviderUsageSnapshot];
    settingsMocks.settings = baseSettings({
      enabledProviders: ["zai"],
      providerOrder: ["zai"],
    });

    render(<FloatingQuota state={state} />);

    expect(screen.getByRole("heading", { level: 2, name: "GLM（智谱 BigModel / z.ai）" }))
      .toBeInTheDocument();
    expect(screen.getByText("Key 1")).toBeInTheDocument();
    expect(screen.getByText("0% 已使用")).toBeInTheDocument();
    expect(screen.getByText("92% 已使用")).toBeInTheDocument();
    expect(screen.getByText("五小时额度")).toBeInTheDocument();
    expect(screen.getByText("本周")).toBeInTheDocument();
  });

  it("跟随主页面设置显示已使用或剩余百分比", () => {
    const { rerender } = render(<FloatingQuota state={state} />);
    expect(screen.getByText("10% 已使用")).toBeInTheDocument();

    settingsMocks.settings = baseSettings({ showAsUsed: false });
    rerender(<FloatingQuota state={state} />);
    expect(screen.getByText("90% 剩余")).toBeInTheDocument();
  });

  it("keeps real Grok and Chutes unknown-cycle percentages ordinary", () => {
    const grok = {
      ...credential("Grok", 1, rate(26, null, ""), "Monthly", null, "On-demand"),
      providerId: "grok",
      displayName: "Grok",
      credentialId: undefined,
      credentialDisplayLabel: undefined,
      credentialDisplayOrdinal: undefined,
      credentialGroupSize: undefined,
      extraRateWindows: [],
    } satisfies ProviderUsageSnapshot;
    const chutes = {
      ...credential(
        "Chutes",
        1,
        rate(14, null, ""),
        "4-hour quota",
        rate(37, null, ""),
        "Monthly quota",
      ),
      providerId: "chutes",
      displayName: "Chutes",
      credentialId: undefined,
      credentialDisplayLabel: undefined,
      credentialDisplayOrdinal: undefined,
      credentialGroupSize: undefined,
      extraRateWindows: [],
    } satisfies ProviderUsageSnapshot;
    providerMocks.providers = [grok, chutes];
    settingsMocks.settings = baseSettings({
      enabledProviders: ["grok", "chutes"],
      providerOrder: ["grok", "chutes"],
    });

    const { container } = render(<FloatingQuota state={state} />);

    expect(screen.getByText("26% 已使用")).toBeInTheDocument();
    expect(screen.getByText("14% 已使用")).toBeInTheDocument();
    expect(screen.getByText("37% 已使用")).toBeInTheDocument();
    expect(container.querySelectorAll(".floating-quota__metric")).toHaveLength(3);
    expect(container.querySelector(".floating-quota__metric--monthly")).not.toBeInTheDocument();
    expect(container.querySelector(".monthly-quota-badge")).not.toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
  });

  it("does not surface statistics, budgets, informational slots, or unknown labels as ordinary quota", () => {
    const rejectedProvider = (
      providerId: string,
      label: string,
      usedPercent: number,
      isInformational = false,
    ) => ({
      ...credential(label, 1, {
        ...rate(usedPercent, null, ""),
        isInformational,
      }, label, null, undefined),
      providerId,
      displayName: providerId,
      credentialId: undefined,
      credentialDisplayLabel: undefined,
      credentialDisplayOrdinal: undefined,
      credentialGroupSize: undefined,
      extraRateWindows: [],
    }) satisfies ProviderUsageSnapshot;
    providerMocks.providers = [
      rejectedProvider("openai", "Spend", 61),
      rejectedProvider("bedrock", "Budget", 42),
      rejectedProvider("openai", "Monthly quota", 53, true),
      rejectedProvider("openai", "Capacity", 74),
    ];
    settingsMocks.settings = baseSettings({
      enabledProviders: ["openai", "bedrock"],
      providerOrder: ["openai", "bedrock"],
    });

    const { container } = render(<FloatingQuota state={state} />);

    expect(screen.getByText("暂无五小时或本周额度。")).toBeInTheDocument();
    expect(container.querySelector(".floating-quota__metric")).not.toBeInTheDocument();
    expect(container.textContent).not.toMatch(/61%|42%|53%|74%/);
  });

  it("显示全部账户级和范围级月额度及其重置时间", () => {
    providerMocks.providers[0].extraRateWindows.push({
      id: "agent-monthly",
      title: "Agent Monthly",
      window: rate(65, 30 * 24 * 60, "Agent reset"),
    });

    render(<FloatingQuota state={state} />);

    expect(screen.getAllByText("月额度")).toHaveLength(2);
    expect(screen.getByText("月额度 · Agent")).toBeInTheDocument();
    expect(screen.getByText("Agent reset")).toBeInTheDocument();
    expect(screen.queryByText("Monthly")).not.toBeInTheDocument();
  });

  it("groups stable provider accounts, masks email, and deduplicates only audited monthly quota", () => {
    const groupedCredential = (label: string, ordinal: number, monthly: number) => ({
      ...credential(
        label,
        ordinal,
        rate(10 + ordinal, 5 * 60, `${ordinal} 小时后`),
        "Rate Limit",
        rate(20 + ordinal, 7 * 24 * 60, `${ordinal} 天后`),
        "Weekly",
      ),
      providerId: "clinepass",
      displayName: "ClinePass",
      accountGroupId: "clinepass:account-42",
      accountEmail: "owner@example.com",
      tertiary: rate(45, 30 * 24 * 60, "共享月重置"),
      extraRateWindows: [{
        id: `member-${ordinal}-monthly`,
        title: `Agent ${ordinal} Monthly`,
        window: rate(monthly, 31 * 24 * 60, `成员 ${ordinal} 月重置`),
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
    providerMocks.providers = [first, second, unmatched];
    settingsMocks.settings = baseSettings({
      enabledProviders: ["clinepass"],
      providerOrder: ["clinepass"],
      hidePersonalInfo: true,
      showAsUsed: false,
    });

    const { container } = render(<FloatingQuota state={state} />);

    const account = screen.getByRole("region", { name: "o••••@example.com" });
    expect(account).toHaveClass("floating-quota__account-group");
    expect(within(account).getByText("Work")).toBeInTheDocument();
    expect(within(account).getByText("Personal")).toBeInTheDocument();
    expect(within(account).getAllByText("五小时额度")).toHaveLength(2);
    expect(within(account).getAllByText("本周")).toHaveLength(2);
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

  it("按 Kimi 账号包住对应 API Key，并把未关联 Key 单独显示", () => {
    providerMocks.providers.push(
      credential(
        "Unlinked",
        3,
        rate(15, 5 * 60, "1 小时后"),
        "Rate Limit",
        rate(25, 7 * 24 * 60, "6 天后"),
        "Weekly",
      ),
    );
    providerMocks.kimiAccounts = [
      {
        accountId: "account-a",
        displayName: "账号 A",
        sourceLabels: ["Edge · Profile 1"],
        usedPercent: 35,
        resetsAt: null,
        updatedAt: "2026-08-23T00:00:00Z",
        status: "ok",
        matchedCredentialIds: ["credential-1"],
      },
      {
        accountId: "account-b",
        displayName: "账号 B",
        sourceLabels: ["Chrome · Default"],
        usedPercent: 50,
        resetsAt: null,
        updatedAt: "2026-08-23T00:00:00Z",
        status: "ok",
        matchedCredentialIds: ["credential-2"],
      },
    ];
    settingsMocks.settings = baseSettings({ kimiMonthlyQuotaEnabled: true });

    render(<FloatingQuota state={state} />);

    const accountA = screen.getByRole("region", { name: "账号 A" });
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

  it("隔离单个 Key 的错误且不暴露原始错误文本", () => {
    providerMocks.providers[0] = {
      ...providerMocks.providers[0],
      error: "raw secret provider failure",
    };

    render(<FloatingQuota state={state} />);

    expect(screen.getByText("Work")).toBeInTheDocument();
    expect(screen.getByText("获取失败")).toBeInTheDocument();
    expect(screen.queryByText("raw secret provider failure")).not.toBeInTheDocument();
    expect(screen.getByText("Personal")).toBeInTheDocument();
    expect(screen.getByText("40% 已使用")).toBeInTheDocument();
  });

  it("隐藏按钮只隐藏窗口并将内容高度限制在原生范围内", async () => {
    render(<FloatingQuota state={state} />);

    fireEvent.click(screen.getByRole("button", { name: "隐藏" }));

    expect(apiMocks.hideFloatingQuota).toHaveBeenCalledTimes(1);
    await waitFor(() => {
      expect(apiMocks.resizeFloatingQuota).toHaveBeenCalledWith(80);
    });
  });

  it("按住顶部栏启动原生拖动，隐藏按钮不启动拖动", () => {
    const { container } = render(<FloatingQuota state={state} />);
    const header = container.querySelector(".floating-quota__header");

    expect(header).not.toBeNull();
    fireEvent.mouseDown(header!);
    expect(apiMocks.startFloatingQuotaDrag).toHaveBeenCalledTimes(1);

    fireEvent.mouseDown(screen.getByRole("button", { name: "隐藏" }));
    expect(apiMocks.startFloatingQuotaDrag).toHaveBeenCalledTimes(1);
  });

  it("普通、置顶和桌面三种状态可以任意一键切换", async () => {
    apiMocks.toggleFloatingQuotaTopmost
      .mockResolvedValueOnce("topmost")
      .mockResolvedValueOnce("topmost")
      .mockResolvedValueOnce("normal");
    render(<FloatingQuota state={state} />);
    const pin = screen.getByRole("button", { name: "固定在最前端" });
    const desktop = screen.getByRole("button", { name: "固定在桌面" });

    expect(pin).toHaveAttribute("aria-pressed", "false");
    expect(desktop).toHaveAttribute("aria-pressed", "false");

    fireEvent.click(pin);
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "取消固定在最前端" }))
        .toHaveAttribute("aria-pressed", "true");
    });

    fireEvent.click(desktop);
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "退出桌面模式" }))
        .toHaveAttribute("aria-pressed", "true");
      expect(screen.getByRole("button", { name: "固定在最前端" }))
        .toHaveAttribute("aria-pressed", "false");
    });

    fireEvent.click(screen.getByRole("button", { name: "固定在最前端" }));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "取消固定在最前端" }))
        .toHaveAttribute("aria-pressed", "true");
      expect(screen.getByRole("button", { name: "固定在桌面" }))
        .toHaveAttribute("aria-pressed", "false");
    });

    fireEvent.click(screen.getByRole("button", { name: "取消固定在最前端" }));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "固定在最前端" }))
        .toHaveAttribute("aria-pressed", "false");
      expect(screen.getByRole("button", { name: "固定在桌面" }))
        .toHaveAttribute("aria-pressed", "false");
    });
  });

  it("打开主窗口按钮调用桌面命令且不触发拖动", () => {
    render(<FloatingQuota state={state} />);
    const openMain = screen.getByRole("button", { name: "打开主窗口" });

    fireEvent.mouseDown(openMain);
    expect(apiMocks.startFloatingQuotaDrag).not.toHaveBeenCalled();

    fireEvent.click(openMain);
    expect(apiMocks.openMainWindowFromFloatingQuota).toHaveBeenCalledTimes(1);
  });
});
