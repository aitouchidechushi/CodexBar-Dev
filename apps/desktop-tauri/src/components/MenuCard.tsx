import {
  Fragment,
  useCallback,
  useEffect,
  useState,
} from "react";
import type {
  DailyCostPoint,
  PaceSnapshot,
  ProviderChartData,
  ProviderLocalUsageSummary,
  ProviderUsageSnapshot,
  RateWindowSnapshot,
  KimiAccountSnapshot,
} from "../types/bridge";
import { getProviderChartData } from "../lib/tauri";
import { useLocale } from "../hooks/useLocale";
import { useFormattedResetTime } from "../hooks/useFormattedResetTime";
import { formatRelativeUpdated } from "../lib/relativeTime";
import { formatEta } from "../lib/formatEta";
import type { LocaleKey } from "../i18n/keys";
import { paceCategory } from "../surfaces/tray/paceCategory";
import { SimpleBarChart, StackedBarChart } from "./MiniBarChart";
import { providerSupportsChartData } from "../lib/providerCharts";
import { getPaceBudget } from "../lib/paceBudget";
import {
  classifyProviderSlotRateWindow,
  classifyRateWindow,
  formatMonthlyQuotaLabel,
  type ProviderQuotaWindow,
  type RateWindowKind,
} from "../lib/rateWindowKind";
import PaceDetailsChart from "./PaceDetailsChart";
import {
  providerSnapshotIdentity,
  providerGroupEntries,
  providerGroupSections,
  selectProviderQuotaWindowsForSection,
  type ProviderPresentationGroup,
  type ProviderPresentationSection,
} from "../lib/providerGroups";
import {
  KimiMonthlyQuotaProgress,
  MonthlyQuotaProgress,
} from "./KimiMonthlyQuotaProgress";

/** Small copy-to-clipboard button matching macOS CopyIconButton (doc.on.doc → checkmark). */
function CopyIconButton({ text }: { text: string }) {
  const { t } = useLocale();
  const [copied, setCopied] = useState(false);
  const handleCopy = useCallback(() => {
    navigator.clipboard.writeText(text).catch(() => {});
    setCopied(true);
    setTimeout(() => setCopied(false), 900);
  }, [text]);
  return (
    <button
      type="button"
      className="menu-card__copy-btn"
      onClick={handleCopy}
      aria-label={copied ? t("PanelCopied") : t("ActionCopyError")}
      title={copied ? t("PanelCopied") : t("ActionCopyError")}
    >
      {copied ? "✓" : (
        <svg width="12" height="12" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
          <rect x="5" y="5" width="9" height="9" rx="1.5" stroke="currentColor" strokeWidth="1.5"/>
          <path d="M11 3V2.5A1.5 1.5 0 009.5 1H2.5A1.5 1.5 0 001 2.5v7A1.5 1.5 0 002.5 11H3" stroke="currentColor" strokeWidth="1.5"/>
        </svg>
      )}
    </button>
  );
}

interface MenuCardProps {
  providerGroup: ProviderPresentationGroup;
  hideEmail: boolean;
  resetTimeRelative: boolean;
  showResetWhenExhausted?: boolean;
  showAsUsed?: boolean;
  compactMetrics?: boolean;
  groupKimiAccounts?: boolean;
  isRefreshing?: boolean;
  onLayoutChange?: () => void;
  canAddApiKey?: boolean;
  onAddApiKey?: () => void;
  onEditApiKey?: (providerId: string, credentialId: string, label: string) => void;
  onReplaceApiKey?: (providerId: string, credentialId: string, label: string) => void;
  onDeleteApiKey?: (providerId: string, credentialId: string, label: string) => void;
}

function maskEmail(email: string): string {
  const at = email.indexOf("@");
  if (at <= 1) return "••••@••••";
  return email[0] + "•".repeat(at - 1) + email.slice(at);
}

/** Localize raw provider window labels using the active locale. */
function localizeWindowLabel(
  raw: string | undefined,
  t: (key: LocaleKey) => string,
): string {
  switch (raw?.trim().toLowerCase()) {
    case "weekly":
      return t("ProviderWeeklyLabel");
    case "rate limit":
      return t("ProviderRateLimitLabel");
    case "monthly":
      return t("ProviderMonthlyLabel");
    case "usage":
      return t("ProviderUsageLabel");
    default:
      return raw ?? "";
  }
}

function displayWindowLabel(
  raw: string | undefined,
  snap: RateWindowSnapshot,
  t: (key: LocaleKey) => string,
  fallback: string,
  monthlyFallbackScope?: string,
  trustMonthlyLabel = true,
): string {
  const kind = trustMonthlyLabel
    ? classifyRateWindow(raw, snap)
    : classifyProviderSlotRateWindow(raw, snap);
  if (kind === "monthly") {
    return formatMonthlyQuotaLabel(
      raw,
      t("ProviderMonthlyLabel"),
      monthlyFallbackScope,
    );
  }
  return localizeWindowLabel(raw, t) || fallback;
}

function localizeProviderError(
  raw: string | null,
  t: (key: LocaleKey) => string,
): string | null {
  if (!raw) return null;
  if (raw.includes("MiniMax API not configured")) {
    return t("ProviderErrorMiniMaxNotConfigured");
  }
  if (raw.includes("not included in the current Token Plan")) {
    return t("ProviderErrorMiniMaxPlanUnavailable");
  }
  if (raw.startsWith("Provider not installed")) {
    return t("ProviderErrorNotInstalled");
  }
  if (raw === "Authentication required") {
    return t("ProviderErrorAuthenticationRequired");
  }
  return raw;
}

/** Format a reserve description from raw pace data at render time. */
function formatReserveDescription(
  snap: RateWindowSnapshot,
  t: (key: LocaleKey) => string,
): string | null {
  if (snap.reservePercent == null) return null;
  if (snap.reserveWillLastToReset) {
    return t("PanelReserveLastsUntilReset");
  }
  const eta = snap.reserveEtaSeconds;
  if (eta == null) return null;
  const h = Math.floor(eta / 3600);
  if (h >= 24) {
    return t("PanelReserveRunsOutInDaysHours")
      .replace("{}", String(Math.floor(h / 24)))
      .replace("{}", String(h % 24));
  }
  return t("PanelReserveRunsOutInHours").replace("{}", String(h));
}

