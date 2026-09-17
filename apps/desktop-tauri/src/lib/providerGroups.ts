import type { KimiAccountSnapshot, ProviderCatalogEntry, ProviderUsageSnapshot } from "../types/bridge";
import { orderProviderSnapshots } from "./providerOrder";
import {
  selectProviderQuotaWindows,
  type ProviderQuotaWindow,
} from "./rateWindowKind";

export interface ProviderPresentationGroup {
  providerId: string;
  displayName: string;
  snapshots: ProviderUsageSnapshot[];
  successfulSnapshots: ProviderUsageSnapshot[];
  failedCredentialCount: number;
  credentialCount: number;
  hasMultipleCredentials: boolean;
  isAllFailed: boolean;
  isPartialFailure: boolean;
  kimiAccounts?: KimiAccountSnapshot[];
}

export type ProviderPresentationEntry =
  | { type: "account"; account: KimiAccountSnapshot }
  | { type: "provider"; provider: ProviderUsageSnapshot };

export type ProviderPresentationSection =
  | {
      type: "kimi-account";
      account: KimiAccountSnapshot;
      providers: ProviderUsageSnapshot[];
    }
  | { type: "kimi-unmatched"; providers: ProviderUsageSnapshot[] }
  | {
      type: "provider-account";
      displayName: string;
      displayNameSource:
        | "accountDisplayName"
        | "accountEmail"
        | "accountOrganization"
        | "fallback";
      providers: ProviderUsageSnapshot[];
      sharedMonthlyQuotas: ProviderQuotaWindow[];
      suppressedChildQuotaIds: string[];
    }
  | { type: "providers"; providers: ProviderUsageSnapshot[] };

const AUDITED_ACCOUNT_SHARED_MONTHLY_WINDOWS: Readonly<Record<string, readonly string[]>> = {
  clinepass: ["tertiary"],
  factory: ["tertiary"],
};

function accountDisplay(
  group: ProviderPresentationGroup,
  providers: ProviderUsageSnapshot[],
): Pick<Extract<ProviderPresentationSection, { type: "provider-account" }>, "displayName" | "displayNameSource"> {
  const candidateFields = [
    ["accountDisplayName", (provider: ProviderUsageSnapshot) => provider.accountDisplayName],
    ["accountEmail", (provider: ProviderUsageSnapshot) => provider.accountEmail],
    ["accountOrganization", (provider: ProviderUsageSnapshot) => provider.accountOrganization],
  ] as const;
  for (const [source, valueFrom] of candidateFields) {
    for (const provider of providers) {
      const displayName = valueFrom(provider)?.trim();
      if (displayName) return { displayName, displayNameSource: source };
    }
  }
  return {
    displayName: `${group.displayName} account`,
    displayNameSource: "fallback",
  };
}

function sharedMonthlyQuotas(
  providerId: string,
  providers: ProviderUsageSnapshot[],
): ProviderQuotaWindow[] {
  const auditedWindowIds = AUDITED_ACCOUNT_SHARED_MONTHLY_WINDOWS[providerId] ?? [];
  return auditedWindowIds.flatMap((windowId) => {
    let selected: ProviderQuotaWindow | undefined;
    let selectedAt = Number.NEGATIVE_INFINITY;
    for (const provider of providers) {
      // Failed-refresh caches remain on their children with the stale warning.
      if (provider.error != null || provider.refreshError != null) continue;
      const quota = selectProviderQuotaWindows(provider).find(
        (candidate) => candidate.kind === "monthly" && candidate.id === windowId,
      );
      if (!quota) continue;
      const parsedAt = Date.parse(provider.updatedAt);
      const updatedAt = Number.isFinite(parsedAt) ? parsedAt : Number.NEGATIVE_INFINITY;
      if (!selected || updatedAt > selectedAt) {
        selected = quota;
        selectedAt = updatedAt;
      }
    }
    return selected ? [selected] : [];
  });
}

