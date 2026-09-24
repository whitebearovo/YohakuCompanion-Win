//! Policy fingerprint: SHA-256 hex (64 lowercase chars) of the canonical
//! JSON of the persisted privacy projection.
//!
//! Owned by agent V. Ground truth:
//! `packages/core/src/privacy/fingerprint.ts` and
//! `.claude/rewrite/specs/privacy.md` §6 (incl. golden vectors). Any change
//! to the effective policy must change the string; cosmetic changes
//! (rule/mapping order, empty rules, alias whitespace, appId case) must
//! not.
//!
//! Byte-parity notes (spec §§6.3-6.4, 11.4-11.8):
//! - Rule/mapping sorts compare UTF-16 CODE UNITS (JS `<`/`>`), stable
//!   (`slice::sort_by` ≡ ES2019+ `Array.prototype.sort`; ties keep config
//!   order).
//! - The mapping sort key is the literal TS runtime key `type + "\\0" +
//!   from` — BACKSLASH + DIGIT ZERO, not NUL (spec A8; provably equivalent
//!   to sorting by (type, from) for the three legal types, reproduced
//!   verbatim anyway). Do not "fix" it.
//! - The projection JSON is emitted by hand with the canonical (sorted) key
//!   order instead of going through `serde_json::Value`, so no serde map
//!   ordering feature can perturb the bytes. All object keys are fixed
//!   ASCII, so their sorted order is hardcoded per spec §6.4.
//! - String escaping delegates to `serde_json` which matches V8
//!   `JSON.stringify` for well-formed strings: short escapes `\b \t \n \f
//!   \r`, lowercase `\u00xx` for the remaining U+0000-U+001F, `\"`, `\\`,
//!   everything else (incl. U+007F, U+2028/U+2029, non-ASCII) raw. Lone
//!   surrogates cannot exist in a Rust `String` (spec A7 — unreachable for
//!   zod-validated configs).
//! - An alias-less normalized rule OMITS the `displayAlias` key entirely.
//! - The preimage contains only strings/booleans/arrays/objects — never
//!   numbers, never `null`.
//!
//! Golden vector: `policy_fingerprint(&default_privacy_config()) ==
//! "f59b6fb20c6c4d845f87f24a2700a3b8de6cfab90700728ad71f0a067098adc6"`.

use std::cmp::Ordering;
use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use crate::model::{
    ApplicationPrivacyRule, PrivacyConfig, PrivacyDefault, PrivacyMapping, PrivacyMappingType,
    PrivacyOverride,
};
use crate::privacy::model::{is_empty_rule, normalized_rule};

/// JS relational string comparison: lexicographic by UTF-16 code units
/// (differs from Rust `str` `Ord` for strings mixing U+E000..U+FFFF BMP
/// chars with astral chars).
fn cmp_utf16(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// The exact serde/zod literal for a mapping type tag.
fn mapping_type_str(mapping_type: PrivacyMappingType) -> &'static str {
    match mapping_type {
        PrivacyMappingType::ProcessName => "process_name",
        PrivacyMappingType::MediaProcessName => "media_process_name",
        PrivacyMappingType::MediaPlayerName => "media_player_name",
    }
}

fn default_str(value: PrivacyDefault) -> &'static str {
    match value {
        PrivacyDefault::Share => "share",
        PrivacyDefault::Hide => "hide",
    }
}

fn override_str(value: PrivacyOverride) -> &'static str {
    match value {
        PrivacyOverride::Inherit => "inherit",
        PrivacyOverride::Share => "share",
        PrivacyOverride::Hide => "hide",
    }
}

/// JSON string literal (quoted + escaped) for arbitrary user data,
/// byte-identical to `JSON.stringify(value)` for well-formed strings.
fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("serializing a &str to JSON cannot fail")
}

fn push_bool(out: &mut String, value: bool) {
    out.push_str(if value { "true" } else { "false" });
}

