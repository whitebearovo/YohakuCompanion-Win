import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { PresenceClient } from "../../src/companion/presenceClient.js";
import {
  CompanionSequencer,
  type SequencePersistence,
} from "../../src/companion/sequencer.js";
import {
  CompanionHttpClient,
  CompanionServerConfiguration,
} from "../../src/companion/transport/httpClient.js";
import {
  CompanionServerError,
  needsRenegotiation,
} from "../../src/companion/transport/errors.js";
import type { SanitizedPresenceSnapshot } from "../../src/privacy/types.js";
import {
  MockCompanionServer,
  errorEnvelope,
  mutationSuccess,
} from "../helpers/mockServer.js";

class MemoryPersistence implements SequencePersistence {
  values = new Map<string, number>();
  async load(deviceId: string): Promise<number | null> {
    return this.values.get(deviceId) ?? null;
  }
  async store(deviceId: string, next: number): Promise<void> {
    this.values.set(deviceId, next);
  }
}

const DEVICE = "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b";
const CONFIG = {
  supportsMediaTimeline: true,
  maximumPayloadBytes: 32768,
  requestsPerMinute: 120,
  leaseMinSeconds: 30,
  leaseMaxSeconds: 120,
  recommendedHeartbeatSeconds: 45,
  maximumClockSkewSeconds: 60,
};

function snapshot(): SanitizedPresenceSnapshot {
  return {
    observedAt: 1_753_500_012_345,
    application: { displayName: "Code", windowTitle: null },
    media: null,
  };
}

