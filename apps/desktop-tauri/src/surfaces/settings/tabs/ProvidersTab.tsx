import { useEffect, useMemo, useState } from "react";
import type {
  ProviderCatalogEntry,
  SettingsUpdate,
} from "../../../types/bridge";
import type { BootstrapState } from "../../../types/bridge";
import { useLocale } from "../../../hooks/useLocale";
import type { LocaleKey } from "../../../i18n/keys";
import {
  ProvidersSidebar,
  type ProviderSidebarRow,
  type ProviderSidebarStatus,
} from "../providers/ProvidersSidebar";
import { ProviderDetailPane } from "../providers/ProviderDetailPane";
import { reorderProviders } from "../../../lib/tauri";
import { useProviders } from "../../../hooks/useProviders";
import { groupProviderSnapshots, type ProviderPresentationGroup } from "../../../lib/providerGroups";

interface ProvidersTabProps {
  settings: BootstrapState["settings"];
  providers: ProviderCatalogEntry[];
  set: (patch: SettingsUpdate) => void;
  saving: boolean;
}

export default function ProvidersTab({
  settings,
  providers,
  set,
  saving,
}: ProvidersTabProps) {
  const { t } = useLocale();
  const { providers: snapshots } = useProviders();
  const [selectedId, setSelectedId] = useState<string | null>(
    providers[0]?.id ?? null,
  );
  // Locally-owned catalog order so drag-reorder feels instant before the
  // backend `reorder_providers` round-trip settles.
  const [orderedProviders, setOrderedProviders] =
    useState<ProviderCatalogEntry[]>(providers);
  const [searchText, setSearchText] = useState("");

  useEffect(() => {
    setOrderedProviders(providers);
  }, [providers]);

  const enabled = useMemo(
    () => new Set(settings.enabledProviders),
    [settings.enabledProviders],
  );

  const toggle = (id: string, on: boolean) => {
    const next = new Set(enabled);
    if (on) next.add(id);
    else next.delete(id);
    set({
      enabledProviders: orderedProviders
        .map((provider) => provider.id)
        .filter((providerId) => next.has(providerId)),
    });
  };

  const rows: ProviderSidebarRow[] = useMemo(() => {
    const groups = groupProviderSnapshots(snapshots, orderedProviders, settings.enabledProviders, settings.providerOrder ?? []);
    const snapshotMap = new Map(groups.map((group) => [group.providerId, group]));
    return orderedProviders.map((p) => {
      const isOn = enabled.has(p.id);
      const group = snapshotMap.get(p.id) ?? null;
      return {
        id: p.id,
        displayName: p.displayName,
        enabled: isOn,
        status: deriveProviderGroupStatus(isOn, group),
        subtitlePrimary: providerGroupSidebarSubtitle(p.id, isOn, group, t),
        subtitleSecondary: providerGroupSidebarMetric(group),
      };
    });
  }, [enabled, orderedProviders, snapshots, t]);

  const normalizedSearch = searchText.trim().toLowerCase();
  const visibleRows = useMemo(
    () =>
      normalizedSearch
        ? rows.filter((row) => {
            const name = row.displayName.toLowerCase();
            const id = row.id.toLowerCase();
            return name.includes(normalizedSearch) || id.includes(normalizedSearch);
          })
        : rows,
    [normalizedSearch, rows],
  );

  useEffect(() => {
    if (visibleRows.length === 0) {
      if (selectedId !== null) setSelectedId(null);
      return;
    }
    if (!selectedId || !visibleRows.some((row) => row.id === selectedId)) {
      setSelectedId(visibleRows[0].id);
    }
  }, [selectedId, visibleRows]);

  const handleReorder = (ids: string[]) => {
    const byId = new Map(orderedProviders.map((p) => [p.id, p]));
    const nextIds = normalizedSearch
      ? mergeFilteredOrder(
          orderedProviders.map((p) => p.id),
          new Set(visibleRows.map((row) => row.id)),
          ids,
        )
      : ids;
    const next = nextIds
      .map((id) => byId.get(id))
      .filter((p): p is ProviderCatalogEntry => Boolean(p));
    setOrderedProviders(next);
    void reorderProviders(nextIds).catch(() => {
      setOrderedProviders(providers);
    });
  };

  const selectedEntry =
    orderedProviders.find((p) => p.id === selectedId) ?? null;

  return (
    <div className="provider-split">
      <ProvidersSidebar
        providers={visibleRows}
        selectedId={selectedId}
        searchText={searchText}
        onSearchTextChange={setSearchText}
        onSelect={setSelectedId}
        onReorder={handleReorder}
        onToggleEnabled={toggle}
        disabled={saving}
      />
      <ProviderDetailPane
        providerId={selectedId}
        cookieDomain={selectedEntry?.cookieDomain ?? null}
        resetTimeRelative={settings.resetTimeRelative}
        providerMetrics={settings.providerMetrics}
        wayfinderGatewayUrl={settings.wayfinderGatewayUrl ?? "http://127.0.0.1:8088"}
        settingsDisabled={saving}
        onSettingsChange={set}
      />
    </div>
  );
}

