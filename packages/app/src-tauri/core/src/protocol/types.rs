//! Wire envelopes and DTO shapes (Companion Protocol v2).
//!
//! Owned by agent P. Ground truth:
//! `packages/core/src/companion/protocol/types.ts` and
//! `.claude/rewrite/specs/protocol-transport.md` §3.
//!
//! Encode side: request-body structs declare fields in the EXACT wire key
//! order; every required-nullable key serializes an explicit `null`
//! (`Option::None`); capability-conditional keys (`artwork`, `link`) have no
//! field at all, so they are omitted, not null. Bodies are serialized
//! compact (`serde_json::to_vec`) — byte-identical to `JSON.stringify`.
//!
//! Decode side: serde derives enforce shape/literals/presence (helpers in
//! `crate::model`); the `decode_*` functions below add the zod refinements
//! (wire identifier, canonical wire date, capability safe-integer bounds,
//! non-blank token) so a malformed body fails decode exactly like zod, with
//! the FIRST issue message surfaced to
//! `CompanionTransportError::Decode`. Unknown keys are ignored at every
//! level (do NOT add `deny_unknown_fields`).

use serde::{Deserialize, Serialize};

use crate::model::{MediaKind, PlaybackState, MAXIMUM_SAFE_WIRE_INTEGER};
use crate::protocol::wire::{decode_wire_date, is_valid_wire_identifier};

// Cross-module wire types live in model.rs; re-exported here so protocol
// code has a local path.
pub use crate::model::{
    ClearReason, MutationData, MutationResponse, PublicLiveDeskState, ResponseMeta,
};

/// Scope that must be present in a pairing claim's `scopes`.
pub const REQUIRED_PRESENCE_SCOPE: &str = "companion:presence:write";

/// Server error codes that demand renegotiation instead of retry.
pub const MUTATION_RENEGOTIATE_CODES: [&str; 2] = [
    "COMPANION_SCHEMA_UNSUPPORTED",
    "COMPANION_FEATURE_UNAVAILABLE",
];

/// Alias: the wire media kind is the sanitized [`MediaKind`], passed through
/// verbatim by the mapper.
pub type MediaWireKind = MediaKind;

/// Alias: the wire playback state is the sanitized [`PlaybackState`].
pub type WirePlaybackState = PlaybackState;

/// `"idle"` iff both application and media are null, else `"active"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireAvailability {
    Idle,
    Active,
}

/// Serialize an `f64` with ECMAScript number formatting: integral values
/// WITHOUT a decimal point (`1`, not `1.0`), non-integral values in shortest
/// round-trip form. Used for `playback.rate` — the only non-integer number
/// this client ever puts on the wire (byte-identity trap, spec §12.1).
pub fn serialize_js_number<S: serde::Serializer>(
    value: &f64,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let integral = value.is_finite()
        && value.fract() == 0.0
        && value.abs() <= crate::model::MAXIMUM_SAFE_WIRE_INTEGER as f64;
    if integral {
        // -0 serializes as 0, matching JSON.stringify.
        serializer.serialize_i64(*value as i64)
    } else {
        serializer.serialize_f64(*value)
    }
}

// ---------------------------------------------------------------------------
// Request bodies (client -> server); field order == wire key order.
// ---------------------------------------------------------------------------

/// Shared meta of both mutations. `request_id` is a freshly generated
/// lowercase UUID v4 (a retry reuses the mapped request, hence the same id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceRequestMeta {
    /// Always [`super::wire::PRESENCE_SCHEMA`].
    pub schema: &'static str,
    /// Always [`super::wire::PRESENCE_SCHEMA_VERSION`].
    pub schema_version: u32,
    pub request_id: String,
    pub device_id: String,
    pub sequence: u64,
    /// Canonical wire date string.
    pub observed_at: String,
}

