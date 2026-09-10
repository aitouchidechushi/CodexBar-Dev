import { useCallback, useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  addApiKey,
  deleteApiKey,
  getApiKeyProviders,
  getApiKeys,
  replaceApiKeySecret,
  updateApiKeyLabel,
} from "../../../lib/tauri";
import { useLocale } from "../../../hooks/useLocale";
import type {
  ApiKeyInfoBridge,
  ApiKeyProviderInfoBridge,
} from "../../../types/bridge";

interface Props {
  providerId: string;
}

type EditorState = {
  label?: string;
  secret?: string;
};

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function formatMessage(template: string, ...values: string[]): string {
  return values.reduce((message, value) => message.replace("{}", value), template);
}

/** Per-provider, redacted API-key management embedded in ProviderDetailPane. */
export function ApiKeySection({ providerId }: Props) {
  const { t } = useLocale();
  const [info, setInfo] = useState<ApiKeyProviderInfoBridge | null>(null);
  const [saved, setSaved] = useState<ApiKeyInfoBridge[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [editors, setEditors] = useState<Record<string, EditorState>>({});
  const [busyById, setBusyById] = useState<Record<string, boolean>>({});
  const [errorById, setErrorById] = useState<Record<string, string>>({});
  const [statusById, setStatusById] = useState<Record<string, string>>({});
  const [addSecret, setAddSecret] = useState("");
  const [addLabel, setAddLabel] = useState("");
  const [addBusy, setAddBusy] = useState(false);
  const [addError, setAddError] = useState<string | null>(null);
  const [addStatus, setAddStatus] = useState<string | null>(null);

  const reload = useCallback(async (signal: { stale: boolean }) => {
    try {
      const [providers, keys] = await Promise.all([
        getApiKeyProviders(),
        getApiKeys(),
      ]);
      if (signal.stale) return;
      setInfo(providers.find((provider) => provider.id === providerId) ?? null);
      setSaved(keys);
    } catch (error: unknown) {
      if (!signal.stale) setLoadError(errorMessage(error));
    } finally {
      if (!signal.stale) setLoaded(true);
    }
  }, [providerId]);

  useEffect(() => {
    const signal = { stale: false };
    setLoaded(false);
    setInfo(null);
    setSaved([]);
    setLoadError(null);
    setEditors({});
    setBusyById({});
    setErrorById({});
    setStatusById({});
    setAddSecret("");
    setAddLabel("");
    setAddBusy(false);
    setAddError(null);
    setAddStatus(null);
    void reload(signal);
    return () => { signal.stale = true; };
  }, [reload]);

  useEffect(() => {
    const signal = { stale: false };
    const unlisten = listen("settings-changed", () => {
      void reload(signal);
    });
    return () => {
      signal.stale = true;
      void unlisten.then((stop) => stop());
    };
  }, [reload]);

  const providerKeys = useMemo(
    () => saved
      .filter((key) => key.providerId === providerId)
      .sort((left, right) => left.displayOrdinal - right.displayOrdinal),
    [providerId, saved],
  );

  const setEditor = (credentialId: string, next: EditorState | null) => {
    setEditors((current) => {
      const updated = { ...current };
      if (next) updated[credentialId] = next;
      else delete updated[credentialId];
      return updated;
    });
  };

  const beginRowMutation = (credentialId: string) => {
    setBusyById((current) => ({ ...current, [credentialId]: true }));
    setErrorById((current) => ({ ...current, [credentialId]: "" }));
    setStatusById((current) => ({ ...current, [credentialId]: "" }));
  };

  const finishRowMutation = (credentialId: string) => {
    setBusyById((current) => ({ ...current, [credentialId]: false }));
  };

  const handleLabelSave = async (key: ApiKeyInfoBridge) => {
    const label = editors[key.credentialId]?.label?.trim();
    beginRowMutation(key.credentialId);
    try {
      setSaved(await updateApiKeyLabel(
        providerId,
        key.credentialId,
        label || undefined,
      ));
      setEditor(key.credentialId, null);
      setStatusById((current) => ({ ...current, [key.credentialId]: t("Saved") }));
    } catch (error: unknown) {
      setErrorById((current) => ({
        ...current,
        [key.credentialId]: errorMessage(error),
      }));
    } finally {
      finishRowMutation(key.credentialId);
    }
  };

  const handleSecretSave = async (key: ApiKeyInfoBridge) => {
    const secret = editors[key.credentialId]?.secret?.trim() ?? "";
    if (!secret) return;
    beginRowMutation(key.credentialId);
    try {
      setSaved(await replaceApiKeySecret(providerId, key.credentialId, secret));
      setEditor(key.credentialId, null);
      setStatusById((current) => ({ ...current, [key.credentialId]: t("Saved") }));
    } catch (error: unknown) {
      setErrorById((current) => ({
        ...current,
        [key.credentialId]: errorMessage(error),
      }));
    } finally {
      finishRowMutation(key.credentialId);
    }
  };

  const handleDelete = async (key: ApiKeyInfoBridge) => {
    if (!window.confirm(formatMessage(t("ApiKeyDeleteConfirmation"), key.label))) return;
    beginRowMutation(key.credentialId);
    try {
      setSaved(await deleteApiKey(providerId, key.credentialId));
    } catch (error: unknown) {
      setErrorById((current) => ({
        ...current,
        [key.credentialId]: errorMessage(error),
      }));
    } finally {
      finishRowMutation(key.credentialId);
    }
  };

  const handleAdd = async (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const secret = addSecret.trim();
    if (!secret) return;
    setAddBusy(true);
    setAddError(null);
    setAddStatus(null);
    try {
      setSaved(await addApiKey(providerId, secret, addLabel.trim() || undefined));
      setAddSecret("");
      setAddLabel("");
      setAddStatus(t("Saved"));
    } catch (error: unknown) {
      setAddError(errorMessage(error));
    } finally {
      setAddBusy(false);
    }
  };

  if (!loaded) return null;
  if (!info && !loadError) return null;

  return (
    <section className="provider-detail-section">
      <h4>{t("ApiKeyTitle")}</h4>

      {loadError && (
        <div className="settings-status settings-status--error" role="alert">
          {loadError}
        </div>
      )}

      {info && (
        <>
          {info.help && <p className="credential-card__help">{info.help}</p>}
          {info.dashboardUrl && (
            <a
              className="credential-card__link"
              href={info.dashboardUrl}
              aria-label={formatMessage(t("OpenProviderDashboard"), info.displayName)}
              target="_blank"
              rel="noopener noreferrer"
            >
              {t("OpenProviderDashboard").replace("{}", info.displayName)} ↗
            </a>
          )}

          <ul className="credential-list credential-list--api-keys">
            {providerKeys.map((key) => {
              const editor = editors[key.credentialId];
              const busy = busyById[key.credentialId] ?? false;
              return (
                <li className="credential-card" key={key.credentialId}>
                  <div className="credential-card__header">
                    <div className="credential-card__info">
                      <h5 className="credential-card__title">{key.label}</h5>
                      <span className="credential-card__date">{key.savedAt}</span>
                    </div>
                    <div className="credential-card__actions">
                      <button
                        type="button"
                        className="credential-btn"
                        aria-label={formatMessage(t("ApiKeyEditLabelAction"), key.label)}
                        disabled={busy}
                        onClick={() => setEditor(key.credentialId, { label: key.customLabel ?? "" })}
                      >
                        {t("ApiKeyEditLabel")}
                      </button>
                      <button
                        type="button"
                        className="credential-btn"
                        aria-label={formatMessage(t("ApiKeyReplaceSecretAction"), key.label)}
                        disabled={busy}
                        onClick={() => setEditor(key.credentialId, { secret: "" })}
                      >
                        {t("ApiKeyReplaceSecret")}
                      </button>
                      <button
                        type="button"
                        className="credential-btn credential-btn--danger"
                        aria-label={formatMessage(t("ApiKeyDeleteAction"), key.label)}
                        disabled={busy}
                        onClick={() => void handleDelete(key)}
                      >
                        {t("ApiKeyDelete")}
                      </button>
                    </div>
                  </div>

                  {editor && Object.prototype.hasOwnProperty.call(editor, "label") && (
                    <form
                      className="credential-card__edit"
                      onSubmit={(event) => {
                        event.preventDefault();
                        void handleLabelSave(key);
                      }}
                    >
                      <label className="credential-field">
                        <span>{formatMessage(t("ApiKeyLabelFor"), key.label)}</span>
                        <input
                          type="text"
                          className="text-input credential-card__input credential-card__input--label"
                          value={editor.label ?? ""}
                          disabled={busy}
                          onChange={(event) => setEditor(key.credentialId, {
                            label: event.target.value,
                          })}
                        />
                      </label>
                      <div className="credential-card__edit-actions">
                        <button
                          type="submit"
                          className="credential-btn credential-btn--primary"
                          aria-label={formatMessage(t("ApiKeySaveLabelAction"), key.label)}
                          disabled={busy}
                        >
                          {t("Save")}
                        </button>
                        <button
                          type="button"
                          className="credential-btn"
                          disabled={busy}
                          onClick={() => setEditor(key.credentialId, null)}
                        >
                          {t("Cancel")}
                        </button>
                      </div>
                    </form>
                  )}

                  {editor && Object.prototype.hasOwnProperty.call(editor, "secret") && (
                    <form
                      className="credential-card__edit"
                      onSubmit={(event) => {
                        event.preventDefault();
                        void handleSecretSave(key);
                      }}
                    >
                      <label className="credential-field">
                        <span>{formatMessage(t("ApiKeyReplacementFor"), key.label)}</span>
                        <input
                          type="password"
                          className="text-input credential-card__input"
                          autoComplete="new-password"
                          value={editor.secret ?? ""}
                          disabled={busy}
                          onChange={(event) => setEditor(key.credentialId, {
                            secret: event.target.value,
                          })}
                        />
                      </label>
                      <div className="credential-card__edit-actions">
                        <button
                          type="submit"
                          className="credential-btn credential-btn--primary"
                          aria-label={formatMessage(t("ApiKeySaveReplacementAction"), key.label)}
                          disabled={busy || !editor.secret?.trim()}
                        >
                          {t("Save")}
                        </button>
                        <button
                          type="button"
                          className="credential-btn"
                          disabled={busy}
                          onClick={() => setEditor(key.credentialId, null)}
                        >
                          {t("Cancel")}
                        </button>
                      </div>
                    </form>
                  )}

                  <div className="credential-row-message" aria-live="polite">
                    {errorById[key.credentialId] && (
                      <span className="credential-row-message--error" role="alert">
                        {errorById[key.credentialId]}
                      </span>
                    )}
                    {statusById[key.credentialId]}
                  </div>
                </li>
              );
            })}
          </ul>

          <form className="credential-add-form credential-add-form--api-key" onSubmit={handleAdd}>
            <label className="credential-field">
              <span>{t("ApiKeyNew")}</span>
              <input
                type="password"
                className="text-input credential-card__input"
                autoComplete="new-password"
                value={addSecret}
                disabled={addBusy}
                onChange={(event) => setAddSecret(event.target.value)}
              />
            </label>
            <label className="credential-field">
              <span>{t("ApiKeyNewLabelOptional")}</span>
              <input
                type="text"
                className="text-input credential-card__input credential-card__input--label"
                value={addLabel}
                disabled={addBusy}
                onChange={(event) => setAddLabel(event.target.value)}
              />
            </label>
            <button
              type="submit"
              className="credential-btn credential-btn--primary credential-add-form__submit"
              disabled={addBusy || !addSecret.trim()}
            >
              {t("ApiKeyAdd")}
            </button>
            <div className="credential-row-message" aria-live="polite">
              {addError && <span className="credential-row-message--error" role="alert">{addError}</span>}
              {addStatus}
            </div>
          </form>
        </>
      )}
    </section>
  );
}