function mergeFilteredOrder(
  fullOrder: string[],
  visibleIds: Set<string>,
  reorderedVisibleIds: string[],
): string[] {
  const nextVisible = [...reorderedVisibleIds];
  return fullOrder.map((id) =>
    visibleIds.has(id) ? (nextVisible.shift() ?? id) : id,
  );
}

// ── Provider sidebar subtitle helpers (port of
//    rust/src/native_ui/preferences.rs::provider_sidebar_subtitle). ─────

export function deriveProviderGroupStatus(
  isEnabled: boolean,
  group: ProviderPresentationGroup | null,
): ProviderSidebarStatus {
  if (!isEnabled) return "disabled";
  if (!group) return "loading";
  if (group.isAllFailed) return "error";
  if (group.successfulSnapshots.some((snap) => snap.refreshError)) return "stale";
  if (group.successfulSnapshots.length > 0 && group.successfulSnapshots.every((snap) => {
    const updatedMs = new Date(snap.updatedAt).getTime();
    return Number.isFinite(updatedMs) && (Date.now() - updatedMs) / 60_000 > 10;
  })) return "stale";
  return "ok";
}

/**
 * Minimal port of `provider_sidebar_source_hint`
 * (rust/src/native_ui/preferences.rs:3753). When we have a live snapshot the
 * backend-supplied `sourceLabel` wins; otherwise we fall back to the neutral
 * "Not detected" / "Disabled" copy.
 */
export function providerGroupSidebarSubtitle(
  providerId: string,
  isEnabled: boolean,
  group: ProviderPresentationGroup | null,
  t: (key: LocaleKey) => string,
): string {
  if (!isEnabled) {
    return `${t("ProviderDisabled")} — ${providerSourceHintShort(providerId, t)}`;
  }
  if (!group) {
    return t("WaitingForUsage");
  }
  if (group.hasMultipleCredentials) {
    return t("ApiKeyCount").replace("{}", String(group.credentialCount));
  }
  const snap = group.snapshots[0];
  const source = snap.sourceLabel || providerSourceHintShort(providerId, t);
  return source;
}

function providerSourceHintShort(
  providerId: string,
  t: (key: LocaleKey) => string,
): string {
  const id = providerId.toLowerCase();
  switch (id) {
    case "cursor":
    case "factory":
    case "droid":
    case "kimi":
    case "kimik2":
    case "augment":
    case "opencode":
    case "amp":
    case "ollama":
    case "alibaba":
    case "infini":
    case "manus":
    case "mimo":
    case "commandcode":
      return t("ProviderSourceWebShort");
    case "gemini":
    case "antigravity":
    case "jetbrains":
      return t("ProviderSourceCliShort");
    case "copilot":
      return t("ProviderSourceOauthShort");
    case "zai":
    case "vertexai":
    case "openrouter":
    case "bedrock":
    case "nanogpt":
    case "warp":
    case "deepinfra":
    case "aiand":
    case "zenmux":
    case "clinepass":
    case "neuralwatt":
    case "doubao":
    case "crof":
    case "stepfun":
    case "venice":
    case "openaiapi":
    case "elevenlabs":
    case "deepgram":
    case "groq":
    case "llmproxy":
      return t("ProviderSourceApiShort");
    case "kiro":
      return t("ProviderSourceKiroEnvShort");
    case "claude":
    case "codex":
    case "minimax":
    default:
      return t("ProviderSourceAutoShort");
  }
}

export function providerGroupSidebarMetric(
  group: ProviderPresentationGroup | null,
): string | undefined {
  if (!group || group.hasMultipleCredentials || group.isAllFailed) return undefined;
  const snap = group.snapshots[0];
  const rate = snap.primary;
  if (!rate) return undefined;
  if (Number.isFinite(rate.usedPercent)) {
    return `${Math.round(Math.max(0, rate.usedPercent))}%`;
  }
  return undefined;
}