/// `activity` object — this client ALWAYS emits `null` for both keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireActivity {
    pub key: Option<String>,
    pub custom_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireWindow {
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireIcon {
    pub url: String,
}

/// Application context. `activity` and `icon` are always serialized as
/// `null` by this client (required-nullable keys).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireApplicationContext {
    pub display_name: String,
    pub activity: Option<WireActivity>,
    pub window: Option<WireWindow>,
    pub icon: Option<WireIcon>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WirePlayer {
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WirePlayback {
    pub state: WirePlaybackState,
    pub duration_ms: Option<u64>,
    pub position_ms: Option<u64>,
    /// Canonical wire date string.
    pub sampled_at: String,
    /// 0..=4; consistency with `state` enforced by the mapper.
    #[serde(serialize_with = "serialize_js_number")]
    pub rate: f64,
}

/// Media context. `artwork` and `link` are capability-conditional keys this
/// client never negotiates: they have NO field here, so the keys are omitted
/// entirely (never `null`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireMediaContext {
    /// Passed through verbatim from the sanitized snapshot (upstream
    /// guarantees a UUID — not re-validated here).
    pub session_id: String,
    pub kind: MediaWireKind,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub player: Option<WirePlayer>,
    pub playback: WirePlayback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireLease {
    pub ttl_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceRequestData {
    pub availability: WireAvailability,
    pub lease: WireLease,
    pub application: Option<WireApplicationContext>,
    pub media: Option<WireMediaContext>,
}

/// Body of `PUT /companion/presence`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceRequest {
    pub meta: PresenceRequestMeta,
    pub data: PresenceRequestData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearRequestData {
    pub reason: ClearReason,
}

/// Body of `POST /companion/presence/clear`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearRequest {
    pub meta: PresenceRequestMeta,
    pub data: ClearRequestData,
}

/// Body of `POST /companion/pairings/claim` (key order: deviceName,
/// pairingCode; input validation lives in `companion::pairing`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingClaimRequest {
    pub device_name: String,
    pub pairing_code: String,
}

// ---------------------------------------------------------------------------
// Response schemas (server -> client). Plain `.int()` capability fields are
// `i64` because zod allowed NEGATIVE integers at decode time — positivity is
// enforced later by `negotiate_presence`, not by the schema (spec §13.10).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitiesFeatures {
    pub live_desk: bool,
    pub media_timeline: bool,
    pub moments: bool,
    pub reading_sessions: bool,
    /// OPTIONAL key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_artwork: Option<bool>,
    /// OPTIONAL key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_playback_links: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitiesLimits {
    pub presence_payload_bytes: i64,
    pub presence_requests_per_minute: i64,
    pub presence_lease_min_seconds: i64,
    pub presence_lease_max_seconds: i64,
    pub recommended_heartbeat_seconds: i64,
    pub maximum_clock_skew_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitiesData {
    /// Any string at decode time; semver-validated during negotiation.
    pub minimum_client_version: String,
    pub presence_schema_versions: Vec<i64>,
    pub moment_schema_versions: Vec<i64>,
    pub features: CapabilitiesFeatures,
    pub limits: CapabilitiesLimits,
}

/// Body of `GET /companion/capabilities` (envelope meta REQUIRED).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitiesResponse {
    pub meta: ResponseMeta,
    pub data: CapabilitiesData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingClaimData {
    /// UUID/ULID (refined by `decode_pairing_claim_response`).
    pub device_id: String,
    /// Non-blank after trim (refined by `decode_pairing_claim_response`).
    pub device_token: String,
    pub scopes: Vec<String>,
    #[serde(deserialize_with = "crate::model::de_wire_u64")]
    pub next_sequence: u64,
}

/// Body of `POST /companion/pairings/claim` — NOTE: no `meta` key is
/// required and no requestId echo check is performed for pairing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingClaimResponse {
    pub data: PairingClaimData,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    /// REQUIRED-NULLABLE; decoded but unused by any logic in this slice.
    #[serde(deserialize_with = "crate::model::de_required_nullable_wire_u64")]
    pub retry_after_ms: Option<u64>,
    /// REQUIRED-NULLABLE; reconciled into the sequencer on server errors.
    #[serde(deserialize_with = "crate::model::de_required_nullable_wire_u64")]
    pub accepted_sequence: Option<u64>,
    pub fields: Vec<String>,
}

/// Protocol error body (non-2xx). Even error envelopes carry the presence
/// schema constants in `meta`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorEnvelope {
    pub meta: ResponseMeta,
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingErrorDetail {
    pub code: String,
}

/// Simplified pairing rejection body: `{ error: { code } }`; everything
/// else ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingErrorEnvelope {
    pub error: PairingErrorDetail,
}

