import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import WebSocket from "ws";
import type { CoreStateSnapshot, ServerMessage } from "@yohaku/shared";
import { defaultPrivacyConfig } from "@yohaku/shared";
import { CompanionService } from "../../src/companion/service.js";
import { createCommandHandler } from "../../src/ipc/handlers.js";
import { IpcServer } from "../../src/ipc/server.js";
import { CaptureService } from "../../src/privacy/captureService.js";
import { ConfigStore } from "../../src/store/configStore.js";
import { FileSequenceStore } from "../../src/store/sequenceStore.js";
import type { CredentialStore } from "../../src/store/credentials.js";

class FakeCredentialStore implements CredentialStore {
  readonly backend = "keyring" as const;
  async get(): Promise<string | null> {
    return null;
  }
  async set(): Promise<void> {}
  async delete(): Promise<void> {}
}

class TestClient {
  private ws!: WebSocket;
  readonly states: CoreStateSnapshot[] = [];
  private readonly results = new Map<
    string,
    (r: { ok: boolean; error?: string }) => void
  >();

  async connect(port: number, token: string): Promise<void> {
    this.ws = new WebSocket(`ws://127.0.0.1:${port}`);
    await new Promise<void>((resolve, reject) => {
      this.ws.on("open", () => resolve());
      this.ws.on("error", reject);
    });
    this.ws.on("message", (raw) => {
      const message = JSON.parse(String(raw)) as ServerMessage;
      if (message.type === "state") this.states.push(message.snapshot);
      if (message.type === "result") {
        this.results.get(message.id)?.({
          ok: message.ok,
          ...(message.error !== undefined ? { error: message.error } : {}),
        });
      }
    });
    this.ws.send(JSON.stringify({ type: "hello", token }));
  }

  command(payload: Record<string, unknown>): Promise<{ ok: boolean; error?: string }> {
    const id = Math.random().toString(36).slice(2);
    return new Promise((resolve) => {
      this.results.set(id, resolve);
      this.ws.send(JSON.stringify({ id, ...payload }));
    });
  }

  async waitClosed(): Promise<number> {
    return new Promise((resolve) => this.ws.on("close", (code) => resolve(code)));
  }

  close(): void {
    this.ws.close();
  }
}

async function until(condition: () => boolean, timeoutMs = 3000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!condition()) {
    if (Date.now() > deadline) throw new Error("condition timeout");
    await new Promise((resolve) => setTimeout(resolve, 15));
  }
}

describe("IpcServer", () => {
  let dir: string;
  let ipc: IpcServer;
  let port: number;
  let config: ConfigStore;
  let shutdownRequested = false;
  const TOKEN = "test-token-123";

  beforeEach(async () => {
    dir = mkdtempSync(join(tmpdir(), "yohaku-ipc-"));
    config = new ConfigStore(dir);
    const capture = new CaptureService(
      {
        current: () => ({
          appId: "code.exe",
          exePath: null,
          displayName: "Code",
          windowTitle: "t",
        }),
      },
      () => null,
      () => config.get().privacy,
    );
    const service = new CompanionService({
      config,
      capture,
      sequenceStore: new FileSequenceStore(dir),
      credentials: async () => new FakeCredentialStore(),
      onChanged: () => ipc.broadcastState(),
    });
    const handler = createCommandHandler({
      config,
      service,
      requestShutdown: () => {
        shutdownRequested = true;
      },
    });
    ipc = new IpcServer({
      token: TOKEN,
      getSnapshot: () => ({
        version: "test",
        runtimeState: service.runtimeState(),
        connection: null,
        privacy: config.get().privacy,
        preview: service.currentPreview(),
        mediaProvider: { kind: "none", healthy: false },
        recentAppIds: [],
        lastPublishAt: null,
        lastError: null,
      }),
      onCommand: handler,
    });
    port = await ipc.start();
  });

  afterEach(async () => {
    await ipc.stop();
    rmSync(dir, { recursive: true, force: true });
  });

  it("rejects a wrong token", async () => {
    const client = new TestClient();
    await client.connect(port, "wrong-token");
    expect(await client.waitClosed()).toBe(4001);
  });

  it("closes unauthenticated connections after the hello timeout", async () => {
    const ws = new WebSocket(`ws://127.0.0.1:${port}`);
    const code = await new Promise<number>((resolve) => {
      ws.on("close", (c) => resolve(c));
    });
    expect(code).toBe(4001);
  }, 8000);

  it("sends an initial state snapshot after hello", async () => {
    const client = new TestClient();
    await client.connect(port, TOKEN);
    await until(() => client.states.length >= 1);
    expect(client.states[0]!.runtimeState).toBe("notPaired");
    expect(client.states[0]!.privacy).toEqual(defaultPrivacyConfig());
    client.close();
  });

  it("routes privacy commands, persists them, and rebroadcasts", async () => {
    const client = new TestClient();
    await client.connect(port, TOKEN);
    await until(() => client.states.length >= 1);

    const result = await client.command({
      cmd: "upsertRule",
      rule: {
        appId: "Secret.EXE",
        application: "hide",
        windowTitle: "inherit",
        media: "inherit",
      },
    });
    expect(result.ok).toBe(true);
    expect(config.get().privacy.rules).toEqual([
      { appId: "secret.exe", application: "hide", windowTitle: "inherit", media: "inherit" },
    ]);
    await until(() => (client.states.at(-1)?.privacy.rules.length ?? 0) === 1);

    const patch = await client.command({
      cmd: "setPrivacy",
      patch: { shareWindowTitles: true },
    });
    expect(patch.ok).toBe(true);
    expect(config.get().privacy.shareWindowTitles).toBe(true);
    client.close();
  });

  it("requestPreview produces a sanitized preview in the snapshot", async () => {
    const client = new TestClient();
    await client.connect(port, TOKEN);
    const result = await client.command({ cmd: "requestPreview" });
    expect(result.ok).toBe(true);
    await until(() => client.states.at(-1)?.preview !== null);
    const preview = client.states.at(-1)!.preview!;
    expect(preview.projection.application?.displayName).toBe("Code");
    expect(preview.projection.application?.windowTitle).toBeNull();
    expect(preview.policyFingerprint).toMatch(/^[0-9a-f]{64}$/);
    client.close();
  });

  it("consent without pairing fails with notPaired", async () => {
    const client = new TestClient();
    await client.connect(port, TOKEN);
    const result = await client.command({
      cmd: "confirmConsent",
      policyFingerprint: "x".repeat(64),
    });
    expect(result).toEqual({ ok: false, error: "notPaired" });
    client.close();
  });

  it("rejects malformed commands with invalidInput", async () => {
    const client = new TestClient();
    await client.connect(port, TOKEN);
    await until(() => client.states.length >= 1);
    const result = await client.command({ cmd: "pair" }); // missing fields
    expect(result).toEqual({ ok: false, error: "invalidInput" });
    client.close();
  });

  it("shutdown command triggers the exit hook", async () => {
    const client = new TestClient();
    await client.connect(port, TOKEN);
    const result = await client.command({ cmd: "shutdown" });
    expect(result.ok).toBe(true);
    expect(shutdownRequested).toBe(true);
    client.close();
  });
});
