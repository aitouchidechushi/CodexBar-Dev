import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const tauriMocks = vi.hoisted(() => ({
  getLocaleStrings: vi.fn(),
  setUiLanguage: vi.fn(),
}));

const eventMocks = vi.hoisted(() => ({ listen: vi.fn() }));

vi.mock("../lib/tauri", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/tauri")>()),
  ...tauriMocks,
}));
vi.mock("@tauri-apps/api/event", () => eventMocks);

import { LocaleProvider } from "../i18n/LocaleProvider";
import type { ProviderPresentationGroup } from "../lib/providerGroups";
import { buildBundle } from "../test/localeHarness";
import type { ProviderUsageSnapshot } from "../types/bridge";
import ProviderGrid from "./ProviderGrid";

function snapshot(credentialId?: string, usedPercent = 30): ProviderUsageSnapshot {
  return {
    providerId: "openrouter",
    displayName: "OpenRouter",
    credentialId,
    credentialDisplayLabel: credentialId ? `Key ${credentialId}` : undefined,
    credentialDisplayOrdinal: credentialId ? Number(credentialId) : undefined,
    primary: {
      usedPercent,
      remainingPercent: 100 - usedPercent,
      windowMinutes: null,
      resetsAt: null,
      resetDescription: null,
      isExhausted: false,
      reservePercent: null,
      reserveDescription: null,
    },
    primaryLabel: "Monthly",
    secondary: null,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "api-key",
    updatedAt: "2026-07-22T00:00:00Z",
    error: null,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
  };
}

function group(snapshots: ProviderUsageSnapshot[]): ProviderPresentationGroup {
  const credentialCount = snapshots.filter((item) => item.credentialId).length;
  return {
    providerId: "openrouter",
    displayName: "OpenRouter",
    snapshots,
    successfulSnapshots: snapshots,
    failedCredentialCount: 0,
    credentialCount,
    hasMultipleCredentials: credentialCount > 1,
    isAllFailed: false,
    isPartialFailure: false,
  };
}

function renderGrid(groups: ProviderPresentationGroup[]) {
  return render(
    <LocaleProvider>
      <ProviderGrid
        providers={groups}
        selectedProviderId={null}
        showAsUsed={true}
        onSelect={() => {}}
      />
    </LocaleProvider>,
  );
}

describe("ProviderGrid grouped providers", () => {
  beforeEach(() => {
    tauriMocks.getLocaleStrings.mockResolvedValue(
      buildBundle({
        PanelAllProviders: "All providers",
        PanelAllProvidersShort: "All",
        ApiKeyCount: "{} claves",
        ApiKeyProviderCount: "{}, {} claves",
      }),
    );
    eventMocks.listen.mockResolvedValue(() => {});
  });

  it("renders one neutral item for multiple credentials without a percentage track", async () => {
    const { container } = renderGrid([group([snapshot("1", 20), snapshot("2", 90)])]);

    expect(await screen.findByRole("button", { name: "OpenRouter, 2 claves" })).toBeInTheDocument();
    expect(container.querySelectorAll(".provider-grid__item")).toHaveLength(2);
    expect(screen.getByRole("button", { name: "OpenRouter, 2 claves" })).toBeInTheDocument();
    expect(screen.getByText("2 claves")).toBeInTheDocument();
    expect(container.querySelector(".provider-grid__weekly-track")).toBeNull();
  });

  it("preserves the original percentage track for a single default provider", async () => {
    const { container } = renderGrid([group([snapshot(undefined, 30)])]);

    const item = await screen.findByRole("button", { name: "OpenRouter" });
    expect(item).toBeInTheDocument();
    expect(container.querySelector(".provider-grid__key-count")).toBeNull();
    expect(container.querySelector<HTMLElement>(".provider-grid__weekly-track")?.style.getPropertyValue("--weekly-pct")).toBe("30%");
  });

  it("renders an account-only Kimi item without reading an absent provider snapshot", async () => {
    const accountOnly: ProviderPresentationGroup = {
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
    };

    const { container } = renderGrid([accountOnly]);

    expect(await screen.findByRole("button", { name: "Kimi" })).toBeInTheDocument();
    expect(container.querySelector(".provider-grid__weekly-track")).toBeNull();
  });
});
