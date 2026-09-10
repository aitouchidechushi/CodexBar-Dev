import { useCallback, useEffect, useMemo, useRef } from "react";
import { ProviderIcon } from "../components/providers/ProviderIcon";
import {
  KimiMonthlyQuotaProgress,
  MonthlyQuotaProgress,
} from "../components/KimiMonthlyQuotaProgress";
import { useFormattedResetTime } from "../hooks/useFormattedResetTime";
import { useLocale } from "../hooks/useLocale";
import { useProviders } from "../hooks/useProviders";
import {
  groupProviderSnapshots,
  providerGroupSections,
  selectProviderQuotaWindowsForSection,
} from "../lib/providerGroups";
import {
  formatMonthlyQuotaLabel,
  selectProviderQuotaWindows,
  type ProviderQuotaWindow,
} from "../lib/rateWindowKind";
import type { BootstrapState, KimiAccountSnapshot, ProviderUsageSnapshot } from "../types/bridge";
import { resizeTrayHover, setTrayHoverPointerInside } from "./api";
import "./TrayHover.css";

const MIN_CONTENT_WIDTH = 360;

function measureNaturalWidth(root: HTMLElement): number {
  const previousInlineWidth = root.style.width;
  try {
    // The rendered root normally fills the current native window. Measure it
    // at its intrinsic width so a narrow initial viewport cannot become a
    // self-fulfilling resize request.
    root.style.width = "max-content";
    return Math.max(
      MIN_CONTENT_WIDTH,
      Math.ceil(Math.max(root.scrollWidth, root.clientWidth, root.getBoundingClientRect().width)),
    );
  } finally {
    root.style.width = previousInlineWidth;
  }
}

function credentialLabel(provider: ProviderUsageSnapshot, index: number): string {
  return provider.credentialDisplayLabel?.trim() ||
    `Key ${provider.credentialDisplayOrdinal ?? index + 1}`;
}

function maskEmail(email: string): string {
  const at = email.indexOf("@");
  if (at <= 1) return "••••@••••";
  return email[0] + "•".repeat(at - 1) + email.slice(at);
}

function KimiAccountQuota({ account, resetTimeRelative, showAsUsed }: { account: KimiAccountSnapshot; resetTimeRelative: boolean; showAsUsed: boolean }) {
  const { t } = useLocale();
  const reset = useFormattedResetTime(account.resetsAt, null, resetTimeRelative);
  return (
    <div className="tray-hover__account">
      <div className="tray-hover__account-identity">
        <div className="tray-hover__credential-label">{account.displayName}</div>
        {account.sourceLabels && account.sourceLabels.length > 0 && (
          <div className="tray-hover__account-source">{account.sourceLabels.join(" · ")}</div>
        )}
      </div>
      <div className="tray-hover__quota">
        <div className="tray-hover__quota-main">
          <span className="tray-hover__monthly-badge monthly-quota-badge">{t("KimiAccountMonthlyQuota")}</span>
          <strong>{account.usedPercent == null ? t("KimiAccountLoginRequired") : `${Math.round(showAsUsed ? account.usedPercent : 100 - account.usedPercent)}%`}</strong>
        </div>
        <KimiMonthlyQuotaProgress
          account={account}
          showAsUsed={showAsUsed}
          ariaLabel={`${account.displayName} ${t("KimiAccountMonthlyQuota")}`}
        />
        {reset && <div className="tray-hover__reset">{reset}</div>}
      </div>
    </div>
  );
}

