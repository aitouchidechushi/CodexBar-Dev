import { describe, expect, it } from "vitest";
import { groupProviderSnapshots } from "../../../lib/providerGroups";
import type { ProviderUsageSnapshot } from "../../../types/bridge";
import {
  deriveProviderGroupStatus,
  providerGroupSidebarMetric,
  providerGroupSidebarSubtitle,
} from "./ProvidersTab";

function snapshot(used: number, credentialId: string, error: string | null = null): ProviderUsageSnapshot {
  return {
    providerId: "openrouter",
    displayName: "OpenRouter",
    credentialId,
    credentialDisplayLabel: "safe label",
    credentialDisplayOrdinal: Number(credentialId.slice(-1)),
    primary: {
      usedPercent: used,
      remainingPercent: 100 - used,
      windowMinutes: null,
      resetsAt: null,
      resetDescription: null,
      isExhausted: false,
      reservePercent: null,
      reserveDescription: null,
    },
    secondary: null,
    modelSpecific: null,
    tertiary: null,
    extraRateWindows: [],
    cost: null,
    planName: null,
    accountEmail: null,
    sourceLabel: "api",
    updatedAt: new Date().toISOString(),
    error,
    pace: null,
    accountOrganization: null,
    trayStatusLabel: null,
  };
}

const t = (key: string) => (key === "ApiKeyCount" ? "{} keys" : key);

describe("ProvidersTab grouped sidebar state", () => {
  it("uses one neutral multi-key metric instead of selecting a sibling percentage", () => {
    const [group] = groupProviderSnapshots(
      [snapshot(91, "credential-1"), snapshot(12, "credential-2")],
      [],
      ["openrouter"],
    );

    expect(deriveProviderGroupStatus(true, group)).toBe("ok");
    expect(providerGroupSidebarSubtitle("openrouter", true, group, t)).toBe("2 keys");
    expect(providerGroupSidebarMetric(group)).toBeUndefined();
  });

  it("keeps partial failure available and marks all failures as one provider error", () => {
    const [partial] = groupProviderSnapshots(
      [snapshot(84, "credential-1"), snapshot(0, "credential-2", "secret failure")],
      [],
      ["openrouter"],
    );
    const [failed] = groupProviderSnapshots(
      [
        snapshot(0, "credential-1", "first secret failure"),
        snapshot(0, "credential-2", "second secret failure"),
      ],
      [],
      ["openrouter"],
    );

    expect(deriveProviderGroupStatus(true, partial)).toBe("ok");
    expect(providerGroupSidebarMetric(partial)).toBeUndefined();
    expect(deriveProviderGroupStatus(true, failed)).toBe("error");
    expect(providerGroupSidebarMetric(failed)).toBeUndefined();
  });

  it("marks a preserved transient API-key quota as stale even when it was recently fetched", () => {
    const [group] = groupProviderSnapshots(
      [{ ...snapshot(42, "credential-1"), refreshError: "Timeout" }],
      [],
      ["openrouter"],
    );

    expect(deriveProviderGroupStatus(true, group)).toBe("stale");
  });
});
