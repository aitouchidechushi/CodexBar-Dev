import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const tauriMocks = vi.hoisted(() => ({
  getCachedProviders: vi.fn(),
  refreshProviders: vi.fn(),
  refreshProvidersIfStale: vi.fn(),
  getSettingsSnapshot: vi.fn(),
  getUpdateState: vi.fn(),
  checkForUpdates: vi.fn(),
  downloadUpdate: vi.fn(),
  applyUpdate: vi.fn(),
  dismissUpdate: vi.fn(),
  openReleasePage: vi.fn(),
  openFlyoutWindow: vi.fn(),
  openSettingsWindow: vi.fn(),
  quitApp: vi.fn(),
  getProviderChartData: vi.fn(),
  getApiKeyProviders: vi.fn(),
  getApiKeys: vi.fn(),
  getApiKeySecret: vi.fn(),
  addApiKey: vi.fn(),
  updateApiKeyLabel: vi.fn(),
  replaceApiKeySecret: vi.fn(),
  deleteApiKey: vi.fn(),
  getLocaleStrings: vi.fn(),
  setUiLanguage: vi.fn(),
}));

const eventMocks = vi.hoisted(() => ({
  listen: vi.fn(),
}));

const windowMocks = vi.hoisted(() => {
  const setSize = vi.fn().mockResolvedValue(undefined);
  const setPosition = vi.fn().mockResolvedValue(undefined);
  const minimize = vi.fn().mockResolvedValue(undefined);
  const toggleMaximize = vi.fn().mockResolvedValue(undefined);
  const close = vi.fn().mockResolvedValue(undefined);
  const isMaximized = vi.fn().mockResolvedValue(false);
  const onResized = vi.fn().mockResolvedValue(() => {});
  return {
    setSize,
    setPosition,
    minimize,
    toggleMaximize,
    close,
    isMaximized,
    onResized,
    getCurrentWindow: vi.fn(() => ({
      setSize,
      setPosition,
      minimize,
      toggleMaximize,
      close,
      isMaximized,
      onResized,
    })),
    LogicalSize: vi.fn((width: number, height: number) => ({ width, height })),
    LogicalPosition: vi.fn((x: number, y: number) => ({ x, y })),
  };
});

const webviewWindowMocks = vi.hoisted(() => {
  const setZoom = vi.fn().mockResolvedValue(undefined);
  return {
    setZoom,
    getCurrentWebviewWindow: vi.fn(() => ({ setZoom })),
  };
});

vi.mock("../lib/tauri", () => tauriMocks);
vi.mock("@tauri-apps/api/event", () => eventMocks);
vi.mock("@tauri-apps/api/window", () => windowMocks);
vi.mock("@tauri-apps/api/webviewWindow", () => webviewWindowMocks);

import PopOutPanel from "./PopOutPanel";
import { LocaleProvider } from "../i18n/LocaleProvider";
import { buildBundle } from "../test/localeHarness";
import { TEST_PROVIDER_CATALOG } from "../test/providerCatalog";
import type {
  BootstrapState,
  ProviderCatalogEntry,
  ProviderUsageSnapshot,
  SettingsSnapshot,
} from "../types/bridge";

function rateWindow(used: number) {
  return {
    usedPercent: used,
    remainingPercent: 100 - used,
    windowMinutes: null,
    resetsAt: null,
    resetDescription: null,
    isExhausted: false,
    reservePercent: null,
    reserveDescription: null,
  };
}

function provider(id: string, displayName: string, used = 20): ProviderUsageSnapshot {
  return {
    providerId: id,
    displayName,
    primary: rateWindow(used),
    primaryLabel: "Monthly",
    secondary: null,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "auto",
    updatedAt: "2026-05-24T00:00:00Z",
    error: null,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
    fetchDurationMs: null,
  };
}

function settings(): SettingsSnapshot {
  return {
    enabledProviders: ["codex", "claude"],
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
    floatBarEnabled: false,
    floatBarOpacity: 80,
    floatBarScale: 100,
    floatBarOrientation: "horizontal",
    floatBarStyle: "floating",
    floatBarClickThrough: false,
    floatBarProviderIds: [],
    floatBarDarkText: false,
    floatBarShowResetInline: false,
    floatBarShowCost: false,
  };
}

