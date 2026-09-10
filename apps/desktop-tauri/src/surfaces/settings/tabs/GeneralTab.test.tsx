import { act, fireEvent, render, screen } from "@testing-library/react";
import type { ReactElement } from "react";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../../hooks/useLocale", () => ({
  useLocale: () => ({ t: (key: string) => key }),
}));

// Mock Tauri invoke for get_available_languages
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn().mockResolvedValue([
    { value: "english", display: "English" },
    { value: "chinese", display: "中文" },
    { value: "chinesetraditional", display: "繁體中文" },
    { value: "japanese", display: "日本語" },
    { value: "korean", display: "한국어" },
    { value: "spanish", display: "Español" },
  ]),
}));

import GeneralTab from "./GeneralTab";
import type { SettingsSnapshot } from "../../../types/bridge";

async function renderTab(element: ReactElement) {
  let result!: ReturnType<typeof render>;
  await act(async () => {
    result = render(element);
  });
  return result;
}

const settings: SettingsSnapshot = {
  enabledProviders: [],
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
  menuBarShowsHighestUsage: true,
  menuBarShowsPercent: true,
  showAsUsed: false,
  showAllTokenAccountsInMenu: true,
  enableAnimations: true,
  resetTimeRelative: true,
  menuBarDisplayMode: "compact",
  windowScalePercent: 125,
  trayScalePercent: 100,
  powertoysStatusPipeEnabled: false,
  hidePersonalInfo: false,
  autoDownloadUpdates: false,
  installUpdatesOnQuit: false,
  globalShortcut: "",
  codexCustomSessionsDirs: [],
  updateChannel: "stable",
  uiLanguage: "english",
  theme: "dark",
  claudeAvoidKeychainPrompts: true,
  codexSparkUsageVisible: true,
  disableKeychainAccess: false,
  providerMetrics: {},
  floatingBallEnabled: false,
  kimiMonthlyQuotaEnabled: false,
  kimiMonthlyConsentAccepted: false,
  floatBarEnabled: false,
  floatBarOpacity: 0.9,
  floatBarScale: 100,
  floatBarOrientation: "horizontal",
  floatBarStyle: "floating",
  floatBarClickThrough: false,
  floatBarProviderIds: [],
  floatBarDarkText: false,
  floatBarShowResetInline: false,
  floatBarShowCost: false,
  showResetWhenExhausted: false,
};

describe("GeneralTab language picker", () => {
  it("shows an unavailable Windows startup state instead of the normal helper", async () => {
    await renderTab(<GeneralTab settings={{ ...settings, startAtLoginIssue: "Installation verification failed" }} set={vi.fn()} saving={false} />);
    expect(screen.getByText("Installation verification failed")).toBeInTheDocument();
    expect(screen.queryByText("StartAtLoginHelper")).not.toBeInTheDocument();
  });

  it("renders 6 language options when Traditional Chinese is wired", async () => {
    await renderTab(<GeneralTab settings={settings} set={vi.fn()} saving={false} />);

    const select = screen.getByDisplayValue("English");
    expect(select).toBeInTheDocument();

    const options = select.querySelectorAll("option");
    expect(options).toHaveLength(6);
  });

  it("includes spanish as a selectable option", async () => {
    await renderTab(<GeneralTab settings={settings} set={vi.fn()} saving={false} />);

    expect(
      screen.getByText("Español"),
    ).toBeInTheDocument();
  });

  it("includes korean as a selectable option", async () => {
    await renderTab(<GeneralTab settings={settings} set={vi.fn()} saving={false} />);

    expect(
      screen.getByText("한국어"),
    ).toBeInTheDocument();
  });

  it("includes Traditional Chinese as a selectable option", async () => {
    await renderTab(<GeneralTab settings={settings} set={vi.fn()} saving={false} />);

    expect(screen.getByText("繁體中文")).toBeInTheDocument();
  });

  it("updates the predictive pace warning preference", async () => {
    const set = vi.fn();
    await renderTab(
      <GeneralTab
        mode="notifications"
        settings={settings}
        set={set}
        saving={false}
      />,
    );

    fireEvent.click(screen.getByRole("checkbox", { name: "PredictivePaceWarnings" }));

    expect(set).toHaveBeenCalledWith({ predictivePaceWarningEnabled: true });
  });

  it("只更新桌面悬浮球开关", async () => {
    const set = vi.fn();
    await renderTab(<GeneralTab settings={settings} set={set} saving={false} />);

    fireEvent.click(screen.getByRole("checkbox", { name: "FloatingBallShow" }));

    expect(set).toHaveBeenCalledWith({ floatingBallEnabled: true });
  });

  it("saves a window override on blur and clears it to resume inheritance", async () => {
    const set = vi.fn();
    const { rerender } = await renderTab(
      <GeneralTab mode="notifications" settings={settings} set={set} saving={false} />,
    );
    const input = screen.getByRole("spinbutton", {
      name: "ProviderNameCodex · ProviderSession HighUsageAlert",
    });

    fireEvent.change(input, { target: { value: "80" } });
    fireEvent.blur(input);
    expect(set).toHaveBeenLastCalledWith({
      providerUsageThresholds: { "codex:session": { high: 80 } },
    });

    rerender(
      <GeneralTab
        mode="notifications"
        settings={{
          ...settings,
          providerUsageThresholds: { "codex:session": { high: 80 } },
        }}
        set={set}
        saving={false}
      />,
    );
    const saved = screen.getByRole("spinbutton", {
      name: "ProviderNameCodex · ProviderSession HighUsageAlert",
    });
    fireEvent.change(saved, { target: { value: "" } });
    fireEvent.blur(saved);
    expect(set).toHaveBeenLastCalledWith({ providerUsageThresholds: {} });
  });

  it("asks for consent before enabling Kimi monthly quota and remembers acceptance", async () => {
    const set = vi.fn();
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    await renderTab(<GeneralTab settings={settings} set={set} saving={false} />);

    fireEvent.click(screen.getByRole("checkbox", { name: "KimiMonthlyQuotaShow" }));

    expect(confirm).toHaveBeenCalledWith("KimiMonthlyConsentMessage");
    expect(set).toHaveBeenCalledWith({
      kimiMonthlyQuotaEnabled: true,
      kimiMonthlyConsentAccepted: true,
    });
    confirm.mockRestore();
  });
});
