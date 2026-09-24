//! Sanitized snapshot -> wire request mapping.
//!
//! Owned by agent P. Ground truth:
//! `packages/core/src/companion/protocol/dtoMapper.ts` and
//! `.claude/rewrite/specs/protocol-transport.md` §5. Text bounding truncates
//! at Unicode SCALAR boundaries (`chars()`, not UTF-16 units or graphemes),
//! then trims with the ECMAScript whitespace set; over-limit text is
//! truncated, never rejected (keeps a publish alive).

use crate::model::{ClearReason, PlaybackState, SanitizedPresenceSnapshot};
use crate::protocol::types::{
    ClearRequest, ClearRequestData, PresenceRequest, PresenceRequestData, PresenceRequestMeta,
    WireApplicationContext, WireAvailability, WireLease, WireMediaContext, WirePlayback,
    WirePlayer, WireWindow,
};
use crate::protocol::wire::{
    encode_wire_date, seconds_to_wire_milliseconds, WireError, PRESENCE_SCHEMA,
    PRESENCE_SCHEMA_VERSION,
};

/// displayName / playerDisplayName limit (Unicode scalars).
pub const DISPLAY_NAME_LIMIT: usize = 120;
/// Window title limit (Unicode scalars).
pub const WINDOW_TITLE_LIMIT: usize = 500;
/// Media title/artist/album limit (Unicode scalars).
pub const MEDIA_TEXT_LIMIT: usize = 300;

/// Options for both mappers (`leaseMin/MaxSeconds` come from the negotiated
/// configuration; unused by `make_clear_request` but accepted for parity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MakeRequestOptions {
    pub device_id: String,
    pub lease_min_seconds: i64,
    pub lease_max_seconds: i64,
}

/// A mapped request: the freshly generated lowercase UUID v4 `request_id`
/// (equal to `body.meta.request_id`) plus the body. A retry reuses the SAME
/// mapped request — same id, same sequence, byte-identical encoding.
#[derive(Debug, Clone, PartialEq)]
pub struct MappedRequest<B> {
    pub request_id: String,
    pub body: B,
}

/// Build a `PUT /companion/presence` body. Order of operations (determines
/// which `WireError` fires first): map application, map media, clamp
/// `ttlSeconds = min(max(round(requested), leaseMin), leaseMax)`, build
/// meta. `availability` is `idle` iff both application and media mapped to
/// null; the request is still sent (an empty state is a publish, not a
/// clear).
pub fn make_presence_request(
    snapshot: &SanitizedPresenceSnapshot,
    sequence: u64,
    requested_lease_seconds: f64,
    options: &MakeRequestOptions,
) -> Result<MappedRequest<PresenceRequest>, WireError> {
    let application = map_application(snapshot)?;
    let media = map_media(snapshot)?;
    // Round half-up, then clamp into the negotiated [leaseMin, leaseMax].
    let ttl_seconds = (requested_lease_seconds.round() as i64)
        .max(options.lease_min_seconds)
        .min(options.lease_max_seconds)
        .max(0) as u64;
    let meta = make_meta(&options.device_id, sequence, snapshot.observed_at)?;
    let availability = if application.is_none() && media.is_none() {
        WireAvailability::Idle
    } else {
        WireAvailability::Active
    };
    Ok(MappedRequest {
        request_id: meta.request_id.clone(),
        body: PresenceRequest {
            meta,
            data: PresenceRequestData {
                availability,
                lease: WireLease { ttl_seconds },
                application,
                media,
            },
        },
    })
}

/// Build a `POST /companion/presence/clear` body carrying `reason` verbatim
/// and the same meta as a presence request.
pub fn make_clear_request(
    reason: ClearReason,
    sequence: u64,
    observed_at_ms: i64,
    options: &MakeRequestOptions,
) -> Result<MappedRequest<ClearRequest>, WireError> {
    let meta = make_meta(&options.device_id, sequence, observed_at_ms)?;
    Ok(MappedRequest {
        request_id: meta.request_id.clone(),
        body: ClearRequest {
            meta,
            data: ClearRequestData { reason },
        },
    })
}