function formatCurrency(amount: number, code: string): string {
  try {
    return new Intl.NumberFormat("en-US", {
      style: "currency",
      currency: code,
    }).format(amount);
  } catch {
    return `${code} ${amount.toFixed(2)}`;
  }
}

function formatCompactCount(value: number | null): string {
  if (value == null || value <= 0) return "—";
  return new Intl.NumberFormat("en-US", {
    notation: "compact",
    maximumFractionDigits: value >= 1_000_000 ? 1 : 0,
  }).format(value);
}

function LocalUsageBlock({
  providerId,
  summary,
  costHistory,
}: {
  providerId: string;
  summary: ProviderLocalUsageSummary;
  costHistory: DailyCostPoint[];
}) {
  const { t } = useLocale();
  const isCodex = providerId === "codex";
  const visibleHistory = costHistory
    .slice(-30)
    .filter((point) => point.value > 0);
  const maxCost = Math.max(...visibleHistory.map((point) => point.value), 0);

  return (
    <section className="menu-card__group menu-card__local-usage">
      <div className="menu-card__local-grid">
        <div>
          <span className="menu-card__local-label">{t("PanelToday")}</span>
          <strong>
            {summary.todayCost != null
              ? formatCurrency(summary.todayCost, "USD")
              : "—"}
          </strong>
        </div>
        <div>
          <span className="menu-card__local-label">{t("PanelThirtyDayCost")}</span>
          <strong>
            {summary.thirtyDayCost != null
              ? formatCurrency(summary.thirtyDayCost, "USD")
              : "—"}
          </strong>
        </div>
        <div>
          <span className="menu-card__local-label">{t("PanelThirtyDayTokens")}</span>
          <strong>{formatCompactCount(summary.thirtyDayTokens)}</strong>
        </div>
        <div>
          <span className="menu-card__local-label">{t("PanelLatestTokens")}</span>
          <strong>{formatCompactCount(summary.latestTokens)}</strong>
        </div>
      </div>

      {isCodex && visibleHistory.length > 0 && (
        <div className="menu-card__local-chart" aria-label={t("PanelThirtyDayCostHistogram")}>
          {visibleHistory.map((point, index) => (
            <span
              key={`${point.date}-${index}`}
              style={{
                height: `${Math.max(4, Math.round((point.value / maxCost) * 64))}px`,
              }}
              title={`${point.date}: ${formatCurrency(point.value, "USD")}`}
            />
          ))}
        </div>
      )}

      <div className="menu-card__local-note">
        {summary.topModel && <strong>{t("PanelTopModelPrefix")}: {summary.topModel}</strong>}
        <span>
          {summary.estimateNote === "Estimated from local logs"
            ? t("PanelEstimatedFromLocalLogs")
            : summary.estimateNote}
        </span>
      </div>
    </section>
  );
}

function WayfinderUsageBlock({
  usage,
}: {
  usage: NonNullable<ProviderUsageSnapshot["wayfinderUsage"]>;
}) {
  const { t } = useLocale();
  const formatAmount = (value: number) =>
    usage.priced ? `${value.toFixed(4)} ${usage.unit.toUpperCase()}` : "—";

  return (
    <section className="menu-card__group">
      <div className="menu-card__local-grid">
        <div>
          <span className="menu-card__local-label">{t("WayfinderGatewayStatus")}</span>
          <strong>{usage.gatewayStatus}</strong>
        </div>
        <div>
          <span className="menu-card__local-label">{t("WayfinderModels")}</span>
          <strong>{usage.modelCount}</strong>
        </div>
        <div>
          <span className="menu-card__local-label">{t("WayfinderRequests")}</span>
          <strong>{formatCompactCount(usage.requests)}</strong>
        </div>
        <div>
          <span className="menu-card__local-label">{t("WayfinderTokens")}</span>
          <strong>{formatCompactCount(usage.tokens)}</strong>
        </div>
      </div>
      <div className="menu-card__cost-line">
        {t("WayfinderSaved")}: {formatAmount(usage.saved)} ({usage.savedPercent.toFixed(1)}%)
      </div>
      {(usage.offline || usage.dryRun || usage.missingKeys.length > 0) && (
        <div className="menu-card__local-note">
          {usage.offline && <span>{t("WayfinderOffline")}</span>}
          {usage.dryRun && <span>{t("WayfinderDryRun")}</span>}
          {usage.missingKeys.length > 0 && (
            <span>{t("WayfinderMissingKeys")}: {usage.missingKeys.join(", ")}</span>
          )}
        </div>
      )}
    </section>
  );
}

function displayPlanName(
  planName: string | null,
  t: (key: LocaleKey) => string,
): string | null {
  if (!planName) return null;
  const normalized = planName.trim().toLowerCase();
  if (normalized === "default_claude_ai") return t("ProviderPlanClaudeAi");
  return planName;
}

function paceStageKey(stage: PaceSnapshot["stage"]): LocaleKey {
  switch (stage) {
    case "on_track":
      return "DetailPaceOnTrack";
    case "slightly_ahead":
      return "DetailPaceSlightlyAhead";
    case "ahead":
      return "DetailPaceAhead";
    case "far_ahead":
      return "DetailPaceFarAhead";
    case "slightly_behind":
      return "DetailPaceSlightlyBehind";
    case "behind":
      return "DetailPaceBehind";
    case "far_behind":
      return "DetailPaceFarBehind";
    default:
      return "DetailPaceOnTrack";
  }
}

type UsageLevel = "normal" | "high" | "critical" | "exhausted";

function levelOf(remainPct: number, exhausted: boolean): UsageLevel {
  if (exhausted) return "exhausted";
  if (remainPct <= 5) return "critical";
  if (remainPct <= 25) return "high";
  return "normal";
}

