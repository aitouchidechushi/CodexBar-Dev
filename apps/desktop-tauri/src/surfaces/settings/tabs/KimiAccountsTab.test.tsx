import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { KimiAccountSnapshot, KimiBrowserScanResult, SettingsSnapshot } from "../../../types/bridge";

const tauri = vi.hoisted(() => ({
  listKimiAccounts: vi.fn(),
  beginKimiAccountLogin: vi.fn(),
  setKimiAccountWebviewBounds: vi.fn(),
  completeKimiAccountLogin: vi.fn(),
  cancelKimiAccountLogin: vi.fn(),
  removeKimiAccount: vi.fn(),
  refreshKimiBrowserAccounts: vi.fn(),
  updateSettings: vi.fn(),
}));

const event = vi.hoisted(() => {
  const listeners = new Map<string, (event: { payload: unknown }) => void>();
  return {
    listeners,
    listen: vi.fn(async (name: string, listener: (event: { payload: unknown }) => void) => {
      listeners.set(name, listener);
      return () => listeners.delete(name);
    }),
  };
});

vi.mock("../../../lib/tauri", () => tauri);
vi.mock("@tauri-apps/api/event", () => event);
vi.mock("../../../hooks/useLocale", () => ({
  useLocale: () => ({ t: (key: string) => key }),
}));

import KimiAccountsTab from "./KimiAccountsTab";

function account(id: string, sourceLabels: string[] = []): KimiAccountSnapshot {
  return {
    accountId: id,
    displayName: `Account ${id}`,
    usedPercent: 26.75,
    resetsAt: null,
    updatedAt: "2026-08-23T00:00:00Z",
    status: "ok",
    matchedCredentialIds: [],
    sourceLabels,
  };
}

