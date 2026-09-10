import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type MouseEvent,
} from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useFormattedResetTime } from "../hooks/useFormattedResetTime";
import { useLocale } from "../hooks/useLocale";
import { useProviders } from "../hooks/useProviders";
import {
  getProviderLocalUsageSummary,
  getSettingsSnapshot,
  refreshProvidersIfStale,
} from "../lib/tauri";
import { ProviderIcon } from "../components/providers/ProviderIcon";
import {
  KimiMonthlyQuotaProgress,
  MonthlyQuotaProgress,
} from "../components/KimiMonthlyQuotaProgress";
import { getProviderIcon } from "../components/providers/providerIcons";
import {
  groupProviderSnapshots,
  providerGroupSections,
  providerSnapshotIdentity,
  selectProviderQuotaWindowsForSection,
} from "../lib/providerGroups";
import {
  formatMonthlyQuotaLabel,
  selectProviderQuotaWindows,
  type ProviderQuotaWindow,
} from "../lib/rateWindowKind";
import type {
  BootstrapState,
  KimiAccountSnapshot,
  ProviderLocalUsageSummary,
  ProviderUsageSnapshot,
  SettingsSnapshot,
} from "../types/bridge";
import {
  completeFloatBarInitialShow,
  FLOAT_BAR_CONFIG_CHANGED_EVENT,
  resizeFloatBar,
} from "./api";
import "./FloatBar.css";