interface MetricEntry {
  id: string;
  label: string;
  snap: RateWindowSnapshot;
  badge?: string;
  windowKind?: RateWindowKind;
}

type MetricPaceView =
  | { kind: "budget"; budget: NonNullable<ReturnType<typeof getPaceBudget>> }
  | { kind: "reserve"; percent: number }
  | { kind: "none" };

function getMetricPaceView(
  snap: RateWindowSnapshot,
  windowKind?: RateWindowKind,
): MetricPaceView {
  if (snap.isExhausted) return { kind: "none" };

  const kind = windowKind ?? classifyRateWindow(undefined, snap);
  const budget = kind === "weekly" || kind === "monthly"
    ? getPaceBudget(snap)
    : null;
  if (budget) return { kind: "budget", budget };

  if (snap.reservePercent != null) {
    return { kind: "reserve", percent: snap.reservePercent };
  }

  return { kind: "none" };
}

/**
 * Single metric row inside the card — mirrors upstream `MetricRow`:
 *   • title (body / medium)
 *   • UsageProgressBar (capsule, 6pt)
 *   • HStack: "N% used"  ··  reset countdown (right-aligned, secondary)
 */
function MetricRow({
  title,
  snap,
  windowKind,
  exhaustedLabel,
  resetTimeRelative,
  showResetWhenExhausted,
  showAsUsed,
  expanded,
  onToggleExpanded,
  badge,
}: {
  title: string;
  snap: RateWindowSnapshot;
  windowKind?: RateWindowKind;
  exhaustedLabel: string;
  resetTimeRelative: boolean;
  showResetWhenExhausted: boolean;
  showAsUsed: boolean;
  expanded: boolean;
  onToggleExpanded: () => void;
  badge?: string;
}) {
  const { t } = useLocale();
  const isInformational = snap.isInformational === true;
  const usedPct = Number.isFinite(snap.usedPercent) ? Math.max(0, snap.usedPercent) : 0;
  const barPct = Math.min(100, usedPct);
  const remain = 100 - usedPct;
  const displayPct = showAsUsed ? usedPct : Math.max(0, remain);
  const barDisplayPct = showAsUsed ? barPct : Math.max(0, Math.min(100, remain));
  const displayLabel = showAsUsed ? t("PanelUsedSuffix") : t("PanelLeftSuffix");
  const level = levelOf(remain, snap.isExhausted);
  const resetText = useFormattedResetTime(
    snap.resetsAt,
    snap.resetDescription,
    resetTimeRelative,
  );
  const resetTarget = snap.resetsAt ? Date.parse(snap.resetsAt) : Number.NaN;
  const replacesPercent =
    showResetWhenExhausted &&
    snap.isExhausted &&
    Number.isFinite(resetTarget) &&
    resetTarget > Date.now() &&
    resetText !== null;
  const paceView = getMetricPaceView(snap, windowKind);
  const reserveDescription = formatReserveDescription(snap, t);
  const formatBudget = (value: number) =>
    value < 10 ? value.toFixed(1).replace(/\.0$/, "") : Math.round(value).toString();
  const metricKind = windowKind === "weekly" || classifyRateWindow(undefined, snap) === "weekly"
    ? " menu-metric--weekly"
    : windowKind === "short" || (snap.windowMinutes != null && snap.windowMinutes <= 5 * 60)
      ? " menu-metric--short-window"
      : windowKind === "monthly" || classifyRateWindow(undefined, snap) === "monthly"
        ? " menu-metric--monthly monthly-quota"
        : "";
  const isMonthly = metricKind.includes("menu-metric--monthly");
  return (
    <div className={`menu-metric${metricKind}`}>
      <span className={`menu-metric__title${isMonthly ? " monthly-quota-badge" : ""}`}>
        {title}
        {badge && <span className="menu-metric__badge">{badge}</span>}
      </span>
      {!isInformational && isMonthly ? (
        <MonthlyQuotaProgress
          usedPercent={snap.usedPercent}
          showAsUsed={showAsUsed}
          ariaLabel={title}
          className="menu-metric__monthly-progress"
        />
      ) : !isInformational && (
        <div className="menu-metric__bar">
          <div className="menu-metric__bar-fill" data-level={level} style={{ width: `${barDisplayPct}%` }} />
        </div>
      )}
      <div className="menu-metric__row">
        <span className="menu-metric__pct">
          {isInformational
            ? resetText ?? "—"
            : replacesPercent
              ? resetText
              : `${Math.round(displayPct)}% ${displayLabel}`}
        </span>
        {!isInformational && resetText && !replacesPercent && (
          <span className="menu-metric__reset">{resetText}</span>
        )}
      </div>
      {!isInformational && snap.isExhausted && (
        <div className="menu-metric__exhausted">{exhaustedLabel}</div>
      )}
      {!isInformational && paceView.kind === "budget" && (
        <div className="menu-metric__budget">
          <button
            type="button"
            className="menu-metric__budget-header"
            onClick={onToggleExpanded}
            aria-expanded={expanded}
          >
            <span>{t("PanelOnPaceBudget")}</span>
            {reserveDescription && <span>{reserveDescription}</span>}
          </button>
          <div className="menu-metric__budget-pills">
            {[
              [t("PanelNow"), paceView.budget.now],
              [t("PanelOneHour"), paceView.budget.nextHour],
              [t("PanelFiveHours"), paceView.budget.nextFiveHours],
              [t("PanelTodayBudget"), paceView.budget.today],
            ].map(([label, value]) => (
              <span className="menu-metric__budget-pill" key={String(label)}>
                {label} {formatBudget(Number(value))}%
              </span>
            ))}
          </div>
          {expanded && <PaceDetailsChart snap={snap} t={t} />}
        </div>
      )}
      {!isInformational && paceView.kind === "reserve" && (
        <div className="menu-metric__row menu-metric__reserve">
          <span className="menu-metric__pct">{Math.round(paceView.percent)}% {t("PanelReserveSuffix")}</span>
          {reserveDescription && (
            <span className="menu-metric__reset">{reserveDescription}</span>
          )}
        </div>
      )}
    </div>
  );
}

