//! Cross-module data model for `yohaku-core`.
//!
//! SCAFFOLD-OWNED AND COMPLETE: implementation agents use these types as-is
//! and must not restructure this file (adding `use` statements in their own
//! files is how they consume it). JSON serialization of every type here is
//! byte-compatible with the zod schemas in `packages/shared/src/*.ts` and the
//! on-disk `config.json` schema in `packages/core/src/store/configStore.ts`
//! (camelCase keys, explicit `null` for required-nullable values, optional
//! keys omitted — never `null`).
//!
//! # Scaffold conventions (binding for all modules)
//!
//! - **Callbacks / unsubscribe**: registering a callback returns an
//!   [`Unsubscribe`] guard. Dropping the guard unregisters the callback;
//!   call [`Unsubscribe::detach`] to keep a callback registered for the
//!   lifetime of the source (the TS code registered and never unsubscribed).
//! - **Errors**: every module defines its own `thiserror` error type named
//!   `<Subject>Error` (e.g. `WireError`, `SequencerError`,
//!   `CompanionTransportError`, `ConfigStoreError`). Logic must branch on
//!   variants/fields, never on message strings, except where ported tests
//!   assert exact messages.
//! - **Async traits**: object-safe async traits use the `async-trait` crate
//!   (`SequenceBacking`, `MediaProvider`, `CredentialStore`).
//! - **Numbers**: epoch timestamps are `i64` milliseconds (`Date.now()`),
//!   media durations/positions are `f64` seconds, wire sequences and other
//!   wire integers are `u64` bounded to `0..=2^53-1`
//!   ([`MAXIMUM_SAFE_WIRE_INTEGER`]).
//! - **Futures in dependency structs**: boxed as [`BoxFuture`].
//! - **Decode strictness split**: serde derives here enforce shape, literal
//!   fields (`schema`, `schemaVersion`, config `version`), required-nullable
//!   key presence, and the 2^53-1 wire-integer bound. Refinements that need
//!   protocol primitives (wire-date canonicality, UUID/ULID identifier
//!   checks, capability safe-integer checks) are enforced by the transport /
//!   protocol decode functions in `protocol::types` AFTER serde decode, so a
//!   malformed body still surfaces as a decode error exactly like zod.
//! - **Privacy**: raw capture types ([`ForegroundInfo`], [`MediaSnapshot`])
//!   deliberately do NOT derive serde — they must never be serialized toward
//!   the network, persistence, the UI, or logs.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

/// Largest integer legal on the wire: 2^53 − 1 (`Number.MAX_SAFE_INTEGER`).
/// Re-exported by `protocol::wire` under the same name.
pub const MAXIMUM_SAFE_WIRE_INTEGER: u64 = 9_007_199_254_740_991;

/// Boxed `Send` future used in dependency-injection structs
/// (mirrors the TS `() => Promise<...>` dependency closures).
pub type BoxFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'static>>;

// ---------------------------------------------------------------------------
// Unsubscribe guard (callback registration convention)
// ---------------------------------------------------------------------------

/// RAII guard returned by every `on_*` callback registration.
///
/// Dropping the guard unregisters the callback. The TS sources returned an
/// unsubscribe closure that call sites frequently ignored (keeping the
/// listener forever); the Rust equivalent of ignoring it is [`Self::detach`].
#[must_use = "dropping an Unsubscribe immediately unregisters the callback; call detach() to keep it"]
pub struct Unsubscribe(Option<Box<dyn FnOnce() + Send>>);

impl Unsubscribe {
    /// Wrap an unregistration closure.
    pub fn new(unsubscribe: impl FnOnce() + Send + 'static) -> Self {
        Self(Some(Box::new(unsubscribe)))
    }

    /// A guard that does nothing (for sources that are already stopped).
    pub fn noop() -> Self {
        Self(None)
    }

    /// Keep the callback registered for the lifetime of the source.
    pub fn detach(mut self) {
        self.0 = None;
    }

    /// Unregister now (identical to dropping, but explicit at call sites).
    pub fn unsubscribe(self) {
        drop(self);
    }
}

