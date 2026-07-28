import type { PrivacyConfig } from "@yohaku/shared";
import type { ForegroundInfo, MediaProvider } from "../capture/types.js";
import { mediaDecision, processDecision } from "./evaluator.js";
import { policyFingerprint } from "./fingerprint.js";
import { applyMapping } from "./model.js";
import { MediaSessionTracker } from "./mediaSessionTracker.js";
import { sanitizeApplication, sanitizeMedia } from "./sanitize.js";
import type { SanitizedPresenceSnapshot } from "./types.js";

const MEDIA_TIMEOUT_MS = 2000;

/**
 * Fresh-capture service, ported from CompanionPresenceCapture.swift.
 * Every delivery captures anew — snapshots are never replayed. The privacy
 * configuration is re-read AFTER every await point (fail-closed: a rule
 * tightened during the media provider's suspension applies to both sources).
 * Media is captured before the application so the application decision uses
 * the newest configuration.
 */
export interface ForegroundSource {
  current(): ForegroundInfo | null;
}

export class CaptureService {
  private readonly tracker = new MediaSessionTracker();

  constructor(
    private readonly foreground: ForegroundSource,
    private readonly mediaProvider: () => MediaProvider | null,
    private readonly currentPrivacy: () => PrivacyConfig,
  ) {}

  fingerprint(): string {
    return policyFingerprint(this.currentPrivacy());
  }

  resetMediaContinuity(): void {
    this.tracker.reset();
  }

  async captureForDelivery(options: {
    includeMedia: boolean;
  }): Promise<SanitizedPresenceSnapshot> {
    const observedAt = Date.now();

    // --- media first (async) -------------------------------------------------
    let media: SanitizedPresenceSnapshot["media"] = null;
    const provider = this.mediaProvider();
    if (options.includeMedia && provider !== null && this.currentPrivacy().sources.media) {
      const raw = await withTimeout(provider.getSnapshot(), MEDIA_TIMEOUT_MS);
      // Re-read the privacy configuration AFTER the await (fail-closed).
      const config = this.currentPrivacy();
      if (raw !== null && config.sources.media && raw.playing) {
        const playerName = raw.playerDisplayName ?? raw.sourceAppUserModelId ?? "";
        const decision = mediaDecision(config, raw.appId, playerName);
        const sanitized = sanitizeMedia(
          {
            kind: raw.kind,
            title: raw.title,
            artist: raw.artist,
            album: raw.album,
            capturedPlayerName: raw.playerDisplayName,
            mappedPlayerName:
              raw.playerDisplayName === null
                ? null
                : applyMapping(config, "media_process_name", raw.playerDisplayName),
            playing: raw.playing,
            durationSeconds: raw.durationSeconds,
            positionSeconds: raw.positionSeconds,
            sampledAt: raw.sampledAt,
          },
          decision,
          { requiresArtist: config.ignoreNullArtist },
        );
        if (sanitized !== null) {
          media = {
            ...sanitized,
            sessionId: this.tracker.sessionId({
              kind: sanitized.kind,
              title: sanitized.title,
              artist: sanitized.artist,
              album: sanitized.album,
              playerDisplayName: sanitized.playerDisplayName,
              durationSeconds: sanitized.playback.durationSeconds,
            }),
          };
        }
      }
      if (media === null) {
        // Paused, hidden, or dropped media breaks session continuity.
        this.tracker.reset();
      }
    } else if (options.includeMedia) {
      this.tracker.reset();
    }

    // --- application second (sync), with the freshest configuration ---------
    let application: SanitizedPresenceSnapshot["application"] = null;
    const config = this.currentPrivacy();
    if (config.sources.application) {
      const info = this.foreground.current();
      if (info !== null) {
        const decision = processDecision(config, info.appId);
        application = sanitizeApplication(
          {
            capturedDisplayName: info.displayName,
            mappedDisplayName: applyMapping(config, "process_name", info.displayName),
            windowTitle: info.windowTitle,
          },
          decision,
          config.shareWindowTitles,
        );
      }
    }

    return { observedAt, application, media };
  }
}

async function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T | null> {
  let timer: NodeJS.Timeout | null = null;
  try {
    return await Promise.race([
      promise,
      new Promise<null>((resolve) => {
        timer = setTimeout(() => resolve(null), ms);
      }),
    ]);
  } catch {
    return null;
  } finally {
    if (timer !== null) clearTimeout(timer);
  }
}