function CredentialQuota({
  provider,
  quotaWindows,
  index,
  groupDisplayName,
  hasMultipleCredentials,
  resetTimeRelative,
  showAsUsed,
}: {
  provider: ProviderUsageSnapshot;
  quotaWindows?: ProviderQuotaWindow[];
  index: number;
  groupDisplayName: string;
  hasMultipleCredentials: boolean;
  resetTimeRelative: boolean;
  showAsUsed: boolean;
}) {
  const { t } = useLocale();
  return (
    <div className="tray-hover__credential">
      <div className="tray-hover__credential-label">
        {hasMultipleCredentials || provider.credentialId
          ? credentialLabel(provider, index)
          : groupDisplayName}
      </div>
      {provider.error ? (
        <div className="tray-hover__error">{t("TrayStatusRowError")}</div>
      ) : (
        <div className="tray-hover__quotas">
          {(quotaWindows ?? selectProviderQuotaWindows(provider)).map((quota) => (
            <Quota key={quota.id} quota={quota} resetTimeRelative={resetTimeRelative} showAsUsed={showAsUsed} />
          ))}
        </div>
      )}
    </div>
  );
}

function Quota({
  quota,
  resetTimeRelative,
  showAsUsed,
}: {
  quota: ProviderQuotaWindow;
  resetTimeRelative: boolean;
  showAsUsed: boolean;
}) {
  const { t } = useLocale();
  const resetText = useFormattedResetTime(
    quota.snapshot.resetsAt,
    quota.snapshot.resetDescription,
    resetTimeRelative,
  );
  const label = quota.kind === "monthly"
    ? formatMonthlyQuotaLabel(
        quota.label,
        t("ProviderMonthlyLabel"),
        quota.source === "modelSpecific"
          ? t("DetailWindowModelSpecific")
          : undefined,
      )
    : quota.label || (
      quota.kind === "short"
        ? t("ProviderRateLimitLabel")
        : quota.kind === "weekly"
          ? t("ProviderWeeklyLabel")
          : quota.source === "secondary"
            ? t("DetailWindowSecondary")
            : t("DetailWindowPrimary")
    );
  const usedPercent = Math.max(0, Math.min(100, quota.snapshot.usedPercent));
  const displayPercent = showAsUsed ? usedPercent : 100 - usedPercent;
  return (
    <div className={`tray-hover__quota${quota.kind === "monthly" ? " tray-hover__quota--monthly monthly-quota" : ""}`}>
      <div className="tray-hover__quota-main">
        <span className={quota.kind === "monthly" ? "monthly-quota-badge" : undefined}>{label}</span>
        <strong>{Math.round(displayPercent)}%</strong>
      </div>
      {quota.kind === "monthly" && (
        <MonthlyQuotaProgress
          usedPercent={quota.snapshot.usedPercent}
          showAsUsed={showAsUsed}
          ariaLabel={label}
          className="tray-hover__monthly-progress"
        />
      )}
      {resetText && <div className="tray-hover__reset">{resetText}</div>}
    </div>
  );
}

