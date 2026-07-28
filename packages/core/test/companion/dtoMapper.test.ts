import { describe, expect, it } from "vitest";
import {
  makeClearRequest,
  makePresenceRequest,
} from "../../src/companion/protocol/dtoMapper.js";
import { WireError } from "../../src/companion/protocol/wire.js";
import type { SanitizedPresenceSnapshot } from "../../src/privacy/types.js";

const OPTS = {
  deviceId: "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b",
  leaseMinSeconds: 30,
  leaseMaxSeconds: 120,
};

function snapshot(patch: Partial<SanitizedPresenceSnapshot> = {}): SanitizedPresenceSnapshot {
  return {
    observedAt: 1_753_500_012_345,
    application: { displayName: "Code", windowTitle: "file.ts" },
    media: {
      sessionId: "aa6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b",
      kind: "music",
      title: "Song",
      artist: "Artist",
      album: null,
      playerDisplayName: "Spotify",
      playback: {
        state: "playing",
        durationSeconds: 200.5,
        positionSeconds: 60.2504,
        sampledAt: 1_753_500_012_345,
        rate: 1,
      },
    },
    ...patch,
  };
}

describe("makePresenceRequest — wire shape", () => {
  it("serializes every required-nullable key explicitly", () => {
    const { body } = makePresenceRequest(snapshot(), 7, 90, OPTS);
    const json = JSON.parse(JSON.stringify(body)) as Record<string, unknown>;
    const data = json["data"] as unknown as Record<string, unknown>;
    for (const key of ["availability", "lease", "application", "media"]) {
      expect(Object.hasOwn(data, key), `data.${key}`).toBe(true);
    }
    const app = data["application"] as unknown as Record<string, unknown>;
    for (const key of ["displayName", "activity", "window", "icon"]) {
      expect(Object.hasOwn(app, key), `application.${key}`).toBe(true);
    }
    expect(app["activity"]).toBeNull();
    expect(app["icon"]).toBeNull();
    const media = data["media"] as unknown as Record<string, unknown>;
    for (const key of ["sessionId", "kind", "title", "artist", "album", "player", "playback"]) {
      expect(Object.hasOwn(media, key), `media.${key}`).toBe(true);
    }
    expect(media["album"]).toBeNull();
    const playback = media["playback"] as unknown as Record<string, unknown>;
    for (const key of ["state", "durationMs", "positionMs", "sampledAt", "rate"]) {
      expect(Object.hasOwn(playback, key), `playback.${key}`).toBe(true);
    }
  });

  it("omits capability-conditional artwork/link keys entirely", () => {
    const { body } = makePresenceRequest(snapshot(), 1, 90, OPTS);
    const media = JSON.parse(JSON.stringify(body)).data.media as Record<string, unknown>;
    expect(Object.hasOwn(media, "artwork")).toBe(false);
    expect(Object.hasOwn(media, "link")).toBe(false);
  });

  it("null application/media keys still present; availability idle", () => {
    const { body } = makePresenceRequest(
      snapshot({ application: null, media: null }),
      1,
      90,
      OPTS,
    );
    const data = JSON.parse(JSON.stringify(body)).data as Record<string, unknown>;
    expect(data["availability"]).toBe("idle");
    expect(Object.hasOwn(data, "application")).toBe(true);
    expect(data["application"]).toBeNull();
    expect(Object.hasOwn(data, "media")).toBe(true);
    expect(data["media"]).toBeNull();
  });

  it("availability active when either source present", () => {
    expect(
      makePresenceRequest(snapshot({ media: null }), 1, 90, OPTS).body.data.availability,
    ).toBe("active");
  });

  it("meta carries schema constants, deviceId, sequence, canonical observedAt", () => {
    const { body, requestId } = makePresenceRequest(snapshot(), 42, 90, OPTS);
    expect(body.meta.schema).toBe("yohaku.companion.presence");
    expect(body.meta.schemaVersion).toBe(2);
    expect(body.meta.deviceId).toBe(OPTS.deviceId);
    expect(body.meta.sequence).toBe(42);
    expect(body.meta.requestId).toBe(requestId);
    expect(body.meta.observedAt).toBe("2025-07-26T03:20:12.345Z");
  });
});

describe("makePresenceRequest — conversions and limits", () => {
  it("converts seconds to rounded milliseconds and clamps position", () => {
    const { body } = makePresenceRequest(snapshot(), 1, 90, OPTS);
    expect(body.data.media?.playback.durationMs).toBe(200_500);
    expect(body.data.media?.playback.positionMs).toBe(60_250);
  });

  it("keeps null duration/position as null (never 0)", () => {
    const s = snapshot();
    s.media!.playback.durationSeconds = null;
    s.media!.playback.positionSeconds = null;
    const { body } = makePresenceRequest(s, 1, 90, OPTS);
    expect(body.data.media?.playback.durationMs).toBeNull();
    expect(body.data.media?.playback.positionMs).toBeNull();
  });

  it("truncates over-limit text at Unicode scalar boundaries", () => {
    const s = snapshot({
      application: { displayName: "𝒜".repeat(200), windowTitle: "t".repeat(600) },
    });
    const { body } = makePresenceRequest(s, 1, 90, OPTS);
    expect([...body.data.application!.displayName].length).toBe(120);
    expect([...body.data.application!.window!.title].length).toBe(500);
  });

  it("clamps lease ttl into the negotiated range", () => {
    expect(makePresenceRequest(snapshot(), 1, 5, OPTS).body.data.lease.ttlSeconds).toBe(30);
    expect(makePresenceRequest(snapshot(), 1, 999, OPTS).body.data.lease.ttlSeconds).toBe(120);
    expect(makePresenceRequest(snapshot(), 1, 90, OPTS).body.data.lease.ttlSeconds).toBe(90);
  });

  it("rejects paused-with-rate and playing-without-rate", () => {
    const paused = snapshot();
    paused.media!.playback.state = "paused";
    paused.media!.playback.rate = 1;
    expect(() => makePresenceRequest(paused, 1, 90, OPTS)).toThrow(WireError);

    const playing = snapshot();
    playing.media!.playback.rate = 0;
    expect(() => makePresenceRequest(playing, 1, 90, OPTS)).toThrow(WireError);
  });

  it("rejects media without title and artist", () => {
    const s = snapshot();
    s.media!.title = null;
    s.media!.artist = null;
    expect(() => makePresenceRequest(s, 1, 90, OPTS)).toThrow(WireError);
  });
});

describe("makeClearRequest", () => {
  it("carries reason and full meta", () => {
    const { body } = makeClearRequest("sleep", 9, 1_753_500_012_345, OPTS);
    expect(body.data.reason).toBe("sleep");
    expect(body.meta.sequence).toBe(9);
    expect(body.meta.schema).toBe("yohaku.companion.presence");
  });
});
