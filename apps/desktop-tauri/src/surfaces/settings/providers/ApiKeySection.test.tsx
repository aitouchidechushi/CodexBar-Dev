import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ApiKeyInfoBridge } from "../../../types/bridge";
import { ApiKeySection } from "./ApiKeySection";

const tauriMocks = vi.hoisted(() => ({
  getApiKeyProviders: vi.fn(),
  getApiKeys: vi.fn(),
  addApiKey: vi.fn(),
  updateApiKeyLabel: vi.fn(),
  replaceApiKeySecret: vi.fn(),
  deleteApiKey: vi.fn(),
}));

const eventMocks = vi.hoisted(() => ({
  listen: vi.fn(),
  settingsChanged: null as null | (() => void),
}));

vi.mock("../../../lib/tauri", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../lib/tauri")>()),
  ...tauriMocks,
}));
vi.mock("@tauri-apps/api/event", () => eventMocks);

vi.mock("../../../hooks/useLocale", () => ({
  useLocale: () => ({
    t: (key: string) => ({
      ApiKeyEditLabel: "Localized edit label",
      ApiKeyEditLabelAction: "Localized edit label {}",
      ApiKeyLabelFor: "Localized label for {}",
      ApiKeySaveLabelAction: "Localized save label {}",
      ApiKeyReplaceSecret: "Localized replace secret",
      ApiKeyReplaceSecretAction: "Localized replace secret {}",
      ApiKeyReplacementFor: "Localized replacement for {}",
      ApiKeySaveReplacementAction: "Localized save replacement {}",
      ApiKeyDelete: "Localized delete",
      ApiKeyDeleteAction: "Localized delete {}",
      ApiKeyDeleteConfirmation: "Localized delete confirmation {}",
      ApiKeyNew: "Localized new key",
      ApiKeyNewLabelOptional: "Localized optional label",
      ApiKeyAdd: "Localized add key",
      OpenProviderDashboard: "Localized open {} dashboard",
    } as Record<string, string>)[key] ?? key,
  }),
}));

const providers = [
  {
    id: "alpha",
    displayName: "Alpha",
    envVar: "ALPHA_KEY",
    help: "Create a key in Alpha settings.",
    dashboardUrl: "https://alpha.example/keys",
  },
  {
    id: "beta",
    displayName: "Beta",
    envVar: "BETA_KEY",
    help: "Create a key in Beta settings.",
    dashboardUrl: null,
  },
];

const alphaKeys: ApiKeyInfoBridge[] = [
  {
    credentialId: "uuid-backup",
    providerId: "alpha",
    provider: "Alpha",
    label: "Backup",
    customLabel: "Backup",
    displayOrdinal: 3,
    savedAt: "2026-07-03",
  },
  {
    credentialId: "uuid-beta",
    providerId: "beta",
    provider: "Beta",
    label: "Beta key",
    customLabel: null,
    displayOrdinal: 1,
    savedAt: "2026-07-02",
  },
  {
    credentialId: "uuid-work",
    providerId: "alpha",
    provider: "Alpha",
    label: "Work",
    customLabel: "Work",
    displayOrdinal: 1,
    savedAt: "2026-07-01",
  },
];