function bootstrap(
  catalog: ProviderCatalogEntry[] = [],
  settingsOverride: Partial<SettingsSnapshot> = {},
): BootstrapState {
  return {
    contractVersion: "v1",
    providers: catalog,
    settings: { ...settings(), ...settingsOverride },
  };
}

function renderPopOut(
  providers: ProviderUsageSnapshot[],
  providerId?: string,
  catalog: ProviderCatalogEntry[] = [],
  settingsOverride: Partial<SettingsSnapshot> = {},
) {
  tauriMocks.getCachedProviders.mockResolvedValue(providers);
  tauriMocks.getSettingsSnapshot.mockResolvedValue({
    ...settings(),
    ...settingsOverride,
  });
  return render(
    <LocaleProvider>
      <PopOutPanel
        state={bootstrap(catalog, settingsOverride)}
        providerId={providerId}
      />
    </LocaleProvider>,
  );
}

describe("PopOutPanel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    tauriMocks.refreshProviders.mockResolvedValue(undefined);
    tauriMocks.refreshProvidersIfStale.mockResolvedValue(undefined);
    tauriMocks.getSettingsSnapshot.mockResolvedValue(settings());
    tauriMocks.getUpdateState.mockResolvedValue({
      status: "idle",
      version: null,
      error: null,
      progress: null,
      releaseUrl: null,
      canDownload: false,
      canApply: false,
      lastCheckedAt: null,
    });
    tauriMocks.getProviderChartData.mockResolvedValue({
      providerId: "codex",
      costHistory: [],
      creditsHistory: [],
      usageBreakdown: [],
      localUsage: null,
    });
    tauriMocks.getApiKeyProviders.mockResolvedValue([
      {
        id: "kimi",
        displayName: "Kimi",
        envVar: "KIMI_CODE_API_KEY",
        help: null,
        dashboardUrl: null,
      },
      {
        id: "minimax",
        displayName: "MiniMax",
        envVar: "MINIMAX_API_KEY",
        help: null,
        dashboardUrl: null,
      },
    ]);
    tauriMocks.addApiKey.mockResolvedValue([]);
    tauriMocks.getApiKeys.mockResolvedValue([]);
    tauriMocks.getApiKeySecret.mockResolvedValue("saved-secret");
    tauriMocks.updateApiKeyLabel.mockResolvedValue([]);
    tauriMocks.replaceApiKeySecret.mockResolvedValue([]);
    tauriMocks.deleteApiKey.mockResolvedValue([]);
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        PanelAllProviders: "All providers",
        PanelAllProvidersShort: "All",
        PanelLeftSuffix: "left",
        PanelShowAllProviders: "Show all providers",
        PanelShowFewerProviders: "Show fewer providers",
        PanelUsedSuffix: "used",
        SummaryProvidersLabel: "providers",
        ApiKeyCount: "{} keys",
        ApiKeyProviderCount: "{}, {} keys",
        ApiKeyAddCard: "＋ 添加 API Key",
        ApiKeyAdd: "添加密钥",
        ApiKeyNew: "新 API 密钥",
        ApiKeyLabelOptional: "标签（可选）",
        ApiKeyDuplicate: "该服务商已存在相同的 API Key。",
        Cancel: "取消",
        ActionRefreshAll: "Refresh all",
        ActionAddProvider: "Add provider",
        ApiKeyEditLabel: "Edit label",
        ApiKeyReplaceSecret: "Replace key",
        ApiKeyDelete: "Delete",
        ApiKeyShowSecret: "Show key",
        ApiKeyHideSecret: "Hide key",
      }),
    );
    tauriMocks.openFlyoutWindow.mockResolvedValue(undefined);
    eventMocks.listen.mockResolvedValue(() => {});
  });

  it("groups sibling credentials while preserving provider-focused routing", async () => {
    const first = {
      ...provider("codex", "Codex", 15),
      credentialId: "credential-a",
      credentialDisplayLabel: "Work",
      credentialDisplayOrdinal: 1,
    };
    const second = {
      ...provider("codex", "Codex", 75),
      credentialId: "credential-b",
      credentialDisplayLabel: "Key 2",
      credentialDisplayOrdinal: 2,
    };
    const { container } = renderPopOut(
      [second, first],
      "codex",
      [{ id: "codex", displayName: "Codex", cookieDomain: null }],
      { enabledProviders: ["codex"] },
    );

    await waitFor(() => {
      expect(container.querySelectorAll(".provider-grid__item")).toHaveLength(2);
    });
    expect(container.querySelectorAll(".menu-stack__item")).toHaveLength(1);
    expect(screen.getByRole("region", { name: "Work" })).toHaveTextContent("15% used");
    expect(screen.getByRole("region", { name: "Key 2" })).toHaveTextContent("75% used");
    expect(container.querySelector(".provider-grid__item--active")).toHaveAttribute(
      "aria-label",
      "Codex, 2 keys",
    );
    await waitFor(() => {
      expect(tauriMocks.getProviderChartData).toHaveBeenCalledTimes(1);
    });
  });

  it("keeps API key cards static when the pointer is held and moved", async () => {
    const first = {
      ...provider("codex", "Codex", 15),
      credentialId: "credential-a",
      credentialDisplayLabel: "Work",
      credentialDisplayOrdinal: 1,
    };
    const second = {
      ...provider("codex", "Codex", 75),
      credentialId: "credential-b",
      credentialDisplayLabel: "Key 2",
      credentialDisplayOrdinal: 2,
    };
    renderPopOut(
      [first, second],
      "codex",
      [{ id: "codex", displayName: "Codex", cookieDomain: null }],
      { enabledProviders: ["codex"] },
    );

    const work = await screen.findByRole("region", { name: "Work" });
    const key2 = screen.getByRole("region", { name: "Key 2" });
    Object.defineProperty(document, "elementFromPoint", {
      configurable: true,
      value: vi.fn().mockReturnValue(key2),
    });
    vi.useFakeTimers();
    try {
      fireEvent.pointerDown(work, { pointerId: 1, clientX: 20, clientY: 20 });
      act(() => vi.advanceTimersByTime(300));
      fireEvent.pointerMove(work, { pointerId: 1, clientX: 20, clientY: 140 });
      fireEvent.pointerUp(work, { pointerId: 1, clientX: 20, clientY: 140 });

      expect(screen.getAllByRole("region").map((card) => card.getAttribute("aria-label"))).toEqual([
        "Work",
        "Key 2",
      ]);
    } finally {
      vi.useRealTimers();
      Reflect.deleteProperty(document, "elementFromPoint");
    }
  });

  it("binds each eligible provider card to its own add-key action", async () => {
    renderPopOut(
      [provider("minimax", "MiniMax", 3), provider("kimi", "Kimi", 20)],
      undefined,
      [
        { id: "minimax", displayName: "MiniMax", cookieDomain: null },
        { id: "kimi", displayName: "Kimi", cookieDomain: null },
      ],
      { enabledProviders: ["minimax", "kimi"] },
    );

    const addButtons = await screen.findAllByRole("button", {
      name: "＋ 添加 API Key",
    });
    expect(addButtons).toHaveLength(2);

    fireEvent.click(addButtons[0]);
    const dialog = await screen.findByRole("dialog", {
      name: "添加密钥 — MiniMax",
    });
    fireEvent.change(screen.getByLabelText("Token Plan Key"), {
      target: { value: "minimax-test-key" },
    });
    fireEvent.submit(dialog);

    await waitFor(() => {
      expect(tauriMocks.addApiKey).toHaveBeenCalledWith(
        "minimax",
        "minimax-test-key",
        undefined,
      );
    });

    fireEvent.click(addButtons[1]);
    const kimiDialog = await screen.findByRole("dialog", {
      name: "添加密钥 — Kimi",
    });
    fireEvent.change(screen.getByLabelText("新 API 密钥"), {
      target: { value: "kimi-test-key" },
    });
    fireEvent.submit(kimiDialog);

    await waitFor(() => {
      expect(tauriMocks.addApiKey).toHaveBeenNthCalledWith(
        2,
        "kimi",
        "kimi-test-key",
        undefined,
      );
    });
  });

  it("opens a prefilled masked replacement field and toggles the eye button", async () => {
    const key = {
      ...provider("kimi", "Kimi", 20),
      credentialId: "credential-a",
      credentialDisplayLabel: "Key 1",
      credentialDisplayOrdinal: 1,
    };
    tauriMocks.getApiKeys.mockResolvedValue([
      {
        credentialId: "credential-a",
        providerId: "kimi",
        provider: "Kimi",
        label: "Key 1",
        customLabel: null,
        displayOrdinal: 1,
        savedAt: "2026-08-02 12:00",
      },
    ]);
    renderPopOut(
      [key],
      undefined,
      [{ id: "kimi", displayName: "Kimi", cookieDomain: null }],
      { enabledProviders: ["kimi"] },
    );

    fireEvent.click(await screen.findByRole("button", { name: "Replace key" }));
    const secretInput = await screen.findByDisplayValue("saved-secret");
    expect(tauriMocks.getApiKeySecret).toHaveBeenCalledWith("kimi", "credential-a");
    expect(secretInput).toHaveAttribute("type", "password");
    expect(screen.getByRole("button", { name: "Show key" }).querySelector("svg")).not.toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show key" }));
    expect(screen.getByDisplayValue("saved-secret")).toHaveAttribute("type", "text");
  });

  it("offers one refresh-all control for the homepage providers", async () => {
    renderPopOut([provider("kimi", "Kimi", 20)]);

    fireEvent.click(await screen.findByRole("button", { name: "Refresh all" }));

    await waitFor(() => {
      expect(tauriMocks.refreshProviders).toHaveBeenCalledTimes(1);
    });
  });

  it("opens the provider settings tab from the homepage toolbar", async () => {
    renderPopOut([provider("kimi", "Kimi", 20)]);

    fireEvent.click(await screen.findByRole("button", { name: "Add provider" }));

    expect(tauriMocks.openSettingsWindow).toHaveBeenCalledWith("providers");
  });

  it("uses one compact action style for refresh and add-provider", async () => {
    renderPopOut([provider("kimi", "Kimi", 20)]);

    const refresh = await screen.findByRole("button", { name: "Refresh all" });
    const detection = screen.getByRole("button", { name: "检测并发" });
    expect(refresh.previousElementSibling).toContainElement(detection);
    expect(screen.getAllByRole("button", { name: "检测并发" })).toHaveLength(1);
    const addProvider = screen.getByRole("button", { name: "Add provider" });

    expect(refresh).toHaveClass("popout-provider-toolbar__action");
    expect(addProvider).toHaveClass("popout-provider-toolbar__action");
    expect(refresh.querySelector("svg")).not.toBeNull();
  });

  it("shows the provider grid and focuses provider targets", async () => {
    const { container } = renderPopOut(
      [provider("codex", "Codex", 80), provider("claude", "Claude", 30)],
      "claude",
    );

    await waitFor(() => {
      expect(container.querySelectorAll(".provider-grid__item")).toHaveLength(3);
    });

    expect(container.querySelector(".provider-grid__item--active")?.getAttribute("aria-label")).toBe("Claude");
    expect(screen.getAllByText("Claude").length).toBeGreaterThanOrEqual(2);
    expect(container.querySelectorAll(".menu-stack__item")).toHaveLength(1);
  });

  it("renders cleanly with the flyout-window rewiring for goTray's onClick", async () => {
    // goTray's onClick now calls openFlyoutWindow() (formerly
    // setSurfaceMode("trayPanel", ...)) — asserted directly against the mock
    // import rather than via a click because `headerActions` (the array
    // goTray's handler lives in) is currently never rendered by
    // MenuSurface: `actions` is destructured in MenuSurfaceProps but not
    // consumed in its JSX (components/MenuSurface.tsx), so there is no
    // "back to tray" button in the DOM to click today. That's a pre-existing
    // gap tracked separately, not introduced by this rewiring. This test
    // instead pins down that the component still renders without error and
    // that openFlyoutWindow is never called on mount (only on the — for now
    // unreachable — click), so the rewiring doesn't regress anything that
    // currently DOES work.
    renderPopOut([provider("codex", "Codex", 80)]);

    await waitFor(() => {
      expect(screen.getAllByText("Codex").length).toBeGreaterThan(0);
    });

    expect(tauriMocks.openFlyoutWindow).not.toHaveBeenCalled();
  });

  it("applies the persisted PopOut display scale", async () => {
    const { container } = renderPopOut(
      [provider("codex", "Codex", 80)],
      undefined,
      [],
      { windowScalePercent: 175 },
    );

    await waitFor(() => {
      expect(container.querySelector(".popout-scale-shell")).not.toBeNull();
    });

    // Scaling is applied via the webview's native zoom, not an inline
    // `--window-scale` style (which the earlier CSS-zoom approach used).
    await waitFor(() => {
      expect(webviewWindowMocks.setZoom).toHaveBeenCalledWith(1.75);
    });
  });

  it("does not resize or reposition the native window on mount", async () => {
    renderPopOut([provider("codex", "Codex", 80)]);

    await waitFor(() => {
      expect(screen.getAllByText("Codex").length).toBeGreaterThan(0);
    });

    // The PopOut title bar reads window state (isMaximized) on mount, so
    // getCurrentWindow is legitimately called; assert only that the surface
    // itself never resizes or repositions the native window.
    expect(windowMocks.setSize).not.toHaveBeenCalled();
    expect(windowMocks.setPosition).not.toHaveBeenCalled();
  });

  it("localizes static popout panel footer labels in Japanese", async () => {
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle(
        {
          MenuAbout: "CodexBar について",
          MenuQuit: "終了",
          TooltipSettings: "設定",
        },
        "japanese",
      ),
    );

    renderPopOut([provider("codex", "Codex", 80)]);

    expect(await screen.findByText("設定")).toBeInTheDocument();
    expect(screen.getByText("CodexBar について")).toBeInTheDocument();
    expect(screen.getByText("終了")).toBeInTheDocument();
  });

  it("renders overview cards in settings catalog order instead of fetch order", async () => {
    const catalog: ProviderCatalogEntry[] = [
      { id: "codex", displayName: "Codex", cookieDomain: null },
      { id: "claude", displayName: "Claude", cookieDomain: null },
      { id: "cursor", displayName: "Cursor", cookieDomain: null },
    ];

    const { container } = renderPopOut(
      [
        provider("cursor", "Cursor", 15),
        provider("codex", "Codex", 95),
        provider("claude", "Claude", 40),
      ],
      undefined,
      catalog,
    );

    await waitFor(() => {
      expect(container.querySelectorAll(".menu-stack__item")).toHaveLength(3);
    });

    expect(
      Array.from(container.querySelectorAll(".menu-card__name")).map(
        (node) => node.textContent,
      ),
    ).toEqual(["Codex", "Claude", "Cursor"]);
  });

  it("keeps the popout overview focused until the provider grid expands", async () => {
    const providers = TEST_PROVIDER_CATALOG.map(([id, displayName], index) =>
      provider(id, displayName, (index * 7) % 100),
    );

    const { container } = renderPopOut(providers);

    await waitFor(() => {
      expect(container.querySelector(".provider-grid--compact")).not.toBeNull();
    });

    expect(container.querySelectorAll(".provider-grid__item")).toHaveLength(20);
    expect(container.querySelectorAll(".menu-stack__item")).toHaveLength(4);

    const expand = container.querySelector<HTMLButtonElement>(
      '.provider-grid__item--more[aria-label="Show all providers"]',
    );
    expect(expand).not.toBeNull();

    fireEvent.click(expand!);

    await waitFor(() => {
      expect(container.querySelectorAll(".provider-grid__item")).toHaveLength(
        providers.length + 2,
      );
    });
    expect(container.querySelectorAll(".menu-stack__item")).toHaveLength(
      providers.length,
    );
  });
});