function CredentialQuotaCard({
  provider,
  quotaWindows,
  resetTimeRelative,
  showResetWhenExhausted,
  showAsUsed,
  compactMetrics,
  onLayoutChange,
  onEditApiKey,
  onReplaceApiKey,
  onDeleteApiKey,
}: {
  provider: ProviderUsageSnapshot;
  quotaWindows?: ProviderQuotaWindow[];
  resetTimeRelative: boolean;
  showResetWhenExhausted: boolean;
  showAsUsed: boolean;
  compactMetrics: boolean;
  onLayoutChange?: () => void;
  onEditApiKey?: (providerId: string, credentialId: string, label: string) => void;
  onReplaceApiKey?: (providerId: string, credentialId: string, label: string) => void;
  onDeleteApiKey?: (providerId: string, credentialId: string, label: string) => void;
}) {
  const { t } = useLocale();
  const credentialId = provider.credentialId;
  if (!credentialId) return null;
  const label = provider.credentialDisplayLabel ?? t("ApiKeyTitle");
  const action = (callback?: (providerId: string, credentialId: string, label: string) => void) => (
    event: React.MouseEvent<HTMLButtonElement>,
  ) => {
    event.stopPropagation();
    callback?.(provider.providerId, credentialId, label);
  };
  const stopPointer = (event: React.PointerEvent<HTMLButtonElement>) => event.stopPropagation();

  return (
    <article
      className="menu-card__credential-card"
      role="region"
      aria-label={label}
    >
      <div className="menu-card__credential-card-header">
        <h3 className="menu-card__credential-heading">{label}</h3>
        {(onEditApiKey || onReplaceApiKey || onDeleteApiKey) && (
          <div className="menu-card__credential-actions">
            {onEditApiKey && (
              <button type="button" className="credential-btn" onPointerDown={stopPointer} onClick={action(onEditApiKey)}>
                {t("ApiKeyEditLabel")}
              </button>
            )}
            {onReplaceApiKey && (
              <button type="button" className="credential-btn" onPointerDown={stopPointer} onClick={action(onReplaceApiKey)}>
                {t("ApiKeyReplaceSecret")}
              </button>
            )}
            {onDeleteApiKey && (
              <button type="button" className="credential-btn credential-btn--danger" onPointerDown={stopPointer} onClick={action(onDeleteApiKey)}>
                {t("ApiKeyDelete")}
              </button>
            )}
          </div>
        )}
      </div>
      {provider.error ? (
        <div className="menu-card__credential-error">
          {t("ProviderStatusError")}
        </div>
      ) : (
        <ProviderQuotaBlock
          provider={provider}
          quotaWindows={quotaWindows}
          resetTimeRelative={resetTimeRelative}
          showResetWhenExhausted={showResetWhenExhausted}
          showAsUsed={showAsUsed}
          compactMetrics={compactMetrics}
          onLayoutChange={onLayoutChange}
        />
      )}
    </article>
  );
}

