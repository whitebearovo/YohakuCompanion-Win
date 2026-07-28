import { describe, expect, it } from "vitest";
import {
  compareSemanticVersions,
  negotiatePresence,
  parseSemanticVersion,
} from "../../src/companion/protocol/capabilities.js";
import type { CapabilitiesData } from "../../src/companion/protocol/types.js";

describe("parseSemanticVersion", () => {
  it("parses core, prerelease and build", () => {
    expect(parseSemanticVersion("1.8.3")).toEqual({
      major: 1,
      minor: 8,
      patch: 3,
      prerelease: [],
    });
    expect(parseSemanticVersion("1.0.0-alpha.1+build.5")?.prerelease).toEqual([
      "alpha",
      "1",
    ]);
  });
  const invalid = ["1.8", "1.8.3.4", "01.0.0", "1.0.0-alpha.01", "1.0.0-", "1.0.0+", "v1.0.0", "1.0.x"];
  for (const v of invalid) {
    it(`rejects ${JSON.stringify(v)}`, () => {
      expect(parseSemanticVersion(v)).toBeNull();
    });
  }
});

describe("compareSemanticVersions (semver 2.0 ordering)", () => {
  const ordered = [
    "1.0.0-alpha",
    "1.0.0-alpha.1",
    "1.0.0-alpha.beta",
    "1.0.0-beta",
    "1.0.0-beta.2",
    "1.0.0-beta.11",
    "1.0.0-rc.1",
    "1.0.0",
    "1.0.1",
    "1.1.0",
    "2.0.0",
  ];
  it("orders the canonical semver example chain", () => {
    for (let i = 0; i < ordered.length - 1; i += 1) {
      const a = parseSemanticVersion(ordered[i]!)!;
      const b = parseSemanticVersion(ordered[i + 1]!)!;
      expect(compareSemanticVersions(a, b)).toBeLessThan(0);
      expect(compareSemanticVersions(b, a)).toBeGreaterThan(0);
    }
  });
  it("ignores build metadata", () => {
    const a = parseSemanticVersion("1.0.0+a")!;
    const b = parseSemanticVersion("1.0.0+b")!;
    expect(compareSemanticVersions(a, b)).toBe(0);
  });
});

function capabilities(patch: Partial<CapabilitiesData> = {}): CapabilitiesData {
  return {
    minimumClientVersion: "1.7.0",
    presenceSchemaVersions: [2],
    momentSchemaVersions: [1],
    features: {
      liveDesk: true,
      mediaTimeline: true,
      moments: true,
      readingSessions: false,
    },
    limits: {
      presencePayloadBytes: 32768,
      presenceRequestsPerMinute: 30,
      presenceLeaseMinSeconds: 30,
      presenceLeaseMaxSeconds: 120,
      recommendedHeartbeatSeconds: 45,
      maximumClockSkewSeconds: 60,
    },
    ...patch,
  };
}

describe("negotiatePresence", () => {
  it("returns available with the negotiated configuration", () => {
    const result = negotiatePresence(capabilities(), "1.8.3");
    expect(result).toEqual({
      kind: "available",
      configuration: {
        supportsMediaTimeline: true,
        maximumPayloadBytes: 32768,
        requestsPerMinute: 30,
        leaseMinSeconds: 30,
        leaseMaxSeconds: 120,
        recommendedHeartbeatSeconds: 45,
        maximumClockSkewSeconds: 60,
      },
    });
  });

  it("clientUpdateRequired when below minimumClientVersion", () => {
    expect(
      negotiatePresence(capabilities({ minimumClientVersion: "2.0.0" }), "1.8.3").kind,
    ).toBe("clientUpdateRequired");
  });

  it("schemaUnsupported when v2 missing", () => {
    expect(
      negotiatePresence(capabilities({ presenceSchemaVersions: [3] }), "1.8.3").kind,
    ).toBe("schemaUnsupported");
  });

  it("featureUnavailable when liveDesk off", () => {
    expect(
      negotiatePresence(
        capabilities({
          features: {
            liveDesk: false,
            mediaTimeline: true,
            moments: true,
            readingSessions: false,
          },
        }),
        "1.8.3",
      ).kind,
    ).toBe("featureUnavailable");
  });

  const invalidLimits: Array<[string, Partial<CapabilitiesData["limits"]>]> = [
    ["zero payload", { presencePayloadBytes: 0 }],
    ["zero rpm", { presenceRequestsPerMinute: 0 }],
    ["zero lease min", { presenceLeaseMinSeconds: 0 }],
    ["lease min > max", { presenceLeaseMinSeconds: 200 }],
    ["heartbeat below lease min", { recommendedHeartbeatSeconds: 10 }],
    ["heartbeat above lease max", { recommendedHeartbeatSeconds: 500 }],
    ["negative skew", { maximumClockSkewSeconds: -1 }],
  ];
  for (const [name, patch] of invalidLimits) {
    it(`invalidCapabilities: ${name}`, () => {
      const caps = capabilities();
      expect(
        negotiatePresence(
          capabilities({ limits: { ...caps.limits, ...patch } }),
          "1.8.3",
        ).kind,
      ).toBe("invalidCapabilities");
    });
  }

  it("invalidCapabilities on unparseable versions or non-positive schema versions", () => {
    expect(
      negotiatePresence(capabilities({ minimumClientVersion: "1.7" }), "1.8.3").kind,
    ).toBe("invalidCapabilities");
    expect(negotiatePresence(capabilities(), "not-a-version").kind).toBe(
      "invalidCapabilities",
    );
    expect(
      negotiatePresence(capabilities({ presenceSchemaVersions: [0, 2] }), "1.8.3").kind,
    ).toBe("invalidCapabilities");
  });
});
