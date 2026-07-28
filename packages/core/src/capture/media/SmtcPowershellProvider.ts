import { logger } from "../../runtime/logger.js";
import type { MediaKind } from "../../privacy/types.js";
import type { MediaProvider, MediaSnapshot } from "../types.js";
import { exeStemDisplayName } from "../foreground/displayName.js";
import { NdjsonProcessHost } from "./NdjsonProcessHost.js";

/**
 * Implementation B (fallback): SMTC via a bundled PowerShell 5.1 helper that
 * emits NDJSON frames every second. No artwork support (not needed in v1).
 */

interface PsSessionFrame {
  sourceAppId?: string;
  title?: string;
  artist?: string;
  album?: string;
  kind?: string;
  playing?: boolean;
  duration?: number;
  position?: number;
  updatedAt?: number;
}

const KINDS = new Set<MediaKind>(["music", "podcast", "video", "unknown"]);

function toNull(value: string | undefined): string | null {
  const trimmed = (value ?? "").trim();
  return trimmed.length === 0 ? null : trimmed;
}

export class SmtcPowershellProvider implements MediaProvider {
  readonly kind = "powershell" as const;
  private readonly host: NdjsonProcessHost;
  private lastFrame: PsSessionFrame | null = null;
  private lastFrameAt = 0;
  private lastSemanticKey = "";
  private readonly listeners = new Set<() => void>();

  constructor(scriptPath: string) {
    this.host = new NdjsonProcessHost({ scriptPath, scope: "media-ps" });
    this.host.onMessage((message) => this.handleMessage(message));
  }

  async start(): Promise<void> {
    this.host.start();
    logger.info("media", "powershell SMTC provider started");
  }

  async stop(): Promise<void> {
    this.host.stop();
    logger.info("media", "powershell SMTC provider stopped");
  }

  healthy(): boolean {
    return this.host.healthy();
  }

  onSemanticChange(callback: () => void): () => void {
    this.listeners.add(callback);
    return () => this.listeners.delete(callback);
  }

  async getSnapshot(): Promise<MediaSnapshot | null> {
    const frame = this.lastFrame;
    if (frame === null) return null;
    // A frame older than 10s means the helper is stalled — treat as unknown.
    if (Date.now() - this.lastFrameAt > 10_000) return null;
    return this.toSnapshot(frame);
  }

  private handleMessage(message: unknown): void {
    const typed = message as { type?: string; session?: PsSessionFrame | null };
    if (typed.type !== "media") return;
    this.lastFrame = typed.session ?? null;
    this.lastFrameAt = Date.now();
    const f = this.lastFrame;
    const key =
      f === null
        ? ""
        : JSON.stringify([f.sourceAppId, f.title, f.artist, f.album, f.playing]);
    if (key !== this.lastSemanticKey) {
      this.lastSemanticKey = key;
      for (const listener of this.listeners) listener();
    }
  }

  private toSnapshot(frame: PsSessionFrame): MediaSnapshot | null {
    const source = (frame.sourceAppId ?? "").trim();
    if (source.length === 0 && toNull(frame.title) === null) return null;
    const appId = /\.exe$/i.test(source) ? source.toLowerCase() : null;
    const playing = frame.playing === true;
    const now = Date.now();

    const rawDuration = frame.duration ?? 0;
    const duration = Number.isFinite(rawDuration) && rawDuration > 0 ? rawDuration : null;
    let position: number | null =
      Number.isFinite(frame.position ?? Number.NaN) && (frame.position ?? -1) >= 0
        ? frame.position!
        : null;
    if (position !== null && playing && Number.isFinite(frame.updatedAt ?? Number.NaN)) {
      const elapsed = (now - frame.updatedAt!) / 1000;
      if (elapsed > 0 && elapsed < 6 * 3600) position += elapsed;
      if (duration !== null && position > duration) position = duration;
    }

    const kind = KINDS.has(frame.kind as MediaKind) ? (frame.kind as MediaKind) : "unknown";
    return {
      appId,
      sourceAppUserModelId: source.length > 0 ? source : null,
      playerDisplayName:
        appId !== null ? exeStemDisplayName(appId) : toNull(source),
      kind,
      title: toNull(frame.title),
      artist: toNull(frame.artist),
      album: toNull(frame.album),
      playing,
      durationSeconds: duration,
      positionSeconds: position,
      sampledAt: now,
    };
  }
}
