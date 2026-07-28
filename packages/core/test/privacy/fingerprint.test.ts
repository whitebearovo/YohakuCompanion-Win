import { describe, expect, it } from "vitest";
import { defaultPrivacyConfig, type PrivacyConfig } from "@yohaku/shared";
import { policyFingerprint } from "../../src/privacy/fingerprint.js";

function config(patch: Partial<PrivacyConfig> = {}): PrivacyConfig {
  return { ...defaultPrivacyConfig(), ...patch };
}

describe("policyFingerprint", () => {
  it("is stable for identical configs", () => {
    expect(policyFingerprint(config())).toBe(policyFingerprint(config()));
  });

  it("is order-insensitive for rules and mappings", () => {
    const ruleA = {
      appId: "a.exe",
      application: "hide",
      windowTitle: "inherit",
      media: "inherit",
    } as const;
    const ruleB = {
      appId: "b.exe",
      application: "inherit",
      windowTitle: "share",
      media: "inherit",
    } as const;
    const m1 = { type: "process_name", from: "x", to: "y" } as const;
    const m2 = { type: "media_process_name", from: "x", to: "y" } as const;
    expect(
      policyFingerprint(config({ rules: [ruleA, ruleB], mappings: [m1, m2] })),
    ).toBe(policyFingerprint(config({ rules: [ruleB, ruleA], mappings: [m2, m1] })));
  });

  it("ignores empty rules and alias whitespace", () => {
    const empty = {
      appId: "noop.exe",
      application: "inherit",
      windowTitle: "inherit",
      media: "inherit",
      displayAlias: "   ",
    } as const;
    expect(policyFingerprint(config({ rules: [empty] }))).toBe(
      policyFingerprint(config()),
    );
  });

  const changes: Array<[string, Partial<PrivacyConfig>]> = [
    ["shareWindowTitles", { shareWindowTitles: true }],
    ["ignoreNullArtist", { ignoreNullArtist: true }],
    ["sources.media", { sources: { application: true, media: false } }],
    [
      "defaults.windowTitle",
      { defaults: { application: "share", windowTitle: "share", media: "share" } },
    ],
    [
      "a rule",
      {
        rules: [
          { appId: "a.exe", application: "hide", windowTitle: "inherit", media: "inherit" },
        ],
      },
    ],
    ["a mapping", { mappings: [{ type: "process_name", from: "a", to: "b" }] }],
  ];
  for (const [name, patch] of changes) {
    it(`changes when ${name} changes`, () => {
      expect(policyFingerprint(config(patch))).not.toBe(policyFingerprint(config()));
    });
  }
});
