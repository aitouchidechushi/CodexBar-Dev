import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { LocaleProvider } from "../../../i18n/LocaleProvider";
import { buildBundle } from "../../../test/localeHarness";
import type { ProviderDetail } from "../../../types/bridge";

const tauriMocks = vi.hoisted(() => ({
  getLocaleStrings: vi.fn(),
  setUiLanguage: vi.fn(),
}));

beforeEach(() => {
  tauriMocks.getLocaleStrings.mockResolvedValue(
    buildBundle({
      ApiKeyCount: "{} keys",
      ApiKeyFailureCount: "{}/{} failed",
      ProviderStatusError: "Error",
    }),
  );
});

vi.mock("../../../lib/tauri", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../lib/tauri")>()),
  ...tauriMocks,
}));

import { ProviderDetailUsageState } from "./ProviderDetailPane";

function detail(overrides: Partial<ProviderDetail> = {}): ProviderDetail {
  return {
    id: "openrouter",
    displayName: "OpenRouter",
    enabled: true,
    email: null,
    plan: null,
    authType: null,
    sourceLabel: "api",
    organization: null,
    lastUpdated: "2026-07-22T00:00:00Z",
    session: null,
    weekly: null,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: null,
    pace: null,
    lastError: null,
    dashboardUrl: null,
    statusPageUrl: null,
    buyCreditsUrl: null,
    hasSnapshot: true,
    credentialCount: 2,
    failedCredentialCount: 0,
    cookieSource: null,
    region: null,
    ...overrides,
  };
}

describe("ProviderDetailUsageState", () => {
  it("renders a partial multi-key provider as neutral N keys without quota or raw error", async () => {
    render(
      <LocaleProvider>
        <ProviderDetailUsageState
          detail={detail({ failedCredentialCount: 1, lastError: "secret sibling failure" })}
          resetTimeRelative
        />
      </LocaleProvider>,
    );

    expect(await screen.findByText("2 keys")).toBeInTheDocument();
    expect(screen.getByText("1/2 failed")).toBeInTheDocument();
    expect(screen.queryByText(/secret sibling failure/)).not.toBeInTheDocument();
    expect(document.body.textContent).not.toContain("%");
  });

  it("renders all-key failure as one safe provider error", async () => {
    render(
      <LocaleProvider>
        <ProviderDetailUsageState
          detail={detail({
            failedCredentialCount: 2,
            lastError: "first raw failure; second raw failure",
          })}
          resetTimeRelative
        />
      </LocaleProvider>,
    );

    expect(await screen.findByText("2 keys")).toBeInTheDocument();
    expect(screen.getByText("Error")).toBeInTheDocument();
    expect(screen.queryByText(/raw failure/)).not.toBeInTheDocument();
  });
});
