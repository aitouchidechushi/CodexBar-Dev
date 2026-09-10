import { describe, it, expect } from "vitest";
import { ALL_LOCALE_KEYS } from "./keys";

describe("ALL_LOCALE_KEYS", () => {
  it("does not duplicate canonical language catalog entries", () => {
    expect(ALL_LOCALE_KEYS).not.toContain("LanguageEnglishOption");
    expect(ALL_LOCALE_KEYS).not.toContain("LanguageChineseOption");
    expect(ALL_LOCALE_KEYS).not.toContain("LanguageJapaneseOption");
    expect(ALL_LOCALE_KEYS).not.toContain("LanguageKoreanOption");
    expect(ALL_LOCALE_KEYS).not.toContain("LanguageSpanishOption");
  });

  it("contains the canonical multi-key UI catalog", () => {
    expect(ALL_LOCALE_KEYS).toEqual(expect.arrayContaining([
      "ApiKeyCount",
      "ApiKeyProviderCount",
      "ApiKeyFailureCount",
      "ApiKeyFailureDetails",
      "ApiKeyEditLabel",
      "ApiKeyEditLabelAction",
      "ApiKeyReplaceSecret",
      "ApiKeyReplaceSecretAction",
      "ApiKeyDelete",
      "ApiKeyDeleteAction",
      "ApiKeyDeleteConfirmation",
      "ApiKeyNew",
      "ApiKeyNewLabelOptional",
      "ApiKeyAdd",
    ]));
  });
});
