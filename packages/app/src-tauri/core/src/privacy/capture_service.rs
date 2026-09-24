//! Capture pipeline: raw capture -> evaluator -> sanitize ->
//! `SanitizedPresenceSnapshot`. Ported from CompanionPresenceCapture.swift
//! via `packages/core/src/privacy/captureService.ts`.
//!
//! Owned by agent V. Ground truth: captureService.ts and
//! `.claude/rewrite/specs/privacy.md` §8. Every delivery captures anew —
//! snapshots are never replayed. The privacy configuration is re-read AFTER
//! every await point (fail-closed: a rule tightened during the media
//! provider's suspension applies to both sources). Media is captured before
//! the application so the application decision uses the newest
//! configuration. The media path must NEVER propagate an error: timeout,
//! provider error and null snapshot all become "no media" (the provider
//! trait already folds its errors into `None`; the outer timeout here is
//! the TS caller-side 2000 ms race, and the losing snapshot future is
//! dropped, not cancelled cooperatively — its late result is discarded).

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use crate::capture::{ForegroundSource, MediaProvider};
use crate::model::{
    PrivacyConfig, PrivacyMappingType, SanitizedMediaPresence, SanitizedPresenceSnapshot,
};
use crate::privacy::evaluator::{media_decision, process_decision};
use crate::privacy::fingerprint::policy_fingerprint;
use crate::privacy::media_session_tracker::{MediaSemanticIdentity, MediaSessionTracker};
use crate::privacy::model::apply_mapping;
use crate::privacy::sanitize::{
    sanitize_application, sanitize_media, ApplicationSanitizeInput, MediaSanitizeInput,
    SanitizeMediaOptions,
};

/// Outer bound on one `get_snapshot` call (the TS 2000 ms race).
pub const MEDIA_TIMEOUT_MS: u64 = 2_000;

/// Options for one delivery capture. `include_media == false` leaves the
/// session tracker UNTOUCHED (caller-requested media-less deliveries do not
/// break continuity); every other media-less outcome resets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureOptions {
    pub include_media: bool,
}

pub struct CaptureService {
    foreground: Arc<dyn ForegroundSource>,
    /// Getter — the provider may appear/disappear at runtime.
    media_provider: Box<dyn Fn() -> Option<Arc<dyn MediaProvider>> + Send + Sync>,
    /// FRESH config read on EVERY call.
    current_privacy: Box<dyn Fn() -> PrivacyConfig + Send + Sync>,
    /// Never locked across an `.await`.
    tracker: Mutex<MediaSessionTracker>,
}

