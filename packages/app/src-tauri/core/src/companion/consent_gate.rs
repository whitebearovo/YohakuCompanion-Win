//! Consent gate: binds explicit user consent to (policy fingerprint,
//! sanitized projection) and re-validates against fresh captures.
//!
//! Owned by agent K. Ground truth:
//! `packages/core/src/companion/consentGate.ts` and
//! `.claude/rewrite/specs/companion-service.md` §6. Comparison is
//! structural equality of the typed [`PreviewProjection`] (`PartialEq` ≡
//! the TS key-order-insensitive `deepEqual`).
//!
//! A confirmation binds the policy fingerprint under which the user reviewed
//! the preview to the exact projection they saw. Enabling Live Desk validates
//! three things: the candidate equals the latest recorded confirmation, its
//! fingerprint matches the current policy, and the projection still equals
//! the projection of a fresh capture. Any policy change clears the
//! confirmation.

use crate::model::{
    PreviewApplication, PreviewMedia, PreviewPlayback, PreviewProjection, SanitizedPresenceSnapshot,
};

/// A recorded consent basis.
#[derive(Debug, Clone, PartialEq)]
pub struct Confirmation {
    pub policy_fingerprint: String,
    pub projection: PreviewProjection,
}

/// State: the current policy fingerprint plus at most one recorded
/// confirmation (starts empty).
pub struct ConsentGate {
    current_fingerprint: String,
    confirmation: Option<Confirmation>,
}

impl ConsentGate {
    /// The gate's initial fingerprint is the policy fingerprint at service
    /// construction time.
    pub fn new(initial_fingerprint: String) -> Self {
        Self {
            current_fingerprint: initial_fingerprint,
            confirmation: None,
        }
    }

    /// Current policy fingerprint.
    pub fn fingerprint(&self) -> &str {
        &self.current_fingerprint
    }

    /// Any effective policy change invalidates the recorded confirmation.
    /// Same fingerprint -> NO-OP (confirmation kept).
    pub fn policy_did_change(&mut self, fingerprint: String) {
        if fingerprint == self.current_fingerprint {
            return;
        }
        self.current_fingerprint = fingerprint;
        self.confirmation = None;
    }

    /// Records the projection the user is currently looking at under the
    /// CURRENT fingerprint and returns the confirmation (the consent
    /// candidate handed back on confirm).
    pub fn record(&mut self, projection: PreviewProjection) -> Confirmation {
        let confirmation = Confirmation {
            policy_fingerprint: self.current_fingerprint.clone(),
            projection,
        };
        self.confirmation = Some(confirmation.clone());
        confirmation
    }

    /// Drop the recorded confirmation.
    pub fn clear(&mut self) {
        self.confirmation = None;
    }

    /// True only when the candidate is the latest recorded confirmation, was
    /// given under the current policy, and still matches the current capture.
    pub fn validates(
        &self,
        candidate: &Confirmation,
        current_projection: &PreviewProjection,
    ) -> bool {
        match &self.confirmation {
            None => false,
            Some(confirmation) => {
                candidate == confirmation
                    && candidate.policy_fingerprint == self.current_fingerprint
                    && candidate.projection == *current_projection
            }
        }
    }
}

/// What consent binds: application `{displayName, windowTitle}` and media
/// `{kind, title, artist, album, playerDisplayName, playback {state,
/// durationSeconds, rate}}`. Deliberately EXCLUDES `observedAt`, media
/// `sessionId`, `positionSeconds`, `sampledAt` — natural playback progress
/// is continuity, not new disclosure; track changes, pause/play flips,
/// duration and rate changes all invalidate.
pub fn projection_of(snapshot: &SanitizedPresenceSnapshot) -> PreviewProjection {
    PreviewProjection {
        application: snapshot
            .application
            .as_ref()
            .map(|application| PreviewApplication {
                display_name: application.display_name.clone(),
                window_title: application.window_title.clone(),
            }),
        media: snapshot.media.as_ref().map(|media| PreviewMedia {
            kind: media.kind,
            title: media.title.clone(),
            artist: media.artist.clone(),
            album: media.album.clone(),
            player_display_name: media.player_display_name.clone(),
            playback: PreviewPlayback {
                state: media.playback.state,
                duration_seconds: media.playback.duration_seconds,
                rate: media.playback.rate,
            },
        }),
    }
}