/// ECMAScript WhiteSpace ∪ LineTerminator (what `String.prototype.trim`
/// removes): TAB VT FF SP NBSP ZWNBSP, Zs code points, LF CR LS PS. Differs
/// from Rust `char::is_whitespace` on U+FEFF (JS trims it) and U+0085 (JS
/// does not).
pub(crate) fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// Truncate at Unicode scalar boundaries FIRST, then trim; blank -> None.
/// Over-limit text is truncated, not rejected (keeps a publish alive).
fn bounded_text(value: Option<&str>, limit: usize) -> Option<String> {
    let value = value?;
    let truncated: String = if value.chars().count() <= limit {
        value.to_string()
    } else {
        value.chars().take(limit).collect()
    };
    let trimmed = truncated.trim_matches(is_js_whitespace);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn make_meta(
    device_id: &str,
    sequence: u64,
    observed_at_ms: i64,
) -> Result<PresenceRequestMeta, WireError> {
    Ok(PresenceRequestMeta {
        schema: PRESENCE_SCHEMA,
        schema_version: PRESENCE_SCHEMA_VERSION,
        // RFC 4122 v4, lowercase hyphenated (uuid's default formatting).
        request_id: uuid::Uuid::new_v4().to_string(),
        device_id: device_id.to_string(),
        // `sequence` is not range-checked here (the sequencer guarantees
        // 0..2^53-2); `device_id` is not re-validated here.
        sequence,
        observed_at: encode_wire_date(observed_at_ms)?,
    })
}

fn map_application(
    snapshot: &SanitizedPresenceSnapshot,
) -> Result<Option<WireApplicationContext>, WireError> {
    let Some(application) = &snapshot.application else {
        return Ok(None);
    };
    let display_name = bounded_text(Some(&application.display_name), DISPLAY_NAME_LIMIT)
        .ok_or_else(|| WireError("application displayName empty after bounding".to_string()))?;
    let title = bounded_text(application.window_title.as_deref(), WINDOW_TITLE_LIMIT);
    Ok(Some(WireApplicationContext {
        display_name,
        activity: None,
        window: title.map(|title| WireWindow { title }),
        icon: None,
    }))
}

fn map_media(snapshot: &SanitizedPresenceSnapshot) -> Result<Option<WireMediaContext>, WireError> {
    let Some(media) = &snapshot.media else {
        return Ok(None);
    };

    let title = bounded_text(media.title.as_deref(), MEDIA_TEXT_LIMIT);
    let artist = bounded_text(media.artist.as_deref(), MEDIA_TEXT_LIMIT);
    let album = bounded_text(media.album.as_deref(), MEDIA_TEXT_LIMIT);
    if title.is_none() && artist.is_none() {
        return Err(WireError("media requires title or artist".to_string()));
    }
    let player_display_name =
        bounded_text(media.player_display_name.as_deref(), DISPLAY_NAME_LIMIT);

    let playback = &media.playback;
    let rate = playback.rate;
    if !rate.is_finite() || !(0.0..=4.0).contains(&rate) {
        return Err(WireError(format!("playback rate out of range: {rate}")));
    }
    if playback.state == PlaybackState::Paused && rate != 0.0 {
        return Err(WireError("paused playback must have rate 0".to_string()));
    }
    if playback.state == PlaybackState::Playing && rate <= 0.0 {
        return Err(WireError("playing playback must have rate > 0".to_string()));
    }

    let duration_ms = playback
        .duration_seconds
        .map(|seconds| seconds_to_wire_milliseconds(seconds, "durationMs"))
        .transpose()?;
    let mut position_ms = playback
        .position_seconds
        .map(|seconds| seconds_to_wire_milliseconds(seconds, "positionMs"))
        .transpose()?;
    // Clamp position into duration when both known; null is NEVER coerced
    // to 0.
    if let (Some(position), Some(duration)) = (position_ms, duration_ms) {
        if position > duration {
            position_ms = Some(duration);
        }
    }

    Ok(Some(WireMediaContext {
        // sessionId and kind pass through VERBATIM (upstream sanitizer
        // guarantees them; no re-validation at this layer).
        session_id: media.session_id.clone(),
        kind: media.kind,
        title,
        artist,
        album,
        player: player_display_name.map(|display_name| WirePlayer { display_name }),
        playback: WirePlayback {
            state: playback.state,
            duration_ms,
            position_ms,
            sampled_at: encode_wire_date(playback.sampled_at)?,
            rate,
        },
    }))
}
