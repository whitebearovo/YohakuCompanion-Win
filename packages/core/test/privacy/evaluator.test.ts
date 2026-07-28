import { describe, expect, it } from "vitest";
import { defaultPrivacyConfig, type PrivacyConfig } from "@yohaku/shared";
import { mediaDecision, processDecision } from "../../src/privacy/evaluator.js";

function config(patch: Partial<PrivacyConfig> = {}): PrivacyConfig {
  return { ...defaultPrivacyConfig(), ...patch };
}

describe("processDecision", () => {
  it("shares by default with new-installation defaults", () => {
    const d = processDecision(config(), "code.exe");
    expect(d).toEqual({
      sharesApplication: true,
      sharesWindowTitle: false, // defaults.windowTitle = hide
      displayAlias: null,
    });
  });

  it("hide wins: rule application=hide hides app, title, and alias", () => {
    const c = config({
      rules: [
        {
          appId: "secret.exe",
          application: "hide",
          windowTitle: "share",
          media: "inherit",
          displayAlias: "Alias",
        },
      ],
    });
    const d = processDecision(c, "secret.exe");
    expect(d).toEqual({
      sharesApplication: false,
      sharesWindowTitle: false,
      displayAlias: null,
    });
  });

  it("global default application=hide hides apps without a rule", () => {
    const c = config({
      defaults: { application: "hide", windowTitle: "hide", media: "share" },
    });
    expect(processDecision(c, "anything.exe").sharesApplication).toBe(false);
  });

  it("rule share overrides a hide default", () => {
    const c = config({
      defaults: { application: "hide", windowTitle: "hide", media: "share" },
      rules: [
        {
          appId: "code.exe",
          application: "share",
          windowTitle: "inherit",
          media: "inherit",
        },
      ],
    });
    expect(processDecision(c, "code.exe").sharesApplication).toBe(true);
  });

  // Window title truth table over (appHidden, ruleTitle resolved) — the third
  // switch (global shareWindowTitles) is applied at the sanitize layer.
  const titleCases: Array<{
    name: string;
    application: "share" | "hide";
    windowTitle: "share" | "hide";
    expected: boolean;
  }> = [
    { name: "app share + title share", application: "share", windowTitle: "share", expected: true },
    { name: "app share + title hide", application: "share", windowTitle: "hide", expected: false },
    { name: "app hide + title share", application: "hide", windowTitle: "share", expected: false },
    { name: "app hide + title hide", application: "hide", windowTitle: "hide", expected: false },
  ];
  for (const tc of titleCases) {
    it(`window title: ${tc.name} -> ${tc.expected}`, () => {
      const c = config({
        rules: [
          {
            appId: "x.exe",
            application: tc.application,
            windowTitle: tc.windowTitle,
            media: "inherit",
          },
        ],
      });
      expect(processDecision(c, "x.exe").sharesWindowTitle).toBe(tc.expected);
    });
  }

  it("matches appId case-insensitively and trims", () => {
    const c = config({
      rules: [
        { appId: "Code.EXE", application: "hide", windowTitle: "inherit", media: "inherit" },
      ],
    });
    expect(processDecision(c, "  code.exe ").sharesApplication).toBe(false);
  });

  it("normalizes alias: blank alias becomes null", () => {
    const c = config({
      rules: [
        {
          appId: "a.exe",
          application: "share",
          windowTitle: "inherit",
          media: "inherit",
          displayAlias: "   ",
        },
      ],
    });
    expect(processDecision(c, "a.exe").displayAlias).toBeNull();
  });
});

describe("mediaDecision", () => {
  it("shares by default", () => {
    expect(mediaDecision(config(), "spotify.exe", "Spotify")).toEqual({
      sharesMedia: true,
      displayAlias: null,
    });
  });

  it("rule media=hide hides media and alias", () => {
    const c = config({
      rules: [
        {
          appId: "spotify.exe",
          application: "inherit",
          windowTitle: "inherit",
          media: "hide",
          displayAlias: "MyPlayer",
        },
      ],
    });
    expect(mediaDecision(c, "spotify.exe", "Spotify")).toEqual({
      sharesMedia: false,
      displayAlias: null,
    });
  });

  it("app hidden does NOT hide media (independent dimensions)", () => {
    const c = config({
      rules: [
        { appId: "spotify.exe", application: "hide", windowTitle: "inherit", media: "inherit" },
      ],
    });
    expect(mediaDecision(c, "spotify.exe", "Spotify").sharesMedia).toBe(true);
  });

  it("falls back to player-name matching when appId is null", () => {
    const c = config({
      rules: [
        { appId: "Spotify", application: "inherit", windowTitle: "inherit", media: "hide" },
      ],
    });
    expect(mediaDecision(c, null, "spotify").sharesMedia).toBe(false);
    expect(mediaDecision(c, null, "Other Player").sharesMedia).toBe(true);
  });

  it("appId match takes precedence over player-name fallback", () => {
    const c = config({
      rules: [
        { appId: "spotify", application: "inherit", windowTitle: "inherit", media: "hide" },
      ],
    });
    // appId provided and different -> no rule match -> default share
    expect(mediaDecision(c, "spotify.exe", "Spotify").sharesMedia).toBe(true);
  });

  it("global media=hide default", () => {
    const c = config({
      defaults: { application: "share", windowTitle: "hide", media: "hide" },
    });
    expect(mediaDecision(c, "x.exe", "X").sharesMedia).toBe(false);
  });
});
