import { describe, expect, it } from "vitest";
import { ConsentGate, projectionOf } from "../../src/companion/consentGate.js";
import type { SanitizedPresenceSnapshot } from "../../src/privacy/types.js";

function snapshot(patch: Partial<SanitizedPresenceSnapshot> = {}): SanitizedPresenceSnapshot {
  return {
    observedAt: 1_753_500_000_000,
    application: { displayName: "Code", windowTitle: null },
    media: {
      sessionId: "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b",
      kind: "music",
      title: "Song",
      artist: "Artist",
      album: "Album",
      playerDisplayName: "Spotify",
      playback: {
        state: "playing",
        durationSeconds: 200,
        positionSeconds: 60,
        sampledAt: 1_753_500_000_000,
        rate: 1,
      },
    },
    ...patch,
  };
}

describe("projectionOf", () => {
  it("excludes observedAt, sessionId, position and sampledAt", () => {
    const a = projectionOf(snapshot());
    const b = projectionOf(
      snapshot({
        observedAt: 9_999_999_999_999,
        media: {
          ...snapshot().media!,
          sessionId: "00000000-0000-4000-8000-000000000000",
          playback: { ...snapshot().media!.playback, positionSeconds: 190, sampledAt: 1 },
        },
      }),
    );
    expect(a).toEqual(b);
  });

  it("differs when duration, state, rate, track or app changes", () => {
    const base = projectionOf(snapshot());
    const changedTrack = projectionOf(
      snapshot({ media: { ...snapshot().media!, title: "Other" } }),
    );
    const changedState = projectionOf(
      snapshot({
        media: {
          ...snapshot().media!,
          playback: { ...snapshot().media!.playback, state: "paused", rate: 0 },
        },
      }),
    );
    const changedApp = projectionOf(
      snapshot({ application: { displayName: "Другое", windowTitle: null } }),
    );
    expect(changedTrack).not.toEqual(base);
    expect(changedState).not.toEqual(base);
    expect(changedApp).not.toEqual(base);
  });
});

describe("ConsentGate", () => {
  it("starts without confirmation and rejects unrecorded candidates", () => {
    const gate = new ConsentGate("fp1");
    const projection = projectionOf(snapshot());
    expect(
      gate.validates({ policyFingerprint: "fp1", projection }, projection),
    ).toBe(false);
  });

  it("validates recorded confirmation against an equal fresh capture", () => {
    const gate = new ConsentGate("fp1");
    const projection = projectionOf(snapshot());
    const confirmation = gate.record(projection);
    // Fresh capture with progressed position — projection unchanged.
    const fresh = projectionOf(
      snapshot({
        media: {
          ...snapshot().media!,
          playback: { ...snapshot().media!.playback, positionSeconds: 120 },
        },
      }),
    );
    expect(gate.validates(confirmation, fresh)).toBe(true);
  });

  it("policy change invalidates a recorded confirmation", () => {
    const gate = new ConsentGate("fp1");
    const projection = projectionOf(snapshot());
    const confirmation = gate.record(projection);
    gate.policyDidChange("fp2");
    expect(gate.validates(confirmation, projection)).toBe(false);
  });

  it("same-fingerprint policyDidChange keeps confirmation", () => {
    const gate = new ConsentGate("fp1");
    const projection = projectionOf(snapshot());
    const confirmation = gate.record(projection);
    gate.policyDidChange("fp1");
    expect(gate.validates(confirmation, projection)).toBe(true);
  });

  it("rejects when the current capture drifted semantically", () => {
    const gate = new ConsentGate("fp1");
    const confirmation = gate.record(projectionOf(snapshot()));
    const drifted = projectionOf(
      snapshot({ media: { ...snapshot().media!, title: "New Track" } }),
    );
    expect(gate.validates(confirmation, drifted)).toBe(false);
  });

  it("rejects a stale candidate after re-recording", () => {
    const gate = new ConsentGate("fp1");
    const old = gate.record(projectionOf(snapshot()));
    const projection2 = projectionOf(
      snapshot({ media: { ...snapshot().media!, title: "Second" } }),
    );
    gate.record(projection2);
    expect(gate.validates(old, projectionOf(snapshot()))).toBe(false);
  });
});
