import type { RuntimeState } from "@yohaku/shared";
import type { CaptureService } from "../privacy/captureService.js";
import type { StoredConnection } from "../store/configStore.js";
import { logger } from "../runtime/logger.js";
import { AuthorityRegistry } from "./authority.js";
import { negotiatePresence } from "./protocol/capabilities.js";
import { capabilitiesResponseSchema, type ClearReason } from "./protocol/types.js";
import { PROTOCOL_CLIENT_VERSION } from "./protocol/wire.js";
import { PresenceClient } from "./presenceClient.js";
import { CompanionSequencer, type SequencePersistence } from "./sequencer.js";
import { needsRenegotiation } from "./transport/errors.js";
import {
  CompanionHttpClient,
  CompanionServerConfiguration,
} from "./transport/httpClient.js";

/**
 * Live Desk lifecycle owner, ported from CompanionLiveDeskCoordinator.swift.
 *
 * - A monotonic generation invalidates all in-flight async work on any state
 *   transition (privacy change, sleep, shutdown, reconnect).
 * - All triggers (foreground change, media semantic change, heartbeat,
 *   recovery) coalesce into one refresh loop; every publish is a FRESH
 *   capture — snapshots are never replayed.
 * - Sleep/lock: new generation (suspended) + bounded best-effort clear;
 *   wake JOINS the pending clear before restarting negotiation.
 * - Schema/feature rejection discards the authority and renegotiates;
 *   network failures degrade with a reconnect timer (no path monitor on
 *   Windows — failure-driven recovery plus heartbeat).
 */

export interface CoordinatorDeps {
  capture: CaptureService;
  sequenceStore: SequencePersistence;
  getConnection: () => StoredConnection | null;
  getToken: (deviceId: string) => Promise<string | null>;
  onStateChange: (state: RuntimeState) => void;
  onPublished?: (() => void) | undefined;
}

export interface CoordinatorTimings {
  networkRetryMs: number;
  featureRetryMs: number;
  sleepClearTimeoutMs: number;
  shutdownClearTimeoutMs: number;
}

const DEFAULT_TIMINGS: CoordinatorTimings = {
  networkRetryMs: 30_000,
  featureRetryMs: 300_000,
  sleepClearTimeoutMs: 500,
  shutdownClearTimeoutMs: 800,
};

const sleep = (ms: number) =>
  new Promise<void>((resolve) => {
    const t = setTimeout(resolve, ms);
    t.unref?.();
  });

export class LiveDeskCoordinator {
  readonly registry = new AuthorityRegistry();
  private readonly timings: CoordinatorTimings;

  private generation = 0;
  private state: RuntimeState = "disabled";
  private stopping = false;
  private refreshRequested = false;
  private refreshLoopRunning = false;
  private heartbeatTimer: NodeJS.Timeout | null = null;
  private reconnectTimer: NodeJS.Timeout | null = null;
  private cleanupTask: Promise<void> | null = null;
  private lastSendStartedAt = 0;

  private client: PresenceClient | null = null;
  private includeMedia = false;
  private requestedLeaseSeconds = 90;
  private minSendIntervalMs = 2000;

  constructor(
    private readonly deps: CoordinatorDeps,
    timings: Partial<CoordinatorTimings> = {},
  ) {
    this.timings = { ...DEFAULT_TIMINGS, ...timings };
  }

  currentState(): RuntimeState {
    return this.state;
  }

  /** (Re)start negotiation and publishing under a fresh generation. */
  start(): void {
    if (this.stopping) return;
    this.beginNewGeneration("connecting");
    void this.configure(this.generation);
  }

  /** External trigger: foreground/media changed, publish a fresh snapshot. */
  requestFreshSnapshot(): void {
    if (this.state !== "active" && this.state !== "degraded") return;
    this.refreshRequested = true;
    void this.runRefreshLoop(this.generation);
  }

  handleSleepOrLock(): void {
    if (this.state === "suspended" || this.stopping) return;
    const client = this.client;
    this.beginNewGeneration("suspended");
    if (client !== null) {
      this.cleanupTask = this.clearBestEffort(
        client,
        "sleep",
        this.timings.sleepClearTimeoutMs,
      );
    }
    logger.info("coordinator", "suspended (lock/sleep)");
  }

  handleWakeOrUnlock(): void {
    if (this.state !== "suspended") return;
    const pending = this.cleanupTask;
    this.cleanupTask = null;
    void (async () => {
      // Join the in-flight clear before restarting so the wake snapshot can
      // never be reordered ahead of it.
      if (pending !== null) await pending;
      if (this.state === "suspended" && !this.stopping) this.start();
    })();
  }

  /** Bounded final clear + stop. Idempotent. */
  async shutdown(reason: ClearReason = "shutdown"): Promise<void> {
    if (this.stopping) return;
    this.stopping = true;
    try {
      const client = this.client;
      const wasSuspended = this.state === "suspended";
      this.beginNewGeneration("disabled");
      const pending = this.cleanupTask;
      this.cleanupTask = null;
      if (pending !== null) await pending;
      if (client !== null && !wasSuspended) {
        await this.clearBestEffort(client, reason, this.timings.shutdownClearTimeoutMs);
      }
      this.client = null;
    } finally {
      this.stopping = false;
    }
  }

  /** Capability rejection or unpair: the ordered writer must be rebuilt. */
  discardAuthority(): void {
    this.registry.discard();
    this.client = null;
  }

  // -------------------------------------------------------------------------

  private beginNewGeneration(state: RuntimeState): void {
    this.generation += 1;
    if (this.heartbeatTimer !== null) clearInterval(this.heartbeatTimer);
    if (this.reconnectTimer !== null) clearTimeout(this.reconnectTimer);
    this.heartbeatTimer = null;
    this.reconnectTimer = null;
    this.refreshRequested = false;
    this.deps.capture.resetMediaContinuity();
    this.setState(state);
  }

