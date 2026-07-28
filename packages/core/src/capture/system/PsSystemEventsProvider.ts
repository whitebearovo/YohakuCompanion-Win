import { logger } from "../../runtime/logger.js";
import type { SystemEventsProvider } from "../types.js";
import { NdjsonProcessHost } from "../media/NdjsonProcessHost.js";

/**
 * Lock/sleep signals via the bundled system-events.ps1 helper. Deduplicates
 * repeated notifications (lock followed by suspend collapses into one
 * lock-or-sleep transition until the matching resume).
 */
export class PsSystemEventsProvider implements SystemEventsProvider {
  private readonly host: NdjsonProcessHost;
  private readonly lockListeners = new Set<() => void>();
  private readonly resumeListeners = new Set<() => void>();
  private suspended = false;

  constructor(scriptPath: string) {
    this.host = new NdjsonProcessHost({ scriptPath, scope: "system-events" });
    this.host.onMessage((message) => this.handle(message));
  }

  async start(): Promise<void> {
    this.host.start();
    logger.info("system-events", "provider started");
  }

  async stop(): Promise<void> {
    this.host.stop();
  }

  onLockOrSleep(callback: () => void): () => void {
    this.lockListeners.add(callback);
    return () => this.lockListeners.delete(callback);
  }

  onUnlockOrResume(callback: () => void): () => void {
    this.resumeListeners.add(callback);
    return () => this.resumeListeners.delete(callback);
  }

  private handle(message: unknown): void {
    const typed = message as { type?: string; event?: string };
    if (typed.type !== "event") return;
    if (typed.event === "lock" || typed.event === "suspend") {
      if (this.suspended) return; // duplicate notifications are idempotent
      this.suspended = true;
      for (const listener of this.lockListeners) listener();
    } else if (typed.event === "unlock" || typed.event === "resume") {
      if (!this.suspended) return;
      this.suspended = false;
      for (const listener of this.resumeListeners) listener();
    }
  }
}
