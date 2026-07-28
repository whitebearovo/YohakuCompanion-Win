import { spawn, type ChildProcessByStdio } from "node:child_process";
import type { Readable } from "node:stream";
import { createInterface } from "node:readline";
import { logger } from "../../runtime/logger.js";

/**
 * Hosts a long-running PowerShell helper that emits NDJSON on stdout.
 * Shared by the SMTC fallback provider and the system-events provider.
 * Heartbeat-supervised: a silent helper is restarted with exponential
 * backoff; after `maxRestarts` consecutive failures the host goes unhealthy.
 */

export interface NdjsonHostOptions {
  scriptPath: string;
  scope: string;
  heartbeatTimeoutMs?: number;
  maxRestarts?: number;
}

export class NdjsonProcessHost {
  private child: ChildProcessByStdio<null, Readable, Readable> | null = null;
  private watchdog: NodeJS.Timeout | null = null;
  private restarts = 0;
  private stopped = true;
  private readonly listeners = new Set<(message: unknown) => void>();
  private readonly heartbeatTimeoutMs: number;
  private readonly maxRestarts: number;

  constructor(private readonly options: NdjsonHostOptions) {
    this.heartbeatTimeoutMs = options.heartbeatTimeoutMs ?? 90_000;
    this.maxRestarts = options.maxRestarts ?? 5;
  }

  onMessage(callback: (message: unknown) => void): () => void {
    this.listeners.add(callback);
    return () => this.listeners.delete(callback);
  }

  healthy(): boolean {
    return this.child !== null;
  }

  start(): void {
    this.stopped = false;
    this.restarts = 0;
    this.spawnChild();
  }

  stop(): void {
    this.stopped = true;
    this.clearWatchdog();
    if (this.child !== null) {
      this.child.kill();
      this.child = null;
    }
  }

  private spawnChild(): void {
    if (this.stopped) return;
    const child = spawn(
      "powershell.exe",
      [
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        this.options.scriptPath,
      ],
      { windowsHide: true, stdio: ["ignore", "pipe", "pipe"] },
    );
    this.child = child;
    this.armWatchdog();

    const lines = createInterface({ input: child.stdout });
    lines.on("line", (line) => {
      this.armWatchdog();
      let message: unknown;
      try {
        message = JSON.parse(line);
      } catch {
        return; // non-JSON noise from the host is ignored
      }
      for (const listener of this.listeners) listener(message);
    });
    child.stderr.on("data", () => {
      /* helper stderr may contain window titles/media text — never logged */
    });
    child.on("exit", (code) => {
      if (this.child !== child) return;
      this.child = null;
      this.clearWatchdog();
      if (!this.stopped) {
        logger.warn(this.options.scope, `helper exited (code ${code ?? "?"})`);
        this.scheduleRestart();
      }
    });
    child.on("error", () => {
      if (this.child !== child) return;
      this.child = null;
      this.clearWatchdog();
      if (!this.stopped) this.scheduleRestart();
    });
  }

  private scheduleRestart(): void {
    if (this.stopped) return;
    this.restarts += 1;
    if (this.restarts > this.maxRestarts) {
      logger.error(this.options.scope, "helper restart limit reached; giving up");
      return;
    }
    const delay = Math.min(30_000, 1000 * 2 ** (this.restarts - 1));
    const timer = setTimeout(() => this.spawnChild(), delay);
    timer.unref?.();
    logger.info(
      this.options.scope,
      `helper restart ${this.restarts}/${this.maxRestarts} in ${delay}ms`,
    );
  }

  private armWatchdog(): void {
    this.clearWatchdog();
    this.watchdog = setTimeout(() => {
      logger.warn(this.options.scope, "helper heartbeat timeout; restarting");
      const child = this.child;
      this.child = null;
      child?.kill();
      this.scheduleRestart();
    }, this.heartbeatTimeoutMs);
    this.watchdog.unref?.();
  }

  private clearWatchdog(): void {
    if (this.watchdog !== null) clearTimeout(this.watchdog);
    this.watchdog = null;
  }
}