  private setState(state: RuntimeState): void {
    if (this.state === state) return;
    this.state = state;
    this.deps.onStateChange(state);
  }

  private async configure(gen: number): Promise<void> {
    const connection = this.deps.getConnection();
    if (connection === null || !connection.liveDeskEnabled) {
      this.setState("disabled");
      return;
    }

    let token: string | null;
    try {
      token = await this.deps.getToken(connection.deviceId);
    } catch {
      token = null;
    }
    if (gen !== this.generation) return;
    if (token === null) {
      this.setState("degraded");
      this.scheduleRetry(gen, this.timings.networkRetryMs);
      return;
    }

    let http: CompanionHttpClient;
    try {
      http = new CompanionHttpClient(new CompanionServerConfiguration(connection.baseUrl));
    } catch {
      this.setState("degraded"); // invalid stored base URL — no retry loop
      return;
    }

    let capabilities;
    try {
      capabilities = await http.execute({
        method: "GET",
        path: "/companion/capabilities",
        responseSchema: capabilitiesResponseSchema,
      });
    } catch {
      if (gen !== this.generation) return;
      this.setState("degraded");
      this.scheduleRetry(gen, this.timings.networkRetryMs);
      return;
    }
    if (gen !== this.generation) return;

    const negotiation = negotiatePresence(capabilities.data, PROTOCOL_CLIENT_VERSION);
    if (negotiation.kind === "clientUpdateRequired") {
      this.setState("updateRequired"); // terminal until an app update
      return;
    }
    if (negotiation.kind === "schemaUnsupported" || negotiation.kind === "featureUnavailable") {
      this.setState("serverFeatureUnavailable");
      this.scheduleRetry(gen, this.timings.featureRetryMs);
      return;
    }
    if (negotiation.kind === "invalidCapabilities") {
      this.setState("degraded");
      this.scheduleRetry(gen, this.timings.featureRetryMs);
      return;
    }

    const config = negotiation.configuration;
    this.includeMedia = config.supportsMediaTimeline;
    this.requestedLeaseSeconds = Math.min(
      Math.max(90, config.leaseMinSeconds),
      config.leaseMaxSeconds,
    );
    const heartbeatSeconds = Math.min(
      config.recommendedHeartbeatSeconds,
      Math.max(1, this.requestedLeaseSeconds / 3),
    );
    this.minSendIntervalMs = Math.ceil(60_000 / config.requestsPerMinute);

    // The sequencer survives renegotiation for the same (baseUrl, deviceId);
    // the client (and its mapper limits) is rebuilt from the fresh
    // negotiation — deliberately avoiding the stale-mapper reuse the macOS
    // implementation exhibits.
    const authority = this.registry.resolve(
      connection.baseUrl,
      connection.deviceId,
      () => ({
        sequencer: new CompanionSequencer(
          this.deps.sequenceStore,
          connection.deviceId,
          connection.pairingNextSequence,
        ),
        client: null as unknown as PresenceClient,
      }),
    );
    authority.client = new PresenceClient(
      http,
      { deviceId: connection.deviceId, deviceToken: token },
      authority.sequencer,
      config,
    );
    this.client = authority.client;

    this.setState("active");
    this.heartbeatTimer = setInterval(() => {
      if (gen !== this.generation) return;
      this.refreshRequested = true;
      void this.runRefreshLoop(gen);
    }, heartbeatSeconds * 1000);
    this.heartbeatTimer.unref?.();

    this.refreshRequested = true;
    void this.runRefreshLoop(gen);
  }

  private async runRefreshLoop(gen: number): Promise<void> {
    if (this.refreshLoopRunning) return;
    this.refreshLoopRunning = true;
    try {
      while (gen === this.generation && this.refreshRequested) {
        this.refreshRequested = false;

        const wait = this.lastSendStartedAt + this.minSendIntervalMs - Date.now();
        if (wait > 0) await sleep(wait);
        if (gen !== this.generation) return;

        const client = this.client;
        if (client === null) return;
        this.lastSendStartedAt = Date.now();

        const snapshot = await this.deps.capture.captureForDelivery({
          includeMedia: this.includeMedia,
        });
        if (gen !== this.generation) return;

        try {
          await client.replacePresence(snapshot, this.requestedLeaseSeconds);
          if (gen !== this.generation) return;
          this.setState("active");
          this.deps.onPublished?.();
        } catch (error) {
          if (gen !== this.generation) return;
          if (needsRenegotiation(error)) {
            logger.warn("coordinator", "schema/feature rejected; renegotiating");
            this.discardAuthority();
            this.start();
            return;
          }
          logger.warn("coordinator", "publish failed; degraded");
          this.setState("degraded");
          this.scheduleRetry(gen, this.timings.networkRetryMs);
          return;
        }
      }
    } finally {
      this.refreshLoopRunning = false;
    }
  }

  private scheduleRetry(gen: number, delayMs: number): void {
    if (this.reconnectTimer !== null) clearTimeout(this.reconnectTimer);
    this.reconnectTimer = setTimeout(() => {
      if (gen !== this.generation || this.stopping) return;
      this.start();
    }, delayMs);
    this.reconnectTimer.unref?.();
  }

  private async clearBestEffort(
    client: PresenceClient,
    reason: ClearReason,
    timeoutMs: number,
  ): Promise<void> {
    try {
      await Promise.race([
        client.clearPresence(reason, Date.now()).then(() => undefined),
        sleep(timeoutMs),
      ]);
    } catch {
      /* best-effort: the server lease expiry is the correctness backstop */
    }
  }
}
