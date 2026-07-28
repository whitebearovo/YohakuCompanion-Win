import { describe, expect, it } from "vitest";
import type { PrivacyConfig } from "@yohaku/shared";
import { applyMapping } from "../../src/privacy/model.js";

const config: PrivacyConfig = {
  defaults: { application: "share", windowTitle: "hide", media: "share" },
  rules: [],
  mappings: [
    { type: "process_name", from: "code.exe", to: "Visual Studio Code" },
    { type: "media_process_name", from: "Spotify.EXE", to: "Spotify" },
  ],
  shareWindowTitles: false,
  ignoreNullArtist: false,
  sources: { application: true, media: true },
};

describe("process-name application mappings", () => {
  it("replaces an app display name using the executable appId", () => {
    expect(applyMapping(config, "process_name", "CODE.EXE")).toBe("Visual Studio Code");
  });

  it("normalizes media process names with the same matching rules", () => {
    expect(applyMapping(config, "media_process_name", "spotify.exe")).toBe("Spotify");
  });

  it("does not cross mapping types", () => {
    expect(applyMapping(config, "media_process_name", "code.exe")).toBeNull();
  });
});