// ---------------------------------------------------------------------------
// Decode functions (zod-schema equivalents). Passed to
// `CompanionHttpClient::execute` as its `response_schema` argument; also used
// internally by `execute` for error envelopes. `Err` carries the FIRST issue
// message (feeds `CompanionDecodeError("response decode failed: ...")`).
// ---------------------------------------------------------------------------

/// zod `wireIdentifier` / `wireDate` refinements shared by every envelope
/// meta; `Err` carries the zod refinement message literals.
fn refine_response_meta(meta: &ResponseMeta) -> Result<(), String> {
    if !is_valid_wire_identifier(&meta.request_id) {
        return Err("expected UUID or ULID".to_string());
    }
    if decode_wire_date(&meta.server_time).is_err() {
        return Err("expected canonical RFC3339 millisecond UTC date".to_string());
    }
    Ok(())
}

/// zod `.int()` also demanded a SAFE integer (|v| <= 2^53-1); serde's `i64`
/// alone would accept up to 2^63-1.
fn refine_safe_integer(value: i64) -> Result<(), String> {
    if value.unsigned_abs() > MAXIMUM_SAFE_WIRE_INTEGER {
        return Err("expected safe integer".to_string());
    }
    Ok(())
}

/// serde decode + refinements: `meta.requestId` wire identifier,
/// `meta.serverTime` canonical wire date.
pub fn decode_capabilities_response(
    value: &serde_json::Value,
) -> Result<CapabilitiesResponse, String> {
    let decoded = CapabilitiesResponse::deserialize(value).map_err(|e| e.to_string())?;
    refine_response_meta(&decoded.meta)?;
    for version in decoded
        .data
        .presence_schema_versions
        .iter()
        .chain(decoded.data.moment_schema_versions.iter())
    {
        refine_safe_integer(*version)?;
    }
    let limits = &decoded.data.limits;
    for value in [
        limits.presence_payload_bytes,
        limits.presence_requests_per_minute,
        limits.presence_lease_min_seconds,
        limits.presence_lease_max_seconds,
        limits.recommended_heartbeat_seconds,
        limits.maximum_clock_skew_seconds,
    ] {
        refine_safe_integer(value)?;
    }
    Ok(decoded)
}

/// serde decode + refinements: `deviceId` wire identifier, `deviceToken`
/// non-blank after trim ("empty token").
pub fn decode_pairing_claim_response(
    value: &serde_json::Value,
) -> Result<PairingClaimResponse, String> {
    let decoded = PairingClaimResponse::deserialize(value).map_err(|e| e.to_string())?;
    if !is_valid_wire_identifier(&decoded.data.device_id) {
        return Err("expected UUID or ULID".to_string());
    }
    // JS String.prototype.trim semantics (incl. U+FEFF), matching zod's
    // `v.trim().length > 0` refinement.
    if decoded
        .data
        .device_token
        .trim_matches(super::dto_mapper::is_js_whitespace)
        .is_empty()
    {
        return Err("empty token".to_string());
    }
    Ok(decoded)
}

/// serde decode + refinements: meta requestId/serverTime, `data.receivedAt`
/// wire date, `state.epoch` wire identifier.
pub fn decode_mutation_response(value: &serde_json::Value) -> Result<MutationResponse, String> {
    let decoded = MutationResponse::deserialize(value).map_err(|e| e.to_string())?;
    refine_response_meta(&decoded.meta)?;
    if decode_wire_date(&decoded.data.received_at).is_err() {
        return Err("expected canonical RFC3339 millisecond UTC date".to_string());
    }
    if !is_valid_wire_identifier(&decoded.data.state.epoch) {
        return Err("expected UUID or ULID".to_string());
    }
    Ok(decoded)
}

/// serde decode + meta refinements for the protocol error envelope.
pub fn decode_error_envelope(value: &serde_json::Value) -> Result<ErrorEnvelope, String> {
    let decoded = ErrorEnvelope::deserialize(value).map_err(|e| e.to_string())?;
    refine_response_meta(&decoded.meta)?;
    Ok(decoded)
}

/// serde decode of the simplified pairing envelope.
pub fn decode_pairing_error_envelope(
    value: &serde_json::Value,
) -> Result<PairingErrorEnvelope, String> {
    PairingErrorEnvelope::deserialize(value).map_err(|e| e.to_string())
}
