import type { PreviewProjection } from "@yohaku/shared";
import type { SanitizedPresenceSnapshot } from "../privacy/types.js";

/**
 * Consent gate, ported from CompanionPreviewConsentGate.swift.
 *
 * A confirmation binds the policy fingerprint under which the user reviewed
 * the preview to the exact projection they saw. Enabling Live Desk validates
 * three things: the candidate equals the latest recorded confirmation, its
 * fingerprint matches the current policy, and the projection still equals the
 * projection of a fresh capture. Any policy change clears the confirmation.
 *
 * The projection deliberately excludes observedAt, media sessionId, position
 * and sampledAt: natural playback progress is continuity, not new disclosure.
 * Track changes, pause/play flips, duration and rate changes all invalidate.
 */

export interface ConsentConfirmation {
  policyFingerprint: string;
  projection: PreviewProjection;
}

export function projectionOf(
  snapshot: SanitizedPresenceSnapshot,
): PreviewProjection {
  return {
    application:
      snapshot.application === null
        ? null
        : {
            displayName: snapshot.application.displayName,
            windowTitle: snapshot.application.windowTitle,
          },
    media:
      snapshot.media === null
        ? null
        : {
            kind: snapshot.media.kind,
            title: snapshot.media.title,
            artist: snapshot.media.artist,
            album: snapshot.media.album,
            playerDisplayName: snapshot.media.playerDisplayName,
            playback: {
              state: snapshot.media.playback.state,
              durationSeconds: snapshot.media.playback.durationSeconds,
              rate: snapshot.media.playback.rate,
            },
          },
  };
}

function deepEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) {
    return false;
  }
  const keysA = Object.keys(a as Record<string, unknown>);
  const keysB = Object.keys(b as Record<string, unknown>);
  if (keysA.length !== keysB.length) return false;
  return keysA.every((key) =>
    deepEqual(
      (a as Record<string, unknown>)[key],
      (b as Record<string, unknown>)[key],
    ),
  );
}

export class ConsentGate {
  private currentFingerprint: string;
  private confirmation: ConsentConfirmation | null = null;

  constructor(initialFingerprint: string) {
    this.currentFingerprint = initialFingerprint;
  }

  get fingerprint(): string {
    return this.currentFingerprint;
  }

  /** Any effective policy change invalidates the recorded confirmation. */
  policyDidChange(fingerprint: string): void {
    if (fingerprint === this.currentFingerprint) return;
    this.currentFingerprint = fingerprint;
    this.confirmation = null;
  }

  /** Records the projection the user is currently looking at. */
  record(projection: PreviewProjection): ConsentConfirmation {
    this.confirmation = {
      policyFingerprint: this.currentFingerprint,
      projection,
    };
    return this.confirmation;
  }

  clear(): void {
    this.confirmation = null;
  }

  /**
   * True only when the candidate is the latest recorded confirmation, was
   * given under the current policy, and still matches the current capture.
   */
  validates(
    candidate: ConsentConfirmation,
    currentProjection: PreviewProjection,
  ): boolean {
    return (
      this.confirmation !== null &&
      deepEqual(candidate, this.confirmation) &&
      candidate.policyFingerprint === this.currentFingerprint &&
      deepEqual(candidate.projection, currentProjection)
    );
  }
}
