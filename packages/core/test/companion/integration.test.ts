import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CompanionService, ServiceError } from "../../src/companion/service.js";
import { CaptureService } from "../../src/privacy/captureService.js";
import { ConfigStore } from "../../src/store/configStore.js";
import { FileSequenceStore } from "../../src/store/sequenceStore.js";
import type { CredentialStore } from "../../src/store/credentials.js";
import type { ForegroundInfo, MediaProvider, MediaSnapshot } from "../../src/capture/types.js";
import {
  MockCompanionServer,
  capabilitiesResponse,
  errorEnvelope,
  mutationSuccess,
  responseMeta,
} from "../helpers/mockServer.js";
import { randomUUID } from "node:crypto";

/**
 * Headless end-to-end: real CompanionService + CaptureService + privacy
 * pipeline + protocol client against a mock Mix Space Core, with fake
 * foreground/media sources.
 */

class FakeCredentialStore implements CredentialStore {
  readonly backend = "keyring" as const;
  tokens = new Map<string, string>();
  async get(deviceId: string): Promise<string | null> {
    return this.tokens.get(deviceId) ?? null;
  }
  async set(deviceId: string, token: string): Promise<void> {
    this.tokens.set(deviceId, token);
  }
  async delete(deviceId: string): Promise<void> {
    this.tokens.delete(deviceId);
  }
}

class FakeMediaProvider implements MediaProvider {
  readonly kind = "npm" as const;
  snapshot: MediaSnapshot | null = null;
  async start(): Promise<void> {}
  async stop(): Promise<void> {}
  async getSnapshot(): Promise<MediaSnapshot | null> {
    return this.snapshot;
  }
  onSemanticChange(): () => void {
    return () => undefined;
  }
  healthy(): boolean {
    return true;
  }
}

const DEVICE_ID = "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b";
const DEVICE_TOKEN = "test-device-token";

function pairingClaimResponse() {
  return {
    status: 200,
    body: {
      meta: responseMeta(randomUUID()),
      data: {
        deviceId: DEVICE_ID,
        deviceToken: DEVICE_TOKEN,
        scopes: ["companion:presence:write"],
        nextSequence: 10,
      },
    },
  };
}

async function until(condition: () => boolean, timeoutMs = 3000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!condition()) {
    if (Date.now() > deadline) throw new Error("condition timeout");
    await new Promise((resolve) => setTimeout(resolve, 15));
  }
}