/// Compact canonical JSON of one normalized rule. Sorted key order (spec
/// §6.4): `appId`, `application`, [`displayAlias` if present], `media`,
/// `windowTitle` ("appId" < "application" because `I` U+0049 < `l` U+006C).
fn push_rule(out: &mut String, rule: &ApplicationPrivacyRule) {
    out.push_str("{\"appId\":");
    out.push_str(&json_string(&rule.app_id));
    out.push_str(",\"application\":\"");
    out.push_str(override_str(rule.application));
    out.push('"');
    if let Some(alias) = &rule.display_alias {
        out.push_str(",\"displayAlias\":");
        out.push_str(&json_string(alias));
    }
    out.push_str(",\"media\":\"");
    out.push_str(override_str(rule.media));
    out.push_str("\",\"windowTitle\":\"");
    out.push_str(override_str(rule.window_title));
    out.push_str("\"}");
}

/// Compact canonical JSON of one RAW mapping (original case and whitespace;
/// only the sort key is derived). Sorted key order: `from`, `to`, `type`.
fn push_mapping(out: &mut String, mapping: &PrivacyMapping) {
    out.push_str("{\"from\":");
    out.push_str(&json_string(&mapping.from));
    out.push_str(",\"to\":");
    out.push_str(&json_string(&mapping.to));
    out.push_str(",\"type\":\"");
    out.push_str(mapping_type_str(mapping.mapping_type));
    out.push_str("\"}");
}

/// Exact algorithm from fingerprint.ts: normalize + filter empty + stable
/// sort rules by appId; stable sort raw mappings by `type\0from`
/// (backslash-then-zero); canonicalize (sorted keys, `undefined` dropped ≡
/// omitted alias key); compact `JSON.stringify`; SHA-256 over UTF-8;
/// lowercase hex.
pub fn policy_fingerprint(config: &PrivacyConfig) -> String {
    let mut rules: Vec<ApplicationPrivacyRule> = config
        .rules
        .iter()
        .map(normalized_rule)
        .filter(|rule| !is_empty_rule(rule))
        .collect();
    rules.sort_by(|a, b| cmp_utf16(&a.app_id, &b.app_id));

    let mut mappings: Vec<&PrivacyMapping> = config.mappings.iter().collect();
    // The TS template literal contains `\\0`: the runtime separator is the
    // TWO characters U+005C U+0030 (spec A8). Reproduced verbatim.
    let sort_key =
        |m: &PrivacyMapping| format!("{}\\0{}", mapping_type_str(m.mapping_type), m.from);
    mappings.sort_by(|a, b| cmp_utf16(&sort_key(a), &sort_key(b)));

    // canonicalize + JSON.stringify, hand-emitted. Top-level sorted key
    // order: defaults, ignoreNullArtist, mappings, rules, shareWindowTitles,
    // sources; defaults: application, media, windowTitle; sources:
    // application, media.
    let mut out = String::new();
    out.push_str("{\"defaults\":{\"application\":\"");
    out.push_str(default_str(config.defaults.application));
    out.push_str("\",\"media\":\"");
    out.push_str(default_str(config.defaults.media));
    out.push_str("\",\"windowTitle\":\"");
    out.push_str(default_str(config.defaults.window_title));
    out.push_str("\"},\"ignoreNullArtist\":");
    push_bool(&mut out, config.ignore_null_artist);
    out.push_str(",\"mappings\":[");
    for (index, mapping) in mappings.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_mapping(&mut out, mapping);
    }
    out.push_str("],\"rules\":[");
    for (index, rule) in rules.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_rule(&mut out, rule);
    }
    out.push_str("],\"shareWindowTitles\":");
    push_bool(&mut out, config.share_window_titles);
    out.push_str(",\"sources\":{\"application\":");
    push_bool(&mut out, config.sources.application);
    out.push_str(",\"media\":");
    push_bool(&mut out, config.sources.media);
    out.push_str("}}");

    let digest = Sha256::digest(out.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}
