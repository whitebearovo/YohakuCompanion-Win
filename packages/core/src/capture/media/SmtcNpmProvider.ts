import { logger } from "../../runtime/logger.js";
import type { MediaKind } from "../../privacy/types.js";
import type { MediaProvider, MediaSnapshot } from "../types.js";
import { exeStemDisplayName } from "../foreground/displayName.js";

/**
 * Implementation A: in-process SMTC via @coooookies/windows-smtc-monitor
 * (napi-rs prebuilt). Timeline units confirmed from the package README:
 * position/duration are float seconds; lastUpdatedTime is epoch ms.
 * sourceAppId is usually an exe name ("spotify.exe") or an AUMID.
 */

const PLAYING = 4; // PlaybackStatus.PLAYING
const KIND_BY_PLAYBACK_TYPE: Record<number, MediaKind> = {
  1: "music",
  3: "video",
};

interface NpmMediaInfo {
  sourceAppId: string;
  media: { title: string; artist: string; albumTitle: string };
  playback: { playbackStatus: number; playbackType: number };
  timeline: { position: number; duration: number };
  lastUpdatedTime: number;
}

interface NpmMonitorModule {
  SMTCMonitor: {
    new (): {
      on(event: string, cb: (...args: unknown[]) => void): unknown;
      destroy(): void;
    };
    getCurrentMediaSession(): NpmMediaInfo | null;
  };
}

function toNull(value: string | undefined): string | null {
  const trimmed = (value ?? "").trim();
  return trimmed.length === 0 ? null : trimmed;
}

export class SmtcNpmProvider implements MediaProvider {
  readonly kind = "npm" as const;
  private module: NpmMonitorModule | null = null;
  private monitor: InstanceType<NpmMonitorModule["SMTCMonitor"]> | null = null;
  private readonly listeners = new Set<() => void>();
  private lastSemanticKey = "";
  private isHealthy = false;

  async start(): Promise<void> {
    const imported = (await import("@coooookies/windows-smtc-monitor")) as unknown;
    this.module = imported as NpmMonitorModule;
    // Probe: a native call must succeed before we call this provider healthy.
    this.module.SMTCMonitor.getCurrentMediaSession();
    this.monitor = new this.module.SMTCMonitor();
    const semantic = () => this.notifyIfSemanticChange();
    this.monitor.on("session-media-changed", semantic);
    this.monitor.on("session-playback-changed", semantic);
    this.monitor.on("session-added", semantic);
    this.monitor.on("session-removed", semantic);
    this.monitor.on("current-session-changed", semantic);
    // Deliberately NOT session-timeline-changed: progress ticks are not
    // semantic changes and must not trigger refresh storms.
    this.isHealthy = true;
    logger.info("media", "npm SMTC provider started");
  }

  async stop(): Promise<void> {
    this.monitor?.destroy();
    this.monitor = null;
    this.isHealthy = false;
    logger.info("media", "npm SMTC provider stopped");
  }

  healthy(): boolean {
    return this.isHealthy;
  }

  onSemanticChange(callback: () => void): () => void {
    this.listeners.add(callback);
    return () => this.listeners.delete(callback);
  }

  async getSnapshot(): Promise<MediaSnapshot | null> {
    if (this.module === null) return null;
    let info: NpmMediaInfo | null;
    try {
      info = this.module.SMTCMonitor.getCurrentMediaSession();
    } catch (error) {
      this.isHealthy = false;
      logger.warn("media", `npm SMTC snapshot failed: ${(error as Error).name}`);
      return null;
    }
    return info === null ? null : this.toSnapshot(info);
  }

  private toSnapshot(info: NpmMediaInfo): MediaSnapshot {
    const now = Date.now();
    const playing = info.playback.playbackStatus === PLAYING;
    const source = info.sourceAppId.trim();
    const appId = /\.exe$/i.test(source) ? source.toLowerCase() : null;

    const duration = info.timeline.duration > 0 ? info.timeline.duration : null;
    let position: number | null =
      Number.isFinite(info.timeline.position) && info.timeline.position >= 0
        ? info.timeline.position
        : null;
    // Extrapolate a playing position from the last native update to now.
    if (position !== null && playing && Number.isFinite(info.lastUpdatedTime)) {
      const elapsed = (now - info.lastUpdatedTime) / 1000;
      if (elapsed > 0 && elapsed < 6 * 3600) position += elapsed;
      if (duration !== null && position > duration) position = duration;
    }

    return {
      appId,
      sourceAppUserModelId: source.length > 0 ? source : null,
      playerDisplayName:
        appId !== null
          ? exeStemDisplayName(appId)
          : toNull(source.split("!")[0]?.split(".").pop() ?? source),
      kind: KIND_BY_PLAYBACK_TYPE[info.playback.playbackType] ?? "unknown",
      title: toNull(info.media.title),
      artist: toNull(info.media.artist),
      album: toNull(info.media.albumTitle),
      playing,
      durationSeconds: duration,
      positionSeconds: position,
      sampledAt: now,
    };
  }

  private notifyIfSemanticChange(): void {
    let key = "";
    try {
      const info = this.module?.SMTCMonitor.getCurrentMediaSession() ?? null;
      key =
        info === null
          ? ""
          : JSON.stringify([
              info.sourceAppId,
              info.media.title,
              info.media.artist,
              info.media.albumTitle,
              info.playback.playbackStatus,
            ]);
    } catch {
      return;
    }
    if (key === this.lastSemanticKey) return;
    this.lastSemanticKey = key;
    for (const listener of this.listeners) listener();
  }
}
