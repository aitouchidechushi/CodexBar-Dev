import { useEffect, useState } from "react";
import { useLocale } from "../../../hooks/useLocale";
import { getApiKeyProviders } from "../../../lib/tauri";
import type {
  ApiKeyProviderInfoBridge,
  ProviderCatalogEntry,
} from "../../../types/bridge";
import { ApiKeySection } from "../providers/ApiKeySection";

export default function ApiKeysTab({ providers: _providers }: { providers: ProviderCatalogEntry[] }) {
  const { t } = useLocale();
  const [apiKeyProviders, setApiKeyProviders] = useState<ApiKeyProviderInfoBridge[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let stale = false;
    getApiKeyProviders()
      .then((entries) => {
        if (!stale) setApiKeyProviders(entries);
      })
      .catch((reason: unknown) => {
        if (!stale) setError(reason instanceof Error ? reason.message : String(reason));
      });
    return () => { stale = true; };
  }, []);

  return (
    <section className="settings-section">
      <h3 className="settings-section__title">{t("SectionApiKeys")}</h3>
      <p className="settings-section__hint">{t("ApiKeysTabHint")}</p>
      {error && <div className="settings-status settings-status--error" role="alert">{error}</div>}
      {apiKeyProviders.map((provider) => (
        <ApiKeySection key={provider.id} providerId={provider.id} />
      ))}
    </section>
  );
}
