import { randomUUID } from "node:crypto";
import type { MediaKind } from "./types.js";

/**
 * Assigns a stable random session UUID per media "semantic identity"
 * (kind + title + artist + album + player + duration). The same track keeps
 * its sessionId across position updates; any semantic change, or an explicit
 * continuity reset (pause-hide, generation change), mints a new one. The id
 * is never derived from content — it must not be a fingerprintable hash.
 */

export interface MediaSemanticIdentity {
  kind: MediaKind;
  title: string | null;
  artist: string | null;
  album: string | null;
  playerDisplayName: string | null;
  durationSeconds: number | null;
}

function identityKey(identity: MediaSemanticIdentity): string {
  return JSON.stringify([
    identity.kind,
    identity.title,
    identity.artist,
    identity.album,
    identity.playerDisplayName,
    identity.durationSeconds,
  ]);
}

export class MediaSessionTracker {
  private current: { key: string; sessionId: string } | null = null;

  sessionId(identity: MediaSemanticIdentity): string {
    const key = identityKey(identity);
    if (this.current !== null && this.current.key === key) {
      return this.current.sessionId;
    }
    this.current = { key, sessionId: randomUUID() };
    return this.current.sessionId;
  }

  /** Called when media continuity breaks (hidden, stopped, new generation). */
  reset(): void {
    this.current = null;
  }
}
