import type { SanitizedPresenceSnapshot } from "../privacy/types.js";
import type { NegotiatedPresenceConfiguration } from "./protocol/capabilities.js";
import {
  makeClearRequest,
  makePresenceRequest,
  type MappedRequest,
} from "./protocol/dtoMapper.js";
import {
  mutationResponseSchema,
  type ClearReason,
  type MutationResponse,
} from "./protocol/types.js";
import type { CompanionSequencer } from "./sequencer.js";
import {
  CompanionCredentialDeviceMismatchError,
  acceptedSequenceOf,
  isSafeForImmediateIdempotentRetry,
} from "./transport/errors.js";
import type {
  CompanionCredential,
  CompanionHttpClient,
} from "./transport/httpClient.js";

/**
 * Ordered presence writer, ported from YohakuPresenceClient.swift:
 * - all mutations are serialized through a FIFO send slot;
 * - the sequence is durably reserved before sending (crash -> legal gap);
 * - an ambiguous or retry-safe failure is retried EXACTLY once with the same
 *   sequence, requestId and body bytes (idempotent resend);
 * - acceptedSequence from success or error envelopes is reconciled back.
 */
export class PresenceClient {
  private sendSlot: Promise<unknown> = Promise.resolve();

  constructor(
    private readonly http: CompanionHttpClient,
    private readonly credential: CompanionCredential,
    private readonly sequencer: CompanionSequencer,
    private readonly configuration: NegotiatedPresenceConfiguration,
  ) {}

  private acquireSlot<T>(task: () => Promise<T>): Promise<T> {
    const next = this.sendSlot.then(task, task);
    this.sendSlot = next.catch(() => undefined);
    return next;
  }

  replacePresence(
    snapshot: SanitizedPresenceSnapshot,
    requestedLeaseSeconds: number,
  ): Promise<MutationResponse> {
    return this.acquireSlot(async () => {
      const sequence = await this.sequencer.reserve();
      const mapped = makePresenceRequest(snapshot, sequence, requestedLeaseSeconds, {
        deviceId: this.credential.deviceId,
        leaseMinSeconds: this.configuration.leaseMinSeconds,
        leaseMaxSeconds: this.configuration.leaseMaxSeconds,
      });
      this.assertDeviceMatches(mapped.body.meta.deviceId);
      return this.performWithSingleRetry("PUT", "/companion/presence", mapped);
    });
  }

  clearPresence(reason: ClearReason, observedAtMs: number): Promise<MutationResponse> {
    return this.acquireSlot(async () => {
      const sequence = await this.sequencer.reserve();
      const mapped = makeClearRequest(reason, sequence, observedAtMs, {
        deviceId: this.credential.deviceId,
        leaseMinSeconds: this.configuration.leaseMinSeconds,
        leaseMaxSeconds: this.configuration.leaseMaxSeconds,
      });
      this.assertDeviceMatches(mapped.body.meta.deviceId);
      return this.performWithSingleRetry("POST", "/companion/presence/clear", mapped);
    });
  }

  private assertDeviceMatches(metaDeviceId: string): void {
    if (metaDeviceId !== this.credential.deviceId) {
      throw new CompanionCredentialDeviceMismatchError(
        "request deviceId does not match credential",
      );
    }
  }

  private async performWithSingleRetry(
    method: "PUT" | "POST",
    path: string,
    mapped: MappedRequest<unknown>,
  ): Promise<MutationResponse> {
    // Encode once: a retry must resend the exact same bytes.
    const encodedBody = this.http.encodeBody(mapped.body);
    const attempt = () =>
      this.http.execute({
        method,
        path,
        encodedBody,
        credential: this.credential,
        responseSchema: mutationResponseSchema,
        expectedRequestId: mapped.requestId,
        maximumPayloadBytes: this.configuration.maximumPayloadBytes,
      });

    try {
      const response = await attempt();
      await this.sequencer.reconcile(response.data.acceptedSequence);
      return response;
    } catch (error) {
      await this.reconcileFromError(error);
      if (!isSafeForImmediateIdempotentRetry(error)) throw error;
      try {
        const response = await attempt();
        await this.sequencer.reconcile(response.data.acceptedSequence);
        return response;
      } catch (retryError) {
        await this.reconcileFromError(retryError);
        throw retryError;
      }
    }
  }

  private async reconcileFromError(error: unknown): Promise<void> {
    const accepted = acceptedSequenceOf(error);
    if (accepted !== null) {
      await this.sequencer.reconcile(accepted);
    }
  }
}