/// `Date.now()` equivalent (epoch milliseconds).
fn now_epoch_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl CaptureService {
    /// `media_provider` is a getter (the provider may appear/disappear at
    /// runtime); `current_privacy` performs a FRESH config read on EVERY
    /// call.
    pub fn new(
        foreground: Arc<dyn ForegroundSource>,
        media_provider: Box<dyn Fn() -> Option<Arc<dyn MediaProvider>> + Send + Sync>,
        current_privacy: Box<dyn Fn() -> PrivacyConfig + Send + Sync>,
    ) -> Self {
        Self {
            foreground,
            media_provider,
            current_privacy,
            tracker: Mutex::new(MediaSessionTracker::new()),
        }
    }

    /// `policy_fingerprint(current_privacy())` — fresh read.
    pub fn fingerprint(&self) -> String {
        policy_fingerprint(&(self.current_privacy)())
    }

    /// Tracker reset (coordinator generation teardown: a new Live Desk
    /// generation never continues an old session id).
    pub fn reset_media_continuity(&self) {
        self.tracker.lock().reset();
    }

    /// Exact order: `observed_at = now` BEFORE any await; media branch
    /// first (gated by include_media + provider + sources.media at read #1,
    /// bounded 2 s, config RE-READ after the await, playing gate, decision,
    /// sanitize, tracker session id / reset per the drop matrix);
    /// application branch second (freshest config read #3,
    /// sources.application gate, decision, process_name mapping, sanitize).
    /// Never fails, never panics.
    pub async fn capture_for_delivery(&self, options: CaptureOptions) -> SanitizedPresenceSnapshot {
        let observed_at = now_epoch_ms();

        // --- media first (async) ---------------------------------------------
        let mut media: Option<SanitizedMediaPresence> = None;
        let provider = (self.media_provider)();
        match provider {
            // Config read #1 happens only after include_media and the
            // provider check pass (TS short-circuit order).
            Some(provider) if options.include_media && (self.current_privacy)().sources.media => {
                let timeout = Duration::from_millis(MEDIA_TIMEOUT_MS);
                // Caller-side race: a hung provider yields None after ~2 s;
                // provider errors never escape (the trait returns Option).
                let raw = tokio::time::timeout(timeout, provider.get_snapshot(timeout))
                    .await
                    .ok()
                    .flatten();
                // Re-read the privacy configuration AFTER the await (fail-closed).
                let config = (self.current_privacy)();
                if let Some(raw) = raw {
                    // Paused/absent media is dropped here; sources.media
                    // flipped off during the await also drops it.
                    if config.sources.media && raw.playing {
                        // Rule fallback MAY use the raw AUMID when no display
                        // name exists; mapping lookup below never does (A5).
                        let player_name = raw
                            .player_display_name
                            .as_deref()
                            .or(raw.source_app_user_model_id.as_deref())
                            .unwrap_or("");
                        let decision = media_decision(&config, raw.app_id.as_deref(), player_name);
                        let mapped_player_name = match raw.player_display_name.as_deref() {
                            None => None,
                            Some(name) => {
                                apply_mapping(&config, PrivacyMappingType::MediaPlayerName, name)
                                    .or_else(|| {
                                        // Compatibility with mappings saved before
                                        // media_player_name (keyed by the CURRENT
                                        // player display name, A6).
                                        apply_mapping(
                                            &config,
                                            PrivacyMappingType::MediaProcessName,
                                            name,
                                        )
                                    })
                            }
                        };
                        let sanitized = sanitize_media(
                            &MediaSanitizeInput {
                                kind: raw.kind,
                                title: raw.title.as_deref(),
                                artist: raw.artist.as_deref(),
                                album: raw.album.as_deref(),
                                captured_player_name: raw.player_display_name.as_deref(),
                                mapped_player_name,
                                playing: raw.playing,
                                duration_seconds: raw.duration_seconds,
                                position_seconds: raw.position_seconds,
                                sampled_at: raw.sampled_at,
                            },
                            &decision,
                            &SanitizeMediaOptions {
                                requires_artist: config.ignore_null_artist,
                            },
                        );
                        if let Some(sanitized) = sanitized {
                            // Identity from SANITIZED fields (spec §7.1).
                            let session_id =
                                self.tracker.lock().session_id(&MediaSemanticIdentity {
                                    kind: sanitized.kind,
                                    title: sanitized.title.clone(),
                                    artist: sanitized.artist.clone(),
                                    album: sanitized.album.clone(),
                                    player_display_name: sanitized.player_display_name.clone(),
                                    duration_seconds: sanitized.playback.duration_seconds,
                                });
                            media = Some(SanitizedMediaPresence {
                                session_id,
                                kind: sanitized.kind,
                                title: sanitized.title,
                                artist: sanitized.artist,
                                album: sanitized.album,
                                player_display_name: sanitized.player_display_name,
                                playback: sanitized.playback,
                            });
                        }
                    }
                }
                if media.is_none() {
                    // Paused, hidden, or dropped media breaks session continuity.
                    self.tracker.lock().reset();
                }
            }
            // includeMedia requested but no provider / sources.media off at
            // read #1: continuity still breaks.
            _ if options.include_media => {
                self.tracker.lock().reset();
            }
            // include_media == false: tracker left UNTOUCHED.
            _ => {}
        }

        // --- application second (sync), with the freshest configuration ------
        let mut application = None;
        let config = (self.current_privacy)();
        if config.sources.application {
            if let Some(info) = self.foreground.current() {
                let decision = process_decision(&config, &info.app_id);
                application = sanitize_application(
                    &ApplicationSanitizeInput {
                        captured_display_name: &info.display_name,
                        mapped_display_name: apply_mapping(
                            &config,
                            PrivacyMappingType::ProcessName,
                            &info.app_id,
                        ),
                        window_title: info.window_title.as_deref(),
                    },
                    &decision,
                    config.share_window_titles,
                );
            }
        }

        SanitizedPresenceSnapshot {
            observed_at,
            application,
            media,
        }
    }
}