describe("KimiAccountsTab", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    event.listeners.clear();
    tauri.listKimiAccounts.mockResolvedValue([]);
    tauri.beginKimiAccountLogin.mockResolvedValue({ sessionId: "session-a" });
    tauri.setKimiAccountWebviewBounds.mockResolvedValue(undefined);
    tauri.cancelKimiAccountLogin.mockResolvedValue(true);
    tauri.completeKimiAccountLogin.mockResolvedValue(undefined);
    tauri.refreshKimiBrowserAccounts.mockResolvedValue({
      accounts: [],
      discovered: 0,
      refreshed: 0,
      failed: 0,
    });
    tauri.updateSettings.mockResolvedValue({
      kimiBrowserConsentAccepted: true,
    } as SettingsSnapshot);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("keeps the previous quota visible and blocks account writes after a storage error", async () => {
    tauri.listKimiAccounts.mockResolvedValue([{ ...account("saved"), status: "storageError" }]);
    render(<KimiAccountsTab enabled browserConsentAccepted />);

    expect(await screen.findByText("Account saved")).toBeInTheDocument();
    expect(screen.getByText("KimiAccountMonthlyQuota: 27%")).toBeInTheDocument();
    expect(screen.getByText("KimiAccountStorageError")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "KimiAccountAdd" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "KimiAccountRemove" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "KimiBrowserRefresh" })).toBeDisabled();
  });

  it("blocks writes when the first account load fails without inventing an empty successful store", async () => {
    tauri.listKimiAccounts.mockRejectedValue("KimiAccounts: DecryptFailed");
    render(<KimiAccountsTab enabled browserConsentAccepted />);

    expect(await screen.findByText("KimiAccountStorageError")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "KimiAccountAdd" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "KimiBrowserRefresh" })).toBeDisabled();
    expect(screen.queryByText("KimiBrowserNoAccount")).not.toBeInTheDocument();
  });

  it("handles storage failure without a cached account and recovers on a successful reload", async () => {
    render(<KimiAccountsTab enabled browserConsentAccepted />);
    await waitFor(() => expect(tauri.listKimiAccounts).toHaveBeenCalledTimes(1));
    act(() => {
      event.listeners.get("kimi-account-storage-state")?.({ payload: { failed: true } });
    });
    expect(screen.getByRole("button", { name: "KimiAccountAdd" })).toBeDisabled();
    expect(screen.getByText("KimiAccountStorageError")).toBeInTheDocument();

    act(() => {
      event.listeners.get("kimi-account-storage-state")?.({ payload: { failed: false } });
    });
    expect(screen.getByRole("button", { name: "KimiAccountAdd" })).toBeEnabled();
    expect(screen.queryByText("KimiAccountStorageError")).not.toBeInTheDocument();
  });

  it("does not let a slow initial list overwrite a newer storage failure event", async () => {
    let resolveList!: (value: KimiAccountSnapshot[]) => void;
    tauri.listKimiAccounts.mockReturnValue(new Promise((resolve) => { resolveList = resolve; }));
    render(<KimiAccountsTab enabled browserConsentAccepted />);
    await waitFor(() => expect(tauri.listKimiAccounts).toHaveBeenCalledTimes(1));
    act(() => {
      event.listeners.get("kimi-accounts-updated")?.({
        payload: [{ ...account("saved"), status: "storageError" }],
      });
    });
    await act(async () => { resolveList([]); });
    expect(screen.getByText("Account saved")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "KimiAccountAdd" })).toBeDisabled();
  });

  it.each([
    { label: "empty", accounts: [] },
    { label: "healthy", accounts: [account("saved")] },
  ])("does not infer storage recovery from a $label account publication", async ({ accounts }) => {
    render(<KimiAccountsTab enabled browserConsentAccepted />);
    await act(async () => {});
    act(() => event.listeners.get("kimi-account-storage-state")?.({ payload: { failed: true } }));
    act(() => event.listeners.get("kimi-accounts-updated")?.({ payload: accounts }));
    expect(screen.getByRole("button", { name: "KimiAccountAdd" })).toBeDisabled();
    expect(screen.getByText("KimiAccountStorageError")).toBeInTheDocument();
    act(() => event.listeners.get("kimi-account-storage-state")?.({ payload: { failed: false } }));
    expect(screen.getByRole("button", { name: "KimiAccountAdd" })).toBeEnabled();
  });

  it("reads browser accounts without another confirmation after browser consent", async () => {
    const confirm = vi.spyOn(window, "confirm");
    render(<KimiAccountsTab enabled browserConsentAccepted />);
    await waitFor(() => expect(tauri.listKimiAccounts).toHaveBeenCalledTimes(1));
    expect(tauri.refreshKimiBrowserAccounts).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "KimiBrowserRefresh" }));

    await waitFor(() => expect(tauri.refreshKimiBrowserAccounts).toHaveBeenCalledTimes(1));
    expect(confirm).not.toHaveBeenCalled();
  });

  it.each(["ok", "storageError"] as const)("does not let a late scan replace a newer %s account event", async (status) => {
    tauri.listKimiAccounts.mockResolvedValue([account("before")]);
    let resolveScan!: (value: KimiBrowserScanResult) => void;
    tauri.refreshKimiBrowserAccounts.mockReturnValue(new Promise((resolve) => { resolveScan = resolve; }));
    render(<KimiAccountsTab enabled browserConsentAccepted />);
    await screen.findByText("Account before");
    fireEvent.click(screen.getByRole("button", { name: "KimiBrowserRefresh" }));
    act(() => {
      event.listeners.get("kimi-accounts-updated")?.({
        payload: [{ ...account("newer", ["Microsoft Edge · Default"]), status }],
      });
    });
    await act(async () => { resolveScan({ accounts: [], discovered: 0, refreshed: 0, failed: 0 }); });

    expect(screen.getByText("Account newer")).toBeInTheDocument();
    expect(screen.getByText("KimiAccountMonthlyQuota: 27%")).toBeInTheDocument();
    expect(screen.queryByText("KimiBrowserNoAccount")).not.toBeInTheDocument();
    if (status === "storageError") {
      expect(screen.getByRole("alert")).toHaveTextContent("KimiAccountStorageError");
      expect(screen.getByRole("button", { name: "KimiAccountAdd" })).toBeDisabled();
    }
  });

  it("does not show a successful empty scan after a newer storage failure without accounts", async () => {
    let resolveScan!: (value: KimiBrowserScanResult) => void;
    tauri.refreshKimiBrowserAccounts.mockReturnValue(new Promise((resolve) => { resolveScan = resolve; }));
    render(<KimiAccountsTab enabled browserConsentAccepted />);
    await act(async () => {});
    fireEvent.click(screen.getByRole("button", { name: "KimiBrowserRefresh" }));
    act(() => {
      event.listeners.get("kimi-account-storage-state")?.({ payload: { failed: true } });
    });
    await act(async () => { resolveScan({ accounts: [], discovered: 0, refreshed: 0, failed: 0 }); });

    expect(screen.getByRole("alert")).toHaveTextContent("KimiAccountStorageError");
    expect(screen.queryByText("KimiBrowserNoAccount")).not.toBeInTheDocument();
  });

  it("still shows a successful empty scan when its account event arrives before its response", async () => {
    let resolveScan!: (value: KimiBrowserScanResult) => void;
    tauri.refreshKimiBrowserAccounts.mockReturnValue(new Promise((resolve) => { resolveScan = resolve; }));
    render(<KimiAccountsTab enabled browserConsentAccepted />);
    await act(async () => {});
    fireEvent.click(screen.getByRole("button", { name: "KimiBrowserRefresh" }));
    act(() => {
      event.listeners.get("kimi-accounts-updated")?.({ payload: [] });
      event.listeners.get("kimi-account-storage-state")?.({ payload: { failed: false } });
    });
    await act(async () => { resolveScan({ accounts: [], discovered: 0, refreshed: 0, failed: 0 }); });

    expect(screen.getByText("KimiBrowserNoAccount")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("persists first browser consent on the Kimi page before scanning", async () => {
    vi.spyOn(window, "confirm").mockReturnValue(true);
    render(<KimiAccountsTab enabled browserConsentAccepted={false} />);

    fireEvent.click(screen.getByRole("button", { name: "KimiBrowserRefresh" }));

    await waitFor(() =>
      expect(tauri.updateSettings).toHaveBeenCalledWith({
        kimiBrowserConsentAccepted: true,
      }),
    );
    await waitFor(() => expect(tauri.refreshKimiBrowserAccounts).toHaveBeenCalledTimes(1));
    expect(tauri.updateSettings.mock.invocationCallOrder[0]).toBeLessThan(
      tauri.refreshKimiBrowserAccounts.mock.invocationCallOrder[0],
    );
  });

  it("does nothing when first browser consent is declined", async () => {
    vi.spyOn(window, "confirm").mockReturnValue(false);
    render(<KimiAccountsTab enabled browserConsentAccepted={false} />);
    await waitFor(() => expect(tauri.listKimiAccounts).toHaveBeenCalledTimes(1));

    const button = screen.getByRole("button", { name: "KimiBrowserRefresh" });
    fireEvent.click(button);

    expect(tauri.updateSettings).not.toHaveBeenCalled();
    expect(tauri.refreshKimiBrowserAccounts).not.toHaveBeenCalled();
    expect(tauri.beginKimiAccountLogin).not.toHaveBeenCalled();
    await waitFor(() => expect(button).toBeEnabled());
  });

  it("renders Edge and Chrome profile labels without profile paths", async () => {
    tauri.listKimiAccounts.mockResolvedValue([
      account("edge", ["Microsoft Edge · Default"]),
      account("chrome", ["Google Chrome · Profile 1"]),
    ]);
    render(<KimiAccountsTab enabled browserConsentAccepted />);

    expect(await screen.findByText("Microsoft Edge · Default")).toBeInTheDocument();
    expect(screen.getByText("Google Chrome · Profile 1")).toBeInTheDocument();
    expect(document.body.textContent).not.toContain("User Data");
  });

  it("shows the no-account message for an empty successful scan", async () => {
    render(<KimiAccountsTab enabled browserConsentAccepted />);

    fireEvent.click(screen.getByRole("button", { name: "KimiBrowserRefresh" }));

    expect(await screen.findByText("KimiBrowserNoAccount")).toBeInTheDocument();
  });

  it("shows the no-browser-account message when only an internal account exists", async () => {
    const internalAccount = account("internal");
    tauri.listKimiAccounts.mockResolvedValue([internalAccount]);
    tauri.refreshKimiBrowserAccounts.mockResolvedValue({
      accounts: [internalAccount],
      discovered: 0,
      refreshed: 0,
      failed: 0,
    });
    render(<KimiAccountsTab enabled browserConsentAccepted />);

    fireEvent.click(screen.getByRole("button", { name: "KimiBrowserRefresh" }));

    expect(await screen.findByText("KimiBrowserNoAccount")).toBeInTheDocument();
  });

  it("keeps the legacy login action separate from browser refresh", async () => {
    render(<KimiAccountsTab enabled browserConsentAccepted />);

    fireEvent.click(screen.getByRole("button", { name: "KimiBrowserRefresh" }));
    await waitFor(() => expect(tauri.refreshKimiBrowserAccounts).toHaveBeenCalledTimes(1));
    expect(tauri.beginKimiAccountLogin).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "KimiAccountAdd" }));
    await waitFor(() => expect(tauri.beginKimiAccountLogin).toHaveBeenCalledTimes(1));
  });

  it("keeps browser refresh disabled only when monthly quota display is disabled", async () => {
    render(<KimiAccountsTab enabled={false} browserConsentAccepted={false} />);
    await waitFor(() => expect(tauri.listKimiAccounts).toHaveBeenCalledTimes(1));
    const button = screen.getByRole("button", { name: "KimiBrowserRefresh" });

    expect(button).toBeDisabled();
    fireEvent.click(button);
    expect(tauri.updateSettings).not.toHaveBeenCalled();
    expect(tauri.refreshKimiBrowserAccounts).not.toHaveBeenCalled();
  });

  it("starts the independent Kimi login only after the user asks", async () => {
    render(<KimiAccountsTab enabled browserConsentAccepted />);
    await waitFor(() => expect(tauri.listKimiAccounts).toHaveBeenCalled());
    expect(tauri.beginKimiAccountLogin).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "KimiAccountAdd" }));
    await waitFor(() => expect(tauri.beginKimiAccountLogin).toHaveBeenCalledTimes(1));
    expect(await screen.findByText("KimiAccountLoginHint")).toBeInTheDocument();
    expect(document.querySelector(".kimi-accounts__webview-host")).not.toBeInTheDocument();
    expect(tauri.setKimiAccountWebviewBounds).not.toHaveBeenCalled();
  });

  it("starts an isolated replacement login for the selected expired account", async () => {
    tauri.listKimiAccounts.mockResolvedValue([
      {
        accountId: "account-a",
        displayName: "账号 A",
        usedPercent: null,
        resetsAt: null,
        updatedAt: null,
        status: "loginRequired",
        matchedCredentialIds: [],
      },
    ]);

    render(<KimiAccountsTab enabled browserConsentAccepted />);
    fireEvent.click(await screen.findByRole("button", { name: "KimiAccountRelogin" }));

    await waitFor(() =>
      expect(tauri.beginKimiAccountLogin).toHaveBeenCalledWith("account-a"),
    );
  });

  it("shows the background-reading state when the foreground budget expires", async () => {
    tauri.completeKimiAccountLogin.mockResolvedValue({
      status: "pending",
      snapshot: null,
    });

    render(<KimiAccountsTab enabled browserConsentAccepted />);
    fireEvent.click(screen.getByRole("button", { name: "KimiAccountAdd" }));
    fireEvent.click(await screen.findByRole("button", { name: "KimiAccountFinishLogin" }));

    expect(await screen.findByText("KimiAccountReadingInBackground")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "KimiAccountCancelLogin" })).toBeEnabled();
  });

  it("keeps cancel available while the foreground completion call is waiting", async () => {
    let resolveCompletion!: (value: unknown) => void;
    tauri.completeKimiAccountLogin.mockReturnValue(new Promise((resolve) => {
      resolveCompletion = resolve;
    }));

    render(<KimiAccountsTab enabled browserConsentAccepted />);
    fireEvent.click(screen.getByRole("button", { name: "KimiAccountAdd" }));
    fireEvent.click(await screen.findByRole("button", { name: "KimiAccountFinishLogin" }));

    await waitFor(() => expect(tauri.completeKimiAccountLogin).toHaveBeenCalledTimes(1));
    expect(screen.getByRole("button", { name: "KimiAccountCancelLogin" })).toBeEnabled();

    resolveCompletion({ status: "pending", snapshot: null });
    await screen.findByText("KimiAccountReadingInBackground");
  });

  it("finishes a pending login when the background completion event arrives", async () => {
    tauri.completeKimiAccountLogin.mockResolvedValue({
      status: "pending",
      snapshot: null,
    });

    render(<KimiAccountsTab enabled browserConsentAccepted />);
    fireEvent.click(screen.getByRole("button", { name: "KimiAccountAdd" }));
    fireEvent.click(await screen.findByRole("button", { name: "KimiAccountFinishLogin" }));
    await screen.findByText("KimiAccountReadingInBackground");
    await waitFor(() => expect(event.listeners.has("kimi-account-login-completed")).toBe(true));

    act(() => {
      event.listeners.get("kimi-account-login-completed")?.({
        payload: {
          sessionId: "session-a",
          status: "completed",
          snapshot: null,
          error: null,
        },
      });
    });

    expect(await screen.findByRole("button", { name: "KimiAccountAdd" })).toBeEnabled();
    expect(screen.queryByText("KimiAccountReadingInBackground")).not.toBeInTheDocument();
  });

  it("does not cancel the independent login when the settings tab unmounts", async () => {
    const view = render(<KimiAccountsTab enabled browserConsentAccepted />);
    fireEvent.click(screen.getByRole("button", { name: "KimiAccountAdd" }));
    await waitFor(() => expect(tauri.beginKimiAccountLogin).toHaveBeenCalledTimes(1));

    view.unmount();
    await act(async () => Promise.resolve());

    expect(tauri.cancelKimiAccountLogin).not.toHaveBeenCalled();
  });

  it("keeps the session visible when commit already won the cancellation race", async () => {
    tauri.cancelKimiAccountLogin.mockResolvedValue(false);

    render(<KimiAccountsTab enabled browserConsentAccepted />);
    fireEvent.click(screen.getByRole("button", { name: "KimiAccountAdd" }));
    fireEvent.click(await screen.findByRole("button", { name: "KimiAccountCancelLogin" }));
    await waitFor(() => expect(tauri.cancelKimiAccountLogin).toHaveBeenCalledWith("session-a"));

    expect(screen.getByRole("button", { name: "KimiAccountFinishLogin" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "KimiAccountAdd" })).not.toBeInTheDocument();
  });

  it("ignores a late failure from a cancelled older session", async () => {
    let rejectOldCompletion!: (reason: unknown) => void;
    tauri.beginKimiAccountLogin
      .mockResolvedValueOnce({ sessionId: "session-a" })
      .mockResolvedValueOnce({ sessionId: "session-b" });
    tauri.completeKimiAccountLogin.mockReturnValueOnce(new Promise((_, reject) => {
      rejectOldCompletion = reject;
    }));

    render(<KimiAccountsTab enabled browserConsentAccepted />);
    fireEvent.click(screen.getByRole("button", { name: "KimiAccountAdd" }));
    fireEvent.click(await screen.findByRole("button", { name: "KimiAccountFinishLogin" }));
    await waitFor(() => expect(tauri.completeKimiAccountLogin).toHaveBeenCalledTimes(1));

    fireEvent.click(screen.getByRole("button", { name: "KimiAccountCancelLogin" }));
    fireEvent.click(await screen.findByRole("button", { name: "KimiAccountAdd" }));
    await waitFor(() => expect(tauri.beginKimiAccountLogin).toHaveBeenCalledTimes(2));

    await act(async () => rejectOldCompletion(new Error("old session failed")));

    expect(screen.queryByText(/old session failed/)).not.toBeInTheDocument();
    expect(screen.getByText("KimiAccountLoginHint")).toBeInTheDocument();
  });
});
