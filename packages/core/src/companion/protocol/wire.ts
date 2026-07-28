/**
 * Wire-level primitives for Companion Protocol v2, ported from
 * CompanionProtocolV2.swift. Everything here is deliberately strict:
 * non-canonical input is a protocol error, not something to repair.
 */

/** 2^53 - 1: every integer on the wire must fit JavaScript's safe range. */
export const MAXIMUM_SAFE_WIRE_INTEGER = 9_007_199_254_740_991;

export const PRESENCE_SCHEMA = "yohaku.companion.presence";
export const PRESENCE_SCHEMA_VERSION = 2;

/**
 * Protocol client version sent as X-Yohaku-Companion-Version and compared
 * against the server's minimumClientVersion. Deliberately tracks the original
 * companion's release line (>= 1.7.3 per the Core minimum-client contract),
 * decoupled from this application's own version.
 */
export const PROTOCOL_CLIENT_VERSION = "1.8.3";

const RFC3339_MS_UTC = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/;

/** Encodes epoch milliseconds as RFC3339 UTC with exactly 3 fractional digits. */
export function encodeWireDate(epochMs: number): string {
  if (!Number.isFinite(epochMs)) throw new WireError("non-finite date");
  const encoded = new Date(epochMs).toISOString();
  if (!RFC3339_MS_UTC.test(encoded)) throw new WireError(`non-canonical date: ${encoded}`);
  return encoded;
}

/**
 * Decodes a wire date, rejecting anything that does not round-trip to the
 * exact canonical form (no offset forms, no missing/extra fraction digits).
 */
export function decodeWireDate(value: string): number {
  if (!RFC3339_MS_UTC.test(value)) throw new WireError(`invalid wire date: ${value}`);
  const parsed = Date.parse(value);
  if (Number.isNaN(parsed)) throw new WireError(`unparseable wire date: ${value}`);
  if (new Date(parsed).toISOString() !== value) {
    throw new WireError(`non-canonical wire date: ${value}`);
  }
  return parsed;
}

const UUID_RE =
  /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;
const ULID_RE = /^[0-9A-HJKMNP-TV-Z]{26}$/;

/** Identifiers on the wire are UUIDs or Crockford-Base32 ULIDs. */
export function isValidWireIdentifier(value: string): boolean {
  return UUID_RE.test(value) || ULID_RE.test(value);
}

/** Validates an integer wire field: integral, 0..2^53-1. */
export function requireWireInteger(value: number, field: string): number {
  if (
    !Number.isInteger(value) ||
    value < 0 ||
    value > MAXIMUM_SAFE_WIRE_INTEGER
  ) {
    throw new WireError(`integer out of wire range for ${field}: ${value}`);
  }
  return value;
}

/** Converts non-negative finite seconds to integer milliseconds (round). */
export function secondsToWireMilliseconds(seconds: number, field: string): number {
  if (!Number.isFinite(seconds) || seconds < 0) {
    throw new WireError(`invalid seconds for ${field}: ${seconds}`);
  }
  return requireWireInteger(Math.round(seconds * 1000), field);
}

export class WireError extends Error {
  override readonly name = "WireError";
}

/**
 * Marker used by the JSON encoder for "required nullable" keys: the key must
 * always be present on the wire; absence and null are different states.
 * Plain `null` in our request objects means exactly that — we build request
 * bodies as fully-populated literals, so JSON.stringify keeps every key.
 * This helper exists for decode-side checks.
 */
export function requireKey<T extends object>(
  obj: T,
  key: string,
  context: string,
): unknown {
  if (!Object.prototype.hasOwnProperty.call(obj, key)) {
    throw new WireError(`missing required key "${key}" in ${context}`);
  }
  return (obj as Record<string, unknown>)[key];
}
