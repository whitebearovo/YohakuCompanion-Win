import { basename } from "node:path";
import { logger } from "../../runtime/logger.js";
import type { ForegroundInfo } from "../types.js";
import { displayNameFor } from "./displayName.js";
import { sampleForeground } from "./win32.js";

const POLL_INTERVAL_MS = 1000;
const DEBOUNCE_MS = 500;

/** Polls the foreground window and emits debounced semantic changes. */
export class ForegroundWatcher {
  private timer: NodeJS.Timeout | null = null;
  private debounceTimer: NodeJS.Timeout | null = null;
  private lastKey: string | null = null;
  private lastInfo: ForegroundInfo | null = null;
  private readonly listeners = new Set<(info: ForegroundInfo | null) => void>();

  start(): void {
    if (this.timer !== null) return;
    this.timer = setInterval(() => this.poll(), POLL_INTERVAL_MS);
    this.timer.unref?.();
    this.poll();
    logger.info("foreground", "watcher started");
  }

  stop(): void {
    if (this.timer !== null) clearInterval(this.timer);
    if (this.debounceTimer !== null) clearTimeout(this.debounceTimer);
    this.timer = null;
    this.debounceTimer = null;
    this.lastKey = null;
    logger.info("foreground", "watcher stopped");
  }

  onChange(callback: (info: ForegroundInfo | null) => void): () => void {
    this.listeners.add(callback);
    return () => this.listeners.delete(callback);
  }

  current(): ForegroundInfo | null {
    const raw = sampleForeground();
    if (raw === null || raw.exePath === null) return this.lastInfo;
    const appId = basename(raw.exePath).toLowerCase();
    if (appId.length === 0) return this.lastInfo;
    const info: ForegroundInfo = {
      appId,
      exePath: raw.exePath,
      displayName: displayNameFor(raw.exePath, appId, () => this.poll()),
      windowTitle: raw.windowTitle,
    };
    this.lastInfo = info;
    return info;
  }

  private poll(): void {
    const info = this.current();
    const key = info === null ? "" : `${info.appId}\\0${info.windowTitle ?? ""}`;
    if (key === this.lastKey) return;
    this.lastKey = key;
    if (this.debounceTimer !== null) clearTimeout(this.debounceTimer);
    this.debounceTimer = setTimeout(() => {
      this.debounceTimer = null;
      for (const listener of this.listeners) listener(info);
    }, DEBOUNCE_MS);
    this.debounceTimer.unref?.();
  }
}
