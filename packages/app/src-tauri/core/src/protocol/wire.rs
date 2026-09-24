//! Wire primitives: canonical dates, identifiers, integer bounds.
//!
//! Owned by agent P. Everything is deliberately strict: non-canonical input
//! is a protocol error ([`WireError`]), never repaired. Behavioral ground
//! truth: `packages/core/src/companion/protocol/wire.ts` and
//! `.claude/rewrite/specs/protocol-transport.md` §2.

/// Largest integer legal on the wire: 2^53 − 1.
pub use crate::model::MAXIMUM_SAFE_WIRE_INTEGER;

/// Envelope schema constant carried by every request/response meta.
pub const PRESENCE_SCHEMA: &str = "yohaku.companion.presence";

/// Presence schema version negotiated by this client.
pub const PRESENCE_SCHEMA_VERSION: u32 = 2;

/// Client version: BOTH the `X-Yohaku-Companion-Version` header value and
/// the version fed into capabilities negotiation. Tracks the original
/// companion's release line, decoupled from this application's own version.
pub const PROTOCOL_CLIENT_VERSION: &str = "1.8.3";

/// Error for every wire-primitive failure. Message formats mirror wire.ts
/// (informational; logic branches on the type, tests assert kinds).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct WireError(pub String);

/// Encode epoch milliseconds as canonical RFC3339 UTC with EXACTLY 3
/// fractional digits and `Z` suffix (`YYYY-MM-DDTHH:MM:SS.mmmZ`, 4-digit
/// year 0000–9999). Negative epoch ms (pre-1970) is legal; out-of-range
/// years are a `WireError` (documented deviation from the TS `RangeError`,
/// spec §13.1).
///
/// `encode_wire_date(0) == "1970-01-01T00:00:00.000Z"`;
/// `encode_wire_date(1_753_500_012_345) == "2025-07-26T03:20:12.345Z"`.
pub fn encode_wire_date(epoch_ms: i64) -> Result<String, WireError> {
    // i64 input is always finite; the TS "non-finite date" branch is moot.
    let encoded = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(epoch_ms)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .ok_or_else(|| WireError(format!("non-canonical date: {epoch_ms}")))?;
    // Years outside 0000-9999 format with an explicit sign and fail the
    // canonical-shape check, mirroring the TS regex rejection of expanded
    // +YYYYYY/-YYYYYY forms.
    if !matches_rfc3339_ms_utc(&encoded) {
        return Err(WireError(format!("non-canonical date: {encoded}")));
    }
    Ok(encoded)
}

/// Equivalent of the TS `RFC3339_MS_UTC` regex:
/// `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$`.
fn matches_rfc3339_ms_utc(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 24 {
        return false;
    }
    bytes.iter().enumerate().all(|(i, &c)| match i {
        4 | 7 => c == b'-',
        10 => c == b'T',
        13 | 16 => c == b':',
        19 => c == b'.',
        23 => c == b'Z',
        _ => c.is_ascii_digit(),
    })
}

/// Decode a canonical wire date to epoch milliseconds (may be negative).
/// Accepts a string iff it matches the strict format AND names a real
/// calendar instant AND re-encodes to exactly the input (round-trip check).
pub fn decode_wire_date(value: &str) -> Result<i64, WireError> {
    if !matches_rfc3339_ms_utc(value) {
        return Err(WireError(format!("invalid wire date: {value}")));
    }
    // chrono's strict parser rejects impossible components (month 13, hour
    // 24, Feb 30) that JS Date.parse handled leniently; the round-trip check
    // below neutralized that leniency in TS, so strict parsing is
    // behavior-equivalent (spec §12.8).
    let parsed = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.3fZ")
        .map_err(|_| WireError(format!("unparseable wire date: {value}")))?
        .and_utc()
        .timestamp_millis();
    // Canonical re-encoding must equal the input exactly (rejects e.g. leap
    // seconds chrono parses but the wire format forbids).
    match encode_wire_date(parsed) {
        Ok(encoded) if encoded == value => Ok(parsed),
        _ => Err(WireError(format!("non-canonical wire date: {value}"))),
    }
}

/// True iff the string is a hyphenated 8-4-4-4-12 UUID (any case, no
/// version/variant constraint) OR a 26-char UPPERCASE Crockford Base32 ULID
/// (excluding I, L, O, U; first char unconstrained).
pub fn is_valid_wire_identifier(value: &str) -> bool {
    is_uuid(value) || is_ulid(value)
}

/// `^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$`
fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(i, &c)| match i {
        8 | 13 | 18 | 23 => c == b'-',
        _ => c.is_ascii_hexdigit(),
    })
}

/// `^[0-9A-HJKMNP-TV-Z]{26}$` — Crockford Base32, uppercase, no I/L/O/U;
/// the first character is deliberately NOT constrained to 0-7.
fn is_ulid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 26
        && bytes.iter().all(|&c| {
            matches!(c, b'0'..=b'9' | b'A'..=b'H' | b'J' | b'K' | b'M' | b'N' | b'P'..=b'T' | b'V'..=b'Z')
        })
}

/// Validate a JSON-sourced number as a wire integer: integral (rejects 1.5)
/// and within `0..=2^53-1` inclusive. Mirrors `requireWireInteger(value,
/// field)`; `field` appears in the error message.
pub fn require_wire_integer(value: f64, field: &str) -> Result<u64, WireError> {
    let is_integer = value.is_finite() && value.fract() == 0.0;
    if !is_integer || value < 0.0 || value > MAXIMUM_SAFE_WIRE_INTEGER as f64 {
        return Err(WireError(format!(
            "integer out of wire range for {field}: {value}"
        )));
    }
    Ok(value as u64)
}

/// Convert non-negative finite seconds to rounded wire milliseconds
/// (round half toward +infinity, like JS `Math.round` on non-negative
/// input; do NOT use banker's rounding). Rejects negative and non-finite.
///
/// `seconds_to_wire_milliseconds(1.2345, "x") == 1235`;
/// `seconds_to_wire_milliseconds(60.2504, "x") == 60250`.
pub fn seconds_to_wire_milliseconds(seconds: f64, field: &str) -> Result<u64, WireError> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(WireError(format!("invalid seconds for {field}: {seconds}")));
    }
    // f64::round == JS Math.round for the non-negative inputs allowed here.
    require_wire_integer((seconds * 1000.0).round(), field)
}