export function providerGroupSections(
  group: ProviderPresentationGroup,
  providers: ProviderUsageSnapshot[] = group.snapshots,
): ProviderPresentationSection[] {
  const accounts = group.providerId === "kimi" ? group.kimiAccounts ?? [] : [];
  if (group.providerId !== "kimi") {
    const grouped = new Map<string, ProviderUsageSnapshot[]>();
    const ungrouped: ProviderUsageSnapshot[] = [];
    for (const provider of providers) {
      const accountGroupId = provider.accountGroupId;
      if (provider.error != null || !accountGroupId?.trim()) {
        ungrouped.push(provider);
        continue;
      }
      const accountKey = JSON.stringify([provider.providerId, accountGroupId]);
      const members = grouped.get(accountKey);
      if (members) members.push(provider);
      else grouped.set(accountKey, [provider]);
    }

    const sections: ProviderPresentationSection[] = Array.from(grouped.values(), (members) => {
      const sharedQuotas = sharedMonthlyQuotas(group.providerId, members);
      return {
        type: "provider-account",
        ...accountDisplay(group, members),
        providers: members,
        sharedMonthlyQuotas: sharedQuotas,
        suppressedChildQuotaIds: sharedQuotas.map((quota) => quota.id),
      };
    });
    if (ungrouped.length > 0) sections.push({ type: "providers", providers: ungrouped });
    return sections;
  }
  if (accounts.length === 0) return [{ type: "providers", providers }];

  const firstProviderByCredential = new Map<string, ProviderUsageSnapshot>();
  for (const provider of providers) {
    if (provider.credentialId && !firstProviderByCredential.has(provider.credentialId)) {
      firstProviderByCredential.set(provider.credentialId, provider);
    }
  }

  const claimedProviders = new Set<ProviderUsageSnapshot>();
  const sections: ProviderPresentationSection[] = accounts.map((account) => {
    const matchedProviders: ProviderUsageSnapshot[] = [];
    for (const credentialId of account.matchedCredentialIds) {
      const provider = firstProviderByCredential.get(credentialId);
      if (provider && !claimedProviders.has(provider)) {
        matchedProviders.push(provider);
        claimedProviders.add(provider);
      }
    }
    return { type: "kimi-account", account, providers: matchedProviders };
  });

  const unmatchedProviders = providers.filter((provider) => !claimedProviders.has(provider));
  if (unmatchedProviders.length > 0) {
    sections.push({ type: "kimi-unmatched", providers: unmatchedProviders });
  }
  return sections;
}

export function selectProviderQuotaWindowsForSection(
  section: ProviderPresentationSection,
  provider: ProviderUsageSnapshot,
): ProviderQuotaWindow[] {
  const windows = selectProviderQuotaWindows(provider);
  if (section.type !== "provider-account") return windows;
  return windows.filter((window) => !section.suppressedChildQuotaIds.includes(window.id));
}

export function providerGroupEntries(
  group: ProviderPresentationGroup,
  providers: ProviderUsageSnapshot[] = group.snapshots,
): ProviderPresentationEntry[] {
  const entries: ProviderPresentationEntry[] = [];
  for (const section of providerGroupSections(group, providers)) {
    if (section.type === "kimi-account") {
      entries.push({ type: "account", account: section.account });
    }
    for (const provider of section.providers) {
        entries.push({ type: "provider", provider });
    }
  }
  return entries;
}

export function providerSnapshotIdentity(
  providerId: string,
  credentialId?: string | null,
): string {
  return JSON.stringify([providerId, credentialId ?? null]);
}

