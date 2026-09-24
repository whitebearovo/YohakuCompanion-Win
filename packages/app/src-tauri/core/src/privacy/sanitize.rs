//! Sanitization boundary, ported from CompanionApplicationPresenceSanitizer /
//! CompanionMediaPresenceSanitizer. These functions deliberately do not
//! accept appIds or executable paths — original process identity cannot pass
//! this boundary by construction. Display name precedence: alias > mapping >
//! raw.
//!
//! Owned by agent V. Ground truth:
//! `packages/core/src/privacy/sanitize.ts` and
//! `.claude/rewrite/specs/privacy.md` §5. The ONLY string transforms here
//! are NFC + ECMAScript trim + empty->None (`normalize_text`); wire-side
//! length truncation belongs to `protocol::dto_mapper`.

use crate::model::{MediaKind, PlaybackState, SanitizedApplicationPresence, SanitizedPlayback};
use crate::privacy::evaluator::{MediaDecision, ProcessDecision};
use crate::privacy::model::normalize_text;

/// Inputs for [`sanitize_application`] (built by `CaptureService`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationSanitizeInput<'a> {
    /// Raw display name from the capture layer (FileDescription or exe stem).
    pub captured_display_name: &'a str,
    /// `apply_mapping(config, ProcessName, appId)` result, verbatim.
    pub mapped_display_name: Option<&'a str>,
    pub window_title: Option<&'a str>,
}

/// `None` when not shared or when every name source is blank (the WHOLE
/// application presence is dropped even though the app is shared). Title
/// present ⇔ app not hidden AND rule/default resolves to share AND global
/// `shareWindowTitles` AND the title is non-blank.
pub fn sanitize_application(
    input: &ApplicationSanitizeInput<'_>,
    decision: &ProcessDecision,
    global_share_window_titles: bool,
) -> Option<SanitizedApplicationPresence> {
    if !decision.shares_application {
        return None;
    }
    // Display name precedence: alias > mapping > raw. The alias is already
    // normalized (and non-empty when Some) by the evaluator; `normalize_text`
    // never yields `""`, so `or_else` fallthrough ≡ the TS `??` chain.
    let display_name = decision
        .display_alias
        .clone()
        .or_else(|| normalize_text(input.mapped_display_name))
        .or_else(|| normalize_text(Some(input.captured_display_name)))?;
    let window_title = if decision.shares_window_title && global_share_window_titles {
        normalize_text(input.window_title)
    } else {
        None
    };
    Some(SanitizedApplicationPresence {
        display_name,
        window_title,
    })
}

/// Inputs for [`sanitize_media`] (built by `CaptureService`).
#[derive(Debug, Clone, PartialEq)]
pub struct MediaSanitizeInput<'a> {
    pub kind: MediaKind,
    pub title: Option<&'a str>,
    pub artist: Option<&'a str>,
    pub album: Option<&'a str>,
    /// Raw player display name from the capture layer.
    pub captured_player_name: Option<&'a str>,
    /// `media_player_name` mapping result, falling back to the legacy
    /// `media_process_name` mapping (both keyed by the CURRENT player
    /// display name), verbatim.
    pub mapped_player_name: Option<&'a str>,
    pub playing: bool,
    pub duration_seconds: Option<f64>,
    pub position_seconds: Option<f64>,
    /// Epoch milliseconds at which position was sampled.
    pub sampled_at: i64,
}

/// The global `ignoreNullArtist` switch: drop media that has no artist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SanitizeMediaOptions {
    pub requires_artist: bool,
}

/// `SanitizedMediaPresence` minus `session_id` — the id is assigned by the
/// caller via `MediaSessionTracker` AFTER sanitization succeeds.
#[derive(Debug, Clone, PartialEq)]
pub struct SanitizedMediaWithoutSession {
    pub kind: MediaKind,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub player_display_name: Option<String>,
    pub playback: SanitizedPlayback,
}

/// `None`, `NaN`, `±Infinity`, and negatives become `None`; `0` and
/// fractional values pass through unchanged (no rounding).
fn normalized_seconds(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite() && *v >= 0.0)
}

/// Returns the sanitized media presence, or `None` when the media must not
/// be shared (hidden, artist policy, or no meaningful text).
///
/// Exact order: share gate -> normalize title/artist/album -> requiresArtist
/// gate -> "title or artist" gate (album alone is not meaningful text) ->
/// player name precedence (alias > mapping > raw, may end `None`; media is
/// still shared) -> normalized seconds -> clamp position to duration (only
/// when both non-`None`) -> playback. `sampled_at` passes through with NO
/// validation. The paused branch is unreachable through
/// `capture_for_delivery` (playing gate) but MUST be kept (privacy spec A4).
pub fn sanitize_media(
    input: &MediaSanitizeInput<'_>,
    decision: &MediaDecision,
    options: &SanitizeMediaOptions,
) -> Option<SanitizedMediaWithoutSession> {
    if !decision.shares_media {
        return None;
    }

    let title = normalize_text(input.title);
    let artist = normalize_text(input.artist);
    let album = normalize_text(input.album);
    if options.requires_artist && artist.is_none() {
        return None;
    }
    if title.is_none() && artist.is_none() {
        return None;
    }

    let player_display_name = decision
        .display_alias
        .clone()
        .or_else(|| normalize_text(input.mapped_player_name))
        .or_else(|| normalize_text(input.captured_player_name));

    let duration = normalized_seconds(input.duration_seconds);
    let mut position = normalized_seconds(input.position_seconds);
    if let (Some(p), Some(d)) = (position, duration) {
        if p > d {
            position = Some(d);
        }
    }

    let playback = SanitizedPlayback {
        state: if input.playing {
            PlaybackState::Playing
        } else {
            PlaybackState::Paused
        },
        duration_seconds: duration,
        position_seconds: position,
        sampled_at: input.sampled_at,
        // SMTC does not expose a reliable playback rate; derive from state,
        // matching the macOS sanitizer (playing -> 1, paused -> 0).
        rate: if input.playing { 1.0 } else { 0.0 },
    };

    Some(SanitizedMediaWithoutSession {
        kind: input.kind,
        title,
        artist,
        album,
        player_display_name,
        playback,
    })
}
