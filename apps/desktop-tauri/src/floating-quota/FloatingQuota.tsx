import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type MouseEvent,
} from "react";
import { ProviderIcon } from "../components/providers/ProviderIcon";
import {
  KimiMonthlyQuotaProgress,
  MonthlyQuotaProgress,
} from "../components/KimiMonthlyQuotaProgress";
import { useFormattedResetTime } from "../hooks/useFormattedResetTime";
import { useLocale } from "../hooks/useLocale";
import { useProviders } from "../hooks/useProviders";
import { useSettings } from "../hooks/useSettings";
import { useTheme } from "../hooks/useTheme";
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
import type {
  BootstrapState,
  KimiAccountSnapshot,
  ProviderUsageSnapshot,
} from "../types/bridge";
import {
  hideFloatingQuota,
  openMainWindowFromFloatingQuota,
  resizeFloatingQuota,
  startFloatingQuotaDrag,
  toggleFloatingQuotaDesktop,
  toggleFloatingQuotaTopmost,
  type FloatingQuotaMode,
} from "./api";

function credentialLabel(provider: ProviderUsageSnapshot, index: number): string {
  return (
    provider.credentialDisplayLabel?.trim() ||
    `Key ${provider.credentialDisplayOrdinal ?? index + 1}`
  );
}

function maskEmail(email: string): string {
  const at = email.indexOf("@");
  if (at <= 1) return "••••@••••";
  return email[0] + "•".repeat(at - 1) + email.slice(at);
}

function KimiMonthlyMetric({
  account,
  showAsUsed,
  resetTimeRelative,
}: {
  account: KimiAccountSnapshot;
  showAsUsed: boolean;
  resetTimeRelative: boolean;
}) {
  const { t } = useLocale();
  const reset = useFormattedResetTime(account.resetsAt, null, resetTimeRelative);
  const used = account.usedPercent == null ? null : Math.max(0, account.usedPercent);
  const percent = used == null ? null : showAsUsed ? used : Math.max(0, 100 - used);
  return (
    <div className="floating-quota__account">
      <h3>{account.displayName}</h3>
      {account.sourceLabels && account.sourceLabels.length > 0 && (
        <div className="floating-quota__account-source">{account.sourceLabels.join(" · ")}</div>
      )}
      <div className="floating-quota__metric">
        <div className="floating-quota__metric-heading">
          <span className="floating-quota__monthly-badge monthly-quota-badge">{t("KimiAccountMonthlyQuota")}</span>
          {percent == null ? (
            <strong>{t("KimiAccountLoginRequired")}</strong>
          ) : (
            <strong>{Math.round(percent)}% {showAsUsed ? t("PanelUsedSuffix") : t("PanelLeftSuffix")}</strong>
          )}
        </div>
        <KimiMonthlyQuotaProgress
          account={account}
          showAsUsed={showAsUsed}
          ariaLabel={`${account.displayName} ${t("KimiAccountMonthlyQuota")}`}
        />
        {reset && <div className="floating-quota__reset">{reset}</div>}
      </div>
    </div>
  );
}

function QuotaMetric({
  quota,
  showAsUsed,
  resetTimeRelative,
}: {
  quota: ProviderQuotaWindow;
  showAsUsed: boolean;
  resetTimeRelative: boolean;
}) {
  const { t } = useLocale();
  const resetText = useFormattedResetTime(
    quota.snapshot.resetsAt,
    quota.snapshot.resetDescription,
    resetTimeRelative,
  );
  const usedPercent = Number.isFinite(quota.snapshot.usedPercent)
    ? Math.max(0, quota.snapshot.usedPercent)
    : 0;
  const percent = showAsUsed
    ? usedPercent
    : Math.max(0, 100 - usedPercent);
  const label = quota.kind === "short"
    ? t("ProviderRateLimitLabel")
    : quota.kind === "weekly"
      ? t("ProviderWeeklyLabel")
      : quota.kind === "monthly"
        ? formatMonthlyQuotaLabel(
            quota.label,
            t("ProviderMonthlyLabel"),
            quota.source === "modelSpecific"
              ? t("DetailWindowModelSpecific")
              : undefined,
          )
        : quota.label || (
            quota.source === "secondary"
              ? t("DetailWindowSecondary")
              : t("DetailWindowPrimary")
          );

  return (
    <div className={`floating-quota__metric${quota.kind === "monthly" ? " floating-quota__metric--monthly monthly-quota" : ""}`}>
      <div className="floating-quota__metric-heading">
        <span className={quota.kind === "monthly" ? "monthly-quota-badge" : undefined}>{label}</span>
        <strong>
          {Math.round(percent)}% {showAsUsed ? t("PanelUsedSuffix") : t("PanelLeftSuffix")}
        </strong>
      </div>
      {quota.kind === "monthly" && (
        <MonthlyQuotaProgress
          usedPercent={quota.snapshot.usedPercent}
          showAsUsed={showAsUsed}
          ariaLabel={label}
          className="floating-quota__monthly-progress"
        />
      )}
      {resetText && <div className="floating-quota__reset">{resetText}</div>}
    </div>
  );
}