describe("CompanionService end-to-end (mock Core)", () => {
  let server: MockCompanionServer;
  let dir: string;
  let config: ConfigStore;
  let credentials: FakeCredentialStore;
  let media: FakeMediaProvider;
  let foregroundInfo: ForegroundInfo | null;
  let service: CompanionService;

  beforeEach(async () => {
    server = new MockCompanionServer();
    await server.start();
    server.setFallback((req) => {
      if (req.path === "/companion/capabilities") return capabilitiesResponse();
      if (req.path === "/companion/pairings/claim") return pairingClaimResponse();
      return mutationSuccess(req);
    });

    dir = mkdtempSync(join(tmpdir(), "yohaku-test-"));
    config = new ConfigStore(dir);
    credentials = new FakeCredentialStore();
    media = new FakeMediaProvider();
    foregroundInfo = {
      appId: "code.exe",
      exePath: "C:/apps/code.exe",
      displayName: "Visual Studio Code",
      windowTitle: "secret.ts — project",
    };
    const capture = new CaptureService(
      { current: () => foregroundInfo },
      () => media,
      () => config.get().privacy,
    );
    service = new CompanionService({
      config,
      capture,
      sequenceStore: new FileSequenceStore(dir),
      credentials: async () => credentials,
      onChanged: () => undefined,
      coordinatorTimings: { networkRetryMs: 100, featureRetryMs: 200 },
    });
  });

  afterEach(async () => {
    await service.shutdown();
    await server.stop();
    rmSync(dir, { recursive: true, force: true });
  });

  async function pairAndConsent(): Promise<void> {
    await service.pair(server.baseUrl, "Test PC", "PAIR-CODE");
    const preview = await service.refreshPreview();
    await service.confirmConsent(preview.policyFingerprint);
    await until(() => service.runtimeState() === "active");
  }

  function requestsTo(path: string) {
    return server.requests.filter((r) => r.path === path);
  }

  it("pairs with live desk disabled and stores the token securely", async () => {
    await service.pair(server.baseUrl, "  Test PC  ", "PAIR-CODE");
    const connection = config.get().connection!;
    expect(connection.liveDeskEnabled).toBe(false);
    expect(connection.deviceId).toBe(DEVICE_ID);
    expect(connection.pairingNextSequence).toBe(10);
    expect(credentials.tokens.get(DEVICE_ID)).toBe(DEVICE_TOKEN);
    // Config file must never contain the token.
    expect(JSON.stringify(config.get())).not.toContain(DEVICE_TOKEN);
    // Pairing alone must not publish anything.
    expect(requestsTo("/companion/presence")).toHaveLength(0);
    expect(service.runtimeState()).toBe("disabled");
  });

  it("capabilities are negotiated BEFORE the one-time code is consumed", async () => {
    await service.pair(server.baseUrl, "Test PC", "PAIR-CODE");
    const paths = server.requests.map((r) => r.path);
    expect(paths.indexOf("/companion/capabilities")).toBeLessThan(
      paths.indexOf("/companion/pairings/claim"),
    );
  });

  it("rejects pairing when the presence scope is missing", async () => {
    server.enqueue(() => capabilitiesResponse());
    server.enqueue((req) => {
      const ok = pairingClaimResponse();
      ok.body.data.scopes = ["companion:moment:write"];
      return ok;
    });
    await expect(service.pair(server.baseUrl, "PC", "CODE")).rejects.toMatchObject({
      code: "requiredScopeMissing",
    });
  });

  it("maps COMPANION_PAIRING_EXPIRED to pairingExpired", async () => {
    server.enqueue(() => capabilitiesResponse());
    server.enqueue(() => ({
      status: 410,
      body: { error: { code: "COMPANION_PAIRING_EXPIRED" } },
    }));
    await expect(service.pair(server.baseUrl, "PC", "CODE")).rejects.toMatchObject({
      code: "pairingExpired",
    });
  });

  it("consent with a stale fingerprint fails and never enables", async () => {
    await service.pair(server.baseUrl, "PC", "CODE");
    await service.refreshPreview();
    await expect(service.confirmConsent("stale-fingerprint")).rejects.toMatchObject({
      code: "previewOutOfDate",
    });
    expect(config.get().connection!.liveDeskEnabled).toBe(false);
  });

  it("consent fails when the capture drifts between preview and click", async () => {
    await service.pair(server.baseUrl, "PC", "CODE");
    const preview = await service.refreshPreview();
    foregroundInfo = { ...foregroundInfo!, appId: "other.exe", displayName: "Other" };
    await expect(service.confirmConsent(preview.policyFingerprint)).rejects.toMatchObject({
      code: "previewOutOfDate",
    });
    expect(config.get().connection!.liveDeskEnabled).toBe(false);
  });

  it("full flow: consent -> active -> sanitized presence published", async () => {
    media.snapshot = {
      appId: "spotify.exe",
      sourceAppUserModelId: "spotify.exe",
      playerDisplayName: "Spotify",
      kind: "music",
      title: "Song",
      artist: "Artist",
      album: "Album",
      playing: true,
      durationSeconds: 200,
      positionSeconds: 60,
      sampledAt: Date.now(),
    };
    await pairAndConsent();
    await until(() => requestsTo("/companion/presence").length >= 1);

    const put = requestsTo("/companion/presence")[0]!;
    expect(put.headers.authorization).toBe(`Bearer ${DEVICE_TOKEN}`);
    const body = put.json as {
      meta: { sequence: number; deviceId: string };
      data: {
        availability: string;
        application: { displayName: string; window: unknown } | null;
        media: { title: string; sessionId: string } | null;
      };
    };
    expect(body.meta.deviceId).toBe(DEVICE_ID);
    expect(body.meta.sequence).toBeGreaterThanOrEqual(10);
    expect(body.data.availability).toBe("active");
    expect(body.data.application?.displayName).toBe("Visual Studio Code");
    // windowTitle: rule default hide + global switch off -> no title on the wire
    expect(body.data.application?.window).toBeNull();
    expect(body.data.media?.title).toBe("Song");
    // Raw identifiers never leave the process.
    expect(put.rawBody).not.toContain("code.exe");
    expect(put.rawBody).not.toContain("spotify.exe");
    expect(put.rawBody).not.toContain("C:/apps");
  });

  it("privacy change while enabled republishes under the new policy", async () => {
    await pairAndConsent();
    await until(() => requestsTo("/companion/presence").length >= 1);
    const countBefore = requestsTo("/companion/presence").length;

    config.update((c) => ({
      ...c,
      privacy: {
        ...c.privacy,
        rules: [
          {
            appId: "code.exe",
            application: "share",
            windowTitle: "inherit",
            media: "inherit",
            displayAlias: "编辑器",
          },
        ],
      },
    }));
    service.policyMaybeChanged();

    await until(() => requestsTo("/companion/presence").length > countBefore);
    const last = requestsTo("/companion/presence").at(-1)!;
    const body = last.json as { data: { application: { displayName: string } } };
    expect(body.data.application.displayName).toBe("编辑器");
    // Consent stays valid (already enabled), but the recorded preview is gone.
    expect(service.currentPreview()).toBeNull();
    expect(config.get().connection!.liveDeskEnabled).toBe(true);
  });

  it("lock clears with reason sleep; wake renegotiates and republishes", async () => {
    await pairAndConsent();
    await until(() => requestsTo("/companion/presence").length >= 1);
    const capabilitiesBefore = requestsTo("/companion/capabilities").length;

    service.handleSleepOrLock();
    await until(() => requestsTo("/companion/presence/clear").length >= 1);
    const clear = requestsTo("/companion/presence/clear")[0]!;
    expect((clear.json as { data: { reason: string } }).data.reason).toBe("sleep");
    expect(service.runtimeState()).toBe("suspended");

    const putsBefore = requestsTo("/companion/presence").length;
    service.handleWakeOrUnlock();
    await until(() => service.runtimeState() === "active");
    await until(() => requestsTo("/companion/presence").length > putsBefore);
    expect(requestsTo("/companion/capabilities").length).toBeGreaterThan(
      capabilitiesBefore,
    );
  });

  it("sequences stay monotonic across clear and wake (single sequencer)", async () => {
    await pairAndConsent();
    await until(() => requestsTo("/companion/presence").length >= 1);
    service.handleSleepOrLock();
    await until(() => requestsTo("/companion/presence/clear").length >= 1);
    service.handleWakeOrUnlock();
    await until(() => service.runtimeState() === "active");
    await until(
      () =>
        requestsTo("/companion/presence").length +
          requestsTo("/companion/presence/clear").length >=
        3,
    );
    const sequences = server.requests
      .filter((r) => r.path.startsWith("/companion/presence"))
      .map((r) => (r.json as { meta: { sequence: number } }).meta.sequence);
    const sorted = [...sequences].sort((a, b) => a - b);
    expect(sequences).toEqual(sorted);
    expect(new Set(sequences).size).toBe(sequences.length);
  });

  it("schema rejection triggers renegotiation, then publishing resumes", async () => {
    await pairAndConsent();
    await until(() => requestsTo("/companion/presence").length >= 1);
    const capabilitiesBefore = requestsTo("/companion/capabilities").length;

    server.enqueue((req) => errorEnvelope(req, 409, "COMPANION_SCHEMA_UNSUPPORTED"));
    service.coordinator.requestFreshSnapshot();

    await until(() => requestsTo("/companion/capabilities").length > capabilitiesBefore);
    await until(() => service.runtimeState() === "active", 5000);
  });

  it("disable persists first and clears with reason paused", async () => {
    await pairAndConsent();
    await service.disableLiveDesk();
    expect(config.get().connection!.liveDeskEnabled).toBe(false);
    const clears = requestsTo("/companion/presence/clear");
    expect(clears.length).toBeGreaterThanOrEqual(1);
    expect((clears.at(-1)!.json as { data: { reason: string } }).data.reason).toBe(
      "paused",
    );
    expect(service.runtimeState()).toBe("disabled");
  });

  it("unpair clears remotely, removes credentials and connection", async () => {
    await pairAndConsent();
    await service.unpair();
    const clears = requestsTo("/companion/presence/clear");
    expect(
      (clears.at(-1)!.json as { data: { reason: string } }).data.reason,
    ).toBe("connectionRemoved");
    expect(credentials.tokens.size).toBe(0);
    expect(config.get().connection).toBeNull();
    expect(service.runtimeState()).toBe("notPaired");
  });

  it("degrades on network failure and recovers via retry", async () => {
    await pairAndConsent();
    await until(() => requestsTo("/companion/presence").length >= 1);
    server.enqueue(() => ({ socketDestroy: true }));
    server.enqueue(() => ({ socketDestroy: true })); // second: kill the single retry too
    service.coordinator.requestFreshSnapshot();
    await until(() => service.runtimeState() === "degraded");
    // networkRetryMs=100 -> renegotiation restores active.
    await until(() => service.runtimeState() === "active", 5000);
  });
});
