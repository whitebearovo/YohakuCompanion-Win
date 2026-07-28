import type { z } from "zod";
import { PROTOCOL_CLIENT_VERSION } from "../protocol/wire.js";
import {
  CompanionDecodeError,
  CompanionEmptyResponseError,
  CompanionHttpStatusError,
  CompanionNetworkError,
  CompanionPairingServerError,
  CompanionPayloadTooLargeError,
  CompanionRequestIdMismatchError,
  CompanionServerError,
} from "./errors.js";
import {
  errorEnvelopeSchema,
  pairingErrorEnvelopeSchema,
} from "../protocol/types.js";

/**
 * HTTP layer for Companion Protocol v2, ported from CompanionHTTPClient.swift.
 * Base URL must be HTTPS (loopback HTTP allowed for local development), with
 * no credentials, query, or fragment. Path segments are appended to any
 * existing base path prefix. Bearer token + version header are attached only
 * to authenticated requests. 10s timeout; encoded payloads are size-checked
 * before sending; response requestId must echo the request's.
 */

const REQUEST_TIMEOUT_MS = 10_000;
const DEFAULT_MAX_PAYLOAD_BYTES = 32 * 1024;
export const VERSION_HEADER = "X-Yohaku-Companion-Version";

export function isLoopbackHost(host: string): boolean {
  const lowered = host.toLowerCase();
  if (lowered === "localhost" || lowered === "::1" || lowered === "[::1]") {
    return true;
  }
  const parts = lowered.split(".");
  if (parts.length !== 4 || parts[0] !== "127") return false;
  return parts.every((p) => /^\d{1,3}$/.test(p) && Number.parseInt(p, 10) <= 255);
}

export class CompanionServerConfigurationError extends Error {
  override readonly name = "CompanionServerConfigurationError";
}

export class CompanionServerConfiguration {
  readonly baseUrl: URL;

  constructor(baseUrl: string) {
    let parsed: URL;
    try {
      parsed = new URL(baseUrl);
    } catch {
      throw new CompanionServerConfigurationError(`invalid base URL`);
    }
    const scheme = parsed.protocol.replace(":", "").toLowerCase();
    if (scheme !== "https" && !(scheme === "http" && isLoopbackHost(parsed.hostname))) {
      throw new CompanionServerConfigurationError(
        "base URL must use HTTPS (HTTP is allowed only for loopback hosts)",
      );
    }
    if (parsed.username !== "" || parsed.password !== "") {
      throw new CompanionServerConfigurationError("base URL must not embed credentials");
    }
    if (parsed.search !== "" || parsed.hash !== "") {
      throw new CompanionServerConfigurationError(
        "base URL must not contain query or fragment",
      );
    }
    this.baseUrl = parsed;
  }

  endpoint(path: string): URL {
    const url = new URL(this.baseUrl.toString());
    const basePath = url.pathname.endsWith("/")
      ? url.pathname.slice(0, -1)
      : url.pathname;
    const suffix = path.startsWith("/") ? path : `/${path}`;
    url.pathname = `${basePath}${suffix}`;
    return url;
  }
}

export interface CompanionCredential {
  deviceId: string;
  deviceToken: string;
}

export interface ExecuteOptions<T> {
  method: "GET" | "POST" | "PUT";
  path: string;
  body?: unknown;
  credential?: CompanionCredential;
  responseSchema: z.ZodType<T>;
  /** Expected echo of meta.requestId; checked when provided. */
  expectedRequestId?: string;
  maximumPayloadBytes?: number;
  /** Pre-encoded body bytes — used by idempotent retries to resend exactly. */
  encodedBody?: Uint8Array;
}

export class CompanionHttpClient {
  constructor(
    private readonly configuration: CompanionServerConfiguration,
    private readonly fetchImpl: typeof fetch = fetch,
  ) {}

  encodeBody(body: unknown): Uint8Array {
    return new TextEncoder().encode(JSON.stringify(body));
  }

  async execute<T>(options: ExecuteOptions<T>): Promise<T> {
    const url = this.configuration.endpoint(options.path);
    const headers: Record<string, string> = { Accept: "application/json" };
    if (options.credential) {
      headers.Authorization = `Bearer ${options.credential.deviceToken}`;
      headers[VERSION_HEADER] = PROTOCOL_CLIENT_VERSION;
    }

    let bodyBytes: Uint8Array | undefined;
    if (options.encodedBody !== undefined) {
      bodyBytes = options.encodedBody;
    } else if (options.body !== undefined) {
      bodyBytes = this.encodeBody(options.body);
    }
    if (bodyBytes !== undefined) {
      headers["Content-Type"] = "application/json";
      const limit = options.maximumPayloadBytes ?? DEFAULT_MAX_PAYLOAD_BYTES;
      if (bodyBytes.byteLength > limit) {
        throw new CompanionPayloadTooLargeError(
          `payload ${bodyBytes.byteLength} bytes exceeds limit ${limit}`,
        );
      }
    }

    const init: RequestInit = {
      method: options.method,
      headers,
      signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    };
    if (bodyBytes !== undefined) {
      init.body = bodyBytes as unknown as Exclude<RequestInit["body"], undefined>;
    }

    let response: Response;
    try {
      response = await this.fetchImpl(url, init);
    } catch (error) {
      throw new CompanionNetworkError(error);
    }

    let text: string;
    try {
      text = await response.text();
    } catch (error) {
      throw new CompanionNetworkError(error);
    }
    if (text.length === 0) {
      throw new CompanionEmptyResponseError(`empty response (${response.status})`);
    }

    let json: unknown;
    try {
      json = JSON.parse(text);
    } catch {
      if (response.ok) throw new CompanionDecodeError("response is not JSON");
      throw new CompanionHttpStatusError(response.status);
    }

    if (response.ok) {
      const parsed = options.responseSchema.safeParse(json);
      if (!parsed.success) {
        throw new CompanionDecodeError(
          `response decode failed: ${parsed.error.issues[0]?.message ?? "unknown"}`,
        );
      }
      this.checkRequestIdEcho(parsed.data, options.expectedRequestId);
      return parsed.data;
    }

    const envelope = errorEnvelopeSchema.safeParse(json);
    if (envelope.success) {
      this.checkRequestIdEcho(envelope.data, options.expectedRequestId);
      throw new CompanionServerError(response.status, envelope.data);
    }
    const pairingEnvelope = pairingErrorEnvelopeSchema.safeParse(json);
    if (pairingEnvelope.success) {
      throw new CompanionPairingServerError(
        response.status,
        pairingEnvelope.data.error.code,
      );
    }
    throw new CompanionHttpStatusError(response.status);
  }

  private checkRequestIdEcho(payload: unknown, expected: string | undefined): void {
    if (expected === undefined) return;
    const meta = (payload as { meta?: { requestId?: unknown } }).meta;
    if (meta?.requestId !== expected) {
      throw new CompanionRequestIdMismatchError(
        "response requestId does not echo the request",
      );
    }
  }
}
