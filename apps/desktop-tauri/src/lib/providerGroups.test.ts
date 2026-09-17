import { describe, expect, it } from "vitest";
import type { KimiAccountSnapshot, ProviderCatalogEntry, ProviderUsageSnapshot } from "../types/bridge";
import {
  groupProviderSnapshots,
  providerGroupEntries,
  providerGroupSections,
  providerSnapshotIdentity,
  selectProviderQuotaWindowsForSection,
} from "./providerGroups";

const catalog: ProviderCatalogEntry[] = [
  { id: "codex", displayName: "Codex", cookieDomain: null },
  { id: "openrouter", displayName: "OpenRouter", cookieDomain: null },
];

function snapshot(
  providerId: string,
  options: {
    credentialId?: string;
    label?: string;
    ordinal?: number;
    error?: string | null;
    usedPercent?: number;
    accountEmail?: string | null;
    accountOrganization?: string | null;
    accountGroupId?: string;
    accountDisplayName?: string;
    groupSize?: number;
    tertiary?: ProviderUsageSnapshot["tertiary"];
  } = {},
): ProviderUsageSnapshot {
  const usedPercent = options.usedPercent ?? 20;
  return {
    providerId,
    displayName: providerId,
    credentialId: options.credentialId,
    credentialDisplayLabel: options.label,
    credentialDisplayOrdinal: options.ordinal,
    credentialGroupSize: options.groupSize,
    primary: {
      usedPercent,
      remainingPercent: 100 - usedPercent,
      windowMinutes: null,
      resetsAt: null,
      resetDescription: null,
      isExhausted: false,
      reservePercent: null,
      reserveDescription: null,
    },
    secondary: null,
    modelSpecific: null,
    tertiary: options.tertiary ?? null,
    extraRateWindows: [],
    cost: null,
    planName: null,
    accountEmail: options.accountEmail ?? null,
    accountGroupId: options.accountGroupId,
    accountDisplayName: options.accountDisplayName,
    sourceLabel: "test",
    updatedAt: "2026-01-01T00:00:00Z",
    error: options.error ?? null,
    pace: null,
    accountOrganization: options.accountOrganization ?? null,
    trayStatusLabel: null,
  };
}

function monthlyRate(usedPercent: number, resetsAt: string): ProviderUsageSnapshot["primary"] {
  return {
    usedPercent,
    remainingPercent: 100 - usedPercent,
    windowMinutes: 30 * 24 * 60,
    resetsAt,
    resetDescription: null,
    isExhausted: false,
    reservePercent: null,
    reserveDescription: null,
  };
}

describe("providerSnapshotIdentity", () => {
  it("distinguishes default, credential, and separator-like values without account data", () => {
    expect(providerSnapshotIdentity("a:b", undefined)).not.toBe(
      providerSnapshotIdentity("a", "b:default"),
    );
    expect(providerSnapshotIdentity("openrouter", undefined)).not.toBe(
      providerSnapshotIdentity("openrouter", "default"),
    );
  });
});