describe("PresenceClient", () => {
  let server: MockCompanionServer;
  let persistence: MemoryPersistence;
  let client: PresenceClient;

  beforeEach(async () => {
    server = new MockCompanionServer();
    await server.start();
    persistence = new MemoryPersistence();
    const sequencer = new CompanionSequencer(persistence, DEVICE, 5);
    client = new PresenceClient(
      new CompanionHttpClient(new CompanionServerConfiguration(server.baseUrl)),
      { deviceId: DEVICE, deviceToken: "secret-token" },
      sequencer,
      CONFIG,
    );
  });

  afterEach(async () => {
    await server.stop();
  });

  it("PUTs to /companion/presence with Bearer token and version header", async () => {
    server.setFallback((req) => mutationSuccess(req));
    await client.replacePresence(snapshot(), 90);
    const req = server.requests[0]!;
    expect(req.method).toBe("PUT");
    expect(req.path).toBe("/companion/presence");
    expect(req.headers.authorization).toBe("Bearer secret-token");
    expect(req.headers["x-yohaku-companion-version"]).toBe("1.8.3");
    const body = req.json as { meta: { sequence: number } };
    expect(body.meta.sequence).toBe(5);
  });

  it("retries EXACTLY once with byte-identical body on retryable 5xx", async () => {
    server.enqueue((req) => errorEnvelope(req, 500, "INTERNAL_ERROR", { retryable: true }));
    server.setFallback((req) => mutationSuccess(req));
    await client.replacePresence(snapshot(), 90);
    expect(server.requests).toHaveLength(2);
    expect(server.requests[1]!.rawBody).toBe(server.requests[0]!.rawBody);
    // Same sequence + requestId, no new sequence allocated for the retry.
    const first = server.requests[0]!.json as { meta: { sequence: number; requestId: string } };
    const second = server.requests[1]!.json as { meta: { sequence: number; requestId: string } };
    expect(second.meta.sequence).toBe(first.meta.sequence);
    expect(second.meta.requestId).toBe(first.meta.requestId);
  });

  it("retries once on ambiguous transport failure (socket destroyed)", async () => {
    server.enqueue(() => ({ socketDestroy: true }));
    server.setFallback((req) => mutationSuccess(req));
    await client.replacePresence(snapshot(), 90);
    expect(server.requests).toHaveLength(2);
    expect(server.requests[1]!.rawBody).toBe(server.requests[0]!.rawBody);
  });

  it("does not retry non-retryable 5xx", async () => {
    server.enqueue((req) => errorEnvelope(req, 500, "INTERNAL_ERROR", { retryable: false }));
    await expect(client.replacePresence(snapshot(), 90)).rejects.toThrow(
      CompanionServerError,
    );
    expect(server.requests).toHaveLength(1);
  });

  it("does not retry 4xx and surfaces the envelope", async () => {
    server.enqueue((req) => errorEnvelope(req, 422, "VALIDATION_FAILED"));
    await expect(client.replacePresence(snapshot(), 90)).rejects.toThrow(
      CompanionServerError,
    );
    expect(server.requests).toHaveLength(1);
  });

  it("fails on two consecutive failures (no second retry)", async () => {
    server.enqueue(() => ({ socketDestroy: true }));
    server.enqueue(() => ({ socketDestroy: true }));
    await expect(client.replacePresence(snapshot(), 90)).rejects.toThrow();
    expect(server.requests).toHaveLength(2);
  });

  it("reconciles acceptedSequence from success responses", async () => {
    server.enqueue((req) => mutationSuccess(req, 100));
    server.setFallback((req) => mutationSuccess(req));
    await client.replacePresence(snapshot(), 90);
    await client.replacePresence(snapshot(), 90);
    const second = server.requests[1]!.json as { meta: { sequence: number } };
    expect(second.meta.sequence).toBe(101);
  });

  it("reconciles acceptedSequence from error envelopes", async () => {
    server.enqueue((req) =>
      errorEnvelope(req, 409, "COMPANION_SEQUENCE_BEHIND", { acceptedSequence: 200 }),
    );
    server.setFallback((req) => mutationSuccess(req));
    await expect(client.replacePresence(snapshot(), 90)).rejects.toThrow();
    await client.replacePresence(snapshot(), 90);
    const second = server.requests[1]!.json as { meta: { sequence: number } };
    expect(second.meta.sequence).toBe(201);
  });

  it("rejects a response that does not echo the requestId, then retries", async () => {
    server.enqueue((req) => {
      const ok = mutationSuccess(req);
      (ok.body.meta as { requestId: string }).requestId =
        "00000000-0000-4000-8000-000000000000";
      return ok;
    });
    server.setFallback((req) => mutationSuccess(req));
    await client.replacePresence(snapshot(), 90);
    expect(server.requests).toHaveLength(2);
  });

  it("clearPresence consumes a sequence and posts the reason", async () => {
    server.setFallback((req) => mutationSuccess(req));
    await client.replacePresence(snapshot(), 90);
    await client.clearPresence("sleep", 1_753_500_012_345);
    const clear = server.requests[1]!;
    expect(clear.path).toBe("/companion/presence/clear");
    const body = clear.json as { meta: { sequence: number }; data: { reason: string } };
    expect(body.data.reason).toBe("sleep");
    expect(body.meta.sequence).toBe(6);
  });

  it("serializes concurrent mutations through the send slot", async () => {
    server.setFallback((req) => mutationSuccess(req));
    await Promise.all([
      client.replacePresence(snapshot(), 90),
      client.replacePresence(snapshot(), 90),
      client.clearPresence("paused", 1_753_500_012_345),
    ]);
    const sequences = server.requests.map(
      (r) => (r.json as { meta: { sequence: number } }).meta.sequence,
    );
    expect(sequences).toEqual([5, 6, 7]);
  });

  it("classifies schema rejection and bare 426 as renegotiation signals", async () => {
    server.enqueue((req) => errorEnvelope(req, 409, "COMPANION_SCHEMA_UNSUPPORTED"));
    const error1 = await client.replacePresence(snapshot(), 90).catch((e: unknown) => e);
    expect(needsRenegotiation(error1)).toBe(true);

    server.enqueue(() => ({ status: 426, body: { upgrade: "required" } }));
    const error2 = await client.replacePresence(snapshot(), 90).catch((e: unknown) => e);
    expect(needsRenegotiation(error2)).toBe(true);

    server.enqueue((req) => errorEnvelope(req, 422, "VALIDATION_FAILED"));
    const error3 = await client.replacePresence(snapshot(), 90).catch((e: unknown) => e);
    expect(needsRenegotiation(error3)).toBe(false);
  });
});
