import {
  Fragment,
  type FormEvent,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { listen } from "@tauri-apps/api/event";
import ConcurrencyCheck from "../components/ConcurrencyCheck";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import type { ApiKeyInfoBridge, BootstrapState } from "../types/bridge";
import {
  addApiKey,
  deleteApiKey,
  getApiKeySecret,
  getApiKeys,
  getApiKeyProviders,
  openFlyoutWindow,
  openSettingsWindow,
  quitApp as quitApplication,
  replaceApiKeySecret,
  reorderProviders,
  updateApiKeyLabel,
} from "../lib/tauri";
import { useProviders } from "../hooks/useProviders";
import { useSettings } from "../hooks/useSettings";
import { useUpdateState } from "../hooks/useUpdateState";
import { useLocale } from "../hooks/useLocale";
import MenuCard from "../components/MenuCard";
import PopOutTitleBar from "../components/PopOutTitleBar";
import MenuSurface, {
  MenuEmpty,
  type MenuFooterRow,
} from "../components/MenuSurface";
import UpdateBanner from "../components/UpdateBanner";
import ProviderGrid, { prioritizeProviders } from "../components/ProviderGrid";
import { groupProviderSnapshots } from "../lib/providerGroups";

/**
 * Pop-out window — dashboard and provider deep-links both keep the full card
 * stack. A provider target only scrolls/focuses the requested card so the
 * layout stays consistent with the tray/menu surface.
 */
export default function PopOutPanel({
  state,
  providerId,
}: {
  state: BootstrapState;
  providerId?: string;
}) {
  const {
    providers,
    kimiAccounts,
    isRefreshing,
    refreshingProviderIds,
    refresh,
    hasCachedData,
  } = useProviders();
  const { settings } = useSettings(state.settings);
  const { updateState, checkNow, download, apply, dismiss, openRelease } =
    useUpdateState();
  const { t } = useLocale();

  const sorted = useMemo(() => {
    return groupProviderSnapshots(
      providers,
      state.providers,
      settings.enabledProviders,
      settings.providerOrder,
      settings.kimiMonthlyQuotaEnabled ? kimiAccounts : [],
    );
  }, [providers, kimiAccounts, settings.enabledProviders, settings.kimiMonthlyQuotaEnabled, settings.providerOrder, state.providers]);
  const [selectedProviderId, setSelectedProviderId] = useState<string | null>(
    providerId ?? null,
  );
  const [gridExpanded, setGridExpanded] = useState(false);
  const cardRefs = useRef(new Map<string, HTMLDivElement>());
  const [apiKeyProviderIds, setApiKeyProviderIds] = useState<Set<string>>(
    () => new Set(),
  );
  const [apiKeys, setApiKeys] = useState<ApiKeyInfoBridge[]>([]);
  const [addKeyTarget, setAddKeyTarget] = useState<{
    providerId: string;
    displayName: string;
  } | null>(null);
  const [addKeySecret, setAddKeySecret] = useState("");
  const [addKeyLabel, setAddKeyLabel] = useState("");
  const [addKeyBusy, setAddKeyBusy] = useState(false);
  const [addKeyError, setAddKeyError] = useState<string | null>(null);
  const [keyEditor, setKeyEditor] = useState<{
    mode: "label" | "replace";
    providerId: string;
    credentialId: string;
    label: string;
    secret: string;
    reveal: boolean;
  } | null>(null);
  const [keyEditorBusy, setKeyEditorBusy] = useState(false);
  const [keyEditorError, setKeyEditorError] = useState<string | null>(null);
  const windowScale = useMemo(() => {
    const scalePercent = Number(settings.windowScalePercent);
    return (
      Math.min(250, Math.max(100, Number.isFinite(scalePercent) ? scalePercent : 100)) / 100
    );
  }, [settings.windowScalePercent]);

  // Scale the dashboard via the webview's native zoom (like a browser's Ctrl-+):
  // it reflows content at the real window width, so the side-by-side cards keep
  // filling the window at any scale — unlike CSS `zoom`, which overflows. The
  // main window is shared with the tray surface, so reset zoom to 1 on unmount.
  useEffect(() => {
    const webview = getCurrentWebviewWindow();
    void webview.setZoom(windowScale).catch(() => {});
    return () => {
      void webview.setZoom(1).catch(() => {});
    };
  }, [windowScale]);

  useEffect(() => {
    setSelectedProviderId(providerId ?? null);
  }, [providerId]);

  const reloadApiKeyData = useCallback(async () => {
    const [providers, keys] = await Promise.all([getApiKeyProviders(), getApiKeys()]);
    setApiKeyProviderIds(new Set(providers.map((provider) => provider.id)));
    setApiKeys(keys);
  }, []);

  useEffect(() => {
    const reload = () => {
      void reloadApiKeyData().catch(() => {});
    };
    reload();
    const unlisten = listen("settings-changed", reload);
    return () => {
      void unlisten.then((stop) => stop());
    };
  }, [reloadApiKeyData]);

  const openAddKey = (providerId: string, displayName: string) => {
    setAddKeyTarget({ providerId, displayName });
    setAddKeySecret("");
    setAddKeyLabel("");
    setAddKeyError(null);
  };

  const closeAddKey = () => {
    if (!addKeyBusy) setAddKeyTarget(null);
  };

  const submitAddKey = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!addKeyTarget || !addKeySecret.trim()) return;
    setAddKeyBusy(true);
    setAddKeyError(null);
    try {
      await addApiKey(
        addKeyTarget.providerId,
        addKeySecret.trim(),
        addKeyLabel.trim() || undefined,
      );
      setAddKeyTarget(null);
    } catch (error: unknown) {
      const raw = error instanceof Error ? error.message : String(error);
      setAddKeyError(
        raw.includes("An identical API key already exists")
          ? t("ApiKeyDuplicate")
          : raw,
      );
    } finally {
      setAddKeyBusy(false);
    }
  };

  const openLabelEditor = (providerId: string, credentialId: string, label: string) => {
    const saved = apiKeys.find((key) => key.credentialId === credentialId);
    setKeyEditor({
      mode: "label",
      providerId,
      credentialId,
      label: saved?.customLabel ?? label,
      secret: "",
      reveal: false,
    });
    setKeyEditorError(null);
  };

  const openReplacementEditor = async (
    providerId: string,
    credentialId: string,
    label: string,
  ) => {
    setKeyEditor({
      mode: "replace",
      providerId,
      credentialId,
      label,
      secret: "",
      reveal: false,
    });
    setKeyEditorBusy(true);
    setKeyEditorError(null);
    try {
      const secret = await getApiKeySecret(providerId, credentialId);
      setKeyEditor((current) => current && {
        ...current,
        secret,
      });
    } catch (error: unknown) {
      setKeyEditorError(error instanceof Error ? error.message : String(error));
    } finally {
      setKeyEditorBusy(false);
    }
  };

  const closeKeyEditor = () => {
    if (!keyEditorBusy) setKeyEditor(null);
  };

  const submitKeyEditor = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!keyEditor) return;
    setKeyEditorBusy(true);
    setKeyEditorError(null);
    try {
      const keys = keyEditor.mode === "label"
        ? await updateApiKeyLabel(
          keyEditor.providerId,
          keyEditor.credentialId,
          keyEditor.label.trim() || undefined,
        )
        : await replaceApiKeySecret(
          keyEditor.providerId,
          keyEditor.credentialId,
          keyEditor.secret.trim(),
        );
      setApiKeys(keys);
      setKeyEditor(null);
    } catch (error: unknown) {
      setKeyEditorError(error instanceof Error ? error.message : String(error));
    } finally {
      setKeyEditorBusy(false);
    }
  };

  const removeApiKey = async (providerId: string, credentialId: string, label: string) => {
    if (!window.confirm(t("ApiKeyDeleteConfirmation").replace("{}", label))) return;
    try {
      setApiKeys(await deleteApiKey(providerId, credentialId));
    } catch {
      // The existing settings-changed event keeps the homepage metadata in sync.
    }
  };

  const visibleProviders = useMemo(
    () => {
      if (selectedProviderId === null) {
        if (sorted.length + 1 > 32 && !gridExpanded) {
          return prioritizeProviders(sorted, null).slice(0, 4);
        }
        return sorted;
      }
      const match = sorted.find((p) => p.providerId === selectedProviderId);
      return match ? [match] : sorted;
    },
    [sorted, selectedProviderId, gridExpanded],
  );
  const providerOrderKey = useMemo(
    () => sorted.map((provider) => provider.providerId).join(","),
    [sorted],
  );

  const handleGridClick = useCallback((nextProviderId: string | null) => {
    setSelectedProviderId(nextProviderId);
  }, []);
  const handleReorder = useCallback((orderedIds: string[]) => {
    void reorderProviders(orderedIds).catch(() => {});
  }, []);

  useEffect(() => {
    if (!providerId || selectedProviderId !== providerId || providerOrderKey.length === 0) return;

    let cancelled = false;
    const scrollToProvider = () => {
      if (cancelled) return;
      const target = cardRefs.current.get(providerId);
      if (!target) return;

      window.scrollTo(0, 0);
      if (document.scrollingElement) {
        document.scrollingElement.scrollTop = 0;
      }
      document.documentElement.scrollTop = 0;
      document.body.scrollTop = 0;

      for (const selector of [".menu-stack", ".menu-surface__body"]) {
        const container = target.closest<HTMLElement>(selector);
        if (!container) continue;
        container.scrollTop = 0;
        const targetRect = target.getBoundingClientRect();
        const containerRect = container.getBoundingClientRect();
        container.scrollTop += targetRect.top - containerRect.top;
      }
    };

    requestAnimationFrame(() => {
      requestAnimationFrame(scrollToProvider);
    });
    const timer = window.setTimeout(scrollToProvider, 100);
    const lateTimer = window.setTimeout(scrollToProvider, 350);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
      window.clearTimeout(lateTimer);
    };
  }, [providerId, selectedProviderId, providerOrderKey]);

  const openSettings = useCallback(() => {
    openSettingsWindow("general");
  }, []);
  const openProviderSettings = useCallback(() => {
    openSettingsWindow("providers");
  }, []);
  const goTray = useCallback(() => {
    // The flyout ("Pop Out Dashboard") is now its own dedicated OS window
    // rather than a state of the shared `main` window's surface-mode
    // machine, so "back to tray" opens it directly instead of switching
    // `main`'s mode.
    void openFlyoutWindow().catch(() => {});
  }, []);
  const openAbout = useCallback(() => {
    openSettingsWindow("about");
  }, []);
  const quitApp = useCallback(() => {
    void quitApplication();
  }, []);

  const headerActions = [
    { icon: "⊟", title: t("TooltipBackToTray"), onClick: goTray },
  ];

  const footerRows: MenuFooterRow[] = [
    { icon: "⚙", label: t("TooltipSettings"), shortcut: "Ctrl+,", onClick: openSettings },
    { icon: "ℹ", label: t("MenuAbout"), onClick: openAbout },
    { icon: "✕", label: t("MenuQuit"), shortcut: "Ctrl+Q", onClick: quitApp },
  ];

  // Keyboard shortcuts
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (!e.ctrlKey || e.shiftKey || e.altKey || e.metaKey) return;
      switch (e.key.toLowerCase()) {
        case "r":
          e.preventDefault();
          refresh();
          break;
        case ",":
          e.preventDefault();
          openSettings();
          break;
        case "q":
          e.preventDefault();
          quitApp();
          break;
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [refresh, openSettings, quitApp]);

  const banner = (
    <UpdateBanner
      updateState={updateState}
      onCheck={checkNow}
      onDownload={download}
      onApply={apply}
      onDismiss={dismiss}
      onOpenRelease={openRelease}
    />
  );

  const surface = sorted.length === 0 ? (
    <MenuSurface
      variant="popout"
      titleBar={<PopOutTitleBar />}
      onRefresh={refresh}
      isRefreshing={isRefreshing}
      actions={headerActions}
      banner={banner}
      footerRows={footerRows}
    >
      <MenuEmpty
        isLoading={isRefreshing && !hasCachedData}
        onSettings={openSettings}
      />
    </MenuSurface>
  ) : (
    <MenuSurface
      variant="popout"
      titleBar={<PopOutTitleBar />}
      onRefresh={refresh}
      isRefreshing={isRefreshing}
      actions={headerActions}
      banner={banner}
      footerRows={footerRows}
    >
      <div className="popout-provider-toolbar">
        <ProviderGrid
          providers={sorted}
          selectedProviderId={selectedProviderId}
          showAsUsed={settings.showAsUsed}
          showProviderIcons={settings.switcherShowsIcons}
          expanded={gridExpanded}
          onExpandedChange={setGridExpanded}
          onSelect={handleGridClick}
          onReorder={handleReorder}
        />
        <ConcurrencyCheck />
        <button
          type="button"
          className="popout-provider-toolbar__action popout-provider-toolbar__refresh credential-btn"
          disabled={isRefreshing}
          onClick={refresh}
        >
          <svg className={isRefreshing ? "spin" : ""} aria-hidden="true" viewBox="0 0 24 24" fill="none">
            <path d="M20 11a8 8 0 1 0 1.3 4.3" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
            <path d="M20 4v7h-7" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
          {t("ActionRefreshAll")}
        </button>
        <button
          type="button"
          className="popout-provider-toolbar__action popout-provider-toolbar__add-provider credential-btn"
          onClick={openProviderSettings}
        >
          {t("ActionAddProvider")}
        </button>
      </div>
      <div className="provider-grid__divider" />
      <div className="menu-stack">
        {visibleProviders.map((p, idx) => (
          <Fragment key={p.providerId}>
            {idx > 0 && <div className="menu-stack__sep" />}
            <div
              className={`menu-stack__item${selectedProviderId === p.providerId ? " menu-stack__item--selected" : ""}`}
              ref={(node) => {
                if (node) {
                  cardRefs.current.set(p.providerId, node);
                } else {
                  cardRefs.current.delete(p.providerId);
                }
              }}
            >
              <MenuCard
                providerGroup={p}
                isRefreshing={refreshingProviderIds.has(p.providerId)}
                hideEmail={settings.hidePersonalInfo}
                resetTimeRelative={settings.resetTimeRelative}
                showResetWhenExhausted={settings.showResetWhenExhausted}
                showAsUsed={settings.showAsUsed}
                compactMetrics={selectedProviderId === null}
                groupKimiAccounts
                canAddApiKey={apiKeyProviderIds.has(p.providerId)}
                onAddApiKey={() => openAddKey(p.providerId, p.displayName)}
                onEditApiKey={openLabelEditor}
                onReplaceApiKey={(providerId, credentialId, label) => {
                  void openReplacementEditor(providerId, credentialId, label);
                }}
                onDeleteApiKey={(providerId, credentialId, label) => {
                  void removeApiKey(providerId, credentialId, label);
                }}
              />
            </div>
          </Fragment>
        ))}
      </div>
    </MenuSurface>
  );

  return (
    <div className="popout-scale-shell">
      {surface}
      {addKeyTarget && (
        <div className="api-key-dialog-backdrop" onClick={closeAddKey}>
          <form
            className="api-key-dialog"
            role="dialog"
            aria-modal="true"
            aria-label={`${t("ApiKeyAdd")} — ${addKeyTarget.displayName}`}
            onClick={(event) => event.stopPropagation()}
            onSubmit={(event) => void submitAddKey(event)}
          >
            <h2>{t("ApiKeyAdd")} — {addKeyTarget.displayName}</h2>
            <label className="credential-field">
              <span>
                {addKeyTarget.providerId === "minimax"
                  ? "Token Plan Key"
                  : t("ApiKeyNew")}
              </span>
              <input
                autoFocus
                type="password"
                autoComplete="new-password"
                className="text-input"
                value={addKeySecret}
                disabled={addKeyBusy}
                onChange={(event) => setAddKeySecret(event.target.value)}
              />
            </label>
            <label className="credential-field">
              <span>{t("ApiKeyLabelOptional")}</span>
              <input
                type="text"
                className="text-input"
                value={addKeyLabel}
                disabled={addKeyBusy}
                onChange={(event) => setAddKeyLabel(event.target.value)}
              />
            </label>
            {addKeyError && (
              <div className="credential-row-message--error" role="alert">
                {addKeyError}
              </div>
            )}
            <div className="api-key-dialog__actions">
              <button
                type="button"
                className="credential-btn"
                disabled={addKeyBusy}
                onClick={closeAddKey}
              >
                {t("Cancel")}
              </button>
              <button
                type="submit"
                className="credential-btn credential-btn--primary"
                disabled={addKeyBusy || !addKeySecret.trim()}
              >
                {t("ApiKeyAdd")}
              </button>
            </div>
          </form>
        </div>
      )}
      {keyEditor && (
        <div className="api-key-dialog-backdrop" onClick={closeKeyEditor}>
          <form
            className="api-key-dialog"
            role="dialog"
            aria-modal="true"
            aria-label={`${keyEditor.mode === "label" ? t("ApiKeyEditLabel") : t("ApiKeyReplaceSecret")} — ${keyEditor.label}`}
            onClick={(event) => event.stopPropagation()}
            onSubmit={(event) => void submitKeyEditor(event)}
          >
            <h2>{keyEditor.mode === "label" ? t("ApiKeyEditLabel") : t("ApiKeyReplaceSecret")}</h2>
            <label className="credential-field">
              <span>{keyEditor.mode === "label" ? t("ApiKeyLabelOptional") : t("ApiKeyNew")}</span>
              <span className="api-key-dialog__input-wrap">
                <input
                  autoFocus
                  type={keyEditor.mode === "replace" && !keyEditor.reveal ? "password" : "text"}
                  autoComplete={keyEditor.mode === "replace" ? "new-password" : "off"}
                  className="text-input"
                  value={keyEditor.mode === "label" ? keyEditor.label : keyEditor.secret}
                  disabled={keyEditorBusy}
                  onChange={(event) => setKeyEditor((current) => current && (
                    current.mode === "label"
                      ? { ...current, label: event.target.value }
                      : { ...current, secret: event.target.value }
                  ))}
                />
                {keyEditor.mode === "replace" && (
                  <button
                    type="button"
                    className="api-key-dialog__reveal"
                    aria-label={keyEditor.reveal ? t("ApiKeyHideSecret") : t("ApiKeyShowSecret")}
                    title={keyEditor.reveal ? t("ApiKeyHideSecret") : t("ApiKeyShowSecret")}
                    onClick={() => setKeyEditor((current) => current && ({
                      ...current,
                      reveal: !current.reveal,
                    }))}
                  >
                    <svg className="api-key-dialog__reveal-icon" aria-hidden="true" viewBox="0 0 24 24" fill="none">
                      <path d="M2.5 12s3.5-6 9.5-6 9.5 6 9.5 6-3.5 6-9.5 6-9.5-6-9.5-6Z" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" />
                      <circle cx="12" cy="12" r="2.7" stroke="currentColor" strokeWidth="1.8" />
                      {keyEditor.reveal && <path d="M4 4l16 16" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />}
                    </svg>
                  </button>
                )}
              </span>
            </label>
            {keyEditorError && (
              <div className="credential-row-message--error" role="alert">
                {keyEditorError}
              </div>
            )}
            <div className="api-key-dialog__actions">
              <button type="button" className="credential-btn" disabled={keyEditorBusy} onClick={closeKeyEditor}>
                {t("Cancel")}
              </button>
              <button
                type="submit"
                className="credential-btn credential-btn--primary"
                disabled={keyEditorBusy || (keyEditor.mode === "replace" && !keyEditor.secret.trim())}
              >
                {t("Save")}
              </button>
            </div>
          </form>
        </div>
      )}
    </div>
  );
}
