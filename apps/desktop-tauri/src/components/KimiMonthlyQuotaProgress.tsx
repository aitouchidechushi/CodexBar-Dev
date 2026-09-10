import type { CSSProperties } from "react";
import type { KimiAccountSnapshot } from "../types/bridge";

export function kimiMonthlyDisplayPercent(
  account: KimiAccountSnapshot,
  showAsUsed: boolean,
): number | null {
  return monthlyDisplayPercent(account.usedPercent, showAsUsed);
}

export function monthlyDisplayPercent(
  usedPercent: number | null | undefined,
  showAsUsed: boolean,
): number | null {
  if (usedPercent == null || !Number.isFinite(usedPercent)) {
    return null;
  }
  const clampedUsedPercent = Math.min(100, Math.max(0, usedPercent));
  return showAsUsed ? clampedUsedPercent : 100 - clampedUsedPercent;
}

export function MonthlyQuotaProgress({
  usedPercent,
  showAsUsed,
  ariaLabel,
  className,
}: {
  usedPercent: number | null | undefined;
  showAsUsed: boolean;
  ariaLabel: string;
  className?: string;
}) {
  const percent = monthlyDisplayPercent(usedPercent, showAsUsed);
  if (percent == null) return null;

  const roundedPercent = Math.round(percent);
  return (
    <div
      className={["monthly-quota-progress", "kimi-monthly-progress", className]
        .filter(Boolean)
        .join(" ")}
      role="progressbar"
      aria-label={ariaLabel}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={roundedPercent}
      style={{ "--kimi-monthly-progress": `${percent}%` } as CSSProperties}
    >
      <span className="monthly-quota-progress__fill kimi-monthly-progress__fill" />
    </div>
  );
}

export function KimiMonthlyQuotaProgress({
  account,
  showAsUsed,
  ariaLabel,
  className,
}: {
  account: KimiAccountSnapshot;
  showAsUsed: boolean;
  ariaLabel: string;
  className?: string;
}) {
  return (
    <MonthlyQuotaProgress
      usedPercent={account.usedPercent}
      showAsUsed={showAsUsed}
      ariaLabel={ariaLabel}
      className={className}
    />
  );
}
