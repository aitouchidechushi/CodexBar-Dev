import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { useLocale } from "../../../hooks/useLocale";
import {
  beginKimiAccountLogin,
  cancelKimiAccountLogin,
  completeKimiAccountLogin,
  listKimiAccounts,
  refreshKimiBrowserAccounts,
  removeKimiAccount,
  updateSettings,
} from "../../../lib/tauri";
import type { KimiAccountSnapshot, KimiLoginCompletionEvent } from "../../../types/bridge";

interface KimiAccountsTabProps {
  enabled: boolean;
  browserConsentAccepted: boolean;
}

export default function KimiAccountsTab({
  enabled,
  browserConsentAccepted,
}: KimiAccountsTabProps) {
  const { t } = useLocale();
  const [accounts, setAccounts] = useState<KimiAccountSnapshot[]>([]);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [reloginAccountId, setReloginAccountId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [readingInBackground, setReadingInBackground] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [storageBlocked, setStorageBlocked] = useState(false);
  const [browserScanCompleted, setBrowserScanCompleted] = useState(false);
  const [browserConsentGranted, setBrowserConsentGranted] = useState(browserConsentAccepted);
  const activeSessionRef = useRef<string | null>(null);
  const accountRevisionRef = useRef(0);

  const reload = useCallback(() => {
    const revision = ++accountRevisionRef.current;
    void listKimiAccounts().then((loaded) => {
      if (revision !== accountRevisionRef.current) return;
      setAccounts(loaded);
      setStorageBlocked(loaded.some((account) => account.status === "storageError"));
    }).catch(() => {
      if (revision === accountRevisionRef.current) setStorageBlocked(true);
    });
  }, []);

  useEffect(reload, [reload]);

  useEffect(() => {
    if (browserConsentAccepted) setBrowserConsentGranted(true);
  }, [browserConsentAccepted]);

  useEffect(() => {
    const unlisten = listen<KimiAccountSnapshot[]>("kimi-accounts-updated", (event) => {
      accountRevisionRef.current++;
      setAccounts(event.payload);
      // Account presentation is not proof of storage recovery, especially
      // when removal or disabling quota publishes an empty list.
      setStorageBlocked((blocked) => blocked || event.payload.some((account) => account.status === "storageError"));
    });
    const unlistenStorage = listen<{ failed: boolean }>("kimi-account-storage-state", (event) => {
      accountRevisionRef.current++;
      setStorageBlocked(event.payload.failed);
    });
    return () => {
      accountRevisionRef.current++;
      void unlisten.then((stop) => stop());
      void unlistenStorage.then((stop) => stop());
    };
  }, []);

  useEffect(() => {
    const unlisten = listen<KimiLoginCompletionEvent>("kimi-account-login-completed", (event) => {
      if (event.payload.sessionId !== activeSessionRef.current) return;
      setBusy(false);
      setReadingInBackground(false);
      if (event.payload.status === "completed") {
        activeSessionRef.current = null;
        setSessionId(null);
        setReloginAccountId(null);
        reload();
      } else if (event.payload.status === "failed") {
        setError(event.payload.error);
      } else {
        activeSessionRef.current = null;
        setSessionId(null);
        setReloginAccountId(null);
      }
    });
    return () => {
      void unlisten.then((stop) => stop());
    };
  }, [reload]);

  const startLogin = async (expectedAccountId?: string) => {
    if (storageBlocked) return;
    setBusy(true);
    setError(null);
    try {
      const session = await beginKimiAccountLogin(expectedAccountId);
      setReloginAccountId(expectedAccountId ?? null);
      activeSessionRef.current = session.sessionId;
      setSessionId(session.sessionId);
      setReadingInBackground(false);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const readBrowserAccounts = async () => {
    if (!enabled || busy || storageBlocked) return;
    setBusy(true);
    setError(null);
    setBrowserScanCompleted(false);
    try {
      if (!browserConsentGranted) {
        if (!window.confirm(t("KimiBrowserConsentMessage"))) return;
        const updated = await updateSettings({ kimiBrowserConsentAccepted: true });
        if (updated.kimiBrowserConsentAccepted !== true) {
          throw new Error("Kimi browser consent was not persisted");
        }
        setBrowserConsentGranted(true);
      }
      const revision = ++accountRevisionRef.current;
      const result = await refreshKimiBrowserAccounts();
      if (revision === accountRevisionRef.current) {
        setAccounts(result.accounts);
        setStorageBlocked(result.accounts.some((account) => account.status === "storageError"));
      }
      setBrowserScanCompleted(true);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const finishLogin = async () => {
    if (!sessionId || storageBlocked) return;
    const id = sessionId;
    setBusy(true);
    setError(null);
    try {
      const result = await completeKimiAccountLogin(id, reloginAccountId ?? undefined);
      if (activeSessionRef.current !== id) return;
      setBusy(false);
      if (result.status === "completed") {
        activeSessionRef.current = null;
        setSessionId(null);
        setReloginAccountId(null);
        reload();
      } else {
        setReadingInBackground(true);
      }
    } catch (reason) {
      if (activeSessionRef.current === id) {
        setError(String(reason));
        setBusy(false);
      }
    }
  };

  const cancelLogin = async () => {
    if (!sessionId) return;
    const id = sessionId;
    const cancelled = await cancelKimiAccountLogin(id).catch(() => false);
    if (!cancelled || activeSessionRef.current !== id) return;
    activeSessionRef.current = null;
    setSessionId(null);
    setReloginAccountId(null);
    setReadingInBackground(false);
    setBusy(false);
    setError(null);
  };

  const remove = async (accountId: string) => {
    if (storageBlocked) return;
    setBusy(true);
    setError(null);
    try {
      await removeKimiAccount(accountId);
      reload();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="settings-section kimi-accounts">
      <h3 className="settings-section__title">{t("TabKimiAccounts")}</h3>
      <p className="settings-section__hint">{t("KimiAccountsDescription")}</p>
      {!enabled && <p className="settings-section__hint">{t("KimiMonthlyQuotaShowHelper")}</p>}
      <div className="kimi-accounts__browser-actions">
        <p className="settings-section__hint">{t("KimiBrowserDescription")}</p>
        <button
          type="button"
          disabled={!enabled || busy || storageBlocked || sessionId != null}
          onClick={() => void readBrowserAccounts()}
        >
          {t("KimiBrowserRefresh")}
        </button>
      </div>
      {browserScanCompleted && !storageBlocked &&
        !accounts.some((account) => (account.sourceLabels?.length ?? 0) > 0) && (
          <p className="settings-section__hint">{t("KimiBrowserNoAccount")}</p>
        )}

      {accounts.length > 0 && (
        <div className="kimi-accounts__list">
          {accounts.map((account) => (
            <div className="kimi-accounts__item" key={account.accountId}>
              <div>
                <strong>{account.displayName}</strong>
                {account.sourceLabels?.map((label) => (
                  <span className="kimi-accounts__source" key={label}>{label}</span>
                ))}
                {enabled && (account.status === "ok" || account.status === "stale" || account.status === "storageError") && account.usedPercent != null ? (
                  <span>{t("KimiAccountMonthlyQuota")}: {Math.round(account.usedPercent)}%</span>
                ) : enabled && account.status === "loginRequired" ? (
                  <span>{t("KimiAccountLoginRequired")}</span>
                ) : enabled && account.status === "refreshFailed" ? (
                  <span>{t("KimiAccountRefreshFailed")}</span>
                ) : null}
                {enabled && account.status === "stale" && (
                  <span>{t("KimiAccountRefreshFailed")}</span>
                )}
                {enabled && account.resetsAt && (
                  <span>{t("KimiAccountResetTime")}: {new Date(account.resetsAt).toLocaleString()}</span>
                )}
              </div>
              <div className="kimi-accounts__item-actions">
                {enabled && account.status === "loginRequired" && (
                  <button type="button" disabled={busy || storageBlocked || sessionId != null} onClick={() => void startLogin(account.accountId)}>
                    {t("KimiAccountRelogin")}
                  </button>
                )}
                <button type="button" disabled={busy || storageBlocked || sessionId != null} onClick={() => void remove(account.accountId)}>
                  {t("KimiAccountRemove")}
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      {sessionId ? (
        <>
          <p className="settings-section__hint">{t("KimiAccountLoginHint")}</p>
          {readingInBackground && (
            <p className="settings-section__hint">{t("KimiAccountReadingInBackground")}</p>
          )}
          <div className="kimi-accounts__actions">
            <button type="button" disabled={busy || storageBlocked} onClick={() => void finishLogin()}>
              {t("KimiAccountFinishLogin")}
            </button>
            <button type="button" onClick={() => void cancelLogin()}>
              {t("KimiAccountCancelLogin")}
            </button>
          </div>
        </>
      ) : (
        <button type="button" disabled={!enabled || busy || storageBlocked} onClick={() => void startLogin()}>
          {t("KimiAccountAdd")}
        </button>
      )}
      {storageBlocked && (
        <p role="alert" className="settings-section__error">{t("KimiAccountStorageError")}</p>
      )}
      {error && <p className="settings-section__error">{error}</p>}
    </section>
  );
}
