import { render, screen } from "@testing-library/react";
// @ts-expect-error Vitest runs in Node; the browser tsconfig intentionally omits Node types.
import { readFileSync } from "node:fs";
// @ts-expect-error Vitest runs in Node; the browser tsconfig intentionally omits Node types.
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import type { KimiAccountSnapshot } from "../types/bridge";
import {
  KimiMonthlyQuotaProgress,
  MonthlyQuotaProgress,
} from "./KimiMonthlyQuotaProgress";

declare const process: { cwd(): string };

function account(usedPercent: number | null): KimiAccountSnapshot {
  return {
    accountId: "account-a",
    displayName: "账号 A",
    usedPercent,
    resetsAt: null,
    updatedAt: "2026-08-23T00:00:00Z",
    status: "ok",
    matchedCredentialIds: [],
  };
}

describe("KimiMonthlyQuotaProgress", () => {
  it("uses rose monthly tokens with transparent solid account groups and neutral unmatched groups", () => {
    const styles = readFileSync(resolve(process.cwd(), "src/styles.css"), "utf8");
    const floating = readFileSync(resolve(process.cwd(), "src/floating-quota/FloatingQuota.css"), "utf8");
    const floatbar = readFileSync(resolve(process.cwd(), "src/floatbar/FloatBar.css"), "utf8");
    const tray = readFileSync(resolve(process.cwd(), "src/tray-hover/TrayHover.css"), "utf8");
    const rule = (css: string, selector: string) => {
      const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      return Array.from(
        css.matchAll(new RegExp(`${escaped}\\s*\\{([^}]*)\\}`, "g")),
        (match) => match[1],
      ).join("\n");
    };

    expect(styles).toContain("--kimi-monthly-accent: #c9798f;");
    expect(styles).toContain("--kimi-monthly-accent: #d58ba0;");
    expect(styles).toContain("--kimi-monthly-accent: #a6546b;");

    for (const [css, selector] of [
      [styles, ".menu-card__provider-account-group"],
      [floating, ".floating-quota__account-group"],
      [floatbar, ".floatbar__provider-account-group"],
      [tray, ".tray-hover__account-group"],
    ] as const) {
      const accountRule = rule(css, selector);
      expect(accountRule).toContain("border: 1.5px solid var(--kimi-monthly-accent);");
      expect(accountRule).toContain("background: transparent;");
    }

    for (const [css, selector] of [
      [styles, ".menu-card__kimi-unmatched-group"],
      [floating, ".floating-quota__unmatched-group"],
      [floatbar, ".floatbar__kimi-unmatched"],
      [tray, ".tray-hover__unmatched-group"],
    ] as const) {
      const unmatchedRule = rule(css, selector);
      expect(unmatchedRule).toMatch(/border:\s*1px dashed .*text-secondary|border:\s*1px dashed rgba\(148, 163, 184/);
      expect(unmatchedRule).toContain("background: transparent;");
    }

    for (const [css, selector] of [
      [styles, ".menu-card__kimi-monthly-badge"],
      [floating, ".floating-quota__monthly-badge"],
      [tray, ".tray-hover__monthly-badge"],
    ] as const) {
      expect(rule(css, selector)).toContain("background: transparent;");
    }

    const floatbarMonthly = rule(floatbar, ".floatbar .floatbar__monthly-pill");
    expect(floatbarMonthly).toContain("border: 1px solid var(--kimi-monthly-accent);");
    expect(floatbarMonthly).toContain("background: color-mix(");
    expect(floatbarMonthly).toContain("backdrop-filter: blur(18px) saturate(155%);");
    expect(floatbarMonthly).not.toContain("background: transparent;");

    expect(floatbar).toMatch(
      /\.floatbar--vertical \.floatbar__kimi-group,[\s\S]*?align-items:\s*stretch;/,
    );
    expect(rule(floatbar, ".floatbar__monthly-pill .floatbar__credential-label"))
      .toContain("font-weight: 700;");
    expect(rule(floatbar, ".floatbar__monthly-pill .floatbar__pct"))
      .toContain("font-weight: 800;");

    for (const selector of [
      ".floatbar--floating .floatbar__monthly-pill",
      ".floatbar--floating.floatbar--light-bg .floatbar__monthly-pill",
    ]) {
      const monthlyVariant = rule(floatbar, selector);
      expect(monthlyVariant).toContain("background: color-mix(");
      expect(monthlyVariant).not.toContain("background: transparent;");
    }
  });

  it("renders a generic finite monthly percentage as used or remaining", () => {
    const { rerender } = render(
      <MonthlyQuotaProgress
        usedPercent={27.4}
        showAsUsed
        ariaLabel="Factory monthly quota"
      />,
    );

    const used = screen.getByRole("progressbar", { name: "Factory monthly quota" });
    expect(used).toHaveAttribute("aria-valuemin", "0");
    expect(used).toHaveAttribute("aria-valuemax", "100");
    expect(used).toHaveAttribute("aria-valuenow", "27");
    expect(used).toHaveClass("monthly-quota-progress");

    rerender(
      <MonthlyQuotaProgress
        usedPercent={27.4}
        showAsUsed={false}
        ariaLabel="Factory monthly quota"
      />,
    );
    expect(screen.getByRole("progressbar", { name: "Factory monthly quota" })).toHaveAttribute(
      "aria-valuenow",
      "73",
    );
  });

  it.each([null, undefined, Number.NaN, Number.POSITIVE_INFINITY])(
    "does not invent a generic monthly bar for %s",
    (usedPercent) => {
      const { unmount } = render(
        <MonthlyQuotaProgress
          usedPercent={usedPercent}
          showAsUsed
          ariaLabel="Unavailable monthly quota"
        />,
      );

      expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
      unmount();
    },
  );

  it("shows used or remaining monthly percentage and exposes it accessibly", () => {
    const { rerender } = render(
      <KimiMonthlyQuotaProgress
        account={account(35)}
        showAsUsed
        ariaLabel="Kimi monthly quota"
      />,
    );

    expect(screen.getByRole("progressbar", { name: "Kimi monthly quota" })).toHaveAttribute(
      "aria-valuenow",
      "35",
    );

    rerender(
      <KimiMonthlyQuotaProgress
        account={account(35)}
        showAsUsed={false}
        ariaLabel="Kimi monthly quota"
      />,
    );
    expect(screen.getByRole("progressbar", { name: "Kimi monthly quota" })).toHaveAttribute(
      "aria-valuenow",
      "65",
    );
  });

  it("does not invent a bar when the monthly percentage is unavailable", () => {
    render(
      <KimiMonthlyQuotaProgress
        account={account(null)}
        showAsUsed
        ariaLabel="Kimi monthly quota"
      />,
    );

    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
  });
});
