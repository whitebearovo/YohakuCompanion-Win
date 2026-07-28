import { createServer, type IncomingMessage, type Server } from "node:http";
import { randomUUID } from "node:crypto";

/** Minimal programmable mock of a Companion-enabled Mix Space Core. */

export interface RecordedRequest {
  method: string;
  path: string;
  headers: Record<string, string | string[] | undefined>;
  rawBody: string;
  json: unknown;
}

export type MockHandler = (req: RecordedRequest) => {
  status: number;
  body: unknown;
} | { socketDestroy: true };

export class MockCompanionServer {
  readonly requests: RecordedRequest[] = [];
  private handlers: MockHandler[] = [];
  private fallback: MockHandler | null = null;
  private server: Server | null = null;
  private port = 0;

  get baseUrl(): string {
    return `http://127.0.0.1:${this.port}`;
  }

  /** Queue a one-shot handler (consumed in order). */
  enqueue(handler: MockHandler): void {
    this.handlers.push(handler);
  }

  /** Handler used when the queue is empty. */
  setFallback(handler: MockHandler): void {
    this.fallback = handler;
  }

  async start(): Promise<void> {
    this.server = createServer((req, res) => {
      void this.collect(req).then((recorded) => {
        this.requests.push(recorded);
        const handler = this.handlers.shift() ?? this.fallback;
        if (!handler) {
          res.writeHead(500).end(JSON.stringify({ unexpected: true }));
          return;
        }
        const outcome = handler(recorded);
        if ("socketDestroy" in outcome) {
          req.socket.destroy();
          return;
        }
        res
          .writeHead(outcome.status, { "Content-Type": "application/json" })
          .end(JSON.stringify(outcome.body));
      });
    });
    await new Promise<void>((resolve) => {
      this.server!.listen(0, "127.0.0.1", () => {
        const address = this.server!.address();
        if (address && typeof address === "object") this.port = address.port;
        resolve();
      });
    });
  }

  async stop(): Promise<void> {
    if (!this.server) return;
    await new Promise<void>((resolve) => this.server!.close(() => resolve()));
    this.server = null;
  }

  private collect(req: IncomingMessage): Promise<RecordedRequest> {
    return new Promise((resolve) => {
      let raw = "";
      req.on("data", (chunk: Buffer) => {
        raw += chunk.toString("utf8");
      });
      req.on("end", () => {
        let json: unknown = null;
        try {
          json = raw.length > 0 ? JSON.parse(raw) : null;
        } catch {
          json = null;
        }
        resolve({
          method: req.method ?? "",
          path: req.url ?? "",
          headers: req.headers,
          rawBody: raw,
          json,
        });
      });
    });
  }
}

const NOW = "2026-07-26T04:00:12.345Z";

export function responseMeta(requestId: string): Record<string, unknown> {
  return {
    schema: "yohaku.companion.presence",
    schemaVersion: 2,
    requestId,
    serverTime: NOW,
  };
}

export function mutationSuccess(req: RecordedRequest, acceptedSequence?: number) {
  const meta = (req.json as { meta?: { requestId?: string; sequence?: number } }).meta;
  return {
    status: 200,
    body: {
      meta: responseMeta(meta?.requestId ?? randomUUID()),
      data: {
        acceptedSequence: acceptedSequence ?? meta?.sequence ?? 0,
        receivedAt: NOW,
        state: {
          schemaVersion: 2,
          epoch: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
          revision: 1,
          projection: null,
        },
      },
    },
  };
}

export function errorEnvelope(
  req: RecordedRequest,
  status: number,
  code: string,
  options: { retryable?: boolean; acceptedSequence?: number | null } = {},
) {
  const meta = (req.json as { meta?: { requestId?: string } }).meta;
  return {
    status,
    body: {
      meta: responseMeta(meta?.requestId ?? randomUUID()),
      error: {
        code,
        message: `${code} for testing`,
        retryable: options.retryable ?? false,
        retryAfterMs: null,
        acceptedSequence: options.acceptedSequence ?? null,
        fields: [],
      },
    },
  };
}

export function capabilitiesResponse(
  patch: {
    minimumClientVersion?: string;
    liveDesk?: boolean;
    mediaTimeline?: boolean;
    presenceSchemaVersions?: number[];
    requestsPerMinute?: number;
  } = {},
) {
  return {
    status: 200,
    body: {
      meta: responseMeta(randomUUID()),
      data: {
        minimumClientVersion: patch.minimumClientVersion ?? "1.7.0",
        presenceSchemaVersions: patch.presenceSchemaVersions ?? [2],
        momentSchemaVersions: [1],
        features: {
          liveDesk: patch.liveDesk ?? true,
          mediaTimeline: patch.mediaTimeline ?? true,
          moments: true,
          readingSessions: false,
        },
        limits: {
          presencePayloadBytes: 32768,
          presenceRequestsPerMinute: patch.requestsPerMinute ?? 120,
          presenceLeaseMinSeconds: 30,
          presenceLeaseMaxSeconds: 120,
          recommendedHeartbeatSeconds: 45,
          maximumClockSkewSeconds: 60,
        },
      },
    },
  };
}
