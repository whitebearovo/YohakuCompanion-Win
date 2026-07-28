import type { MediaKind } from "../privacy/types.js";

/**
 * Raw capture values. These types exist ONLY between the capture layer and
 * the privacy pipeline — they must never reach the network, persistence, or
 * the UI without passing the sanitizers.
 */

export interface ForegroundInfo {
  /** Lowercased executable file name, e.g. "code.exe". */
  appId: string;
  exePath: string | null;
  /** Friendly name (FileDescription) or exe stem fallback. */
  displayName: string;
  windowTitle: string | null;
}

export interface MediaSnapshot {
  /** Lowercased exe name when the SMTC source id looks like one, else null. */
  appId: string | null;
  sourceAppUserModelId: string | null;
  playerDisplayName: string | null;
  kind: MediaKind;
  title: string | null;
  artist: string | null;
  album: string | null;
  playing: boolean;
  durationSeconds: number | null;
  positionSeconds: number | null;
  /** Epoch ms at which positionSeconds was (re)computed. */
  sampledAt: number;
}

export interface MediaProvider {
  readonly kind: "npm" | "powershell";
  start(): Promise<void>;
  stop(): Promise<void>;
  /** Bounded fresh lookup of the current session; null when nothing plays. */
  getSnapshot(options?: { timeoutMs?: number }): Promise<MediaSnapshot | null>;
  /** Fires on semantic changes (track/state/session), not on progress ticks. */
  onSemanticChange(callback: () => void): () => void;
  healthy(): boolean;
}

export interface SystemEventsProvider {
  start(): Promise<void>;
  stop(): Promise<void>;
  onLockOrSleep(callback: () => void): () => void;
  onUnlockOrResume(callback: () => void): () => void;
}