function ProviderQuotaBlock({
  provider,
  quotaWindows,
  credentialLabel,
  resetTimeRelative,
  showResetWhenExhausted,
  showAsUsed,
  compactMetrics,
  onLayoutChange,
}: {
  provider: ProviderUsageSnapshot;
  quotaWindows?: ProviderQuotaWindow[];
  credentialLabel?: string;
  resetTimeRelative: boolean;
  showResetWhenExhausted: boolean;
  showAsUsed: boolean;
  compactMetrics: boolean;
  onLayoutChange?: () => void;
}) {
  const { t } = useLocale();
  const [expandedPaceWindow, setExpandedPaceWindow] = useState<string | null>(null);
  const formattedCostReset = useFormattedResetTime(
    provider.cost?.resetsAt ?? null,
    null,
    resetTimeRelative,
  );
  const isWayfinder = provider.providerId === "wayfinder";
  const hasUnlimitedWeek = (provider.extraRateWindows ?? []).some(
    (extra) => extra.id === "minimax-weekly-unlimited",
  );
  const metrics: MetricEntry[] = isWayfinder
    ? []
    : [
        {
          id: "primary",
          label: displayWindowLabel(
            provider.primaryLabel,
            provider.primary,
            t,
            t("DetailWindowPrimary"),
            undefined,
            false,
          ),
          snap: provider.primary,
          badge: hasUnlimitedWeek ? t("ProviderUnlimitedWeekly") : undefined,
          windowKind: classifyProviderSlotRateWindow(
            provider.primaryLabel,
            provider.primary,
          ),
        },
      ];
  if (provider.secondary) {
    metrics.push({
      id: "secondary",
      label: displayWindowLabel(
        provider.secondaryLabel,
        provider.secondary,
        t,
        t("DetailWindowSecondary"),
        undefined,
        false,
      ),
      snap: provider.secondary,
      windowKind: classifyProviderSlotRateWindow(
        provider.secondaryLabel,
        provider.secondary,
      ),
    });
  }
  if (provider.modelSpecific) {
    metrics.push({
      id: "model-specific",
      label: displayWindowLabel(
        undefined,
        provider.modelSpecific,
        t,
        t("DetailWindowModelSpecific"),
        t("DetailWindowModelSpecific"),
      ),
      snap: provider.modelSpecific,
      windowKind: classifyRateWindow(undefined, provider.modelSpecific),
    });
  }
  if (provider.tertiary) {
    metrics.push({
      id: "tertiary",
      label: displayWindowLabel(
        undefined,
        provider.tertiary,
        t,
        t("DetailWindowTertiary"),
      ),
      snap: provider.tertiary,
      windowKind: classifyRateWindow(undefined, provider.tertiary),
    });
  }
  for (const extra of provider.extraRateWindows ?? []) {
    if (extra.id === "minimax-weekly-unlimited") continue;
    metrics.push({
      id: `extra-${extra.id}`,
      label: displayWindowLabel(
        extra.title,
        extra.window,
        t,
        extra.title,
      ),
      snap: extra.window,
      windowKind: classifyRateWindow(extra.title, extra.window),
    });
  }
  const selectedCostMonthly = quotaWindows?.find(
    (quota) => quota.id === "cost-monthly" && quota.source === "cost",
  );
  if (selectedCostMonthly) {
    metrics.push({
      id: selectedCostMonthly.id,
      label: formatMonthlyQuotaLabel(
        selectedCostMonthly.label,
        t("ProviderMonthlyLabel"),
      ),
      snap: selectedCostMonthly.snapshot,
      windowKind: "monthly",
    });
  }
  const selectedMetrics = quotaWindows
    ? metrics.filter((metric) => quotaWindows.some((quota) => quota.id === metric.id))
    : metrics;
  const visibleMetrics = compactMetrics
    ? selectedMetrics.filter((metric, index) => index < 2 || metric.windowKind === "monthly")
    : selectedMetrics;
  const hasMetrics = visibleMetrics.length > 0;
  const hasCost = provider.cost != null;
  const hasPace = provider.pace != null;
  const wayfinderUsage = isWayfinder ? provider.wayfinderUsage : null;

  return (
    <section
      className={`menu-card__quota-block${credentialLabel ? " menu-card__quota-block--credential" : ""}`}
      role={credentialLabel ? "region" : undefined}
      aria-label={credentialLabel}
    >
      {credentialLabel && (
        <h3 className="menu-card__credential-heading">{credentialLabel}</h3>
      )}
      {provider.refreshError && (
        <div className="menu-card__refresh-warning" role="status">
          {t("ProviderStatusStale")}: {provider.refreshError}
        </div>
      )}
      {hasMetrics && (
        <section className="menu-card__group menu-card__metrics">
          {visibleMetrics.map((metric) => (
            <MetricRow
              key={metric.id}
              title={metric.label}
              snap={metric.snap}
              windowKind={metric.windowKind}
              exhaustedLabel={t("DetailWindowExhausted")}
              resetTimeRelative={resetTimeRelative}
              showResetWhenExhausted={showResetWhenExhausted}
              showAsUsed={showAsUsed}
              expanded={expandedPaceWindow === metric.id}
              onToggleExpanded={() => {
                setExpandedPaceWindow((current) =>
                  current === metric.id ? null : metric.id,
                );
                requestAnimationFrame(() => onLayoutChange?.());
              }}
              badge={metric.badge}
            />
          ))}
        </section>
      )}

      {wayfinderUsage && <WayfinderUsageBlock usage={wayfinderUsage} />}

      {hasMetrics && hasCost && <div className="menu-card__divider" />}

      {provider.cost && (
        <section className="menu-card__group menu-card__cost">
          <div className="menu-card__group-title">
            {t("DetailCostTitle")} — {provider.cost.period}
          </div>
          <div className="menu-card__cost-line">
            {t("DetailCostUsed")}: {" "}
            {provider.cost.formattedUsed ||
              formatCurrency(provider.cost.used, provider.cost.currencyCode)}
            {provider.cost.limit != null && (
              <>
                {" / "}
                {provider.cost.formattedLimit ||
                  formatCurrency(provider.cost.limit, provider.cost.currencyCode)}
              </>
            )}
          </div>
          {provider.cost.remaining != null && (
            <div className="menu-card__cost-line menu-card__cost-line--muted">
              {t("DetailCostRemaining")}: {" "}
              {formatCurrency(provider.cost.remaining, provider.cost.currencyCode)}
            </div>
          )}
          {formattedCostReset && (
            <div className="menu-card__cost-line menu-card__cost-line--muted">
              {t("DetailCostResets")}: {formattedCostReset}
            </div>
          )}
        </section>
      )}

      {(hasMetrics || hasCost) && hasPace && <div className="menu-card__divider" />}

      {provider.pace && (
        <section className="menu-card__group menu-card__pace">
          <div className="menu-card__pace-header">
            <span className="menu-card__group-title">{t("DetailPaceTitle")}</span>
            <span
              className="menu-card__pace-label"
              data-pace={paceCategory(provider.pace.stage)}
            >
              {t(paceStageKey(provider.pace.stage))} ({provider.pace.deltaPercent >= 0 ? "+" : ""}
              {provider.pace.deltaPercent.toFixed(1)}%)
            </span>
          </div>
          <div className="menu-card__pace-bars">
            <div className="menu-card__pace-track" title={t("PanelExpected")}>
              <div
                className="menu-card__pace-fill menu-card__pace-fill--expected"
                style={{ width: `${provider.pace.expectedUsedPercent.toFixed(1)}%` }}
              />
            </div>
            <div className="menu-card__pace-track" title={t("PanelActual")}>
              <div
                className="menu-card__pace-fill"
                data-pace={paceCategory(provider.pace.stage)}
                style={{ width: `${provider.pace.actualUsedPercent.toFixed(1)}%` }}
              />
            </div>
          </div>
          {provider.pace.etaSeconds != null && !provider.pace.willLastToReset && (
            <div className="menu-card__pace-eta">
              ⚠ {t("DetailPaceRunsOutIn")} {formatEta(provider.pace.etaSeconds)}
            </div>
          )}
          {provider.pace.willLastToReset && (
            <div className="menu-card__pace-ok">
              ✓ {t("DetailPaceWillLastToReset")}
            </div>
          )}
        </section>
      )}
    </section>
  );
}

function providerAccountDisplayName(
  section: Extract<ProviderPresentationSection, { type: "provider-account" }>,
  hideEmail: boolean,
): string {
  return hideEmail && section.displayNameSource === "accountEmail"
    ? maskEmail(section.displayName)
    : section.displayName;
}

