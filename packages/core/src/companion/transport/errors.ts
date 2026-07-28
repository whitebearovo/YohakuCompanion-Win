import type { ErrorEnvelope } from "../protocol/types.js";
import { MUTATION_RENEGOTIATE_CODES } from "../protocol/types.js";

/** Transport/protocol error taxonomy, ported from CompanionHTTPClient.swift. */

export class CompanionTransportError extends Error {
  override readonly name: string = "CompanionTransportError";
}

/** Network-layer failure (fetch rejection, timeout). Ambiguous: the server
 * may or may not have committed the mutation — retried once with the exact
 * same encoded request. */
export class CompanionNetworkError extends CompanionTransportError {
  override readonly name = "CompanionNetworkError";
  constructor(readonly cause_: unknown) {
    super(`network failure: ${String(cause_)}`);
  }
}

export class CompanionEmptyResponseError extends CompanionTransportError {
  override readonly name = "CompanionEmptyResponseError";
}

export class CompanionDecodeError extends CompanionTransportError {
  override readonly name = "CompanionDecodeError";
}

export class CompanionRequestIdMismatchError extends CompanionTransportError {
  override readonly name = "CompanionRequestIdMismatchError";
}

export class CompanionPayloadTooLargeError extends CompanionTransportError {
  override readonly name = "CompanionPayloadTooLargeError";
}

export class CompanionCredentialDeviceMismatchError extends CompanionTransportError {
  override readonly name = "CompanionCredentialDeviceMismatchError";
}

/** Non-2xx with a decodable protocol error envelope. */
export class CompanionServerError extends CompanionTransportError {
  override readonly name = "CompanionServerError";
  constructor(
    readonly status: number,
    readonly envelope: ErrorEnvelope,
  ) {
    super(`server error ${status}: ${envelope.error.code}`);
  }
}

/** Non-2xx whose body could not be decoded as a protocol envelope. */
export class CompanionHttpStatusError extends CompanionTransportError {
  override readonly name = "CompanionHttpStatusError";
  constructor(readonly status: number) {
    super(`http status ${status} without decodable envelope`);
  }
}

/** Non-2xx carrying the simplified pairing envelope { error: { code } }. */
export class CompanionPairingServerError extends CompanionTransportError {
  override readonly name = "CompanionPairingServerError";
  constructor(
    readonly status: number,
    readonly code: string,
  ) {
    super(`pairing rejected ${status}: ${code}`);
  }
}

/**
 * Retry classification for mutations (single idempotent retry with the same
 * sequence + requestId + body bytes).
 */
export function isSafeForImmediateIdempotentRetry(error: unknown): boolean {
  if (error instanceof CompanionPayloadTooLargeError) return false;
  if (error instanceof CompanionCredentialDeviceMismatchError) return false;
  if (error instanceof CompanionServerError) {
    return (
      error.status >= 500 && error.status <= 599 && error.envelope.error.retryable
    );
  }
  if (error instanceof CompanionHttpStatusError) {
    return error.status >= 500 && error.status <= 599;
  }
  if (
    error instanceof CompanionEmptyResponseError ||
    error instanceof CompanionDecodeError ||
    error instanceof CompanionRequestIdMismatchError
  ) {
    return true;
  }
  // Ambiguous transport failure: retry the exact request once.
  if (error instanceof CompanionNetworkError) return true;
  return false;
}

/**
 * Schema/feature rejection is not a transport degradation: it must terminate
 * the current authority and re-enter capability negotiation. An HTTP 426
 * without a decodable envelope is the compatibility signal for servers that
 * cannot encode an envelope this client can read.
 */
export function needsRenegotiation(error: unknown): boolean {
  if (error instanceof CompanionServerError) {
    return MUTATION_RENEGOTIATE_CODES.has(error.envelope.error.code);
  }
  if (error instanceof CompanionHttpStatusError) {
    return error.status === 426;
  }
  return false;
}

export function acceptedSequenceOf(error: unknown): number | null {
  if (error instanceof CompanionServerError) {
    return error.envelope.error.acceptedSequence;
  }
  return null;
}
