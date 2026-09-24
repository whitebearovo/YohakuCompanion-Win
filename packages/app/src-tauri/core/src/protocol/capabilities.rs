//! Capability negotiation with a strict SemVer 2.0 comparator.
//!
//! Owned by agent P. Ground truth:
//! `packages/core/src/companion/protocol/capabilities.ts` and
//! `.claude/rewrite/specs/protocol-transport.md` §4. Do NOT use the `semver`
//! crate — the TS parser/comparator has bespoke rules (first `+`/`-` split,
//! leading-zero rejection, numeric < textual identifiers) that must be
//! ported exactly.

use crate::model::NegotiatedPresenceConfiguration;
use crate::protocol::types::CapabilitiesData;

/// Parsed strict SemVer 2.0 version. Build metadata is validated then
/// DISCARDED (not stored) and never participates in comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub prerelease: Vec<String>,
}

/// Strict parse; `None` for e.g. `"v1.0.0"`, `"1.8"`, `"1.8.3.4"`,
/// `"01.0.0"`, `"1.0.0-alpha.01"`, `"1.0.0-"`, `"1.0.0+"`, `"1.0.x"`.
pub fn parse_semantic_version(input: &str) -> Option<SemanticVersion> {
    let mut rest = input;
    // 1. Build metadata: split at the FIRST `+`; validated then discarded.
    if let Some(plus) = rest.find('+') {
        let build = &rest[plus + 1..];
        if build.is_empty() || !build.split('.').all(is_alphanumeric_identifier) {
            return None;
        }
        rest = &rest[..plus];
    }
    // 2. Prerelease: split at the FIRST `-` (so `1.0.0-alpha-beta` yields the
    //    single identifier "alpha-beta"); purely numeric identifiers must
    //    have no leading zeros.
    let mut prerelease: Vec<String> = Vec::new();
    if let Some(dash) = rest.find('-') {
        let pre = &rest[dash + 1..];
        if pre.is_empty() {
            return None;
        }
        for id in pre.split('.') {
            if !is_alphanumeric_identifier(id) {
                return None;
            }
            if is_all_digits(id) && !is_numeric_identifier(id) {
                return None; // leading zero
            }
            prerelease.push(id.to_string());
        }
        rest = &rest[..dash];
    }
    // 3. Core: EXACTLY major.minor.patch, no leading zeros, no signs.
    let mut nums = [0u64; 3];
    let mut count = 0usize;
    for part in rest.split('.') {
        if count == 3 || !is_numeric_identifier(part) {
            return None;
        }
        // Cores beyond u64 are rejected (JS parseInt lost precision instead).
        nums[count] = part.parse().ok()?;
        count += 1;
    }
    if count != 3 {
        return None;
    }
    Some(SemanticVersion {
        major: nums[0],
        minor: nums[1],
        patch: nums[2],
        prerelease,
    })
}

