import { describe, expect, it } from "vitest";
import { usageSpendRowCurrency, usageSpendRowSource } from "./UsageSpendTab";

describe("usageSpendRowSource", () => {
  it("renders a localized neutral key count for a multi-key provider row", () => {
    const row = {
          providerId: "openrouter",
          displayName: "OpenRouter",
          sevenDay: null,
          thirtyDay: null,
          currency: "",
          source: "",
          credentialCount: 2,
        };
    expect(usageSpendRowSource(row, "{} keys")).toBe("2 keys");
    expect(usageSpendRowCurrency(row)).toBe("—");
  });
});