describe.each(["factory", "clinepass"])("%s shared quota freshness", (providerId) => {
  function member(id: string, used: number, updatedAt: string, refreshError?: string) {
    return {
      ...snapshot(providerId, {
        credentialId: id,
        accountGroupId: "same-account",
        tertiary: monthlyRate(used, "2026-10-01T00:00:00Z"),
      }),
      updatedAt,
      refreshError,
    };
  }

  it.each([
    ["invalid-date", "2026-09-02T00:00:00Z", 40],
    ["2026-09-02T00:00:00Z", "invalid-date", 10],
    ["invalid-date", "invalid-date", 10],
    ["2026-09-02T00:00:00Z", "2026-09-02T00:00:00Z", 10],
  ] as const)("handles timestamps %s and %s with stable selection", (firstAt, secondAt, expected) => {
    const members = [member("a", 10, firstAt), member("b", 40, secondAt)];
    const [group] = groupProviderSnapshots(members, [], [providerId]);
    const [section] = providerGroupSections(group);
    if (section.type !== "provider-account") throw new Error("expected account");
    expect(section.sharedMonthlyQuotas.map((q) => q.snapshot.usedPercent)).toEqual([expected]);
  });

  it("skips newer snapshots without the shared window", () => {
    const members = [
      { ...member("a", 90, "2026-09-03T00:00:00Z"), tertiary: null },
      member("b", 40, "2026-09-02T00:00:00Z"),
    ];
    const [group] = groupProviderSnapshots(members, [], [providerId]);
    const [section] = providerGroupSections(group);
    if (section.type !== "provider-account") throw new Error("expected account");
    expect(section.sharedMonthlyQuotas.map((q) => q.snapshot.usedPercent)).toEqual([40]);
  });

  it("uses the newest successful quota regardless of key order", () => {
    const older = member("a", 10, "2026-09-01T00:00:00Z");
    const newer = member("b", 40, "2026-09-02T00:00:00Z");
    for (const members of [[older, newer], [newer, older]]) {
      const [group] = groupProviderSnapshots(members, [], [providerId]);
      const [section] = providerGroupSections(group, members);
      if (section.type !== "provider-account") throw new Error("expected account");
      expect(section.sharedMonthlyQuotas.map((q) => q.snapshot.usedPercent)).toEqual([40]);
      expect(section.providers).toEqual(members);
    }
  });

  it("does not promote failed-refresh cache over a successful sibling", () => {
    const members = [
      member("a", 90, "2026-09-03T00:00:00Z", "Timeout"),
      member("b", 40, "2026-09-02T00:00:00Z"),
    ];
    const [group] = groupProviderSnapshots(members, [], [providerId]);
    const [section] = providerGroupSections(group);
    if (section.type !== "provider-account") throw new Error("expected account");
    expect(section.sharedMonthlyQuotas.map((q) => q.snapshot.usedPercent)).toEqual([40]);
  });

  it("keeps all-failed cached quotas on their children instead of promoting them", () => {
    const members = [
      member("a", 10, "2026-09-01T00:00:00Z", "Timeout"),
      member("b", 40, "2026-09-02T00:00:00Z", "Timeout"),
    ];
    const [group] = groupProviderSnapshots(members, [], [providerId]);
    const [section] = providerGroupSections(group);
    if (section.type !== "provider-account") throw new Error("expected account");
    expect(section.sharedMonthlyQuotas).toEqual([]);
    expect(section.suppressedChildQuotaIds).toEqual([]);
    expect(members.flatMap((p) => selectProviderQuotaWindowsForSection(section, p)
      .filter((q) => q.id === "tertiary").map((q) => q.snapshot.usedPercent))).toEqual([10, 40]);
  });
});

