//! Text primitives, rule model and display-name mappings.
//!
//! Owned by agent V. Ground truth: `packages/core/src/privacy/model.ts` and
//! `.claude/rewrite/specs/privacy.md` §§2-3. TRAPS: "trim" is the
//! ECMAScript whitespace set (includes U+FEFF, excludes U+0085) — do NOT
//! use `str::trim`; lowercase is full Unicode Default Case Conversion
//! (`str::to_lowercase` matches); NFC via `unicode-normalization`.

use unicode_normalization::UnicodeNormalization;

use crate::model::{
    ApplicationPrivacyRule, PrivacyConfig, PrivacyDefault, PrivacyMappingType, PrivacyOverride,
};

/// ECMAScript WhiteSpace ∪ LineTerminator — the exact set
/// `String.prototype.trim` strips (privacy spec §11.1). Differs from Rust's
/// Unicode `White_Space`: JS trims U+FEFF (ZWNBSP/BOM), Rust does not; Rust
/// trims U+0085 NEL, JS does not. Public so the wire truncation re-trim in
/// `protocol::dto_mapper` can share the table instead of drifting.
pub fn is_js_whitespace(c: char) -> bool {
    // U+2000..U+200A are the typographic spaces (EN QUAD .. HAIR SPACE).
    ('\u{2000}'..='\u{200A}').contains(&c)
        || matches!(
            c,
            '\u{0009}'
                | '\u{000A}'
                | '\u{000B}'
                | '\u{000C}'
                | '\u{000D}'
                | '\u{0020}'
                | '\u{00A0}'
                | '\u{1680}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
        )
}

/// `String.prototype.trim` equivalent (both ends, ECMAScript set).
pub fn js_trim(value: &str) -> &str {
    value.trim_matches(is_js_whitespace)
}

/// NFC -> ECMAScript trim -> empty becomes `None`. The single normalization
/// primitive of the pipeline ("empty result means value not present";
/// `None`, never `""`, flows onward). Mirrors the macOS implementation's
/// `precomposedStringWithCanonicalMapping`.
pub fn normalize_text(value: Option<&str>) -> Option<String> {
    let value = value?;
    let composed: String = value.nfc().collect();
    let trimmed = js_trim(&composed);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Unicode scalar count (`chars().count()`); a surrogate pair counts as 1.
/// Dead export in TS (the wire truncation has its own private helper) —
/// ported because it is part of the module's public surface; nothing in
/// this pipeline calls it (privacy spec A1).
pub fn scalar_length(value: &str) -> usize {
    value.chars().count()
}

/// NFC, then ECMAScript trim, then full-Unicode lowercase — exactly this
/// order. Never `None`; may be `""`. Applied to BOTH sides of every rule
/// and mapping lookup.
pub fn normalize_app_id(app_id: &str) -> String {
    let composed: String = app_id.nfc().collect();
    js_trim(&composed).to_lowercase()
}

/// `Inherit` resolves to `fallback`; `Share`/`Hide` win outright.
pub fn resolve_override(value: PrivacyOverride, fallback: PrivacyDefault) -> PrivacyDefault {
    match value {
        PrivacyOverride::Inherit => fallback,
        PrivacyOverride::Share => PrivacyDefault::Share,
        PrivacyOverride::Hide => PrivacyDefault::Hide,
    }
}

/// All three overrides `Inherit` AND no effective alias
/// (`normalize_text(display_alias)` is `None` — a whitespace-only alias
/// counts as absent). Empty rules carry no information and are excluded
/// from the fingerprint.
pub fn is_empty_rule(rule: &ApplicationPrivacyRule) -> bool {
    rule.application == PrivacyOverride::Inherit
        && rule.window_title == PrivacyOverride::Inherit
        && rule.media == PrivacyOverride::Inherit
        && normalize_text(rule.display_alias.as_deref()).is_none()
}

/// New rule with `app_id` normalized (normalize_app_id), overrides copied,
/// and `display_alias` normalized — the key OMITTED (field `None`) when the
/// alias normalizes away, never `Some("")`.
pub fn normalized_rule(rule: &ApplicationPrivacyRule) -> ApplicationPrivacyRule {
    ApplicationPrivacyRule {
        app_id: normalize_app_id(&rule.app_id),
        application: rule.application,
        window_title: rule.window_title,
        media: rule.media,
        display_alias: normalize_text(rule.display_alias.as_deref()),
    }
}

/// FIRST rule in config array order whose normalized appId equals the
/// normalized lookup key; the returned rule is the ORIGINAL config object
/// (callers normalize `display_alias` themselves at decision time).
pub fn find_rule<'a>(
    config: &'a PrivacyConfig,
    app_id: &str,
) -> Option<&'a ApplicationPrivacyRule> {
    let key = normalize_app_id(app_id);
    config
        .rules
        .iter()
        .find(|rule| normalize_app_id(&rule.app_id) == key)
}

/// Windows adaptation of the macOS "legacy hidden media names" fallback: a
/// media session that cannot be attributed to an executable is matched
/// against rules by the player's display name instead of an appId. A blank
/// normalized name matches nothing.
pub fn find_rule_by_player_name<'a>(
    config: &'a PrivacyConfig,
    player_name: &str,
) -> Option<&'a ApplicationPrivacyRule> {
    let key = normalize_app_id(player_name);
    if key.is_empty() {
        return None;
    }
    config
        .rules
        .iter()
        .find(|rule| normalize_app_id(&rule.app_id) == key)
}

/// FIRST mapping in array order with the exact `mapping_type` (types never
/// cross) and `normalize_app_id(from) == normalize_app_id(m.from)`. Returns
/// `to` VERBATIM (no trim/NFC/case change — a whitespace-only `to`
/// normalizes to `None` later inside the sanitizer and falls through to the
/// next precedence tier). Mappings never change the identifier used for
/// rule lookup.
pub fn apply_mapping<'a>(
    config: &'a PrivacyConfig,
    mapping_type: PrivacyMappingType,
    from: &str,
) -> Option<&'a str> {
    let key = normalize_app_id(from);
    config
        .mappings
        .iter()
        .find(|m| m.mapping_type == mapping_type && normalize_app_id(&m.from) == key)
        .map(|m| m.to.as_str())
}