impl Drop for Unsubscribe {
    fn drop(&mut self) {
        if let Some(unsubscribe) = self.0.take() {
            unsubscribe();
        }
    }
}

impl fmt::Debug for Unsubscribe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Unsubscribe")
            .field(&self.0.as_ref().map(|_| "armed").unwrap_or("detached"))
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Serde helpers (deserializer plumbing used by model + protocol types)
// ---------------------------------------------------------------------------

/// Wire integer: JSON integer in `0..=2^53-1`. Rejects floats (`3.0`) —
/// stricter than zod, a documented deviation (protocol spec §13.9) — and
/// enforces the safe-integer upper bound serde alone would not.
pub fn de_wire_u64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let value = u64::deserialize(deserializer)?;
    if value > MAXIMUM_SAFE_WIRE_INTEGER {
        return Err(D::Error::custom("integer out of wire range"));
    }
    Ok(value)
}

/// REQUIRED-NULLABLE wire integer: the key must be present (`deserialize_with`
/// without `default` makes serde error on a missing key), `null` is legal.
pub fn de_required_nullable_wire_u64<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    let value = Option::<u64>::deserialize(deserializer)?;
    if let Some(v) = value {
        if v > MAXIMUM_SAFE_WIRE_INTEGER {
            return Err(D::Error::custom("integer out of wire range"));
        }
    }
    Ok(value)
}

/// REQUIRED-NULLABLE arbitrary JSON value (`state.projection`): key must be
/// present, `null` is legal (zod v4 `z.unknown().nullable()` semantics).
pub fn de_required_nullable_value<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<serde_json::Value>, D::Error> {
    Option::<serde_json::Value>::deserialize(deserializer)
}

/// OPTIONAL-KEY string (`displayAlias`): key may be absent (`None` via
/// `default`), but when present it must be a string — explicit `null` is
/// rejected, matching zod `z.string().optional()`.
pub fn de_present_string<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Ok(Some(String::deserialize(deserializer)?))
}

/// Literal `"yohaku.companion.presence"` (every response envelope meta).
pub fn de_presence_schema_literal<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value != "yohaku.companion.presence" {
        return Err(D::Error::custom("expected schema \"yohaku.companion.presence\""));
    }
    Ok(value)
}

/// Literal `2` (`schemaVersion` on every envelope and live-desk state).
pub fn de_schema_version_literal<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<u32, D::Error> {
    let value = u32::deserialize(deserializer)?;
    if value != 2 {
        return Err(D::Error::custom("expected schemaVersion 2"));
    }
    Ok(value)
}

/// Literal `1` (`config.json` schema version; anything else = corrupt file).
pub fn de_config_version_literal<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<u32, D::Error> {
    let value = u32::deserialize(deserializer)?;
    if value != 1 {
        return Err(D::Error::custom("expected config version 1"));
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// Privacy configuration (packages/shared/src/privacy.ts)
// ---------------------------------------------------------------------------

/// `"share" | "hide"` — per-dimension default sharing policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrivacyDefault {
    Share,
    Hide,
}

/// `"inherit" | "share" | "hide"` — per-rule override of a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrivacyOverride {
    Inherit,
    Share,
    Hide,
}

/// Per-application privacy rule. `appId` is the lowercased executable file
/// name (e.g. `"code.exe"`), or — for media sessions with no exe attribution
/// — a normalized player display name.
///
/// `displayAlias` is an OPTIONAL KEY: absent vs present, never `null`.
/// Serializing `"displayAlias": null` would corrupt the policy fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationPrivacyRule {
    pub app_id: String,
    pub application: PrivacyOverride,
    pub window_title: PrivacyOverride,
    pub media: PrivacyOverride,
    #[serde(
        default,
        deserialize_with = "de_present_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub display_alias: Option<String>,
}

/// `"process_name" | "media_process_name" | "media_player_name"`.
/// `media_process_name` is kept valid only for configs written by older
/// releases; new mappings use `media_player_name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyMappingType {
    ProcessName,
    MediaProcessName,
    MediaPlayerName,
}