function CredentialQuota({
  provider,
  quotaWindows,
  index,
  showAsUsed,
  resetTimeRelative,
}: {
  provider: ProviderUsageSnapshot;
  quotaWindows?: ProviderQuotaWindow[];
  index: number;
  showAsUsed: boolean;
  resetTimeRelative: boolean;
}) {
  const { t } = useLocale();
  return (
    <article className="floating-quota__credential">
      <h3>{credentialLabel(provider, index)}</h3>
      {provider.error ? (
        <div className="floating-quota__error">{t("ProviderStatusError")}</div>
      ) : (
        <div className="floating-quota__metrics">
          {(quotaWindows ?? selectProviderQuotaWindows(provider)).map((quota) => (
            <QuotaMetric
              key={quota.id}
              quota={quota}
              showAsUsed={showAsUsed}
              resetTimeRelative={resetTimeRelative}
            />
          ))}
        </div>
      )}
    </article>
  );
}

export default function FloatingQuota({ state }: { state: BootstrapState }) {
  const { t } = useLocale();
  const { settings } = useSettings(state.settings);
  const { providers, kimiAccounts, hasLoadedCache } = useProviders({ refreshOnMount: false });
  const rootRef = useRef<HTMLDivElement>(null);
  const [windowMode, setWindowMode] = useState<FloatingQuotaMode>("normal");
  useTheme(settings.theme);

  const groups = useMemo(
    () =>
      groupProviderSnapshots(
        providers,
        [],
        settings.enabledProviders,
        settings.providerOrder ?? [],
        settings.kimiMonthlyQuotaEnabled ? kimiAccounts : [],
      )
        .map((group) => ({
          ...group,
          snapshots: group.snapshots.filter(
            (provider) =>
              provider.error || selectProviderQuotaWindows(provider).length > 0,
          ),
        }))
        .filter((group) => group.snapshots.length > 0 || (group.kimiAccounts?.length ?? 0) > 0),
    [providers, kimiAccounts, settings.enabledProviders, settings.kimiMonthlyQuotaEnabled, settings.providerOrder],
  );

  const fitWindow = useCallback(() => {
    const height = Math.min(
      520,
      Math.max(80, Math.ceil(rootRef.current?.scrollHeight ?? 0)),
    );
    void resizeFloatingQuota(height).catch(() => {});
  }, []);

  const startDrag = useCallback((event: MouseEvent<HTMLElement>) => {
    if (event.button !== 0 || (event.target as HTMLElement).closest("button")) return;
    void startFloatingQuotaDrag().catch(() => {});
  }, []);

  useEffect(() => {
    document.body.classList.add("floating-quota-window");
    fitWindow();
    const observer =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(fitWindow);
    if (rootRef.current) observer?.observe(rootRef.current);
    return () => {
      observer?.disconnect();
      document.body.classList.remove("floating-quota-window");
    };
  }, [fitWindow]);

  return (
    <div className="floating-quota" ref={rootRef}>
      <header className="floating-quota__header" onMouseDown={startDrag}>
        <h1>{t("CreditsTitle")}</h1>
        <div className="floating-quota__controls">
          <button
            type="button"
            className="floating-quota__control"
            aria-label={t("FloatingQuotaOpenMain")}
            title={t("FloatingQuotaOpenMain")}
            onClick={() => void openMainWindowFromFloatingQuota().catch(() => {})}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M4 5h10v2H6v11h11v-8h2v10H4V5Zm9-2h8v8h-2V6.41l-7.29 7.3-1.42-1.42L17.59 5H13V3Z" />
            </svg>
          </button>
          <button
            type="button"
            className={`floating-quota__control${windowMode === "desktop" ? " is-active" : ""}`}
            aria-label={t(
              windowMode === "desktop"
                ? "FloatingQuotaUndockDesktop"
                : "FloatingQuotaDesktop",
            )}
            title={t(
              windowMode === "desktop"
                ? "FloatingQuotaUndockDesktop"
                : "FloatingQuotaDesktop",
            )}
            aria-pressed={windowMode === "desktop"}
            onClick={() =>
              void toggleFloatingQuotaDesktop().then(setWindowMode).catch(() => {})
            }
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M3 4h18v14h-7v2h3v2H7v-2h3v-2H3V4Zm2 2v10h14V6H5Z" />
            </svg>
          </button>
          <button
            type="button"
            className={`floating-quota__control${windowMode === "topmost" ? " is-active" : ""}`}
            aria-label={t(
              windowMode === "topmost" ? "FloatingQuotaUnpin" : "FloatingQuotaPin",
            )}
            title={t(
              windowMode === "topmost" ? "FloatingQuotaUnpin" : "FloatingQuotaPin",
            )}
            aria-pressed={windowMode === "topmost"}
            onClick={() =>
              void toggleFloatingQuotaTopmost().then(setWindowMode).catch(() => {})
            }
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M8 3h8l-1 6 3 3v2h-5v7l-1 1-1-1v-7H6v-2l3-3-1-6Zm2.35 2 .72 4.3L8.38 12h7.24l-2.69-2.7.72-4.3h-3.3Z" />
            </svg>
          </button>
          <button
            type="button"
            className="floating-quota__control floating-quota__hide"
            aria-label={t("FloatingQuotaHide")}
            title={t("FloatingQuotaHide")}
            onClick={() => void hideFloatingQuota().catch(() => {})}
          >
            −
          </button>
        </div>
      </header>

      <div className="floating-quota__content">
        {groups.map((group) => (
          <section className="floating-quota__provider" key={group.providerId}>
            <header className="floating-quota__provider-heading">
              <ProviderIcon providerId={group.providerId} size={18} />
              <h2>{group.displayName}</h2>
            </header>

            {providerGroupSections(group).map((section, sectionIndex) => {
              if (section.type === "provider-account") {
                const displayName = settings.hidePersonalInfo && section.displayNameSource === "accountEmail"
                  ? maskEmail(section.displayName)
                  : section.displayName;
                return (
                  <section
                    className="floating-quota__account-group"
                    aria-label={displayName}
                    key={`provider-account:${sectionIndex}:${section.providers[0].credentialId ?? "default"}`}
                    role="region"
                  >
                    <div className="floating-quota__account">
                      <h3>{displayName}</h3>
                      <div className="floating-quota__metrics">
                        {section.sharedMonthlyQuotas.map((quota) => (
                          <QuotaMetric
                            key={quota.id}
                            quota={quota}
                            showAsUsed={settings.showAsUsed}
                            resetTimeRelative={settings.resetTimeRelative}
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
                        showAsUsed={settings.showAsUsed}
                        resetTimeRelative={settings.resetTimeRelative}
                      />
                    ))}
                  </section>
                );
              }
              if (section.type === "kimi-account") {
                return (
                  <section
                    className="floating-quota__account-group"
                    aria-label={section.account.displayName}
                    key={`account:${section.account.accountId}`}
                  >
                    <KimiMonthlyMetric
                      account={section.account}
                      showAsUsed={settings.showAsUsed}
                      resetTimeRelative={settings.resetTimeRelative}
                    />
                    {section.providers.length > 0 && (
                      <div className="floating-quota__key-heading">{t("KimiAccountLinkedApiKeys")}</div>
                    )}
                    {section.providers.map((provider, index) => (
                      <CredentialQuota
                        key={provider.credentialId ?? `${group.providerId}:${index}`}
                        provider={provider}
                        index={index}
                        showAsUsed={settings.showAsUsed}
                        resetTimeRelative={settings.resetTimeRelative}
                      />
                    ))}
                  </section>
                );
              }
              if (section.type === "kimi-unmatched") {
                return (
                  <section
                    className="floating-quota__unmatched-group"
                    aria-label={t("KimiAccountUnlinkedApiKeys")}
                    key="kimi-unmatched"
                  >
                    <div className="floating-quota__key-heading">{t("KimiAccountUnlinkedApiKeys")}</div>
                    {section.providers.map((provider, index) => (
                      <CredentialQuota
                        key={provider.credentialId ?? `${group.providerId}:${index}`}
                        provider={provider}
                        index={index}
                        showAsUsed={settings.showAsUsed}
                        resetTimeRelative={settings.resetTimeRelative}
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
                  showAsUsed={settings.showAsUsed}
                  resetTimeRelative={settings.resetTimeRelative}
                />
              ));
            })}
          </section>
        ))}

        {hasLoadedCache && groups.length === 0 && (
          <div className="floating-quota__empty">{t("FloatingQuotaEmpty")}</div>
        )}
      </div>
    </div>
  );
}
