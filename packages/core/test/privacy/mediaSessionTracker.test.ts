import { describe, expect, it } from "vitest";
import {
  MediaSessionTracker,
  type MediaSemanticIdentity,
} from "../../src/privacy/mediaSessionTracker.js";

const identity: MediaSemanticIdentity = {
  kind: "music",
  title: "Song",
  artist: "Artist",
  album: "Album",
  playerDisplayName: "Spotify",
  durationSeconds: 200,
};

describe("MediaSessionTracker", () => {
  it("keeps the same sessionId for the same semantic identity", () => {
    const t = new MediaSessionTracker();
    expect(t.sessionId(identity)).toBe(t.sessionId({ ...identity }));
  });

  it("mints a new sessionId when identity changes", () => {
    const t = new MediaSessionTracker();
    const a = t.sessionId(identity);
    const b = t.sessionId({ ...identity, title: "Other" });
    expect(b).not.toBe(a);
  });

  it("mints a new sessionId after reset (continuity break)", () => {
    const t = new MediaSessionTracker();
    const a = t.sessionId(identity);
    t.reset();
    expect(t.sessionId(identity)).not.toBe(a);
  });

  it("sessionIds are UUIDs, not content hashes", () => {
    const t1 = new MediaSessionTracker();
    const t2 = new MediaSessionTracker();
    expect(t1.sessionId(identity)).not.toBe(t2.sessionId(identity));
    expect(t1.sessionId(identity)).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/,
    );
  });
});
