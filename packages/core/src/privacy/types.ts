/**
 * Sanitized domain model: the only shape allowed to flow toward the network,
 * the preview UI, or persistence. By construction these types have no field
 * for executable paths, appIds, process IDs, or raw capture objects.
 */

export type MediaKind = "music" | "podcast" | "video" | "unknown";
export type PlaybackState = "playing" | "paused";

export interface SanitizedApplicationPresence {
  displayName: string;
  windowTitle: string | null;
}

export interface SanitizedPlayback {
  state: PlaybackState;
  durationSeconds: number | null;
  positionSeconds: number | null;
  /** Epoch milliseconds at which position was sampled. */
  sampledAt: number;
  rate: number;
}

export interface SanitizedMediaPresence {
  /** Stable per-semantic-session UUID; never derived from content. */
  sessionId: string;
  kind: MediaKind;
  title: string | null;
  artist: string | null;
  album: string | null;
  playerDisplayName: string | null;
  playback: SanitizedPlayback;
}

export interface SanitizedPresenceSnapshot {
  /** Epoch milliseconds at which the snapshot was captured. */
  observedAt: number;
  application: SanitizedApplicationPresence | null;
  media: SanitizedMediaPresence | null;
}
