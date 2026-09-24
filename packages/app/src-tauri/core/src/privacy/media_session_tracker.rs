//! Media session identity: stable random UUID per semantic session.
//!
//! Owned by agent V. Ground truth:
//! `packages/core/src/privacy/mediaSessionTracker.ts` and
//! `.claude/rewrite/specs/privacy.md` §7. Only ONE identity is remembered
//! (no LRU): alternating A, B, A mints three distinct ids. Ids come from a
//! CSPRNG (`uuid::Uuid::new_v4()`, lowercase hyphenated) and are NEVER
//! derived from content (not fingerprintable). Position/sampledAt/state/
//! rate are NOT part of identity; kind/title/artist/album/playerDisplayName/
//! durationSeconds are. Debouncing and position extrapolation live in the
//! capture-layer providers, NOT here (spec A2) — this is pure identity
//! caching.

use uuid::Uuid;

use crate::model::MediaKind;

/// Identity tuple built by `CaptureService` from SANITIZED values
/// (post-NFC, post-alias/mapping, post-normalized-seconds) — an alias or
/// mapping edit mints a new session, as does a duration change. Structural
/// equality (`PartialEq`, `f64` semantics) matches the TS JSON string key
/// for every pipeline-reachable value (NaN/-0 are normalized away
/// upstream).
#[derive(Debug, Clone, PartialEq)]
pub struct MediaSemanticIdentity {
    pub kind: MediaKind,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub player_display_name: Option<String>,
    pub duration_seconds: Option<f64>,
}

/// Assigns a stable random session UUID per media "semantic identity". The
/// same track keeps its sessionId across position updates; any semantic
/// change, or an explicit continuity reset (pause-hide, generation change),
/// mints a new one.
pub struct MediaSessionTracker {
    current: Option<(MediaSemanticIdentity, String)>,
}

impl MediaSessionTracker {
    pub fn new() -> Self {
        Self { current: None }
    }

    /// Same identity as the remembered one -> the same id (progress ticks
    /// and repeated captures keep the session); anything else -> remember
    /// the new identity and mint a fresh UUID v4.
    pub fn session_id(&mut self, identity: &MediaSemanticIdentity) -> String {
        if let Some((current, session_id)) = &self.current {
            if current == identity {
                return session_id.clone();
            }
        }
        let session_id = Uuid::new_v4().to_string();
        self.current = Some((identity.clone(), session_id.clone()));
        session_id
    }

    /// Continuity break: the next `session_id` mints a fresh UUID even for
    /// an identical identity. Called whenever a capture that should have
    /// included media ends without it (hidden, stopped, paused, dropped),
    /// and by `CaptureService::reset_media_continuity` on coordinator
    /// generation teardown.
    pub fn reset(&mut self) {
        self.current = None;
    }
}

impl Default for MediaSessionTracker {
    fn default() -> Self {
        Self::new()
    }
}