/// Display-name mapping. Matching on `from` is NFC + trim + lowercase on both
/// sides; `to` is returned verbatim (normalized later by the sanitizer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyMapping {
    #[serde(rename = "type")]
    pub mapping_type: PrivacyMappingType,
    pub from: String,
    pub to: String,
}

/// The `privacy.defaults` object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyDefaults {
    pub application: PrivacyDefault,
    pub window_title: PrivacyDefault,
    pub media: PrivacyDefault,
}

/// The `privacy.sources` object (capture source master switches).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacySources {
    pub application: bool,
    pub media: bool,
}

/// Full privacy configuration (mirrors `privacyConfigSchema`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyConfig {
    pub defaults: PrivacyDefaults,
    pub rules: Vec<ApplicationPrivacyRule>,
    pub mappings: Vec<PrivacyMapping>,
    pub share_window_titles: bool,
    pub ignore_null_artist: bool,
    pub sources: PrivacySources,
}

/// New-installation defaults: window titles are private unless opted in.
/// Byte-for-byte equal to `defaultPrivacyConfig()` in the TS shared package.
pub fn default_privacy_config() -> PrivacyConfig {
    PrivacyConfig {
        defaults: PrivacyDefaults {
            application: PrivacyDefault::Share,
            window_title: PrivacyDefault::Hide,
            media: PrivacyDefault::Share,
        },
        rules: Vec::new(),
        mappings: Vec::new(),
        share_window_titles: false,
        ignore_null_artist: false,
        sources: PrivacySources {
            application: true,
            media: true,
        },
    }
}

// ---------------------------------------------------------------------------
// Core configuration (packages/core/src/store/configStore.ts, config.json v1)
// ---------------------------------------------------------------------------

/// Legacy media provider preference. All three values must keep PARSING from
/// existing `config.json` files (compat invariant), but every one of them now
/// selects the single WinRT SMTC provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaProviderChoice {
    Auto,
    Npm,
    Powershell,
}

/// Credential storage backend pin (`"keyring" | "dpapi"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CredentialBackend {
    Keyring,
    Dpapi,
}

/// The `media` object of `config.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaConfig {
    pub provider: MediaProviderChoice,
}

/// Persisted pairing metadata (non-secret; the device token NEVER lives in
/// `config.json`). `pairingNextSequence` is the sequence floor captured at
/// claim time and never updated afterwards (the moving counter lives in
/// `sequence.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredConnection {
    pub base_url: String,
    pub device_id: String,
    pub device_name: String,
    pub scopes: Vec<String>,
    pub pairing_next_sequence: u64,
    pub live_desk_enabled: bool,
}

/// `config.json` schema version 1, exactly. Unknown keys are ignored on read
/// (zod strip semantics — do NOT add `deny_unknown_fields`); any invalid
/// value fails the WHOLE parse and takes the corrupt-file path (fail-closed:
/// losing `connection` means "not paired", never "publishing enabled").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreConfig {
    #[serde(deserialize_with = "de_config_version_literal")]
    pub version: u32,
    pub privacy: PrivacyConfig,
    pub connection: Option<StoredConnection>,
    pub media: MediaConfig,
    pub credential_backend: Option<CredentialBackend>,
}

/// Byte-for-byte equal to `defaultCoreConfig()` in configStore.ts.
pub fn default_core_config() -> CoreConfig {
    CoreConfig {
        version: 1,
        privacy: default_privacy_config(),
        connection: None,
        media: MediaConfig {
            provider: MediaProviderChoice::Auto,
        },
        credential_backend: None,
    }
}

// ---------------------------------------------------------------------------
// Raw capture types (packages/core/src/capture/types.ts)
//
// PRIVACY BOUNDARY: these exist only between the capture layer and the
// privacy pipeline. They deliberately do NOT derive serde so they cannot be
// serialized toward the network, persistence, the UI, or logs.
// ---------------------------------------------------------------------------

/// Raw foreground application sample (Win32).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundInfo {
    /// Lowercased executable file name, e.g. `"code.exe"`.
    pub app_id: String,
    /// Full executable path. Never consumed by the privacy pipeline; must
    /// never leave the process.
    pub exe_path: Option<String>,
    /// Friendly name (PE `FileDescription`) or exe-stem fallback.
    pub display_name: String,
    /// Raw foreground window title.
    pub window_title: Option<String>,
}