function ProviderAccountSharedMonthly({
  quota,
  resetTimeRelative,
  showResetWhenExhausted,
  showAsUsed,
}: {
  quota: ProviderQuotaWindow;
  resetTimeRelative: boolean;
  showResetWhenExhausted: boolean;
  showAsUsed: boolean;
}) {
  const { t } = useLocale();
  const label = formatMonthlyQuotaLabel(
    quota.label,
    t("ProviderMonthlyLabel"),
    quota.source === "modelSpecific" ? t("DetailWindowModelSpecific") : undefined,
  );
  return (
    <MetricRow
      title={label}
      snap={quota.snapshot}
      windowKind="monthly"
      exhaustedLabel={t("DetailWindowExhausted")}
      resetTimeRelative={resetTimeRelative}
      showResetWhenExhausted={showResetWhenExhausted}
      showAsUsed={showAsUsed}
      expanded={false}
      onToggleExpanded={() => {}}
    />
  );
}

function KimiAccountQuotaBlock({
  account,
  resetTimeRelative,
  showAsUsed,
  showGroupedDetails = false,
}: {
  account: KimiAccountSnapshot;
  resetTimeRelative: boolean;
  showAsUsed: boolean;
  showGroupedDetails?: boolean;
}) {
  const { t } = useLocale();
  const reset = useFormattedResetTime(account.resetsAt, null, resetTimeRelative);
  const used = account.usedPercent == null ? null : Math.max(0, account.usedPercent);
  const percent = used == null ? null : showAsUsed ? used : Math.max(0, 100 - used);
  const monthlyLabel = t("KimiAccountMonthlyQuota");
  return (
    <div
      className="menu-card__kimi-account"
      role={showGroupedDetails ? undefined : "region"}
      aria-label={showGroupedDetails ? undefined : account.displayName}
    >
      <div className="menu-card__kimi-account-heading">
        <strong>{account.displayName}</strong>
        {percent != null ? (
          <span className="menu-card__kimi-account-usage">
            <span className="menu-card__kimi-monthly-badge">{monthlyLabel}</span>
            {Math.round(percent)}% {showAsUsed ? t("PanelUsedSuffix") : t("PanelLeftSuffix")}
          </span>
        ) : (
          <span>{t("KimiAccountLoginRequired")}</span>
        )}
      </div>
      {showGroupedDetails && account.sourceLabels && account.sourceLabels.length > 0 && (
        <div className="menu-card__kimi-account-source">{account.sourceLabels.join(" · ")}</div>
      )}
      {showGroupedDetails && (
        <KimiMonthlyQuotaProgress
          account={account}
          showAsUsed={showAsUsed}
          ariaLabel={`${account.displayName} ${monthlyLabel}`}
        />
      )}
      {reset && <div className="menu-card__kimi-account-reset">{t("KimiAccountResetTime")}: {reset}</div>}
    </div>
  );
}

/**
 * Provider card — direct mirror of SwiftUI `UsageMenuCardView`.
 *
 * Layout (top to bottom):
 *   1. Header VStack(spacing: 3)
 *        – HStack: providerName (headline/semibold)  ··  email (subheadline/secondary, right)
 *        – HStack: subtitle "source · updated"        ··  plan (footnote/secondary, right)
 *   2. Divider (1pt)
 *   3. VStack(spacing: 12)
 *        – Metrics group VStack(spacing: 12) of MetricRow
 *        – (Divider) Cost group: title (body/medium) + session line + month line (footnote)
 *        – (Divider) Pace group (Tauri-only addition; placed last)
 *        – (Divider) Charts group (Tauri-only addition; placed last)
 *
 * Padding: upstream v0.32.2 uses wider horizontal card padding and slightly
 * taller header/content vertical padding so account/plan rows can breathe.
 */
