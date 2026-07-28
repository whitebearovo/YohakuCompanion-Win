import type { Preview, RuntimeState } from "@yohaku/shared";
import type { CaptureService } from "../privacy/captureService.js";
import type { ConfigStore } from "../store/configStore.js";
import type { CredentialStore } from "../store/credentials.js";
import type { FileSequenceStore } from "../store/sequenceStore.js";
import { logger } from "../runtime/logger.js";
import { ConsentGate, projectionOf } from "./consentGate.js";
import { LiveDeskCoordinator, type CoordinatorTimings } from "./coordinator.js";
import { claimPairing, PairingError } from "./pairing.js";

/**
 * Application façade, ported from YohakuCompanionService.swift. Owns the one
 * ConsentGate and LiveDeskCoordinator. Invariants enforced here:
 * - pairing always lands with liveDeskEnabled=false;
 * - enabling Live Desk validates consent BEFORE and AFTER persisting, and
 *   rolls back to disabled on any drift (previewOutOfDate);
 * - disabling persists first, then clears remotely;
 * - a policy change invalidates the preview; when Live Desk is already
 *   enabled it republishes under the new policy instead of revoking consent.
 */

export class ServiceError extends Error {
  override readonly name = "ServiceError";
  constructor(
    readonly code:
      | "notPaired"
      | "previewOutOfDate"
      | "pairingFailed"
      | "pairingExpired"
      | "rateLimited"
      | "validationFailed"
      | "requiredScopeMissing"
      | "clientUpdateRequired"
      | "serverFeatureUnavailable"
      | "credentialStoreUnavailable"
      | "network"
      | "invalidInput",
  ) {
    super(code);
  }
}

export interface CompanionServiceDeps {
  config: ConfigStore;
  capture: CaptureService;
  sequenceStore: FileSequenceStore;
  credentials: () => Promise<CredentialStore>;
  /** Fired on any externally visible change; IPC broadcasts a snapshot. */
  onChanged: () => void;
  coordinatorTimings?: Partial<CoordinatorTimings>;
}

export class CompanionService {
  readonly coordinator: LiveDeskCoordinator;
  private readonly gate: ConsentGate;
  private preview: Preview | null = null;
  private lastPublishAt: number | null = null;
  private lastError: string | null = null;

  constructor(private readonly deps: CompanionServiceDeps) {
    this.gate = new ConsentGate(deps.capture.fingerprint());
    this.coordinator = new LiveDeskCoordinator(
      {
        capture: deps.capture,
        sequenceStore: deps.sequenceStore,
        getConnection: () => deps.config.get().connection,
        getToken: async (deviceId) => (await deps.credentials()).get(deviceId),
        onStateChange: () => deps.onChanged(),
        onPublished: () => {
          this.lastPublishAt = Date.now();
          this.lastError = null;
          deps.onChanged();
        },
      },
      deps.coordinatorTimings ?? {},
    );
  }

  /** Resume publishing after a restart when consent was already given. */
  start(): void {
    const connection = this.deps.config.get().connection;
    if (connection !== null && connection.liveDeskEnabled) {
      this.coordinator.start();
    }
  }

  runtimeState(): RuntimeState {
    if (this.deps.config.get().connection === null) return "notPaired";
    return this.coordinator.currentState();
  }

  currentPreview(): Preview | null {
    return this.preview;
  }

  publishTelemetry(): { lastPublishAt: number | null; lastError: string | null } {
    return { lastPublishAt: this.lastPublishAt, lastError: this.lastError };
  }

  /**
   * Called after ANY privacy-relevant configuration change. Compares policy
   * fingerprints by value; on change: invalidate consent-preview binding and
   * republish immediately under the new policy if already enabled.
   */
  policyMaybeChanged(): void {
    const fingerprint = this.deps.capture.fingerprint();
    if (fingerprint === this.gate.fingerprint) return;
    this.gate.policyDidChange(fingerprint);
    this.preview = null;
    this.coordinator.requestFreshSnapshot();
    this.deps.onChanged();
  }

  /** Captures a fresh sanitized preview and records it as the consent basis. */
  async refreshPreview(): Promise<Preview> {
    const config = this.deps.config.get();
    const snapshot = await this.deps.capture.captureForDelivery({
      includeMedia: config.privacy.sources.media,
    });
    const projection = projectionOf(snapshot);
    this.gate.record(projection);
    this.preview = {
      projection,
      policyFingerprint: this.gate.fingerprint,
      observedAt: snapshot.observedAt,
    };
    this.deps.onChanged();
    return this.preview;
  }

  async pair(baseUrl: string, deviceName: string, pairingCode: string): Promise<void> {
    let credentialStore: CredentialStore;
    try {
      credentialStore = await this.deps.credentials();
    } catch {
      throw new ServiceError("credentialStoreUnavailable");
    }

    let result;
    try {
      result = await claimPairing(baseUrl, deviceName, pairingCode, async () => {
        await this.deps.credentials();
      });
    } catch (error) {
      throw this.mapPairingError(error);
    }

    // Replacing an existing pairing: stop the old lifecycle first.
    await this.coordinator.shutdown("connectionRemoved");
    this.coordinator.discardAuthority();

    // Token into protected storage BEFORE non-secret metadata is committed.
    await credentialStore.set(result.deviceId, result.deviceToken);
    this.deps.config.update((config) => ({
      ...config,
      credentialBackend: credentialStore.backend,
      connection: {
        baseUrl: result.baseUrl,
        deviceId: result.deviceId,
        deviceName: result.deviceName,
        scopes: result.scopes,
        pairingNextSequence: result.nextSequence,
        liveDeskEnabled: false, // pairing NEVER enables publishing
      },
    }));
    logger.info("service", "paired (live desk disabled)");
    await this.refreshPreview();
  }

