import type {
  ProviderUsageSnapshot,
  RateWindowSnapshot,
} from "../types/bridge";

const SHORT_WINDOW_MINUTES = 5 * 60;
const WEEKLY_WINDOW_MINUTES = 7 * 24 * 60;
const MONTHLY_WINDOW_MINUTES_MIN = 28 * 24 * 60;
const MONTHLY_WINDOW_MINUTES_MAX = 31 * 24 * 60;

export type RateWindowKind = "weekly" | "short" | "monthly" | "ordinary";

export interface ProviderQuotaWindow {
  id: string;
  kind: RateWindowKind;
  label?: string;
  snapshot: RateWindowSnapshot;
  source: "primary" | "secondary" | "modelSpecific" | "tertiary" | "extra" | "cost";
}

const MONTHLY_LABEL_PATTERN =
  /\bmonthly\b|\bmonth\b|\bper\s+month\b|\b(?:28|29|30|31)\s*[- ]?\s*days?\b|月额度|月額度|月度|每月|自然月/i;
const QUOTA_LABEL_PATTERN =
  /\bquota\b|\blimit\b|\ballowance\b|额度|額度|限额|限額/i;
const STATISTIC_LABEL_PATTERN =
  /\bcosts?\b|\bspend(?:ing)?\b|\btokens?\b|\brequests?\b|\bactivit(?:y|ies)\b|\bhistor(?:y|ies)\b|\bstatistics?\b|\bstats?\b|\blast\s+(?:28|29|30|31)\s+days\b|成本|消费|消費|令牌|请求|請求|统计|統計|历史|歷史/i;
const NON_QUOTA_LABEL_PATTERN =
  /\bbalance\b|\bbudget\b|余额|餘額|预算|預算/i;
const MONTHLY_COST_PERIOD_PATTERN =
  /^(?:monthly|month|per\s+month|monthly\s+(?:quota|limit|allowance)|(?:quota|limit|allowance)\s+per\s+month|月额度|月額度|月度|每月|自然月)$/i;

function isNonQuotaLabel(label: string): boolean {
  return NON_QUOTA_LABEL_PATTERN.test(label) || STATISTIC_LABEL_PATTERN.test(label);
}

function hasUnknownCycleQuotaSignal(label: string): boolean {
  return QUOTA_LABEL_PATTERN.test(label) || MONTHLY_LABEL_PATTERN.test(label);
}

function monthlyCostWindow(provider: ProviderUsageSnapshot): RateWindowSnapshot | null {
  const cost = provider.cost;
  const limit = cost?.limit;
  if (
    !cost
    || limit == null
    || !Number.isFinite(limit)
    || limit <= 0
    || !Number.isFinite(cost.used)
    || !MONTHLY_COST_PERIOD_PATTERN.test(cost.period.trim())
    || provider.providerId === "bedrock"
  ) {
    return null;
  }

  const usedPercent = Math.min(100, Math.max(0, (cost.used / limit) * 100));
  return {
    usedPercent,
    remainingPercent: 100 - usedPercent,
    windowMinutes: 30 * 24 * 60,
    resetsAt: cost.resetsAt,
    resetDescription: null,
    isExhausted: usedPercent >= 100,
    isInformational: false,
    reservePercent: null,
    reserveDescription: null,
  };
}

export function classifyRateWindow(
  rawLabel: string | undefined,
  snapshot: RateWindowSnapshot,
): RateWindowKind | undefined {
  const label = rawLabel?.trim().toLowerCase() ?? "";
  const minutes = snapshot.windowMinutes;

  if (snapshot.isInformational || isNonQuotaLabel(label)) return undefined;

  if (
    ((minutes != null &&
      minutes >= MONTHLY_WINDOW_MINUTES_MIN &&
      minutes <= MONTHLY_WINDOW_MINUTES_MAX) ||
      MONTHLY_LABEL_PATTERN.test(label))
  ) {
    return "monthly";
  }

  if (minutes != null) {
    if (minutes === WEEKLY_WINDOW_MINUTES) return "weekly";
    if (minutes <= SHORT_WINDOW_MINUTES) return "short";
  }

  if (label === "weekly" || label === "weekly quota") return "weekly";
  if (
    label === "rate limit" ||
    /\bfive[\s-]*hours?\b/.test(label) ||
    /\b5[\s-]*hours?\b/.test(label)
  ) {
    return "short";
  }
  return undefined;
}