export default function TrayHover({ state }: { state: BootstrapState }) {
  const { t } = useLocale();
  const { providers, kimiAccounts, hasLoadedCache } = useProviders({ refreshOnMount: false });
  const rootRef = useRef<HTMLDivElement>(null);
  const groups = useMemo(
    () => groupProviderSnapshots(providers, [], state.settings.enabledProviders, state.settings.providerOrder ?? [], state.settings.kimiMonthlyQuotaEnabled ? kimiAccounts : []),
    [providers, kimiAccounts, state.settings.enabledProviders, state.settings.kimiMonthlyQuotaEnabled, state.settings.providerOrder],
  );
  const showAsUsed = state.settings.showAsUsed !== false;

  const fitWindow = useCallback(() => {
    if (!hasLoadedCache) return;
    const root = rootRef.current;
    if (!root) return;
    const width = measureNaturalWidth(root);
    const height = Math.min(520, Math.max(48, Math.ceil(root.scrollHeight)));
    void resizeTrayHover(width, height).catch(() => {});
  }, [hasLoadedCache]);

  useEffect(() => {
    document.body.classList.add("tray-hover-window");
    fitWindow();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(fitWindow);
    if (rootRef.current) observer?.observe(rootRef.current);
    return () => {
      observer?.disconnect();
      document.body.classList.remove("tray-hover-window");
    };
  }, [fitWindow]);

  return (
    <div
      ref={rootRef}
      className="tray-hover"
      onMouseEnter={() => void setTrayHoverPointerInside(true).catch(() => {})}
      onMouseLeave={() => void setTrayHoverPointerInside(false).catch(() => {})}
    >
      {groups.map((group) => (
        <section className="tray-hover__provider" key={group.providerId}>
          <header>
            <ProviderIcon providerId={group.providerId} size={15} />
            <span>{group.displayName}</span>
          </header>
          {providerGroupSections(group).map((section, sectionIndex) => {
            if (section.type === "provider-account") {
              const displayName = state.settings.hidePersonalInfo && section.displayNameSource === "accountEmail"
                ? maskEmail(section.displayName)
                : section.displayName;
              return (
                <section
                  className="tray-hover__account-group"
                  aria-label={displayName}
                  key={`provider-account:${sectionIndex}:${section.providers[0].credentialId ?? "default"}`}
                  role="region"
                >
                  <div className="tray-hover__account">
                    <div className="tray-hover__account-identity">
                      <div className="tray-hover__credential-label">{displayName}</div>
                    </div>
                    <div className="tray-hover__quotas">
                      {section.sharedMonthlyQuotas.map((quota) => (
                        <Quota
                          key={quota.id}
                          quota={quota}
                          resetTimeRelative={state.settings.resetTimeRelative}
                          showAsUsed={showAsUsed}
                        />
                      ))}
                    </div>
                  </div>
                  {section.providers.map((provider, index) => (
                    <CredentialQuota
                      key={provider.credentialId ?? `${group.providerId}:${index}`}
                      provider={provider}
                      quotaWindows={selectProviderQuotaWindowsForSection(section, provider)}
                      index={index}
                      groupDisplayName={group.displayName}
                      hasMultipleCredentials={group.hasMultipleCredentials}
                      resetTimeRelative={state.settings.resetTimeRelative}
                      showAsUsed={showAsUsed}
                    />
                  ))}
                </section>
              );
            }
            if (section.type === "kimi-account") {
              return (
                <section
                  className="tray-hover__account-group"
                  aria-label={section.account.displayName}
                  key={`account:${section.account.accountId}`}
                >
                  <KimiAccountQuota
                    account={section.account}
                    resetTimeRelative={state.settings.resetTimeRelative}
                    showAsUsed={showAsUsed}
                  />
                  {section.providers.length > 0 && (
                    <div className="tray-hover__key-heading">{t("KimiAccountLinkedApiKeys")}</div>
                  )}
                  {section.providers.map((provider, index) => (
                    <CredentialQuota
                      key={provider.credentialId ?? `${group.providerId}:${index}`}
                      provider={provider}
                      index={index}
                      groupDisplayName={group.displayName}
                      hasMultipleCredentials={group.hasMultipleCredentials}
                      resetTimeRelative={state.settings.resetTimeRelative}
                      showAsUsed={showAsUsed}
                    />
                  ))}
                </section>
              );
            }
            if (section.type === "kimi-unmatched") {
              return (
                <section
                  className="tray-hover__unmatched-group"
                  aria-label={t("KimiAccountUnlinkedApiKeys")}
                  key="kimi-unmatched"
                >
                  <div className="tray-hover__key-heading">{t("KimiAccountUnlinkedApiKeys")}</div>
                  {section.providers.map((provider, index) => (
                    <CredentialQuota
                      key={provider.credentialId ?? `${group.providerId}:${index}`}
                      provider={provider}
                      index={index}
                      groupDisplayName={group.displayName}
                      hasMultipleCredentials={group.hasMultipleCredentials}
                      resetTimeRelative={state.settings.resetTimeRelative}
                      showAsUsed={showAsUsed}
                    />
                  ))}
                </section>
              );
            }
            return section.providers.map((provider, index) => (
              <CredentialQuota
                key={provider.credentialId ?? `${group.providerId}:${index}`}
                provider={provider}
                index={index}
                groupDisplayName={group.displayName}
                hasMultipleCredentials={group.hasMultipleCredentials}
                resetTimeRelative={state.settings.resetTimeRelative}
                showAsUsed={showAsUsed}
              />
            ));
          })}
        </section>
      ))}
    </div>
  );
}