describe("groupProviderSnapshots", () => {
  it("returns one provider group in provider order with credentials ordered by ordinal", () => {
    const groups = groupProviderSnapshots(
      [
        snapshot("openrouter", { credentialId: "third", label: "Key 3", ordinal: 3 }),
        snapshot("codex"),
        snapshot("openrouter", { credentialId: "first", label: "Work", ordinal: 1 }),
      ],
      catalog,
      ["codex", "openrouter"],
    );

    expect(groups.map((group) => group.providerId)).toEqual(["codex", "openrouter"]);
    expect(groups[1].displayName).toBe("OpenRouter");
    expect(groups[1].snapshots.map((item) => item.credentialId)).toEqual(["first", "third"]);
    expect(groups[1].snapshots.map((item) => item.credentialDisplayLabel)).toEqual([
      "Work",
      "Key 3",
    ]);
  });

  it("keeps a default snapshot as an ordinary single-snapshot group", () => {
    const [group] = groupProviderSnapshots([snapshot("codex")], catalog, ["codex"]);

    expect(group.snapshots).toHaveLength(1);
    expect(group.credentialCount).toBe(0);
    expect(group.hasMultipleCredentials).toBe(false);
    expect(group.failedCredentialCount).toBe(0);
    expect(group.isAllFailed).toBe(false);
    expect(group.isPartialFailure).toBe(false);
  });

  it("treats a failing default snapshot as a provider error, not a failed credential", () => {
    const [group] = groupProviderSnapshots(
      [snapshot("codex", { error: "cookie expired" })],
      catalog,
      ["codex"],
    );

    expect(group.credentialCount).toBe(0);
    expect(group.failedCredentialCount).toBe(0);
    expect(group.hasMultipleCredentials).toBe(false);
    expect(group.successfulSnapshots).toEqual([]);
    expect(group.isAllFailed).toBe(true);
    expect(group.isPartialFailure).toBe(false);
  });

  it("counts partial and all failures without choosing or aggregating quota values", () => {
    const partial = groupProviderSnapshots(
      [
        snapshot("openrouter", {
          credentialId: "healthy",
          label: "Work",
          ordinal: 1,
          usedPercent: 17,
          accountEmail: "private@example.com",
        }),
        snapshot("openrouter", {
          credentialId: "failed",
          label: "Key 2",
          ordinal: 2,
          error: "unavailable",
          usedPercent: 93,
        }),
      ],
      catalog,
      ["openrouter"],
    )[0];

    expect(partial.successfulSnapshots.map((item) => item.credentialId)).toEqual(["healthy"]);
    expect(partial.failedCredentialCount).toBe(1);
    expect(partial.credentialCount).toBe(2);
    expect(partial.hasMultipleCredentials).toBe(true);
    expect(partial.isPartialFailure).toBe(true);
    expect(partial.isAllFailed).toBe(false);
    expect(Object.keys(partial)).not.toEqual(
      expect.arrayContaining(["totalUsedPercent", "averageUsedPercent", "maxUsedPercent", "representativeSnapshot"]),
    );
    expect(partial).not.toHaveProperty("accountEmail");

    const allFailed = groupProviderSnapshots(
      [
        snapshot("openrouter", { credentialId: "a", ordinal: 1, error: "bad A" }),
        snapshot("openrouter", { credentialId: "b", ordinal: 2, error: "bad B" }),
      ],
      catalog,
      ["openrouter"],
    )[0];
    expect(allFailed.successfulSnapshots).toEqual([]);
    expect(allFailed.failedCredentialCount).toBe(2);
    expect(allFailed.isAllFailed).toBe(true);
    expect(allFailed.isPartialFailure).toBe(false);
  });

  it("uses deterministic tie breakers without labels or account fields as identity", () => {
    const [group] = groupProviderSnapshots(
      [
        snapshot("openrouter", { credentialId: "b", label: "Same", ordinal: 1 }),
        snapshot("openrouter", { credentialId: "a", label: "Same", ordinal: 1 }),
      ],
      catalog,
      ["openrouter"],
    );

    expect(group.snapshots.map((item) => item.credentialId)).toEqual(["a", "b"]);
  });

  it("keeps a provider neutral while a changed credential is still refreshing", () => {
    const [group] = groupProviderSnapshots(
      [
        snapshot("openrouter", {
          credentialId: "healthy-sibling",
          ordinal: 1,
          groupSize: 2,
        }),
      ],
      catalog,
      ["openrouter"],
    );

    expect(group.snapshots).toHaveLength(1);
    expect(group.credentialCount).toBe(2);
    expect(group.hasMultipleCredentials).toBe(true);
  });

  it("places each Kimi account monthly quota once before its strictly matched keys", () => {
    const accounts: KimiAccountSnapshot[] = [
      {
        accountId: "account-a",
        displayName: "账号 A",
        usedPercent: 35,
        resetsAt: "2026-09-01T00:00:00Z",
        updatedAt: "2026-08-14T00:00:00Z",
        status: "ok",
        matchedCredentialIds: ["key-1", "key-2"],
      },
      {
        accountId: "account-b",
        displayName: "账号 B",
        usedPercent: 50,
        resetsAt: null,
        updatedAt: "2026-08-14T00:00:00Z",
        status: "ok",
        matchedCredentialIds: ["key-3"],
      },
    ];
    const [group] = groupProviderSnapshots(
      [
        snapshot("kimi", { credentialId: "key-3", ordinal: 3 }),
        snapshot("kimi", { credentialId: "key-1", ordinal: 1 }),
        snapshot("kimi", { credentialId: "key-2", ordinal: 2 }),
        snapshot("kimi", { credentialId: "key-4", ordinal: 4 }),
      ],
      [],
      ["kimi"],
      [],
      accounts,
    );

    expect(providerGroupEntries(group).map((entry) =>
      entry.type === "account" ? entry.account.accountId : entry.provider.credentialId,
    )).toEqual(["account-a", "key-1", "key-2", "account-b", "key-3", "key-4"]);

    expect(providerGroupSections(group).map((section) => {
      if (section.type === "kimi-account") {
        return [
          section.type,
          section.account.accountId,
          section.providers.map((provider) => provider.credentialId),
        ];
      }
      return [
        section.type,
        section.providers.map((provider) => provider.credentialId),
      ];
    })).toEqual([
      ["kimi-account", "account-a", ["key-1", "key-2"]],
      ["kimi-account", "account-b", ["key-3"]],
      ["kimi-unmatched", ["key-4"]],
    ]);
  });

  it("keeps an account-only Kimi monthly quota visible without API key snapshots", () => {
    const account: KimiAccountSnapshot = {
      accountId: "account-only",
      displayName: "账号 A",
      usedPercent: 35,
      resetsAt: "2026-09-01T00:00:00Z",
      updatedAt: "2026-08-14T00:00:00Z",
      status: "ok",
      matchedCredentialIds: [],
    };

    const groups = groupProviderSnapshots([], [], [], [], [account]);

    expect(groups).toHaveLength(1);
    expect(groups[0].providerId).toBe("kimi");
    expect(groups[0].snapshots).toEqual([]);
    expect(providerGroupEntries(groups[0])).toEqual([{ type: "account", account }]);
    expect(providerGroupSections(groups[0])).toEqual([
      { type: "kimi-account", account, providers: [] },
    ]);
  });

  it("keeps non-Kimi providers in one ordinary presentation section", () => {
    const providers = [
      snapshot("openrouter", { credentialId: "key-1" }),
      snapshot("openrouter", { credentialId: "key-2" }),
    ];
    const [group] = groupProviderSnapshots(providers, catalog, ["openrouter"]);

    expect(providerGroupSections(group)).toEqual([
      { type: "providers", providers: group.snapshots },
    ]);
  });

  it("groups successful non-Kimi keys only by exact stable account membership", () => {
    const [group] = groupProviderSnapshots(
      [
        snapshot("copilot", {
          credentialId: "key-a",
          ordinal: 1,
          accountGroupId: "github:user-42",
          accountDisplayName: "Ada",
          accountEmail: "first@example.com",
          tertiary: monthlyRate(31, "2026-09-01T00:00:00Z"),
        }),
        snapshot("copilot", {
          credentialId: "key-b",
          ordinal: 2,
          accountGroupId: "github:user-42",
          accountDisplayName: "Changed label",
          accountEmail: "changed@example.com",
          tertiary: monthlyRate(72, "2026-10-01T00:00:00Z"),
        }),
      ],
      [],
      ["copilot"],
    );

    const sections = providerGroupSections(group);
    expect(sections).toHaveLength(1);
    expect(sections[0].type).toBe("provider-account");
    if (sections[0].type !== "provider-account") throw new Error("expected provider account");
    expect(sections[0].displayName).toBe("Ada");
    expect(sections[0].displayNameSource).toBe("accountDisplayName");
    expect(sections[0].providers.map((provider) => provider.credentialId)).toEqual([
      "key-a",
      "key-b",
    ]);
    expect(sections[0].sharedMonthlyQuotas).toEqual([]);
    expect(sections[0].suppressedChildQuotaIds).toEqual([]);
    expect(
      sections[0].providers.flatMap((provider) =>
        selectProviderQuotaWindowsForSection(sections[0], provider)
          .filter((quota) => quota.kind === "monthly")
          .map((quota) => quota.snapshot.usedPercent),
      ),
    ).toEqual([31, 72]);
  });

  it("chooses account display metadata across every member by field priority and member order", () => {
    const [group] = groupProviderSnapshots(
      [
        snapshot("factory", {
          credentialId: "key-a",
          ordinal: 1,
          accountGroupId: "factory:user-42",
          accountOrganization: "First Organization",
        }),
        snapshot("factory", {
          credentialId: "key-b",
          ordinal: 2,
          accountGroupId: "factory:user-42",
          accountEmail: "first-email@example.com",
        }),
        snapshot("factory", {
          credentialId: "key-c",
          ordinal: 3,
          accountGroupId: "factory:user-42",
          accountDisplayName: "First Display Name",
        }),
        snapshot("factory", {
          credentialId: "key-d",
          ordinal: 4,
          accountGroupId: "factory:user-42",
          accountDisplayName: "Later Display Name",
        }),
      ],
      [],
      ["factory"],
    );

    const [section] = providerGroupSections(group);
    expect(section.type).toBe("provider-account");
    if (section.type !== "provider-account") throw new Error("expected provider account");
    expect(section.displayName).toBe("First Display Name");
    expect(section.displayNameSource).toBe("accountDisplayName");
  });

  it("deduplicates only an audited provider window into the account header", () => {
    const [group] = groupProviderSnapshots(
      [
        {
          ...snapshot("clinepass", {
            credentialId: "key-a",
            ordinal: 1,
            accountGroupId: "cline:account-42",
            tertiary: monthlyRate(40, "2026-09-01T00:00:00Z"),
          }),
          primaryLabel: "Monthly",
        },
        {
          ...snapshot("clinepass", {
            credentialId: "key-b",
            ordinal: 2,
            accountGroupId: "cline:account-42",
            tertiary: monthlyRate(40, "2026-09-01T00:00:00Z"),
          }),
          primaryLabel: "Monthly",
        },
      ],
      [],
      ["clinepass"],
    );

    const [section] = providerGroupSections(group);
    expect(section.type).toBe("provider-account");
    if (section.type !== "provider-account") throw new Error("expected provider account");
    expect(section.sharedMonthlyQuotas.map((quota) => ({
      id: quota.id,
      usedPercent: quota.snapshot.usedPercent,
      resetsAt: quota.snapshot.resetsAt,
    }))).toEqual([
      {
        id: "tertiary",
        usedPercent: 40,
        resetsAt: "2026-09-01T00:00:00Z",
      },
    ]);
    expect(section.suppressedChildQuotaIds).toEqual(["tertiary"]);
    expect(
      section.providers.map((provider) =>
        selectProviderQuotaWindowsForSection(section, provider).map((quota) => ({
          id: quota.id,
          kind: quota.kind,
        })),
      ),
    ).toEqual([
      [{ id: "primary", kind: "ordinary" }],
      [{ id: "primary", kind: "ordinary" }],
    ]);
  });

  it("deduplicates Factory's audited standard monthly billing window", () => {
    const [group] = groupProviderSnapshots(
      [
        snapshot("factory", {
          credentialId: "key-a",
          ordinal: 1,
          accountGroupId: "factory:user-42",
          tertiary: monthlyRate(56, "2026-09-01T00:00:00Z"),
        }),
        snapshot("factory", {
          credentialId: "key-b",
          ordinal: 2,
          accountGroupId: "factory:user-42",
          tertiary: monthlyRate(56, "2026-09-01T00:00:00Z"),
        }),
      ],
      [],
      ["factory"],
    );

    const [section] = providerGroupSections(group);
    expect(section.type).toBe("provider-account");
    if (section.type !== "provider-account") throw new Error("expected provider account");
    expect(section.sharedMonthlyQuotas.map((quota) => quota.id)).toEqual(["tertiary"]);
    expect(section.suppressedChildQuotaIds).toEqual(["tertiary"]);
  });

  it("separates different IDs and leaves missing or failed identity snapshots ungrouped", () => {
    const [group] = groupProviderSnapshots(
      [
        snapshot("copilot", {
          credentialId: "account-a",
          ordinal: 1,
          accountGroupId: "github:user-a",
          tertiary: monthlyRate(50, "2026-09-01T00:00:00Z"),
        }),
        snapshot("copilot", {
          credentialId: "account-b",
          ordinal: 2,
          accountGroupId: "github:user-b",
          tertiary: monthlyRate(50, "2026-09-01T00:00:00Z"),
        }),
        snapshot("copilot", {
          credentialId: "missing-id",
          ordinal: 3,
          tertiary: monthlyRate(50, "2026-09-01T00:00:00Z"),
        }),
        snapshot("copilot", {
          credentialId: "failed-identity",
          ordinal: 4,
          accountGroupId: "github:user-a",
          error: "identity unavailable",
          tertiary: monthlyRate(50, "2026-09-01T00:00:00Z"),
        }),
      ],
      [],
      ["copilot"],
    );

    const sections = providerGroupSections(group);
    expect(sections.map((section) => [
      section.type,
      section.providers.map((provider) => provider.credentialId),
    ])).toEqual([
      ["provider-account", ["account-a"]],
      ["provider-account", ["account-b"]],
      ["providers", ["missing-id", "failed-identity"]],
    ]);
  });

  it("uses the display-name fallback without changing membership identity", () => {
    const [group] = groupProviderSnapshots(
      [
        snapshot("openrouter", {
          credentialId: "display",
          ordinal: 1,
          accountGroupId: "account-display",
          accountDisplayName: "Ada",
          accountEmail: "display@example.com",
          accountOrganization: "Display Org",
        }),
        snapshot("openrouter", {
          credentialId: "email",
          ordinal: 2,
          accountGroupId: "account-email",
          accountEmail: "email@example.com",
          accountOrganization: "Email Org",
        }),
        snapshot("openrouter", {
          credentialId: "organization",
          ordinal: 3,
          accountGroupId: "account-organization",
          accountOrganization: "Organization Only",
        }),
        snapshot("openrouter", {
          credentialId: "fallback",
          ordinal: 4,
          accountGroupId: "account-fallback",
        }),
      ],
      catalog,
      ["openrouter"],
    );

    expect(providerGroupSections(group).map((section) =>
      section.type === "provider-account"
        ? [section.displayName, section.displayNameSource]
        : [section.type],
    )).toEqual([
      ["Ada", "accountDisplayName"],
      ["email@example.com", "accountEmail"],
      ["Organization Only", "accountOrganization"],
      ["OpenRouter account", "fallback"],
    ]);
  });

  it.each(["stale", "storageError"] as const)("keeps last quota visible with %s status", (status) => {
    const account: KimiAccountSnapshot = {
      accountId: "account-stale",
      displayName: "账号 A",
      usedPercent: 35,
      resetsAt: "2026-09-01T00:00:00Z",
      updatedAt: "2026-08-14T00:00:00Z",
      status,
      matchedCredentialIds: [],
    };

    const [group] = groupProviderSnapshots([], [], [], [], [account]);

    expect(providerGroupEntries(group)).toEqual([{ type: "account", account }]);
  });

  it("inserts an account-only Kimi group at its configured provider order", () => {
    const account: KimiAccountSnapshot = {
      accountId: "account-only",
      displayName: "账号 A",
      usedPercent: 35,
      resetsAt: null,
      updatedAt: "2026-08-14T00:00:00Z",
      status: "ok",
      matchedCredentialIds: [],
    };

    const groups = groupProviderSnapshots(
      [snapshot("openrouter"), snapshot("codex")],
      catalog,
      ["codex", "openrouter"],
      ["codex", "kimi", "openrouter"],
      [account],
    );

    expect(groups.map((group) => group.providerId)).toEqual([
      "codex",
      "kimi",
      "openrouter",
    ]);
  });
});