export default function MenuCard({
  providerGroup,
  hideEmail,
  resetTimeRelative,
  showResetWhenExhausted = false,
  showAsUsed = false,
  compactMetrics = false,
  groupKimiAccounts = false,
  isRefreshing = false,
  onLayoutChange,
  canAddApiKey = false,
  onAddApiKey,
  onEditApiKey,
  onReplaceApiKey,
  onDeleteApiKey,
}: MenuCardProps) {
  const { t } = useLocale();
  const [chartData, setChartData] = useState<ProviderChartData | null>(null);
  const hasCredentialSnapshots = providerGroup.credentialCount > 0;
  const successfulProviders = providerGroup.successfulSnapshots;
  const headerProvider = successfulProviders[0] ?? providerGroup.snapshots[0] ?? null;
  const headerUpdatedAt = headerProvider?.updatedAt
    ?? providerGroup.kimiAccounts?.[0]?.updatedAt
    ?? "";
  const failedSummary = t("ApiKeyFailureCount")
    .replace("{}", String(providerGroup.failedCredentialCount))
    .replace("{}", String(providerGroup.credentialCount));
  const failedLabels = providerGroup.snapshots
    .filter((snapshot) => snapshot.credentialId && snapshot.error)
    .map((snapshot) => snapshot.credentialDisplayLabel)
    .filter((label): label is string => Boolean(label));
  const safeAllFailedMessage = failedLabels.length > 0
    ? t("ApiKeyFailureDetails")
        .replace("{}", failedSummary)
        .replace("{}", failedLabels.join(", "))
    : failedSummary;

  useEffect(() => {
    if (!providerSupportsChartData(providerGroup.providerId)) {
      setChartData(null);
      return;
    }
    let cancelled = false;
    setChartData(null);
    getProviderChartData(
      providerGroup.providerId,
      hasCredentialSnapshots ? undefined : headerProvider?.accountEmail ?? undefined,
    )
      .then((data) => {
        if (!cancelled) {
          setChartData(data);
          requestAnimationFrame(() => onLayoutChange?.());
        }
      })
      .catch(() => {
        /* chart data is best-effort */
      });
    return () => {
      cancelled = true;
    };
  }, [
    hasCredentialSnapshots,
    headerProvider?.accountEmail,
    onLayoutChange,
    providerGroup.providerId,
  ]);

  const isWayfinder = providerGroup.providerId === "wayfinder";
  const email = !hasCredentialSnapshots && !isWayfinder && headerProvider?.accountEmail
    ? hideEmail
      ? maskEmail(headerProvider.accountEmail)
      : headerProvider.accountEmail
    : null;
  const planName = !hasCredentialSnapshots && !isWayfinder
    ? displayPlanName(headerProvider?.planName ?? null, t)
    : null;
  const friendlyError = localizeProviderError(headerProvider?.error ?? null, t);

  const hasCostHistory =
    chartData !== null && chartData.costHistory.some((point) => point.value > 0);
  const hasCreditsHistory =
    chartData !== null && chartData.creditsHistory.length > 0;
  const hasUsageBreakdown =
    chartData !== null && chartData.usageBreakdown.length > 0;
  const hasCharts = hasCostHistory || hasCreditsHistory || hasUsageBreakdown;
  const localUsage = providerGroup.isAllFailed ? null : chartData?.localUsage ?? null;
  const localCostHistory = chartData?.costHistory ?? [];
  const renderedProviders = hasCredentialSnapshots
    ? providerGroup.snapshots
    : successfulProviders;
  const renderedEntries = providerGroupEntries(providerGroup, renderedProviders);
  const renderedSections = providerGroup.providerId !== "kimi" || groupKimiAccounts
    ? providerGroupSections(providerGroup, renderedProviders)
    : null;
  const hasQuotaDetails = renderedProviders.length > 0;
  const hasAccountDetails = (providerGroup.kimiAccounts?.length ?? 0) > 0;
  const hasDetails =
    (hasCredentialSnapshots && hasQuotaDetails) ||
    (!providerGroup.isAllFailed
      && (hasQuotaDetails || hasAccountDetails || hasCharts || !!localUsage));
  const cardClassName = [
    "menu-card",
    providerGroup.isAllFailed ? "menu-card--error" : null,
    isRefreshing ? "menu-card--refreshing" : null,
    hasDetails ? "menu-card--with-details" : "menu-card--header-only",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <article className={cardClassName} aria-busy={isRefreshing}>
      <header className="menu-card__header">
        <div className="menu-card__title-row">
          <div className="menu-card__name-group">
            <span className="menu-card__name">{providerGroup.displayName}</span>
            {!providerGroup.isAllFailed && email && <span className="menu-card__email">{email}</span>}
          </div>
          {canAddApiKey && onAddApiKey && (
            <button
              type="button"
              className="menu-card__add-api-key"
              onClick={(event) => {
                event.stopPropagation();
                onAddApiKey();
              }}
            >
              {t("ApiKeyAddCard")}
            </button>
          )}
        </div>
        {providerGroup.isAllFailed ? (
          <div
            className="menu-card__error-block"
            role={hasCredentialSnapshots ? "alert" : undefined}
          >
            <div className="menu-card__error-text">
              {hasCredentialSnapshots ? safeAllFailedMessage : friendlyError}
            </div>
            {!hasCredentialSnapshots && headerProvider?.error && (
              <CopyIconButton text={headerProvider.error} />
            )}
          </div>
        ) : (
          <div className="menu-card__subtitle-row">
            {headerUpdatedAt && (
              <span className="menu-card__subtitle">
                {Number.isNaN(Date.parse(headerUpdatedAt))
                  ? headerUpdatedAt
                  : formatRelativeUpdated(Date.parse(headerUpdatedAt), t)}
              </span>
            )}
            {planName && (
              <span className="menu-card__plan-badge">{planName}</span>
            )}
          </div>
        )}
      </header>

      {hasDetails && <div className="menu-card__divider" />}

      {hasDetails && (
        <div className="menu-card__content">
          {providerGroup.isPartialFailure && (
            <div className="menu-card__failed-count" role="status" aria-live="polite">
              {failedSummary}
            </div>
          )}

          {renderedSections ? renderedSections.map((section, sectionIndex) => {
            if (section.type === "provider-account") {
              const displayName = providerAccountDisplayName(section, hideEmail);
              return (
                <section
                  className="menu-card__provider-account-group"
                  aria-label={displayName}
                  key={`provider-account:${sectionIndex}:${providerSnapshotIdentity(section.providers[0].providerId, section.providers[0].credentialId)}`}
                  role="region"
                >
                  <div className="menu-card__provider-account-heading">
                    <strong>{displayName}</strong>
                  </div>
                  {section.sharedMonthlyQuotas.map((quota) => (
                    <ProviderAccountSharedMonthly
                      key={quota.id}
                      quota={quota}
                      resetTimeRelative={resetTimeRelative}
                      showResetWhenExhausted={showResetWhenExhausted}
                      showAsUsed={showAsUsed}
                    />
                  ))}
                  {section.providers.map((provider) => provider.credentialId ? (
                    <CredentialQuotaCard
                      key={providerSnapshotIdentity(provider.providerId, provider.credentialId)}
                      provider={provider}
                      quotaWindows={selectProviderQuotaWindowsForSection(section, provider)}
                      resetTimeRelative={resetTimeRelative}
                      showResetWhenExhausted={showResetWhenExhausted}
                      showAsUsed={showAsUsed}
                      compactMetrics={compactMetrics}
                      onLayoutChange={onLayoutChange}
                      onEditApiKey={onEditApiKey}
                      onReplaceApiKey={onReplaceApiKey}
                      onDeleteApiKey={onDeleteApiKey}
                    />
                  ) : (
                    <ProviderQuotaBlock
                      key={providerSnapshotIdentity(provider.providerId, provider.credentialId)}
                      provider={provider}
                      quotaWindows={selectProviderQuotaWindowsForSection(section, provider)}
                      resetTimeRelative={resetTimeRelative}
                      showResetWhenExhausted={showResetWhenExhausted}
                      showAsUsed={showAsUsed}
                      compactMetrics={compactMetrics}
                      onLayoutChange={onLayoutChange}
                    />
                  ))}
                </section>
              );
            }
            if (section.type === "kimi-account") {
              return (
                <section
                  className="menu-card__kimi-account-group"
                  aria-label={section.account.displayName}
                  key={`account:${section.account.accountId}`}
                >
                  <KimiAccountQuotaBlock
                    account={section.account}
                    resetTimeRelative={resetTimeRelative}
                    showAsUsed={showAsUsed}
                    showGroupedDetails
                  />
                  {section.providers.length > 0 && (
                    <div className="menu-card__kimi-key-heading">{t("KimiAccountLinkedApiKeys")}</div>
                  )}
                  {section.providers.map((provider) => provider.credentialId ? (
                    <CredentialQuotaCard
                      key={providerSnapshotIdentity(provider.providerId, provider.credentialId)}
                      provider={provider}
                      resetTimeRelative={resetTimeRelative}
                      showResetWhenExhausted={showResetWhenExhausted}
                      showAsUsed={showAsUsed}
                      compactMetrics={compactMetrics}
                      onLayoutChange={onLayoutChange}
                      onEditApiKey={onEditApiKey}
                      onReplaceApiKey={onReplaceApiKey}
                      onDeleteApiKey={onDeleteApiKey}
                    />
                  ) : (
                    <ProviderQuotaBlock
                      key={providerSnapshotIdentity(provider.providerId, provider.credentialId)}
                      provider={provider}
                      resetTimeRelative={resetTimeRelative}
                      showResetWhenExhausted={showResetWhenExhausted}
                      showAsUsed={showAsUsed}
                      compactMetrics={compactMetrics}
                      onLayoutChange={onLayoutChange}
                    />
                  ))}
                </section>
              );
            }
            if (section.type === "kimi-unmatched") {
              return (
                <section
                  className="menu-card__kimi-unmatched-group"
                  aria-label={t("KimiAccountUnlinkedApiKeys")}
                  key="kimi-unmatched"
                >
                  <div className="menu-card__kimi-key-heading">{t("KimiAccountUnlinkedApiKeys")}</div>
                  {section.providers.map((provider) => provider.credentialId ? (
                    <CredentialQuotaCard
                      key={providerSnapshotIdentity(provider.providerId, provider.credentialId)}
                      provider={provider}
                      resetTimeRelative={resetTimeRelative}
                      showResetWhenExhausted={showResetWhenExhausted}
                      showAsUsed={showAsUsed}
                      compactMetrics={compactMetrics}
                      onLayoutChange={onLayoutChange}
                      onEditApiKey={onEditApiKey}
                      onReplaceApiKey={onReplaceApiKey}
                      onDeleteApiKey={onDeleteApiKey}
                    />
                  ) : null)}
                </section>
              );
            }
            return section.providers.map((provider) => provider.credentialId ? (
              <CredentialQuotaCard
                key={providerSnapshotIdentity(provider.providerId, provider.credentialId)}
                provider={provider}
                resetTimeRelative={resetTimeRelative}
                showResetWhenExhausted={showResetWhenExhausted}
                showAsUsed={showAsUsed}
                compactMetrics={compactMetrics}
                onLayoutChange={onLayoutChange}
                onEditApiKey={onEditApiKey}
                onReplaceApiKey={onReplaceApiKey}
                onDeleteApiKey={onDeleteApiKey}
              />
            ) : (
              <ProviderQuotaBlock
                key={providerSnapshotIdentity(provider.providerId, provider.credentialId)}
                provider={provider}
                resetTimeRelative={resetTimeRelative}
                showResetWhenExhausted={showResetWhenExhausted}
                showAsUsed={showAsUsed}
                compactMetrics={compactMetrics}
                onLayoutChange={onLayoutChange}
              />
            ));
          }) : renderedEntries.map((entry, index) => (
            <Fragment key={entry.type === "account" ? `account:${entry.account.accountId}` : providerSnapshotIdentity(entry.provider.providerId, entry.provider.credentialId)}>
              {index > 0 && <div className="menu-card__divider menu-card__credential-divider" />}
              {entry.type === "account" ? (
                <KimiAccountQuotaBlock
                  account={entry.account}
                  resetTimeRelative={resetTimeRelative}
                  showAsUsed={showAsUsed}
                />
              ) : entry.provider.credentialId ? (
                <CredentialQuotaCard
                  provider={entry.provider}
                  resetTimeRelative={resetTimeRelative}
                  showResetWhenExhausted={showResetWhenExhausted}
                  showAsUsed={showAsUsed}
                  compactMetrics={compactMetrics}
                  onLayoutChange={onLayoutChange}
                  onEditApiKey={onEditApiKey}
                  onReplaceApiKey={onReplaceApiKey}
                  onDeleteApiKey={onDeleteApiKey}
                />
              ) : (
                <ProviderQuotaBlock
                  provider={entry.provider}
                  resetTimeRelative={resetTimeRelative}
                  showResetWhenExhausted={showResetWhenExhausted}
                  showAsUsed={showAsUsed}
                  compactMetrics={compactMetrics}
                  onLayoutChange={onLayoutChange}
                />
              )}
            </Fragment>
          ))}

          {localUsage && (
            <LocalUsageBlock
              providerId={providerGroup.providerId}
              summary={localUsage}
              costHistory={localCostHistory}
            />
          )}

          {(successfulProviders.length > 0 || localUsage) && hasCharts && (
            <div className="menu-card__divider" />
          )}

          {hasCharts && (
            <section className="menu-card__group menu-card__charts">
              {hasCostHistory && (
                <SimpleBarChart
                  points={chartData!.costHistory}
                  label={t("DetailChartCost")}
                  color="var(--accent)"
                  formatValue={(v) => `$${v.toFixed(2)}`}
                  t={t}
                />
              )}
              {hasCreditsHistory && (
                <SimpleBarChart
                  points={chartData!.creditsHistory}
                  label={t("DetailChartCredits")}
                  color="var(--provider-status-ok)"
                  formatValue={(v) => v.toFixed(1)}
                  t={t}
                />
              )}
              {hasUsageBreakdown && (
                <StackedBarChart
                  points={chartData!.usageBreakdown}
                  label={t("DetailChartUsageBreakdown")}
                  height={56}
                  t={t}
                />
              )}
            </section>
          )}
        </div>
      )}
    </article>
  );
}
