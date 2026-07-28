import { normalizeText } from "./model.js";
import type { MediaDecision, ProcessDecision } from "./evaluator.js";
import type {
  MediaKind,
  SanitizedApplicationPresence,
  SanitizedMediaPresence,
  SanitizedPlayback,
} from "./types.js";

/**
 * Sanitization boundary, ported from CompanionApplicationPresenceSanitizer /
 * CompanionMediaPresenceSanitizer. These functions deliberately do not accept
 * appIds or executable paths — original process identity cannot pass this
 * boundary by construction. Display name precedence: alias > mapping > raw.
 */

export interface ApplicationSanitizeInput {
  /** Raw display name from the capture layer (FileDescription or exe stem). */
  capturedDisplayName: string;
  /** Result of a process_name mapping hit, if any. */
  mappedDisplayName: string | null;
  windowTitle: string | null;
}

export function sanitizeApplication(
  input: ApplicationSanitizeInput,
  decision: ProcessDecision,
  globalShareWindowTitles: boolean,
): SanitizedApplicationPresence | null {
  if (!decision.sharesApplication) return null;
  const displayName =
    decision.displayAlias ??
    normalizeText(input.mappedDisplayName) ??
    normalizeText(input.capturedDisplayName);
  if (displayName === null) return null;
  const windowTitle =
    decision.sharesWindowTitle && globalShareWindowTitles
      ? normalizeText(input.windowTitle)
      : null;
  return { displayName, windowTitle };
}

export interface MediaSanitizeInput {
  kind: MediaKind;
  title: string | null;
  artist: string | null;
  album: string | null;
  /** Raw player display name from the capture layer. */
  capturedPlayerName: string | null;
  /** Result of a media_process_name mapping hit, if any. */
  mappedPlayerName: string | null;
  playing: boolean;
  durationSeconds: number | null;
  positionSeconds: number | null;
  /** Epoch milliseconds at which position was sampled. */
  sampledAt: number;
}

function normalizedSeconds(value: number | null): number | null {
  if (value === null || !Number.isFinite(value) || value < 0) return null;
  return value;
}

export interface MediaSanitizeOptions {
  /** Global ignoreNullArtist switch: drop media that has no artist. */
  requiresArtist: boolean;
}

/**
 * Returns the sanitized media presence, or null when the media must not be
 * shared (hidden, artist policy, or no meaningful text). The sessionId is
 * assigned by the caller via MediaSessionTracker after sanitization succeeds.
 */
export function sanitizeMedia(
  input: MediaSanitizeInput,
  decision: MediaDecision,
  options: MediaSanitizeOptions,
): Omit<SanitizedMediaPresence, "sessionId"> | null {
  if (!decision.sharesMedia) return null;

  const title = normalizeText(input.title);
  const artist = normalizeText(input.artist);
  const album = normalizeText(input.album);
  if (options.requiresArtist && artist === null) return null;
  if (title === null && artist === null) return null;

  const playerDisplayName =
    decision.displayAlias ??
    normalizeText(input.mappedPlayerName) ??
    normalizeText(input.capturedPlayerName);

  const duration = normalizedSeconds(input.durationSeconds);
  let position = normalizedSeconds(input.positionSeconds);
  if (position !== null && duration !== null && position > duration) {
    position = duration;
  }

  const playback: SanitizedPlayback = {
    state: input.playing ? "playing" : "paused",
    durationSeconds: duration,
    positionSeconds: position,
    sampledAt: input.sampledAt,
    // SMTC does not expose a reliable playback rate; derive from state,
    // matching the macOS sanitizer (playing -> 1, paused -> 0).
    rate: input.playing ? 1 : 0,
  };

  return { kind: input.kind, title, artist, album, playerDisplayName, playback };
}