export function groupProviderSnapshots(
  snapshots: ProviderUsageSnapshot[],
  catalog: ProviderCatalogEntry[],
  enabledProviderIds: string[],
  providerOrder: string[] = [],
  kimiAccounts: KimiAccountSnapshot[] = [],
): ProviderPresentationGroup[] {
  const ordered = orderProviderSnapshots(
    snapshots,
    catalog,
    enabledProviderIds,
    providerOrder,
  );
  const grouped = new Map<string, ProviderUsageSnapshot[]>();

  for (const snapshot of ordered) {
    const siblings = grouped.get(snapshot.providerId);
    if (siblings) {
      siblings.push(snapshot);
    } else {
      grouped.set(snapshot.providerId, [snapshot]);
    }
  }

  const catalogNames = new Map(
    catalog.map((provider) => [provider.id, provider.displayName]),
  );
  const visibleKimiAccounts = kimiAccounts.filter(
    (account) =>
      (account.status === "ok" || account.status === "stale" || account.status === "storageError")
      && account.usedPercent != null,
  );
  const syntheticKimiGroup = visibleKimiAccounts.length > 0 && !grouped.has("kimi");
  if (syntheticKimiGroup) {
    grouped.set("kimi", []);
  }

  const groups = Array.from(grouped, ([providerId, providerSnapshots]) => {
    let sortedSnapshots = [...providerSnapshots].sort(compareCredentials);
    const accountSnapshots = providerId === "kimi" ? visibleKimiAccounts : [];
    if (accountSnapshots.length > 0) {
      const accountIndex = new Map<string, number>();
      accountSnapshots.forEach((account, index) => {
        account.matchedCredentialIds.forEach((credentialId) => accountIndex.set(credentialId, index));
      });
      sortedSnapshots = [...sortedSnapshots].sort((left, right) => {
        const leftIndex = left.credentialId ? accountIndex.get(left.credentialId) : undefined;
        const rightIndex = right.credentialId ? accountIndex.get(right.credentialId) : undefined;
        if (leftIndex !== rightIndex) {
          if (leftIndex == null) return 1;
          if (rightIndex == null) return -1;
          return leftIndex - rightIndex;
        }
        return compareCredentials(left, right);
      });
    }
    const successfulSnapshots = sortedSnapshots.filter(
      (snapshot) => snapshot.error == null,
    );
    const credentialSnapshots = sortedSnapshots.filter(
      (snapshot) => snapshot.credentialId != null,
    );
    const failedCredentialCount = credentialSnapshots.filter(
      (snapshot) => snapshot.error != null,
    ).length;
    const declaredCredentialCount = sortedSnapshots.reduce(
      (count, snapshot) => Math.max(count, snapshot.credentialGroupSize ?? 0),
      0,
    );
    const credentialCount = Math.max(
      credentialSnapshots.length,
      declaredCredentialCount,
    );
    const failedSnapshotCount = sortedSnapshots.length - successfulSnapshots.length;

    return {
      providerId,
      displayName:
        catalogNames.get(providerId)
        ?? sortedSnapshots[0]?.displayName
        ?? (providerId === "kimi" ? "Kimi" : providerId),
      snapshots: sortedSnapshots,
      successfulSnapshots,
      failedCredentialCount,
      credentialCount,
      hasMultipleCredentials: credentialCount > 1,
      isAllFailed:
        sortedSnapshots.length > 0 && successfulSnapshots.length === 0,
      isPartialFailure:
        successfulSnapshots.length > 0 && failedSnapshotCount > 0,
      kimiAccounts: accountSnapshots,
    };
  });
  const kimiOrder = providerOrder.indexOf("kimi");
  if (syntheticKimiGroup && kimiOrder >= 0) {
    const currentIndex = groups.findIndex((group) => group.providerId === "kimi");
    if (currentIndex < 0) return groups;
    const [kimiGroup] = groups.splice(currentIndex, 1);
    const insertAt = groups.findIndex((group) => {
      const index = providerOrder.indexOf(group.providerId);
      return index >= 0 && index > kimiOrder;
    });
    groups.splice(insertAt < 0 ? groups.length : insertAt, 0, kimiGroup);
  }
  return groups;
}

function compareCredentials(
  left: ProviderUsageSnapshot,
  right: ProviderUsageSnapshot,
): number {
  if (left.credentialId == null && right.credentialId != null) return -1;
  if (left.credentialId != null && right.credentialId == null) return 1;

  const leftOrdinal = left.credentialDisplayOrdinal ?? Number.MAX_SAFE_INTEGER;
  const rightOrdinal = right.credentialDisplayOrdinal ?? Number.MAX_SAFE_INTEGER;
  if (leftOrdinal !== rightOrdinal) return leftOrdinal - rightOrdinal;

  return providerSnapshotIdentity(left.providerId, left.credentialId).localeCompare(
    providerSnapshotIdentity(right.providerId, right.credentialId),
  );
}
