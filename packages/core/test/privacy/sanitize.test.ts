import { describe, expect, it } from "vitest";
import {
  sanitizeApplication,
  sanitizeMedia,
  type ApplicationSanitizeInput,
  type MediaSanitizeInput,
} from "../../src/privacy/sanitize.js";
import type { MediaDecision, ProcessDecision } from "../../src/privacy/evaluator.js";

const share: ProcessDecision = {
  sharesApplication: true,
  sharesWindowTitle: true,
  displayAlias: null,
};

function appInput(patch: Partial<ApplicationSanitizeInput> = {}): ApplicationSanitizeInput {
  return {
    capturedDisplayName: "Visual Studio Code",
    mappedDisplayName: null,
    windowTitle: "secret.ts - project",
    ...patch,
  };
}

describe("sanitizeApplication", () => {
  it("returns null when application is not shared", () => {
    expect(
      sanitizeApplication(appInput(), { ...share, sharesApplication: false }, true),
    ).toBeNull();
  });

  it("display name precedence: alias > mapping > raw", () => {
    expect(
      sanitizeApplication(
        appInput({ mappedDisplayName: "Mapped" }),
        { ...share, displayAlias: "Alias" },
        true,
      )?.displayName,
    ).toBe("Alias");
    expect(
      sanitizeApplication(appInput({ mappedDisplayName: "Mapped" }), share, true)
        ?.displayName,
    ).toBe("Mapped");
    expect(sanitizeApplication(appInput(), share, true)?.displayName).toBe(
      "Visual Studio Code",
    );
  });

  it("window title requires all three switches", () => {
    // rule says share + global on -> title present
    expect(sanitizeApplication(appInput(), share, true)?.windowTitle).toBe(
      "secret.ts - project",
    );
    // global off -> no title
    expect(sanitizeApplication(appInput(), share, false)?.windowTitle).toBeNull();
    // rule says hide -> no title even with global on
    expect(
      sanitizeApplication(appInput(), { ...share, sharesWindowTitle: false }, true)
        ?.windowTitle,
    ).toBeNull();
  });

  it("normalizes text: NFC + trim, empty title -> null", () => {
    const out = sanitizeApplication(
      appInput({ capturedDisplayName: "  Café  ", windowTitle: "   " }),
      share,
      true,
    );
    expect(out?.displayName).toBe("Café");
    expect(out?.windowTitle).toBeNull();
  });
});

const shareMedia: MediaDecision = { sharesMedia: true, displayAlias: null };

function mediaInput(patch: Partial<MediaSanitizeInput> = {}): MediaSanitizeInput {
  return {
    kind: "music",
    title: "Song",
    artist: "Artist",
    album: "Album",
    capturedPlayerName: "Spotify",
    mappedPlayerName: null,
    playing: true,
    durationSeconds: 200,
    positionSeconds: 60,
    sampledAt: 1_753_500_000_000,
    ...patch,
  };
}

describe("sanitizeMedia", () => {
  it("returns null when media is not shared", () => {
    expect(
      sanitizeMedia(mediaInput(), { sharesMedia: false, displayAlias: null }, {
        requiresArtist: false,
      }),
    ).toBeNull();
  });

  it("requiresArtist drops media without artist", () => {
    expect(
      sanitizeMedia(mediaInput({ artist: null }), shareMedia, { requiresArtist: true }),
    ).toBeNull();
    expect(
      sanitizeMedia(mediaInput({ artist: null }), shareMedia, { requiresArtist: false }),
    ).not.toBeNull();
  });

  it("requires title or artist after normalization", () => {
    expect(
      sanitizeMedia(mediaInput({ title: "  ", artist: null }), shareMedia, {
        requiresArtist: false,
      }),
    ).toBeNull();
  });

  it("clamps position to duration and nulls invalid values", () => {
    const out = sanitizeMedia(
      mediaInput({ durationSeconds: 100, positionSeconds: 150 }),
      shareMedia,
      { requiresArtist: false },
    );
    expect(out?.playback.positionSeconds).toBe(100);

    const bad = sanitizeMedia(
      mediaInput({ durationSeconds: Number.NaN, positionSeconds: -5 }),
      shareMedia,
      { requiresArtist: false },
    );
    expect(bad?.playback.durationSeconds).toBeNull();
    expect(bad?.playback.positionSeconds).toBeNull();
  });

  it("preserves a real zero position (null means unavailable, 0 means start)", () => {
    const out = sanitizeMedia(mediaInput({ positionSeconds: 0 }), shareMedia, {
      requiresArtist: false,
    });
    expect(out?.playback.positionSeconds).toBe(0);
  });

  it("derives rate and state from playing flag", () => {
    expect(
      sanitizeMedia(mediaInput({ playing: true }), shareMedia, { requiresArtist: false })
        ?.playback,
    ).toMatchObject({ state: "playing", rate: 1 });
    expect(
      sanitizeMedia(mediaInput({ playing: false }), shareMedia, { requiresArtist: false })
        ?.playback,
    ).toMatchObject({ state: "paused", rate: 0 });
  });

  it("player display name precedence: alias > mapping > raw", () => {
    expect(
      sanitizeMedia(
        mediaInput({ mappedPlayerName: "Mapped" }),
        { sharesMedia: true, displayAlias: "Alias" },
        { requiresArtist: false },
      )?.playerDisplayName,
    ).toBe("Alias");
    expect(
      sanitizeMedia(mediaInput({ mappedPlayerName: "Mapped" }), shareMedia, {
        requiresArtist: false,
      })?.playerDisplayName,
    ).toBe("Mapped");
  });
});