  /**
   * Consent boundary. `uiFingerprint` is the policy fingerprint the user was
   * looking at when they clicked — a stale click can never enable publishing.
   */
  async confirmConsent(uiFingerprint: string): Promise<void> {
    const connection = this.deps.config.get().connection;
    if (connection === null) throw new ServiceError("notPaired");
    const preview = this.preview;
    if (preview === null || uiFingerprint !== this.gate.fingerprint) {
      await this.refreshPreview();
      throw new ServiceError("previewOutOfDate");
    }
    const candidate = {
      policyFingerprint: uiFingerprint,
      projection: preview.projection,
    };

    // Validation 1: fresh capture still matches what the user confirmed.
    const before = await this.deps.capture.captureForDelivery({
      includeMedia: this.deps.config.get().privacy.sources.media,
    });
    if (!this.gate.validates(candidate, projectionOf(before))) {
      await this.refreshPreview();
      throw new ServiceError("previewOutOfDate");
    }

    // Persist the enable.
    this.deps.config.update((config) => ({
      ...config,
      connection: config.connection === null ? null : { ...config.connection, liveDeskEnabled: true },
    }));

    // Validation 2 (post-persist): anything drifted during the awaits above
    // rolls the persisted state back — a stale write can never start
    // publishing.
    const after = await this.deps.capture.captureForDelivery({
      includeMedia: this.deps.config.get().privacy.sources.media,
    });
    const fingerprintNow = this.deps.capture.fingerprint();
    if (fingerprintNow !== uiFingerprint || !this.gate.validates(candidate, projectionOf(after))) {
      this.deps.config.update((config) => ({
        ...config,
        connection: config.connection === null ? null : { ...config.connection, liveDeskEnabled: false },
      }));
      await this.refreshPreview();
      throw new ServiceError("previewOutOfDate");
    }

    logger.info("service", "live desk enabled by explicit consent");
    this.coordinator.start();
    this.deps.onChanged();
  }

  /** Persist-first disable: a crash mid-way must never resume publishing. */
  async disableLiveDesk(): Promise<void> {
    this.deps.config.update((config) => ({
      ...config,
      connection: config.connection === null ? null : { ...config.connection, liveDeskEnabled: false },
    }));
    await this.coordinator.shutdown("paused");
    this.deps.onChanged();
  }

  async unpair(): Promise<void> {
    const connection = this.deps.config.get().connection;
    if (connection === null) return;
    // Disable durably first, then final clear with the in-memory credential.
    this.deps.config.update((config) => ({
      ...config,
      connection: config.connection === null ? null : { ...config.connection, liveDeskEnabled: false },
    }));
    await this.coordinator.shutdown("connectionRemoved");
    this.coordinator.discardAuthority();
    try {
      const store = await this.deps.credentials();
      await store.delete(connection.deviceId);
    } catch {
      logger.warn("service", "credential deletion failed during unpair");
    }
    await this.deps.sequenceStore.remove(connection.deviceId);
    this.deps.config.update((config) => ({ ...config, connection: null }));
    this.gate.clear();
    this.preview = null;
    logger.info("service", "unpaired; credentials and sequence removed");
    this.deps.onChanged();
  }

  handleSleepOrLock(): void {
    this.coordinator.handleSleepOrLock();
  }

  handleWakeOrUnlock(): void {
    this.coordinator.handleWakeOrUnlock();
  }

  async shutdown(): Promise<void> {
    await this.coordinator.shutdown("shutdown");
  }

  noteError(code: string): void {
    this.lastError = code;
    this.deps.onChanged();
  }

  private mapPairingError(error: unknown): ServiceError {
    if (error instanceof PairingError) {
      switch (error.code) {
        case "clientUpdateRequired":
          return new ServiceError("clientUpdateRequired");
        case "serverFeatureUnavailable":
        case "invalidCapabilities":
          return new ServiceError("serverFeatureUnavailable");
        case "requiredScopeMissing":
          return new ServiceError("requiredScopeMissing");
        case "invalidPairingCode":
        case "invalidDeviceName":
        case "invalidServerUrl":
          return new ServiceError("invalidInput");
        case "pairingRejected":
          if (error.serverCode === "COMPANION_PAIRING_EXPIRED") {
            return new ServiceError("pairingExpired");
          }
          if (error.serverCode === "RATE_LIMITED") {
            return new ServiceError("rateLimited");
          }
          if (error.serverCode === "VALIDATION_FAILED") {
            return new ServiceError("validationFailed");
          }
          return new ServiceError("pairingFailed");
        case "network":
          return new ServiceError("network");
      }
    }
    return new ServiceError("pairingFailed");
  }
}
