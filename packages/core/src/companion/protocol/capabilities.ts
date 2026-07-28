import type { CapabilitiesData } from "./types.js";
import { PRESENCE_SCHEMA_VERSION } from "./wire.js";

/**
 * Capability negotiation, ported from CompanionCapabilityNegotiator.swift,
 * including its strict SemVer 2.0 comparator (prerelease ordering, no leading
 * zeros in numeric identifiers, build metadata ignored).
 */

export interface SemanticVersion {
  major: number;
  minor: number;
  patch: number;
  prerelease: string[];
}

const NUMERIC_ID = /^(0|[1-9]\d*)$/;
const ALNUM_ID = /^[0-9A-Za-z-]+$/;

export function parseSemanticVersion(input: string): SemanticVersion | null {
  let rest = input;
  const plus = rest.indexOf("+");
  if (plus >= 0) {
    const build = rest.slice(plus + 1);
    if (build.length === 0) return null;
    if (!build.split(".").every((id) => id.length > 0 && ALNUM_ID.test(id))) {
      return null;
    }
    rest = rest.slice(0, plus);
  }
  let prerelease: string[] = [];
  const dash = rest.indexOf("-");
  if (dash >= 0) {
    const pre = rest.slice(dash + 1);
    if (pre.length === 0) return null;
    prerelease = pre.split(".");
    for (const id of prerelease) {
      if (id.length === 0 || !ALNUM_ID.test(id)) return null;
      if (/^\d+$/.test(id) && !NUMERIC_ID.test(id)) return null; // leading zero
    }
    rest = rest.slice(0, dash);
  }
  const core = rest.split(".");
  if (core.length !== 3) return null;
  const nums: number[] = [];
  for (const part of core) {
    if (!NUMERIC_ID.test(part)) return null;
    nums.push(Number.parseInt(part, 10));
  }
  return { major: nums[0]!, minor: nums[1]!, patch: nums[2]!, prerelease };
}

/** Returns negative/zero/positive like a comparator; build metadata ignored. */
export function compareSemanticVersions(a: SemanticVersion, b: SemanticVersion): number {
  if (a.major !== b.major) return a.major - b.major;
  if (a.minor !== b.minor) return a.minor - b.minor;
  if (a.patch !== b.patch) return a.patch - b.patch;
  const preA = a.prerelease;
  const preB = b.prerelease;
  if (preA.length === 0 && preB.length === 0) return 0;
  if (preA.length === 0) return 1; // release > prerelease
  if (preB.length === 0) return -1;
  const len = Math.min(preA.length, preB.length);
  for (let i = 0; i < len; i += 1) {
    const idA = preA[i]!;
    const idB = preB[i]!;
    const numA = /^\d+$/.test(idA);
    const numB = /^\d+$/.test(idB);
    if (numA && numB) {
      const diff = Number.parseInt(idA, 10) - Number.parseInt(idB, 10);
      if (diff !== 0) return diff;
    } else if (numA !== numB) {
      return numA ? -1 : 1; // numeric < textual
    } else if (idA !== idB) {
      return idA < idB ? -1 : 1;
    }
  }
  return preA.length - preB.length; // shorter (equal prefix) is smaller
}

export interface NegotiatedPresenceConfiguration {
  supportsMediaTimeline: boolean;
  maximumPayloadBytes: number;
  requestsPerMinute: number;
  leaseMinSeconds: number;
  leaseMaxSeconds: number;
  recommendedHeartbeatSeconds: number;
  maximumClockSkewSeconds: number;
}

export type PresenceNegotiation =
  | { kind: "available"; configuration: NegotiatedPresenceConfiguration }
  | { kind: "clientUpdateRequired" }
  | { kind: "schemaUnsupported" }
  | { kind: "featureUnavailable" }
  | { kind: "invalidCapabilities" };

function limitsAreValid(limits: CapabilitiesData["limits"]): boolean {
  return (
    limits.presencePayloadBytes > 0 &&
    limits.presenceRequestsPerMinute > 0 &&
    limits.presenceLeaseMinSeconds > 0 &&
    limits.presenceLeaseMinSeconds <= limits.presenceLeaseMaxSeconds &&
    limits.presenceLeaseMinSeconds <= limits.recommendedHeartbeatSeconds &&
    limits.recommendedHeartbeatSeconds <= limits.presenceLeaseMaxSeconds &&
    limits.maximumClockSkewSeconds >= 0
  );
}

export function negotiatePresence(
  capabilities: CapabilitiesData,
  clientVersion: string,
): PresenceNegotiation {
  const client = parseSemanticVersion(clientVersion);
  const minimum = parseSemanticVersion(capabilities.minimumClientVersion);
  if (
    client === null ||
    minimum === null ||
    capabilities.presenceSchemaVersions.some((v) => v <= 0) ||
    capabilities.momentSchemaVersions.some((v) => v <= 0) ||
    !limitsAreValid(capabilities.limits)
  ) {
    return { kind: "invalidCapabilities" };
  }
  if (compareSemanticVersions(client, minimum) < 0) {
    return { kind: "clientUpdateRequired" };
  }
  if (!capabilities.presenceSchemaVersions.includes(PRESENCE_SCHEMA_VERSION)) {
    return { kind: "schemaUnsupported" };
  }
  if (!capabilities.features.liveDesk) {
    return { kind: "featureUnavailable" };
  }
  return {
    kind: "available",
    configuration: {
      supportsMediaTimeline: capabilities.features.mediaTimeline,
      maximumPayloadBytes: capabilities.limits.presencePayloadBytes,
      requestsPerMinute: capabilities.limits.presenceRequestsPerMinute,
      leaseMinSeconds: capabilities.limits.presenceLeaseMinSeconds,
      leaseMaxSeconds: capabilities.limits.presenceLeaseMaxSeconds,
      recommendedHeartbeatSeconds: capabilities.limits.recommendedHeartbeatSeconds,
      maximumClockSkewSeconds: capabilities.limits.maximumClockSkewSeconds,
    },
  };
}