/// `"music" | "podcast" | "video" | "unknown"` (shared by raw capture,
/// sanitized presence, the preview projection, and the wire media context —
/// the mapper passes it through verbatim). `"podcast"` exists for
/// cross-platform parity and is never produced on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Music,
    Podcast,
    Video,
    Unknown,
}

/// Raw SMTC media sample (WinRT `Windows.Media.Control`).
#[derive(Debug, Clone, PartialEq)]
pub struct MediaSnapshot {
    /// Lowercased exe name iff the SMTC `SourceAppUserModelId` ends with
    /// `.exe` (case-insensitive), else `None`.
    pub app_id: Option<String>,
    /// Trimmed AUMID; `None` when empty.
    pub source_app_user_model_id: Option<String>,
    pub player_display_name: Option<String>,
    pub kind: MediaKind,
    /// Trimmed; empty becomes `None`.
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// `true` iff SMTC playback status is Playing (4).
    pub playing: bool,
    /// Float seconds; `None` unless finite and `> 0`.
    pub duration_seconds: Option<f64>,
    /// Float seconds; extrapolated while playing (capture-layer math).
    pub position_seconds: Option<f64>,
    /// Epoch ms at which `position_seconds` was (re)computed.
    pub sampled_at: i64,
}

// ---------------------------------------------------------------------------
// Sanitized presence (packages/core/src/privacy/types.ts)
//
// The ONLY shapes allowed to flow toward the network, the preview UI, or
// persistence. By construction they have no field for executable paths,
// appIds, process IDs, or raw capture objects.
// ---------------------------------------------------------------------------

/// `"playing" | "paused"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlaybackState {
    Playing,
    Paused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SanitizedApplicationPresence {
    /// Non-empty by construction (NFC + trim; a blank name drops the whole
    /// application presence instead).
    pub display_name: String,
    /// `None` unless all three title switches allow (rule + default + global
    /// `shareWindowTitles`) and the title is non-blank.
    pub window_title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SanitizedPlayback {
    pub state: PlaybackState,
    /// `None`, or finite `>= 0` float seconds.
    pub duration_seconds: Option<f64>,
    /// `None` means "unavailable"; `0` means "at start". Clamped to duration
    /// when duration is non-`None`.
    pub position_seconds: Option<f64>,
    /// Epoch ms at which position was sampled (passed through unvalidated).
    pub sampled_at: i64,
    /// Derived: playing → 1, paused → 0 (SMTC exposes no reliable rate).
    pub rate: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SanitizedMediaPresence {
    /// Stable per-semantic-session UUID v4 (lowercase hyphenated), random —
    /// never derived from content.
    pub session_id: String,
    pub kind: MediaKind,
    pub title: Option<String>,
    /// Invariant: `title` and `artist` are never both `None` (the media
    /// presence is dropped instead).
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Alias > mapping > raw precedence; may be `None`.
    pub player_display_name: Option<String>,
    pub playback: SanitizedPlayback,
}

/// The sanitized snapshot handed to the companion layer, the dto mapper and
/// `projection_of`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SanitizedPresenceSnapshot {
    /// Epoch ms captured at the START of `capture_for_delivery` (before the
    /// media await), so `media.playback.sampled_at >= observed_at` is legal.
    pub observed_at: i64,
    pub application: Option<SanitizedApplicationPresence>,
    pub media: Option<SanitizedMediaPresence>,
}

// ---------------------------------------------------------------------------
// Runtime state / UI snapshot (packages/shared/src/status.ts)
// ---------------------------------------------------------------------------

/// Runtime state of the Live Desk coordinator, mirrored to the UI.
/// `notPaired` is synthesized at the service level (connection == null); the
/// coordinator itself only ever holds the other seven values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeState {
    NotPaired,
    Disabled,
    Connecting,
    Active,
    Degraded,
    Suspended,
    UpdateRequired,
    ServerFeatureUnavailable,
}