function ResetIcon({ size }: { size: number }) {
  return (
    <svg
      className="floatbar__reset-icon-svg"
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      aria-hidden="true"
    >
      <path
        d="M12.9 7.1a5 5 0 1 0-1.2 3.9"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinecap="round"
      />
      <path
        d="M12.9 3.8v3.3H9.6"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function inlineResetTime(resetText: string): string {
  const normalized = resetText.trim();
  if (/^reset(?:s|ting)?(?:\s+due)?\s*(?:now)?$/i.test(normalized)) {
    return "now";
  }
  return normalized
    .replace(/^resets?\s+in\s+/i, "")
    .replace(/^resets?\s+/i, "")
    .trim();
}

type FloatBarCostSummary = {
  key: string;
  providerId: string;
  displayName: string;
  todayCost: number | null;
  thirtyDayCost: number | null;
};

type FloatBarCostTarget = {
  key: string;
  providerId: string;
  displayName: string;
};

function providerCostKey(providerId: string): string {
  return providerId;
}

function maskEmail(email: string): string {
  const [local, domain] = email.split("@");
  if (!domain) return "••••";
  const prefix = local.slice(0, Math.min(1, local.length));
  return `${prefix}••••@${domain}`;
}

function hasLocalCost(summary: ProviderLocalUsageSummary | null): summary is ProviderLocalUsageSummary {
  return summary?.todayCost != null || summary?.thirtyDayCost != null;
}

function formatUsd(value: number | null): string | null {
  if (value == null || !Number.isFinite(value)) return null;
  return `$${value.toFixed(2)}`;
}

function CostPill({
  summary,
  scale,
  todayLabel,
  thirtyDayLabel,
}: {
  summary: FloatBarCostSummary;
  scale: number;
  todayLabel: string;
  thirtyDayLabel: string;
}) {
  const today = formatUsd(summary.todayCost);
  const thirtyDay = formatUsd(summary.thirtyDayCost);
  const iconSize = Math.round(10 * scale);
  const brand = getProviderIcon(summary.providerId).brandColor;
  const title = [
    today ? `${todayLabel} ${today}` : null,
    thirtyDay ? `${thirtyDayLabel} ${thirtyDay}` : null,
  ]
    .filter(Boolean)
    .join(" / ");

  return (
    <div
      className="floatbar__cost-pill"
      title={`${summary.displayName}: ${title}`}
      data-tauri-drag-region
      style={{ "--brand": brand } as CSSProperties}
    >
      <span className="floatbar__provider-icon" data-tauri-drag-region>
        <ProviderIcon providerId={summary.providerId} size={iconSize} />
      </span>
      <span className="floatbar__cost-items" data-tauri-drag-region>
        {today && (
          <span className="floatbar__cost-item" data-tauri-drag-region>
            <span className="floatbar__cost-label" data-tauri-drag-region>
              {todayLabel}
            </span>
            <span className="floatbar__cost-value" data-tauri-drag-region>
              {today}
            </span>
          </span>
        )}
        {thirtyDay && (
          <span className="floatbar__cost-item" data-tauri-drag-region>
            <span className="floatbar__cost-label" data-tauri-drag-region>
              {thirtyDayLabel}
            </span>
            <span className="floatbar__cost-value" data-tauri-drag-region>
              {thirtyDay}
            </span>
          </span>
        )}
      </span>
    </div>
  );
}
/**
 * The capacity pill shown for a single provider.
 *
 * Color follows usage: green default, amber when remaining drops below the
 * high-usage threshold, red when remaining is below the critical threshold
 * or the provider is exhausted.
 */
function ProviderPill({
  provider,
  quotas,
  providerDisplayName,
  credentialLabel,
  highRemaining,
  critRemaining,
  showAsUsed,
  scale,
  showResetInline,
  resetRelative,
  usedSuffix,
  remainingSuffix,
  errorLabel,
}: {
  provider: ProviderUsageSnapshot;
  quotas?: ProviderQuotaWindow[];
  providerDisplayName: string;
  credentialLabel: string | null;
  highRemaining: number;
  critRemaining: number;
  showAsUsed: boolean;
  scale: number;
  showResetInline: boolean;
  resetRelative: boolean;
  usedSuffix: string;
  remainingSuffix: string;
  errorLabel: string;
}) {
  const displayedQuotas = quotas?.slice(0, 2) ?? [];
  const firstSnapshot = displayedQuotas[0]?.snapshot ?? provider.primary;
  const secondSnapshot = displayedQuotas[1]?.snapshot;
  const snapshots = displayedQuotas.length > 0
    ? displayedQuotas.map((quota) => quota.snapshot)
    : [provider.primary];
  const remainingValues = snapshots.map((snapshot) =>
    Math.max(0, Math.min(100, snapshot.remainingPercent))
  );
  const exhausted = provider.error != null || snapshots.some((snapshot) => snapshot.isExhausted);
  let tone: "ok" | "warn" | "crit" = "ok";
  if (exhausted || remainingValues.some((remaining) => remaining <= critRemaining)) tone = "crit";
  else if (remainingValues.some((remaining) => remaining <= highRemaining)) tone = "warn";

  const brand = getProviderIcon(provider.providerId).brandColor;
  const firstResetText = useFormattedResetTime(
    firstSnapshot.resetsAt,
    firstSnapshot.resetDescription,
    resetRelative,
  );
  const secondResetText = useFormattedResetTime(
    secondSnapshot?.resetsAt ?? null,
    secondSnapshot?.resetDescription ?? null,
    resetRelative,
  );
  const metricQuotas = displayedQuotas.length > 0 ? displayedQuotas : [null];
  const metricResetTexts = [firstResetText, secondResetText];
  const displaySuffix = showAsUsed ? usedSuffix : remainingSuffix;
  const titleMetrics = provider.error
    ? errorLabel
    : metricQuotas.map((quota, metricIndex) => {
        const snapshot = quota?.snapshot ?? provider.primary;
        const percent = showAsUsed
          ? Math.max(0, Math.min(100, snapshot.usedPercent))
          : Math.max(0, Math.min(100, snapshot.remainingPercent));
        const resetText = metricResetTexts[metricIndex];
        return `${quota?.label ? `${quota.label}: ` : ""}${Math.round(percent)}% ${displaySuffix}${resetText ? `\n${resetText}` : ""}`;
      }).join(" · ");
  const iconSize = Math.round(11 * scale);
  const resetIconSize = Math.round(10 * scale);

  return (
    <div
      className={`floatbar__pill floatbar__capacity-pill floatbar__pill--${tone}`}
      title={`${providerDisplayName}${credentialLabel ? ` — ${credentialLabel}` : ""}: ${titleMetrics}`}
      data-tauri-drag-region
      style={{ "--brand": brand } as CSSProperties}
    >
      <span className="floatbar__provider-icon" data-tauri-drag-region>
        <ProviderIcon providerId={provider.providerId} size={iconSize} />
      </span>
      <span className="floatbar__text" data-tauri-drag-region>
        {credentialLabel && (
          <span className="floatbar__credential-label" data-tauri-drag-region>
            {credentialLabel}
          </span>
        )}
        {provider.error ? (
          <span className="floatbar__pct" data-tauri-drag-region>
            {errorLabel}
          </span>
        ) : metricQuotas.map((quota, metricIndex) => {
          const snapshot = quota?.snapshot ?? provider.primary;
          const remaining = Math.max(0, Math.min(100, snapshot.remainingPercent));
          const used = Math.max(0, Math.min(100, snapshot.usedPercent));
          const displayPercent = showAsUsed ? used : remaining;
          const resetText = metricResetTexts[metricIndex];
          const inlineReset = resetText ? inlineResetTime(resetText) : null;
          return (
            <span
              className="floatbar__capacity-metric"
              key={quota?.id ?? "primary"}
              data-tauri-drag-region
            >
              {quota?.label && (
                <span className="floatbar__capacity-label" data-tauri-drag-region>
                  {quota.label}
                </span>
              )}
              <span className="floatbar__pct" data-tauri-drag-region>
                {`${Math.round(displayPercent)}%`}
              </span>
              {showResetInline && resetText && inlineReset && (
                <span
                  className="floatbar__reset"
                  title={resetText}
                  aria-label={resetText}
                  data-tauri-drag-region
                >
                  <ResetIcon size={resetIconSize} />
                  <span className="floatbar__reset-time" data-tauri-drag-region>
                    {inlineReset}
                  </span>
                </span>
              )}
            </span>
          );
        })}
      </span>
    </div>
  );
}

function KimiMonthlyPill({
  account,
  showAsUsed,
  scale,
  showResetInline,
  resetRelative,
}: {
  account: KimiAccountSnapshot;
  showAsUsed: boolean;
  scale: number;
  showResetInline: boolean;
  resetRelative: boolean;
}) {
  const { t } = useLocale();
  const used = Math.max(0, Math.min(100, account.usedPercent ?? 0));
  const percent = showAsUsed ? used : 100 - used;
  const resetText = useFormattedResetTime(account.resetsAt, null, resetRelative);
  const inlineReset = resetText ? inlineResetTime(resetText) : null;
  const monthlyLabel = t("KimiAccountMonthlyQuota");
  const sourceLabel = account.sourceLabels?.join(" · ") ?? "";
  return (
    <div
      className="floatbar__pill floatbar__monthly-pill monthly-quota"
      title={`${account.displayName}${sourceLabel ? ` · ${sourceLabel}` : ""}: ${Math.round(percent)}%`}
      data-tauri-drag-region
    >
      <span className="floatbar__provider-icon" data-tauri-drag-region>
        <ProviderIcon providerId="kimi" size={Math.round(11 * scale)} />
      </span>
      <span className="floatbar__monthly-content" data-tauri-drag-region>
        <span className="floatbar__text" data-tauri-drag-region>
          <span className="floatbar__monthly-badge monthly-quota-badge" aria-label={monthlyLabel} data-tauri-drag-region>M</span>
          <span className="floatbar__credential-label" data-tauri-drag-region>{account.displayName}</span>
          {sourceLabel && (
            <span className="floatbar__account-source" data-tauri-drag-region>{sourceLabel}</span>
          )}
          <span className="floatbar__pct" data-tauri-drag-region>
            {account.usedPercent == null ? "—" : `${Math.round(percent)}%`}
          </span>
          {showResetInline && resetText && inlineReset && (
            <span className="floatbar__reset" title={resetText} data-tauri-drag-region>
              <ResetIcon size={Math.round(10 * scale)} />
              <span className="floatbar__reset-time" data-tauri-drag-region>{inlineReset}</span>
            </span>
          )}
        </span>
        <KimiMonthlyQuotaProgress
          account={account}
          showAsUsed={showAsUsed}
          ariaLabel={`${account.displayName} ${monthlyLabel}`}
          className="floatbar__monthly-progress"
        />
      </span>
    </div>
  );
}

function ProviderMonthlyPill({
  providerId,
  providerDisplayName,
  credentialLabel,
  quota,
  showAsUsed,
  scale,
  showResetInline,
  resetRelative,
}: {
  providerId: string;
  providerDisplayName: string;
  credentialLabel: string | null;
  quota: ProviderQuotaWindow;
  showAsUsed: boolean;
  scale: number;
  showResetInline: boolean;
  resetRelative: boolean;
}) {
  const { t } = useLocale();
  const used = Math.max(0, Math.min(100, quota.snapshot.usedPercent));
  const percent = showAsUsed ? used : 100 - used;
  const resetText = useFormattedResetTime(
    quota.snapshot.resetsAt,
    quota.snapshot.resetDescription,
    resetRelative,
  );
  const inlineReset = resetText ? inlineResetTime(resetText) : null;
  const monthlyLabel = formatMonthlyQuotaLabel(
    quota.label,
    t("ProviderMonthlyLabel"),
    quota.source === "modelSpecific" ? credentialLabel ?? undefined : undefined,
  );
  const needsCredentialScope = monthlyLabel === t("ProviderMonthlyLabel");

  return (
    <div
      className="floatbar__pill floatbar__monthly-pill monthly-quota"
      title={`${providerDisplayName}${credentialLabel ? ` — ${credentialLabel}` : ""}: ${monthlyLabel} ${Math.round(percent)}%`}
      data-tauri-drag-region
    >
      <span className="floatbar__provider-icon" data-tauri-drag-region>
        <ProviderIcon providerId={providerId} size={Math.round(11 * scale)} />
      </span>
      <span className="floatbar__monthly-content" data-tauri-drag-region>
        <span className="floatbar__text" data-tauri-drag-region>
          <span
            className="floatbar__provider-monthly-badge monthly-quota-badge"
            data-tauri-drag-region
          >
            {monthlyLabel}
          </span>
          {credentialLabel && needsCredentialScope && (
            <span className="floatbar__credential-label" data-tauri-drag-region>
              {credentialLabel}
            </span>
          )}
          <span className="floatbar__pct" data-tauri-drag-region>
            {`${Math.round(percent)}%`}
          </span>
          {showResetInline && resetText && inlineReset && (
            <span className="floatbar__reset" title={resetText} data-tauri-drag-region>
              <ResetIcon size={Math.round(10 * scale)} />
              <span className="floatbar__reset-time" data-tauri-drag-region>
                {inlineReset}
              </span>
            </span>
          )}
        </span>
        <MonthlyQuotaProgress
          usedPercent={quota.snapshot.usedPercent}
          showAsUsed={showAsUsed}
          ariaLabel={`${monthlyLabel}${credentialLabel && needsCredentialScope ? ` · ${credentialLabel}` : ""}`}
          className="floatbar__monthly-progress"
        />
      </span>
    </div>
  );
}

/**
 * The always-on-top floating capacity bar.
 *
 * Renders a tiny strip of provider pills. Listens to the same provider
 * refresh cycle as the rest of the app via `useProviders`, and reacts to
 * setting changes (filter list, orientation) live without a reload.
 */
export default function FloatBar({ state }: { state: BootstrapState }) {
  const { t } = useLocale();
  const { providers, kimiAccounts } = useProviders({
    refreshOnMount: false,
  });
  const startDrag = useCallback((event: MouseEvent<HTMLElement>) => {
    if (event.button !== 0) return;
    void getCurrentWindow().startDragging().catch(() => {});
  }, []);

  // Mark the body so our CSS can strip the dark theme background — the
  // floatbar window is meant to be fully transparent around the pills.
  useEffect(() => {
    document.body.classList.add("floatbar-window");
    return () => {
      document.body.classList.remove("floatbar-window");
    };
  }, []);

  // The floatbar window is detached, so it doesn't share React state
  // with the Settings tab. Listen for the Rust-side config-changed event
  // and re-pull the snapshot when fired.
  const [settings, setSettings] = useState<SettingsSnapshot>(state.settings);
  const [localCosts, setLocalCosts] = useState<Record<string, FloatBarCostSummary>>({});

  // The detached floatbar should keep usage fresh, but it must not open or
  // focus any other surface. Refresh data only; provider-updated events feed
  // this window when the backend completes.
  useEffect(() => {
    const intervalMs = Math.max(60_000, settings.refreshIntervalSecs * 1000);
    const tick = () => {
      void refreshProvidersIfStale().catch(() => {});
    };
    tick();
    const id = setInterval(tick, intervalMs);
    return () => clearInterval(id);
  }, [settings.refreshIntervalSecs]);

  useEffect(() => {
    const unlisten = listen(FLOAT_BAR_CONFIG_CHANGED_EVENT, () => {
      void getSettingsSnapshot().then(setSettings).catch(() => {});
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  // Orientation flips re-lay-out the bar without recreating the window.
  const orientation: "horizontal" | "vertical" =
    settings.floatBarOrientation === "vertical" ? "vertical" : "horizontal";
  const style = settings.floatBarStyle === "taskbar" ? "taskbar" : "floating";
  const filterIds = settings.floatBarProviderIds;
  const scale = Math.max(0.75, Math.min(2, settings.floatBarScale / 100));
  const showResetInline = settings.floatBarShowResetInline;
  const showCost = settings.floatBarShowCost;
  const visible = useMemo(() => {
    let list = groupProviderSnapshots(
      providers,
      [],
      settings.enabledProviders,
      settings.providerOrder ?? [],
      settings.kimiMonthlyQuotaEnabled ? kimiAccounts : [],
    );
    if (filterIds && filterIds.length > 0) {
      const wanted = new Set(filterIds);
      list = list.filter((p) => wanted.has(p.providerId));
    }
    if (list.some((group) => group.hasMultipleCredentials)) return list;
    return [...list].sort(
      (a, b) =>
        (b.snapshots[0]?.primary.usedPercent ?? 0) -
        (a.snapshots[0]?.primary.usedPercent ?? 0),
    );
  }, [providers, kimiAccounts, settings.enabledProviders, settings.kimiMonthlyQuotaEnabled, settings.providerOrder, filterIds]);

  const visibleCostTargetKey = visible
    .map((p) => `${providerCostKey(p.providerId)}:${p.providerId}:${p.displayName}`)
    .join("|");
  const visibleCostTargets = useMemo<FloatBarCostTarget[]>(
    () =>
      showCost
        ? visible.map((provider) => ({
            key: providerCostKey(provider.providerId),
            providerId: provider.providerId,
            displayName: provider.displayName,
          }))
        : [],
    [showCost, visibleCostTargetKey],
  );

  useEffect(() => {
    let cancelled = false;
    const targets = visibleCostTargets;

    if (targets.length === 0) {
      setLocalCosts({});
      return () => {
        cancelled = true;
      };
    }

    Promise.allSettled(
      targets.map(async (target) => {
        const localUsage = await getProviderLocalUsageSummary(target.providerId);
        if (!hasLocalCost(localUsage)) return null;
        return {
          key: target.key,
          providerId: target.providerId,
          displayName: target.displayName,
          todayCost: localUsage.todayCost,
          thirtyDayCost: localUsage.thirtyDayCost,
        } satisfies FloatBarCostSummary;
      }),
    )
      .then((results) => {
        if (cancelled) return;
        const next: Record<string, FloatBarCostSummary> = {};
        for (const result of results) {
          if (result.status === "fulfilled" && result.value) {
            next[result.value.key] = result.value;
          }
        }
        setLocalCosts(next);
      })
      .catch(() => {
        if (!cancelled) setLocalCosts({});
      });

    return () => {
      cancelled = true;
    };
  }, [visibleCostTargets]);

  const visibleCosts = visible
    .map((provider) => localCosts[providerCostKey(provider.providerId)])
    .filter((summary): summary is FloatBarCostSummary => Boolean(summary));
  const visibleCostValuesKey = visibleCosts
    .map((summary) => `${summary.key}:${summary.todayCost ?? ""}:${summary.thirtyDayCost ?? ""}`)
    .join("|");
  const visibleCredentialLayoutKey = visible
    .flatMap((group) =>
      providerGroupSections(group).flatMap((section, sectionIndex) => {
        const accountKey = section.type === "kimi-account"
          ? [
              "account",
              section.account.accountId,
              section.account.sourceLabels?.join(",") ?? "",
              section.account.usedPercent ?? "",
              section.account.resetsAt ?? "",
              section.providers.map((provider) => provider.credentialId ?? "default").join(","),
            ].join(":")
          : section.type === "kimi-unmatched"
            ? `unmatched:${section.providers.map((provider) => provider.credentialId ?? "default").join(",")}`
            : section.type === "provider-account"
              ? [
                  "provider-account",
                  sectionIndex,
                  section.displayName,
                  section.sharedMonthlyQuotas.map((quota) => `${quota.id}:${quota.snapshot.usedPercent}`).join(","),
                ].join(":")
              : null;
        const providerKeys = section.providers.map((provider, index) => [
          providerSnapshotIdentity(provider.providerId, provider.credentialId),
          provider.credentialDisplayLabel ?? "",
          provider.credentialDisplayOrdinal ?? index + 1,
          provider.primary.usedPercent,
          provider.primary.remainingPercent,
          provider.primary.resetDescription ?? "",
          provider.error ? "error" : "ok",
          (section.type === "provider-account"
            ? selectProviderQuotaWindowsForSection(section, provider)
            : selectProviderQuotaWindows(provider))
            .map((quota) => `${quota.id}:${quota.snapshot.usedPercent}:${quota.snapshot.resetsAt ?? ""}`)
            .join(","),
        ].join(":"));
        return accountKey ? [accountKey, ...providerKeys] : providerKeys;
      }),
    )
    .join("|");
  // Keep the native floatbar window fitted when late data/fonts/icons change layout.
  const lastResizeRef = useRef<{ w: number; h: number } | null>(null);
  const resizeRafRef = useRef<number | null>(null);
  const hasCompletedInitialShowRef = useRef(false);
  const completeAfterFirstResize = useCallback(() => {
    if (hasCompletedInitialShowRef.current) return;
    hasCompletedInitialShowRef.current = true;
    void completeFloatBarInitialShow().catch((error: unknown) => {
      console.error("FloatBar failed to complete its initial show", error);
    });
  }, []);
  const resizeToContent = useCallback(() => {
    const el = document.querySelector<HTMLElement>(".floatbar");
    if (!el) return;
    if (resizeRafRef.current !== null) {
      cancelAnimationFrame(resizeRafRef.current);
    }
    resizeRafRef.current = requestAnimationFrame(() => {
      resizeRafRef.current = null;
      const rect = el.getBoundingClientRect();
      const padding = 8;
      const w = Math.ceil(rect.width + padding);
      const h = Math.ceil(rect.height + padding);
      const last = lastResizeRef.current;
      if (last && Math.abs(last.w - w) <= 1 && Math.abs(last.h - h) <= 1) return;
      lastResizeRef.current = { w, h };
      void resizeFloatBar(w, h).then(completeAfterFirstResize, completeAfterFirstResize);
    });
  }, [completeAfterFirstResize]);

  useEffect(() => {
    resizeToContent();
  }, [
    resizeToContent,
    visibleCredentialLayoutKey,
    visibleCostValuesKey,
    orientation,
    style,
    scale,
    showResetInline,
    settings.resetTimeRelative,
  ]);

  useEffect(() => {
    const el = document.querySelector<HTMLElement>(".floatbar");
    if (!el || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(resizeToContent);
    observer.observe(el);
    return () => observer.disconnect();
  }, [resizeToContent]);

  useEffect(
    () => () => {
      if (resizeRafRef.current !== null) {
        cancelAnimationFrame(resizeRafRef.current);
      }
    },
    [],
  );

  const highRemaining = 100 - settings.highUsageThreshold;
  const critRemaining = 100 - settings.criticalUsageThreshold;
  const opacityFraction = Math.max(0.3, Math.min(1, settings.floatBarOpacity / 100));
  const horizontalMaxWidth = Math.max(240, (window.screen?.availWidth || 1920) - 16);

  const renderProviderPill = (
    group: (typeof visible)[number],
    provider: ProviderUsageSnapshot,
    index: number,
    quotas?: ProviderQuotaWindow[],
    keyPrefix = "provider",
  ) => {
    const isCredentialGroup = group.hasMultipleCredentials || Boolean(provider.credentialId);
    const configuredLabel = provider.credentialDisplayLabel?.trim();
    const credentialLabel = isCredentialGroup
      ? configuredLabel || `Key ${provider.credentialDisplayOrdinal ?? index + 1}`
      : null;
    return (
      <ProviderPill
        key={quotas && quotas.length > 0
          ? `${keyPrefix}:${providerSnapshotIdentity(provider.providerId, provider.credentialId)}:${quotas.map((quota) => quota.id).join(":")}`
          : providerSnapshotIdentity(provider.providerId, provider.credentialId)}
        provider={provider}
        quotas={quotas}
        providerDisplayName={group.displayName}
        credentialLabel={credentialLabel}
        highRemaining={highRemaining}
        critRemaining={critRemaining}
        showAsUsed={settings.showAsUsed}
        scale={scale}
        showResetInline={showResetInline}
        resetRelative={settings.resetTimeRelative}
        usedSuffix={t("PanelUsedSuffix")}
        remainingSuffix={t("FloatBarRemainingSuffix")}
        errorLabel={t("TrayStatusRowError")}
      />
    );
  };

  const selectCapacityQuotas = (quotas: ProviderQuotaWindow[]): ProviderQuotaWindow[] => {
    const short = quotas.find((quota) => quota.kind === "short");
    const weekly = quotas.find((quota) => quota.kind === "weekly");
    if (short || weekly) {
      return [short, weekly].filter((quota): quota is ProviderQuotaWindow => Boolean(quota));
    }
    return quotas.filter((quota) => quota.kind === "ordinary").slice(0, 2);
  };

  const credentialLabel = (
    group: (typeof visible)[number],
    provider: ProviderUsageSnapshot,
    index: number,
  ): string | null => {
    const isCredentialGroup = group.hasMultipleCredentials || Boolean(provider.credentialId);
    const configuredLabel = provider.credentialDisplayLabel?.trim();
    return isCredentialGroup
      ? configuredLabel || `Key ${provider.credentialDisplayOrdinal ?? index + 1}`
      : null;
  };

  const renderProviderMonthlyPills = (
    group: (typeof visible)[number],
    provider: ProviderUsageSnapshot,
    index: number,
    quotas: ProviderQuotaWindow[],
    keyPrefix: string,
  ) => quotas
    .filter((quota) => quota.kind === "monthly")
    .map((quota) => (
      <ProviderMonthlyPill
        key={`${keyPrefix}:${providerSnapshotIdentity(provider.providerId, provider.credentialId)}:${quota.id}`}
        providerId={provider.providerId}
        providerDisplayName={group.displayName}
        credentialLabel={credentialLabel(group, provider, index)}
        quota={quota}
        showAsUsed={settings.showAsUsed}
        scale={scale}
        showResetInline={showResetInline}
        resetRelative={settings.resetTimeRelative}
      />
    ));

  const renderProviderQuotaPills = (
    group: (typeof visible)[number],
    provider: ProviderUsageSnapshot,
    index: number,
    quotas: ProviderQuotaWindow[],
    keyPrefix: string,
  ) => {
    if (provider.error != null) {
      return [renderProviderPill(group, provider, index)];
    }
    const capacityQuotas = selectCapacityQuotas(quotas);
    const hasMonthlyQuota = quotas.some((quota) => quota.kind === "monthly");
    return [
      ...(capacityQuotas.length > 0
        ? [renderProviderPill(group, provider, index, capacityQuotas, `${keyPrefix}:capacity`)]
        : !hasMonthlyQuota
          ? [renderProviderPill(group, provider, index)]
        : []),
      ...renderProviderMonthlyPills(group, provider, index, quotas, keyPrefix),
    ];
  };

  return (
    <div
      className={`floatbar floatbar--${orientation} floatbar--${style}${settings.floatBarDarkText ? " floatbar--light-bg" : ""}`}
      data-tauri-drag-region
      onMouseDown={startDrag}
      style={
        {
          opacity: opacityFraction,
          "--floatbar-scale": scale,
          "--floatbar-max-width": `${horizontalMaxWidth}px`,
        } as CSSProperties
      }
    >
      <div className="floatbar__handle" data-tauri-drag-region aria-hidden />
      {visible.length === 0 ? (
        <div className="floatbar__empty" data-tauri-drag-region>
          {t("FloatBarNoProviders")}
        </div>
      ) : (
        <>
          {visible.flatMap((group) =>
            providerGroupSections(group).map((section, sectionIndex) => {
              if (section.type === "kimi-account") {
                return (
                  <div
                    className="floatbar__kimi-group"
                    role="region"
                    aria-label={section.account.displayName}
                    key={`account:${section.account.accountId}`}
                    data-tauri-drag-region
                  >
                    <KimiMonthlyPill
                      account={section.account}
                      showAsUsed={settings.showAsUsed}
                      scale={scale}
                      showResetInline={showResetInline}
                      resetRelative={settings.resetTimeRelative}
                    />
                    {section.providers.flatMap((provider, index) =>
                      renderProviderQuotaPills(
                        group,
                        provider,
                        index,
                        selectProviderQuotaWindows(provider),
                        `kimi-account:${section.account.accountId}`,
                      ))}
                  </div>
                );
              }
              if (section.type === "kimi-unmatched") {
                return (
                  <div
                    className="floatbar__kimi-unmatched"
                    role="region"
                    aria-label={t("KimiAccountUnlinkedApiKeys")}
                    key="kimi-unmatched"
                    data-tauri-drag-region
                  >
                    {section.providers.flatMap((provider, index) =>
                      renderProviderQuotaPills(
                        group,
                        provider,
                        index,
                        selectProviderQuotaWindows(provider),
                        "kimi-unmatched",
                      ))}
                  </div>
                );
              }
              if (section.type === "provider-account") {
                const displayName = settings.hidePersonalInfo && section.displayNameSource === "accountEmail"
                  ? maskEmail(section.displayName)
                  : section.displayName;
                return (
                  <div
                    className="floatbar__provider-account-group"
                    role="region"
                    aria-label={displayName}
                    key={`provider-account:${sectionIndex}:${providerSnapshotIdentity(section.providers[0].providerId, section.providers[0].credentialId)}`}
                    data-tauri-drag-region
                  >
                    <span className="floatbar__provider-account-heading" data-tauri-drag-region>
                      {displayName}
                    </span>
                    {section.sharedMonthlyQuotas.map((quota) => (
                      <ProviderMonthlyPill
                        key={`provider-account:${sectionIndex}:shared:${quota.id}`}
                        providerId={group.providerId}
                        providerDisplayName={group.displayName}
                        credentialLabel={null}
                        quota={quota}
                        showAsUsed={settings.showAsUsed}
                        scale={scale}
                        showResetInline={showResetInline}
                        resetRelative={settings.resetTimeRelative}
                      />
                    ))}
                    {section.providers.flatMap((provider, index) =>
                      renderProviderQuotaPills(
                        group,
                        provider,
                        index,
                        selectProviderQuotaWindowsForSection(section, provider),
                        `provider-account:${sectionIndex}`,
                      ))}
                  </div>
                );
              }
              return section.providers.flatMap((provider, index) =>
                renderProviderQuotaPills(
                  group,
                  provider,
                  index,
                  selectProviderQuotaWindows(provider),
                  `providers:${sectionIndex}`,
                ));
            }),
          )}
          {visibleCosts.map((summary) => (
            <CostPill
              key={`cost:${summary.key}`}
              summary={summary}
              scale={scale}
              todayLabel={t("PanelToday")}
              thirtyDayLabel={t("FloatBarThirtyDayShort")}
            />
          ))}
        </>
      )}
    </div>
  );
}
