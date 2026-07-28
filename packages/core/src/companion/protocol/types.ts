import { z } from "zod";
import {
  MAXIMUM_SAFE_WIRE_INTEGER,
  PRESENCE_SCHEMA,
  PRESENCE_SCHEMA_VERSION,
  decodeWireDate,
  isValidWireIdentifier,
} from "./wire.js";

/**
 * Companion Protocol v2 wire DTOs. Request bodies are built as fully
 * populated literals so every "required nullable" key is serialized
 * explicitly (JSON.stringify keeps `null`, drops only `undefined`).
 * Response envelopes are validated with zod: a required `.nullable()` field
 * enforces key presence, matching the protocol's required-nullable rule.
 */

export type MediaWireKind = "music" | "podcast" | "video" | "unknown";
export type WirePlaybackState = "playing" | "paused";
export type WireAvailability = "idle" | "active";
export type ClearReason =
  | "paused"
  | "sleep"
  | "shutdown"
  | "privacyChanged"
  | "connectionRemoved";

export interface PresenceRequestMeta {
  schema: typeof PRESENCE_SCHEMA;
  schemaVersion: typeof PRESENCE_SCHEMA_VERSION;
  requestId: string;
  deviceId: string;
  sequence: number;
  observedAt: string;
}

export interface WireActivity {
  key: string | null;
  customLabel: string | null;
}

export interface WireApplicationContext {
  displayName: string;
  activity: WireActivity | null;
  window: { title: string } | null;
  icon: { url: string } | null;
}

export interface WirePlayback {
  state: WirePlaybackState;
  durationMs: number | null;
  positionMs: number | null;
  sampledAt: string;
  rate: number;
}

/**
 * Media context. `artwork` and `link` are capability-conditional keys: this
 * client never negotiates mediaArtwork/mediaPlaybackLinks, so the keys are
 * omitted entirely (which the protocol permits), not sent as null.
 */
export interface WireMediaContext {
  sessionId: string;
  kind: MediaWireKind;
  title: string | null;
  artist: string | null;
  album: string | null;
  player: { displayName: string } | null;
  playback: WirePlayback;
}

export interface PresenceRequest {
  meta: PresenceRequestMeta;
  data: {
    availability: WireAvailability;
    lease: { ttlSeconds: number };
    application: WireApplicationContext | null;
    media: WireMediaContext | null;
  };
}

export interface ClearRequest {
  meta: PresenceRequestMeta;
  data: { reason: ClearReason };
}

export interface PairingClaimRequest {
  deviceName: string;
  pairingCode: string;
}

// ---------------------------------------------------------------------------
// Response schemas (zod)
// ---------------------------------------------------------------------------

const wireIdentifier = z
  .string()
  .refine(isValidWireIdentifier, "expected UUID or ULID");

const wireDate = z.string().refine((v) => {
  try {
    decodeWireDate(v);
    return true;
  } catch {
    return false;
  }
}, "expected canonical RFC3339 millisecond UTC date");

const wireInteger = z.number().int().min(0).max(MAXIMUM_SAFE_WIRE_INTEGER);

/**
 * Every response envelope (including capabilities and error envelopes) uses
 * the presence schema constants — a hard decode constraint of the protocol.
 */
export const responseMetaSchema = z.object({
  schema: z.literal(PRESENCE_SCHEMA),
  schemaVersion: z.literal(PRESENCE_SCHEMA_VERSION),
  requestId: wireIdentifier,
  serverTime: wireDate,
});
export type ResponseMeta = z.infer<typeof responseMetaSchema>;

export const capabilitiesDataSchema = z.object({
  minimumClientVersion: z.string(),
  presenceSchemaVersions: z.array(z.number().int()),
  momentSchemaVersions: z.array(z.number().int()),
  features: z.object({
    liveDesk: z.boolean(),
    mediaTimeline: z.boolean(),
    moments: z.boolean(),
    readingSessions: z.boolean(),
    mediaArtwork: z.boolean().optional(),
    mediaPlaybackLinks: z.boolean().optional(),
  }),
  limits: z.object({
    presencePayloadBytes: z.number().int(),
    presenceRequestsPerMinute: z.number().int(),
    presenceLeaseMinSeconds: z.number().int(),
    presenceLeaseMaxSeconds: z.number().int(),
    recommendedHeartbeatSeconds: z.number().int(),
    maximumClockSkewSeconds: z.number().int(),
  }),
});
export type CapabilitiesData = z.infer<typeof capabilitiesDataSchema>;

export const capabilitiesResponseSchema = z.object({
  meta: responseMetaSchema,
  data: capabilitiesDataSchema,
});

export const pairingClaimDataSchema = z.object({
  deviceId: wireIdentifier,
  deviceToken: z.string().refine((v) => v.trim().length > 0, "empty token"),
  scopes: z.array(z.string()),
  nextSequence: wireInteger,
});
export type PairingClaimData = z.infer<typeof pairingClaimDataSchema>;

export const pairingClaimResponseSchema = z.object({
  data: pairingClaimDataSchema,
});

export const publicLiveDeskStateSchema = z.object({
  schemaVersion: z.literal(PRESENCE_SCHEMA_VERSION),
  epoch: wireIdentifier,
  revision: wireInteger,
  projection: z.unknown().nullable(),
});

export const mutationResponseSchema = z.object({
  meta: responseMetaSchema,
  data: z.object({
    acceptedSequence: wireInteger,
    receivedAt: wireDate,
    state: publicLiveDeskStateSchema,
  }),
});
export type MutationResponse = z.infer<typeof mutationResponseSchema>;

export const errorEnvelopeSchema = z.object({
  meta: responseMetaSchema,
  error: z.object({
    code: z.string(),
    message: z.string(),
    retryable: z.boolean(),
    retryAfterMs: wireInteger.nullable(),
    acceptedSequence: wireInteger.nullable(),
    fields: z.array(z.string()),
  }),
});
export type ErrorEnvelope = z.infer<typeof errorEnvelopeSchema>;

/** Pairing error envelope has a simpler shape: { error: { code } }. */
export const pairingErrorEnvelopeSchema = z.object({
  error: z.object({ code: z.string() }),
});

export const REQUIRED_PRESENCE_SCOPE = "companion:presence:write";

export const MUTATION_RENEGOTIATE_CODES = new Set([
  "COMPANION_SCHEMA_UNSUPPORTED",
  "COMPANION_FEATURE_UNAVAILABLE",
]);
