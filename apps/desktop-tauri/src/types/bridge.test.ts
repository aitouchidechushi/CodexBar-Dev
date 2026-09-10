import { describe, it, expect } from "vitest";
import type {
  RuntimeIdentityBridge,
  Language,
  LocaleStrings,
  ProviderUsageSnapshot,
  SettingsSnapshot,
} from "./bridge";

describe("RuntimeIdentityBridge contract", () => {
  it("keeps executable and build hashes as exact strings", () => {
    const identity: RuntimeIdentityBridge = {
      build: {
        productName: "CodexBar",
        semanticVersion: "0.46.0",
        gitCommit: "a".repeat(40),
        shortCommit: "a".repeat(12),
        buildChannel: "stable",
        sourceState: "clean",
        buildTimestamp: "2026-08-30T10:00:00Z",
        toolchains: { rustc: "rustc 1.91.0" },
        packageManifestSha256: "b".repeat(64),
      },
      executablePath: String.raw`C:\Users\Example\AppData\Local\Programs\CodexBar\codexbar.exe`,
      executableSha256: "c".repeat(64),
      pid: 4242,
      processStartedAt: "2026-08-30T10:00:00.000Z",
      distribution: "installed-stable",
    };

    expect(typeof identity.executableSha256).toBe("string");
    expect(typeof identity.build.gitCommit).toBe("string");
    expect(identity.distribution).toBe("installed-stable");
  });
});

describe("ProviderUsageSnapshot account contract", () => {
  it("accepts optional provider-scoped account membership and display fields", () => {
    const snapshot: ProviderUsageSnapshot = {
      providerId: "factory",
      displayName: "Factory",
      accountGroupId: "factory:user-42",
      accountDisplayName: "Ada",
      primary: {
        usedPercent: 20,
        remainingPercent: 80,
        windowMinutes: 300,
        resetsAt: null,
        resetDescription: null,
        isExhausted: false,
        reservePercent: null,
        reserveDescription: null,
      },
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
    };

    expect(snapshot.accountGroupId).toBe("factory:user-42");
    expect(snapshot.accountDisplayName).toBe("Ada");
    expect(Object.keys(snapshot)).not.toEqual(
      expect.arrayContaining(["credential", "apiKey", "token", "cookie", "secret"]),
    );
  });
});

describe("Language type", () => {
  it("accepts supported locale labels as valid union members", () => {
    // Type-level assertion: this assignment must compile (tsc --noEmit gate).
    // Vitest strips types at transform time, so the runtime assertion only
    // exercises value correctness; tsc provides the RED/GREEN gate.
    const lang: Language = "spanish";
    expect(lang).toBe("spanish");
    const langKo: Language = "korean";
    expect(langKo).toBe("korean");
    const langZhTw: Language = "chinesetraditional";
    expect(langZhTw).toBe("chinesetraditional");
  });

  it("allows 'spanish' in LocaleStrings payload", () => {
    const payload: LocaleStrings = {
      language: "spanish",
      entries: { TabGeneral: "General" },
    };
    expect(payload.language).toBe("spanish");
    expect(payload.entries.TabGeneral).toBe("General");

    const payloadKo: LocaleStrings = {
      language: "korean",
      entries: { TabGeneral: "일반" },
    };
    expect(payloadKo.language).toBe("korean");
    expect(payloadKo.entries.TabGeneral).toBe("일반");

    const payloadZhTw: LocaleStrings = {
      language: "chinesetraditional",
      entries: { TabGeneral: "一般" },
    };
    expect(payloadZhTw.language).toBe("chinesetraditional");
    expect(payloadZhTw.entries.TabGeneral).toBe("一般");
  });

  it("allows 'spanish' in SettingsSnapshot.uiLanguage", () => {
    const snap: SettingsSnapshot = {
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
      showResetWhenExhausted: false,
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
      uiLanguage: "spanish",
      theme: "dark",
      claudeAvoidKeychainPrompts: true,
      codexSparkUsageVisible: true,
      disableKeychainAccess: false,
      providerMetrics: {},
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
    };
    expect(snap.uiLanguage).toBe("spanish");

    const snapKo: SettingsSnapshot = {
      ...snap,
      uiLanguage: "korean",
    };
    expect(snapKo.uiLanguage).toBe("korean");

    const snapZhTw: SettingsSnapshot = {
      ...snap,
      uiLanguage: "chinesetraditional",
    };
    expect(snapZhTw.uiLanguage).toBe("chinesetraditional");
  });
});