impl RuntimeState {
    /// The exact TS literal (also the serde serialization).
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeState::NotPaired => "notPaired",
            RuntimeState::Disabled => "disabled",
            RuntimeState::Connecting => "connecting",
            RuntimeState::Active => "active",
            RuntimeState::Degraded => "degraded",
            RuntimeState::Suspended => "suspended",
            RuntimeState::UpdateRequired => "updateRequired",
            RuntimeState::ServerFeatureUnavailable => "serverFeatureUnavailable",
        }
    }
}

impl fmt::Display for RuntimeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Non-secret pairing metadata for the UI. Never contains the device token;
/// `pairingNextSequence` is deliberately omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSummary {
    pub base_url: String,
    pub device_id: String,
    pub device_name: String,
    pub scopes: Vec<String>,
    pub live_desk_enabled: bool,
}

/// Consent projection: the exact sanitized values the user confirms before
/// Live Desk may publish. Deliberately excludes `observedAt`, media
/// `sessionId`, `positionSeconds` and `sampledAt` so natural playback
/// progress does not invalidate consent, while any semantic change (track,
/// app, pause state, duration, rate) does. Compared structurally
/// (`PartialEq` here ≡ the TS `deepEqual`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewProjection {
    pub application: Option<PreviewApplication>,
    pub media: Option<PreviewMedia>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewApplication {
    pub display_name: String,
    pub window_title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewMedia {
    pub kind: MediaKind,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub player_display_name: Option<String>,
    pub playback: PreviewPlayback,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewPlayback {
    pub state: PlaybackState,
    pub duration_seconds: Option<f64>,
    pub rate: f64,
}

/// Preview wrapper sent to the UI; `observedAt` rides along for display but
/// is outside the compared projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub projection: PreviewProjection,
    pub policy_fingerprint: String,
    pub observed_at: i64,
}

/// `"winrt" | "none"` — the native rewrite has exactly one media provider.
/// (The legacy TS union `"npm" | "powershell" | "none"` survives only as the
/// still-parsed `media.provider` config values, see [`MediaProviderChoice`].)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaProviderKind {
    Winrt,
    None,
}

/// Media provider health surfaced on the Status page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaProviderHealth {
    pub kind: MediaProviderKind,
    pub healthy: bool,
    /// Optional key (the core never sets it; kept for schema parity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Full non-sensitive state snapshot broadcast to the shell/UI. No secrets,
/// no un-sanitized capture text, no exe paths; `recentAppIds` (local-UI-only
/// MRU, max 10) is the sole appId surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreStateSnapshot {
    pub version: String,
    pub runtime_state: RuntimeState,
    pub connection: Option<ConnectionSummary>,
    pub privacy: PrivacyConfig,
    pub preview: Option<Preview>,
    pub media_provider: MediaProviderHealth,
    pub recent_app_ids: Vec<String>,
    pub last_publish_at: Option<i64>,
    pub last_error: Option<String>,
}

// ---------------------------------------------------------------------------
// IPC error codes (packages/shared/src/ipc.ts)
// ---------------------------------------------------------------------------

/// Error codes returned to the UI. Tauri commands return
/// `Result<(), String>` where the `Err` string is `code.as_str()`.
/// `alreadyPaired` exists in the enum (and has a UI label) but is never
/// produced by the core — pairing silently replaces an existing pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IpcErrorCode {
    PreviewOutOfDate,
    NotPaired,
    AlreadyPaired,
    PairingExpired,
    PairingFailed,
    RequiredScopeMissing,
    ClientUpdateRequired,
    ServerFeatureUnavailable,
    RateLimited,
    ValidationFailed,
    CredentialStoreUnavailable,
    Network,
    InvalidInput,
    Internal,
}