describe("ApiKeySection", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    tauriMocks.getApiKeyProviders.mockResolvedValue(providers);
    tauriMocks.getApiKeys.mockResolvedValue(alphaKeys);
    tauriMocks.addApiKey.mockResolvedValue(alphaKeys);
    tauriMocks.updateApiKeyLabel.mockResolvedValue(alphaKeys);
    tauriMocks.replaceApiKeySecret.mockResolvedValue(alphaKeys);
    tauriMocks.deleteApiKey.mockResolvedValue(alphaKeys);
    eventMocks.settingsChanged = null;
    eventMocks.listen.mockImplementation((event: string, handler: () => void) => {
      if (event === "settings-changed") eventMocks.settingsChanged = handler;
      return Promise.resolve(() => {});
    });
    vi.spyOn(window, "confirm").mockReturnValue(true);
  });

  it("filters the selected provider, preserves ordinal gaps, and renders only redacted row data", async () => {
    const unsafeBackendValue = {
      ...alphaKeys[0],
      maskedKey: "sk-****backup",
      secret: "sk-live-secret",
    } as unknown as ApiKeyInfoBridge;
    tauriMocks.getApiKeys.mockResolvedValueOnce([
      unsafeBackendValue,
      alphaKeys[1],
      alphaKeys[2],
    ]);

    render(<ApiKeySection providerId="alpha" />);

    const rows = await screen.findAllByRole("listitem");
    expect(rows).toHaveLength(2);
    expect(rows.map((row) => within(row).getByRole("heading").textContent)).toEqual([
      "Work",
      "Backup",
    ]);
    expect(screen.getByText("2026-07-01")).toBeInTheDocument();
    expect(screen.getByText("2026-07-03")).toBeInTheDocument();
    expect(screen.queryByText("Beta key")).not.toBeInTheDocument();
    expect(screen.queryByText(/sk-/)).not.toBeInTheDocument();
    expect(screen.getByText("Create a key in Alpha settings.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /Alpha/ })).toHaveAttribute(
      "href",
      "https://alpha.example/keys",
    );
  });

  it("adds a key from an independent blank password and optional-label form", async () => {
    render(<ApiKeySection providerId="alpha" />);

    const secret = await screen.findByLabelText("Localized new key");
    const label = screen.getByRole("textbox", { name: "Localized optional label" });
    expect(secret).toHaveAttribute("type", "password");
    expect(secret).toHaveValue("");

    fireEvent.change(secret, { target: { value: "  sk-new  " } });
    fireEvent.change(label, { target: { value: "  Personal  " } });
    fireEvent.click(screen.getByRole("button", { name: "Localized add key" }));

    await waitFor(() => {
      expect(tauriMocks.addApiKey).toHaveBeenCalledWith("alpha", "sk-new", "Personal");
    });
    expect(secret).toHaveValue("");
    expect(label).toHaveValue("");
  });

  it("edits a label and targets the row credential UUID", async () => {
    render(<ApiKeySection providerId="alpha" />);

    fireEvent.click(await screen.findByRole("button", { name: "Localized edit label Work" }));
    const input = screen.getByRole("textbox", { name: "Localized label for Work" });
    expect(input).toHaveValue("Work");
    fireEvent.change(input, { target: { value: "Primary" } });
    fireEvent.click(screen.getByRole("button", { name: "Localized save label Work" }));

    await waitFor(() => {
      expect(tauriMocks.updateApiKeyLabel).toHaveBeenCalledWith(
        "alpha",
        "uuid-work",
        "Primary",
      );
    });
  });

  it("replaces one secret from a blank non-prefilled password field", async () => {
    render(<ApiKeySection providerId="alpha" />);

    fireEvent.click(await screen.findByRole("button", { name: "Localized replace secret Backup" }));
    const input = screen.getByLabelText("Localized replacement for Backup");
    expect(input).toHaveAttribute("type", "password");
    expect(input).toHaveValue("");
    fireEvent.change(input, { target: { value: "replacement-secret" } });
    fireEvent.click(screen.getByRole("button", { name: "Localized save replacement Backup" }));

    await waitFor(() => {
      expect(tauriMocks.replaceApiKeySecret).toHaveBeenCalledWith(
        "alpha",
        "uuid-backup",
        "replacement-secret",
      );
    });
  });

  it("keeps editor and busy state isolated by credential UUID", async () => {
    let resolveLabel!: (value: ApiKeyInfoBridge[]) => void;
    tauriMocks.updateApiKeyLabel.mockReturnValueOnce(
      new Promise<ApiKeyInfoBridge[]>((resolve) => { resolveLabel = resolve; }),
    );
    render(<ApiKeySection providerId="alpha" />);

    fireEvent.click(await screen.findByRole("button", { name: "Localized edit label Work" }));
    fireEvent.click(screen.getByRole("button", { name: "Localized replace secret Backup" }));
    expect(screen.getByRole("textbox", { name: "Localized label for Work" })).toBeInTheDocument();
    const backupSecret = screen.getByLabelText("Localized replacement for Backup");
    expect(backupSecret).toBeEnabled();

    fireEvent.click(screen.getByRole("button", { name: "Localized save label Work" }));
    expect(await screen.findByLabelText("Localized replacement for Backup")).toBeEnabled();
    resolveLabel(alphaKeys);
    await waitFor(() => expect(tauriMocks.updateApiKeyLabel).toHaveBeenCalledTimes(1));
  });

  it("confirms with the safe label and deletes the exact credential UUID", async () => {
    render(<ApiKeySection providerId="alpha" />);

    fireEvent.click(await screen.findByRole("button", { name: "Localized delete Backup" }));

    expect(window.confirm).toHaveBeenCalledWith("Localized delete confirmation Backup");
    await waitFor(() => {
      expect(tauriMocks.deleteApiKey).toHaveBeenCalledWith("alpha", "uuid-backup");
    });
  });

  it("reloads and clears provider-specific UI when the selected provider changes", async () => {
    const { rerender } = render(<ApiKeySection providerId="alpha" />);
    expect(await screen.findByText("Work")).toBeInTheDocument();

    rerender(<ApiKeySection providerId="beta" />);

    expect(await screen.findByText("Beta key")).toBeInTheDocument();
    expect(screen.queryByText("Work")).not.toBeInTheDocument();
    expect(tauriMocks.getApiKeys).toHaveBeenCalledTimes(2);
  });

  it("reloads saved keys after another window adds one without clearing the draft form", async () => {
    const added = {
      ...alphaKeys[0],
      credentialId: "uuid-new",
      label: "Key 4",
      customLabel: null,
      displayOrdinal: 4,
    };
    tauriMocks.getApiKeys
      .mockResolvedValueOnce(alphaKeys)
      .mockResolvedValueOnce([...alphaKeys, added]);
    render(<ApiKeySection providerId="alpha" />);

    const draft = await screen.findByLabelText("Localized new key");
    fireEvent.change(draft, { target: { value: "draft-secret" } });
    act(() => eventMocks.settingsChanged?.());

    expect(await screen.findByText("Key 4")).toBeInTheDocument();
    expect(draft).toHaveValue("draft-secret");
  });
});