/** Static provider slot labels may recover short/weekly kinds, but never prove a month. */
export function classifyProviderSlotRateWindow(
  rawLabel: string | undefined,
  snapshot: RateWindowSnapshot,
): RateWindowKind | undefined {
  const label = rawLabel?.trim().toLowerCase() ?? "";
  if (snapshot.isInformational || isNonQuotaLabel(label)) return undefined;

  const measuredKind = classifyRateWindow(undefined, snapshot);
  if (measuredKind) return measuredKind;
  const labelKind = classifyRateWindow(rawLabel, snapshot);
  if (labelKind === "short" || labelKind === "weekly") return labelKind;
  return hasUnknownCycleQuotaSignal(label) ? "ordinary" : undefined;
}

export function formatMonthlyQuotaLabel(
  rawLabel: string | undefined,
  monthlyLabel: string,
  fallbackScope?: string,
): string {
  const scope = (rawLabel ?? "")
    .replace(/\b(?:28|29|30|31)\s*[- ]?\s*days?\b/gi, " ")
    .replace(/\b(?:monthly|month|quota|limit|allowance|usage)\b/gi, " ")
    .replace(/月额度|月額度|月度|每月|自然月|额度|額度|限额|限額/g, " ")
    .replace(/\s+/g, " ")
    .replace(/^[\s·:—–\-_/()]+|[\s·:—–\-_/()]+$/g, "")
    .trim();
  const normalizedScope = /^(?:account|total|overall|all models)$/i.test(scope)
    ? ""
    : scope;
  const displayScope = normalizedScope || fallbackScope?.trim() || "";
  return displayScope ? `${monthlyLabel} · ${displayScope}` : monthlyLabel;
}

export function selectProviderQuotaWindows(
  provider: ProviderUsageSnapshot,
): ProviderQuotaWindow[] {
  const candidates: Array<{
    id: string;
    label?: string;
    snapshot: RateWindowSnapshot | null;
    source: ProviderQuotaWindow["source"];
  }> = [
    {
      id: "primary",
      label: provider.primaryLabel,
      snapshot: provider.primary,
      source: "primary",
    },
    {
      id: "secondary",
      label: provider.secondaryLabel,
      snapshot: provider.secondary,
      source: "secondary",
    },
    {
      id: "model-specific",
      snapshot: provider.modelSpecific,
      source: "modelSpecific",
    },
    {
      id: "tertiary",
      snapshot: provider.tertiary,
      source: "tertiary",
    },
    ...(provider.extraRateWindows ?? []).map((extra) => ({
      id: `extra-${extra.id}`,
      label: extra.title,
      snapshot: extra.window,
      source: "extra" as const,
    })),
    {
      id: "cost-monthly",
      label: provider.cost?.period,
      snapshot: monthlyCostWindow(provider),
      source: "cost",
    },
  ];
  const classified = candidates.flatMap((candidate) => {
    if (!candidate.snapshot || !Number.isFinite(candidate.snapshot.usedPercent)) return [];
    const kind = candidate.source === "primary" || candidate.source === "secondary"
      ? classifyProviderSlotRateWindow(candidate.label, candidate.snapshot)
      : classifyRateWindow(candidate.label, candidate.snapshot)
        ?? (provider.providerId === "kimi"
          && candidate.source === "extra"
          && candidate.id.startsWith("extra-kimi-code-limit-")
          && !candidate.snapshot.isInformational ? "ordinary" : undefined);
    // Kimi may omit its weekly slot. A promoted timed quota must not inherit
    // the provider's static "Weekly" label merely because it is now primary.
    const label = provider.providerId === "kimi"
      ? candidate.snapshot.windowMinutes === 300 ? "Rate Limit"
        : candidate.snapshot.windowMinutes === WEEKLY_WINDOW_MINUTES ? "Weekly"
        : candidate.label
      : candidate.label;
    return kind ? [{ ...candidate, label, snapshot: candidate.snapshot, kind }] : [];
  });
  const selected: ProviderQuotaWindow[] = [];
  const short = classified.find((window) => window.kind === "short");
  const weekly = classified.find((window) => window.kind === "weekly");
  if (short) selected.push(short);
  if (weekly) selected.push(weekly);
  selected.push(...classified.filter((window) => window.kind === "ordinary"));
  selected.push(...classified.filter((window) => window.kind === "monthly"));
  return selected;
}
