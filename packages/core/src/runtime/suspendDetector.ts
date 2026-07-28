import { logger } from "./logger.js";

/**
 * Safety net beneath the PowerShell system-events source: a wall-clock jump
 * across a 5s sampling interval means the machine slept without us seeing a
 * suspend event. We then force a resume-style renegotiation so a stale
 * presence cannot survive an unobserved sleep (the server lease already
 * expired it remotely).
 */
export class SuspendDetector {
  private timer: NodeJS.Timeout | null = null;
  private lastTick = Date.now();

  constructor(
    private readonly onGapDetected: () => void,
    private readonly gapThresholdMs = 15_000,
  ) {}

  start(): void {
    if (this.timer !== null) return;
    this.lastTick = Date.now();
    this.timer = setInterval(() => {
      const now = Date.now();
      const gap = now - this.lastTick;
      this.lastTick = now;
      if (gap > this.gapThresholdMs) {
        logger.warn("suspend-detector", `wall-clock gap ${Math.round(gap / 1000)}s`);
        this.onGapDetected();
      }
    }, 5000);
    this.timer.unref?.();
  }

  stop(): void {
    if (this.timer !== null) clearInterval(this.timer);
    this.timer = null;
  }
}