/// `^[0-9A-Za-z-]+$` (non-empty).
fn is_alphanumeric_identifier(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// `^\d+$` (non-empty).
fn is_all_digits(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
}

/// `^(0|[1-9]\d*)$` — digits with no leading zeros.
fn is_numeric_identifier(id: &str) -> bool {
    id == "0" || (is_all_digits(id) && !id.starts_with('0'))
}

/// SemVer precedence. The TS function returned negative/zero/positive; the
/// Rust port returns `Ordering` (`Less`/`Equal`/`Greater` respectively).
/// A release (empty prerelease) is GREATER than any prerelease of the same
/// core; numeric identifiers < textual; equal prefixes — shorter is smaller.
pub fn compare_semantic_versions(a: &SemanticVersion, b: &SemanticVersion) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let core = a
        .major
        .cmp(&b.major)
        .then(a.minor.cmp(&b.minor))
        .then(a.patch.cmp(&b.patch));
    if core != Ordering::Equal {
        return core;
    }
    let (pre_a, pre_b) = (&a.prerelease, &b.prerelease);
    if pre_a.is_empty() && pre_b.is_empty() {
        return Ordering::Equal;
    }
    if pre_a.is_empty() {
        return Ordering::Greater; // release > prerelease
    }
    if pre_b.is_empty() {
        return Ordering::Less;
    }
    for (id_a, id_b) in pre_a.iter().zip(pre_b.iter()) {
        let num_a = is_all_digits(id_a);
        let num_b = is_all_digits(id_b);
        let ord = if num_a && num_b {
            let parsed_a: u64 = id_a.parse().unwrap_or(u64::MAX);
            let parsed_b: u64 = id_b.parse().unwrap_or(u64::MAX);
            parsed_a.cmp(&parsed_b)
        } else if num_a != num_b {
            // numeric < textual
            if num_a {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        } else {
            // ASCII by construction, so byte order == UTF-16 code-unit order.
            id_a.cmp(id_b)
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    pre_a.len().cmp(&pre_b.len()) // shorter (equal prefix) is smaller
}

/// Outcome of `negotiate_presence` (TS `PresenceNegotiation` union).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresenceNegotiation {
    Available {
        configuration: NegotiatedPresenceConfiguration,
    },
    ClientUpdateRequired,
    SchemaUnsupported,
    FeatureUnavailable,
    InvalidCapabilities,
}

/// Pure decision procedure, evaluated strictly in this order:
/// 1. `InvalidCapabilities` (unparseable versions, non-positive schema
///    versions — moment versions included — or invalid limits);
/// 2. `ClientUpdateRequired` (client < minimumClientVersion);
/// 3. `SchemaUnsupported` (presenceSchemaVersions lacks 2);
/// 4. `FeatureUnavailable` (`features.liveDesk` false);
/// 5. `Available` with limits mapped 1:1, no clamping or defaulting.
pub fn negotiate_presence(
    capabilities: &CapabilitiesData,
    client_version: &str,
) -> PresenceNegotiation {
    let client = parse_semantic_version(client_version);
    let minimum = parse_semantic_version(&capabilities.minimum_client_version);
    let invalid = client.is_none()
        || minimum.is_none()
        || capabilities
            .presence_schema_versions
            .iter()
            .any(|&v| v <= 0)
        || capabilities.moment_schema_versions.iter().any(|&v| v <= 0)
        || !limits_are_valid(&capabilities.limits);
    if invalid {
        return PresenceNegotiation::InvalidCapabilities;
    }
    let (client, minimum) = (
        client.expect("checked above"),
        minimum.expect("checked above"),
    );
    if compare_semantic_versions(&client, &minimum) == std::cmp::Ordering::Less {
        return PresenceNegotiation::ClientUpdateRequired;
    }
    if !capabilities
        .presence_schema_versions
        .contains(&i64::from(crate::protocol::wire::PRESENCE_SCHEMA_VERSION))
    {
        return PresenceNegotiation::SchemaUnsupported;
    }
    if !capabilities.features.live_desk {
        return PresenceNegotiation::FeatureUnavailable;
    }
    // Server limits pass through verbatim (no clamping, no defaulting);
    // limitsAreValid guarantees the casts below are lossless.
    let limits = &capabilities.limits;
    PresenceNegotiation::Available {
        configuration: NegotiatedPresenceConfiguration {
            supports_media_timeline: capabilities.features.media_timeline,
            maximum_payload_bytes: limits.presence_payload_bytes as usize,
            requests_per_minute: limits.presence_requests_per_minute as u64,
            lease_min_seconds: limits.presence_lease_min_seconds,
            lease_max_seconds: limits.presence_lease_max_seconds,
            recommended_heartbeat_seconds: limits.recommended_heartbeat_seconds,
            maximum_clock_skew_seconds: limits.maximum_clock_skew_seconds,
        },
    }
}

/// `limitsAreValid` — all bounds positive/coherent; skew merely non-negative.
fn limits_are_valid(limits: &crate::protocol::types::CapabilitiesLimits) -> bool {
    limits.presence_payload_bytes > 0
        && limits.presence_requests_per_minute > 0
        && limits.presence_lease_min_seconds > 0
        && limits.presence_lease_min_seconds <= limits.presence_lease_max_seconds
        && limits.presence_lease_min_seconds <= limits.recommended_heartbeat_seconds
        && limits.recommended_heartbeat_seconds <= limits.presence_lease_max_seconds
        && limits.maximum_clock_skew_seconds >= 0
}
