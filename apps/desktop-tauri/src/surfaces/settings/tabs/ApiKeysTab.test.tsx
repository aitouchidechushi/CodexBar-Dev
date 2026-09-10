import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { ProviderCatalogEntry } from "../../../types/bridge";
import ApiKeysTab from "./ApiKeysTab";

const tauriMocks = vi.hoisted(() => ({ getApiKeyProviders: vi.fn() }));

vi.mock("../../../lib/tauri", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../lib/tauri")>()),
  ...tauriMocks,
}));

vi.mock("../../../hooks/useLocale", () => ({
  useLocale: () => ({ t: (key: string) => key }),
}));

vi.mock("../providers/ApiKeySection", () => ({
  ApiKeySection: ({ providerId }: { providerId: string }) => (
    <div data-testid="api-key-section">{providerId}</div>
  ),
}));

describe("ApiKeysTab", () => {
  it("reuses the canonical multi-key section for API-key providers", async () => {
    tauriMocks.getApiKeyProviders.mockResolvedValue([
      { id: "alpha", displayName: "Alpha", envVar: null, help: null, dashboardUrl: null },
      { id: "beta", displayName: "Beta", envVar: null, help: null, dashboardUrl: null },
    ]);
    const providers = [
      { id: "alpha", displayName: "Alpha", cookieDomain: null },
      { id: "oauth", displayName: "OAuth", cookieDomain: null },
      { id: "beta", displayName: "Beta", cookieDomain: null },
    ] as ProviderCatalogEntry[];

    render(<ApiKeysTab providers={providers} />);

    expect((await screen.findAllByTestId("api-key-section")).map((node) => node.textContent))
      .toEqual(["alpha", "beta"]);
  });
});
