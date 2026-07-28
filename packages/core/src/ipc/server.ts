import { WebSocketServer, type WebSocket } from "ws";
import {
  clientMessageSchema,
  type Command,
  type CoreStateSnapshot,
  type IpcErrorCode,
  type ServerMessage,
} from "@yohaku/shared";
import { logger } from "../runtime/logger.js";

/**
 * Localhost WebSocket control plane for the shell/WebView. Security model:
 * bind 127.0.0.1 only; the first frame must be a hello carrying the launch
 * token (delivered via the shell's environment) within 3 seconds; browser
 * origins are checked against a whitelist (native clients send no Origin).
 * Broadcast snapshots contain no secrets and no un-sanitized capture text.
 */

const ALLOWED_ORIGINS = new Set([
  "tauri://localhost",
  "http://tauri.localhost",
  "https://tauri.localhost",
  "http://localhost:5173",
  "http://localhost:1420",
]);

export interface IpcServerOptions {
  token: string;
  getSnapshot: () => CoreStateSnapshot;
  onCommand: (command: Command) => Promise<IpcErrorCode | null>;
}

export class IpcServer {
  private wss: WebSocketServer | null = null;
  private readonly authenticated = new Set<WebSocket>();
  private broadcastTimer: NodeJS.Timeout | null = null;

  constructor(private readonly options: IpcServerOptions) {}

  start(): Promise<number> {
    return new Promise((resolve, reject) => {
      const wss = new WebSocketServer({ host: "127.0.0.1", port: 0 });
      this.wss = wss;
      wss.on("error", reject);
      wss.on("listening", () => {
        const address = wss.address();
        if (address === null || typeof address === "string") {
          reject(new Error("no listen address"));
          return;
        }
        logger.info("ipc", `listening on 127.0.0.1:${address.port}`);
        resolve(address.port);
      });
      wss.on("connection", (socket, request) => {
        const origin = request.headers.origin;
        if (origin !== undefined && !ALLOWED_ORIGINS.has(origin)) {
          socket.close(4003, "origin rejected");
          return;
        }
        const helloTimeout = setTimeout(() => {
          if (!this.authenticated.has(socket)) socket.close(4001, "hello timeout");
        }, 3000);
        helloTimeout.unref?.();

        socket.on("message", (raw) => {
          void this.handleMessage(socket, raw as Buffer, helloTimeout);
        });
        socket.on("close", () => {
          clearTimeout(helloTimeout);
          this.authenticated.delete(socket);
        });
      });
    });
  }

  async stop(): Promise<void> {
    if (this.broadcastTimer !== null) clearTimeout(this.broadcastTimer);
    const wss = this.wss;
    this.wss = null;
    if (wss !== null) {
      for (const client of wss.clients) client.terminate();
      await new Promise<void>((resolve) => wss.close(() => resolve()));
    }
  }

  /** Coalesces broadcast requests into at most one snapshot per 100ms. */
  broadcastState(): void {
    if (this.broadcastTimer !== null) return;
    this.broadcastTimer = setTimeout(() => {
      this.broadcastTimer = null;
      this.pushSnapshot();
    }, 100);
    this.broadcastTimer.unref?.();
  }

  private pushSnapshot(): void {
    const message: ServerMessage = {
      type: "state",
      snapshot: this.options.getSnapshot(),
    };
    const encoded = JSON.stringify(message);
    for (const socket of this.authenticated) {
      if (socket.readyState === socket.OPEN) socket.send(encoded);
    }
  }

  private send(socket: WebSocket, message: ServerMessage): void {
    if (socket.readyState === socket.OPEN) socket.send(JSON.stringify(message));
  }

  private async handleMessage(
    socket: WebSocket,
    raw: Buffer,
    helloTimeout: NodeJS.Timeout,
  ): Promise<void> {
    let parsed: unknown;
    try {
      parsed = JSON.parse(raw.toString("utf8"));
    } catch {
      socket.close(4002, "invalid frame");
      return;
    }
    const message = clientMessageSchema.safeParse(parsed);
    if (!message.success) {
      if (this.authenticated.has(socket)) {
        const id = (parsed as { id?: unknown }).id;
        if (typeof id === "string") {
          this.send(socket, { type: "result", id, ok: false, error: "invalidInput" });
          return;
        }
      }
      socket.close(4002, "invalid frame");
      return;
    }

    if ("type" in message.data) {
      if (message.data.token === this.options.token) {
        clearTimeout(helloTimeout);
        this.authenticated.add(socket);
        this.send(socket, { type: "state", snapshot: this.options.getSnapshot() });
      } else {
        socket.close(4001, "bad token");
      }
      return;
    }

    if (!this.authenticated.has(socket)) {
      socket.close(4001, "not authenticated");
      return;
    }

    const command = message.data;
    let error: IpcErrorCode | null;
    try {
      error = await this.options.onCommand(command);
    } catch {
      error = "internal";
    }
    if (error === null) {
      this.send(socket, { type: "result", id: command.id, ok: true });
    } else {
      this.send(socket, { type: "result", id: command.id, ok: false, error });
    }
    this.broadcastState();
  }
}