impl IpcErrorCode {
    /// The exact TS literal (also the serde serialization).
    pub fn as_str(self) -> &'static str {
        match self {
            IpcErrorCode::PreviewOutOfDate => "previewOutOfDate",
            IpcErrorCode::NotPaired => "notPaired",
            IpcErrorCode::AlreadyPaired => "alreadyPaired",
            IpcErrorCode::PairingExpired => "pairingExpired",
            IpcErrorCode::PairingFailed => "pairingFailed",
            IpcErrorCode::RequiredScopeMissing => "requiredScopeMissing",
            IpcErrorCode::ClientUpdateRequired => "clientUpdateRequired",
            IpcErrorCode::ServerFeatureUnavailable => "serverFeatureUnavailable",
            IpcErrorCode::RateLimited => "rateLimited",
            IpcErrorCode::ValidationFailed => "validationFailed",
            IpcErrorCode::CredentialStoreUnavailable => "credentialStoreUnavailable",
            IpcErrorCode::Network => "network",
            IpcErrorCode::InvalidInput => "invalidInput",
            IpcErrorCode::Internal => "internal",
        }
    }
}

impl fmt::Display for IpcErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Wire-facing cross-module types (Companion Protocol v2)
// ---------------------------------------------------------------------------

/// Reason attached to `POST /companion/presence/clear`. `privacyChanged` is
/// defined on the wire but has no call site (privacy changes republish
/// instead of clearing); keep it for wire compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClearReason {
    Paused,
    Sleep,
    Shutdown,
    PrivacyChanged,
    ConnectionRemoved,
}

/// Result of a successful capability negotiation; server limits passed
/// through verbatim (no clamping, no defaulting) once `limitsAreValid` held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NegotiatedPresenceConfiguration {
    /// `features.mediaTimeline`.
    pub supports_media_timeline: bool,
    /// `limits.presencePayloadBytes` — request payload cap in bytes.
    pub maximum_payload_bytes: usize,
    /// `limits.presenceRequestsPerMinute`.
    pub requests_per_minute: u64,
    /// `limits.presenceLeaseMinSeconds`.
    pub lease_min_seconds: i64,
    /// `limits.presenceLeaseMaxSeconds`.
    pub lease_max_seconds: i64,
    /// `limits.recommendedHeartbeatSeconds`.
    pub recommended_heartbeat_seconds: i64,
    /// `limits.maximumClockSkewSeconds`.
    pub maximum_clock_skew_seconds: i64,
}

/// Device credential (deviceId + Bearer token) per httpClient.ts.
/// Deliberately NO serde derives and a redacting `Debug` — the token must
/// never be serialized or logged.
#[derive(Clone, PartialEq, Eq)]
pub struct CompanionCredential {
    pub device_id: String,
    pub device_token: String,
}

impl fmt::Debug for CompanionCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompanionCredential")
            .field("device_id", &self.device_id)
            .field("device_token", &"<redacted>")
            .finish()
    }
}

/// Response envelope meta, REQUIRED on every mutation/capabilities/error
/// envelope (even those carry the presence schema constants — a hard decode
/// constraint). `request_id` (UUID/ULID) and `server_time` (canonical wire
/// date) refinements are enforced by the transport decode step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseMeta {
    #[serde(deserialize_with = "de_presence_schema_literal")]
    pub schema: String,
    #[serde(deserialize_with = "de_schema_version_literal")]
    pub schema_version: u32,
    pub request_id: String,
    pub server_time: String,
}

/// `data.state` of a mutation response. `projection` is REQUIRED-NULLABLE
/// (missing key = decode error, `null` legal).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicLiveDeskState {
    #[serde(deserialize_with = "de_schema_version_literal")]
    pub schema_version: u32,
    pub epoch: String,
    #[serde(deserialize_with = "de_wire_u64")]
    pub revision: u64,
    #[serde(deserialize_with = "de_required_nullable_value")]
    pub projection: Option<serde_json::Value>,
}

/// `data` of a mutation response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationData {
    #[serde(deserialize_with = "de_wire_u64")]
    pub accepted_sequence: u64,
    /// Canonical wire date string (refined by the transport decode step).
    pub received_at: String,
    pub state: PublicLiveDeskState,
}

/// Success body of `PUT /companion/presence` and
/// `POST /companion/presence/clear`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationResponse {
    pub meta: ResponseMeta,
    pub data: MutationData,
}

