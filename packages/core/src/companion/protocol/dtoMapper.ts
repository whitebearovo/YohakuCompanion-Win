import { randomUUID } from "node:crypto";
import type { SanitizedPresenceSnapshot } from "../../privacy/types.js";
import type {
  ClearReason,
  ClearRequest,
  PresenceRequest,
  PresenceRequestMeta,
  WireApplicationContext,
  WireMediaContext,
} from "./types.js";
import {
  PRESENCE_SCHEMA,
  PRESENCE_SCHEMA_VERSION,
  WireError,
  encodeWireDate,
  secondsToWireMilliseconds,
} from "./wire.js";

/**
 * Sanitized domain snapshot -> wire request. This is the last validation
 * boundary before the network, ported from CompanionPresenceDTOMapper.swift:
 * field length limits (Unicode scalars), seconds->milliseconds conversion,
 * explicit nullable encoding, playback state/rate consistency, lease clamp.
 *
 * Length policy: over-limit text is truncated at a scalar boundary (the
 * sanitizer upstream produces reasonable values; truncation keeps a publish
 * alive rather than failing it).
 */

export const DISPLAY_NAME_LIMIT = 120;
export const WINDOW_TITLE_LIMIT = 500;
export const MEDIA_TEXT_LIMIT = 300;

function truncateScalars(value: string, limit: number): string {
  const scalars = [...value];
  return scalars.length <= limit ? value : scalars.slice(0, limit).join("");
}

function boundedText(value: string | null, limit: number): string | null {
  if (value === null) return null;
  const truncated = truncateScalars(value, limit).trim();
  return truncated.length === 0 ? null : truncated;
}

export interface PresenceMapperOptions {
  deviceId: string;
  leaseMinSeconds: number;
  leaseMaxSeconds: number;
}

export interface MappedRequest<T> {
  requestId: string;
  body: T;
}

function makeMeta(
  deviceId: string,
  sequence: number,
  observedAtMs: number,
): PresenceRequestMeta {
  return {
    schema: PRESENCE_SCHEMA,
    schemaVersion: PRESENCE_SCHEMA_VERSION,
    requestId: randomUUID(),
    deviceId,
    sequence,
    observedAt: encodeWireDate(observedAtMs),
  };
}

function mapApplication(
  snapshot: SanitizedPresenceSnapshot,
): WireApplicationContext | null {
  if (snapshot.application === null) return null;
  const displayName = boundedText(
    snapshot.application.displayName,
    DISPLAY_NAME_LIMIT,
  );
  if (displayName === null) {
    throw new WireError("application displayName empty after bounding");
  }
  const title = boundedText(snapshot.application.windowTitle, WINDOW_TITLE_LIMIT);
  return {
    displayName,
    activity: null,
    window: title === null ? null : { title },
    icon: null,
  };
}

function mapMedia(snapshot: SanitizedPresenceSnapshot): WireMediaContext | null {
  const media = snapshot.media;
  if (media === null) return null;

  const title = boundedText(media.title, MEDIA_TEXT_LIMIT);
  const artist = boundedText(media.artist, MEDIA_TEXT_LIMIT);
  const album = boundedText(media.album, MEDIA_TEXT_LIMIT);
  if (title === null && artist === null) {
    throw new WireError("media requires title or artist");
  }
  const playerDisplayName = boundedText(media.playerDisplayName, DISPLAY_NAME_LIMIT);

  const { playback } = media;
  if (!Number.isFinite(playback.rate) || playback.rate < 0 || playback.rate > 4) {
    throw new WireError(`playback rate out of range: ${playback.rate}`);
  }
  if (playback.state === "paused" && playback.rate !== 0) {
    throw new WireError("paused playback must have rate 0");
  }
  if (playback.state === "playing" && playback.rate <= 0) {
    throw new WireError("playing playback must have rate > 0");
  }

  const durationMs =
    playback.durationSeconds === null
      ? null
      : secondsToWireMilliseconds(playback.durationSeconds, "durationMs");
  let positionMs =
    playback.positionSeconds === null
      ? null
      : secondsToWireMilliseconds(playback.positionSeconds, "positionMs");
  if (positionMs !== null && durationMs !== null && positionMs > durationMs) {
    positionMs = durationMs;
  }

  return {
    sessionId: media.sessionId,
    kind: media.kind,
    title,
    artist,
    album,
    player: playerDisplayName === null ? null : { displayName: playerDisplayName },
    playback: {
      state: playback.state,
      durationMs,
      positionMs,
      sampledAt: encodeWireDate(playback.sampledAt),
      rate: playback.rate,
    },
  };
}

export function makePresenceRequest(
  snapshot: SanitizedPresenceSnapshot,
  sequence: number,
  requestedLeaseSeconds: number,
  options: PresenceMapperOptions,
): MappedRequest<PresenceRequest> {
  const application = mapApplication(snapshot);
  const media = mapMedia(snapshot);
  const ttlSeconds = Math.min(
    Math.max(Math.round(requestedLeaseSeconds), options.leaseMinSeconds),
    options.leaseMaxSeconds,
  );
  const meta = makeMeta(options.deviceId, sequence, snapshot.observedAt);
  return {
    requestId: meta.requestId,
    body: {
      meta,
      data: {
        availability: application === null && media === null ? "idle" : "active",
        lease: { ttlSeconds },
        application,
        media,
      },
    },
  };
}

export function makeClearRequest(
  reason: ClearReason,
  sequence: number,
  observedAtMs: number,
  options: PresenceMapperOptions,
): MappedRequest<ClearRequest> {
  const meta = makeMeta(options.deviceId, sequence, observedAtMs);
  return { requestId: meta.requestId, body: { meta, data: { reason } } };
}
