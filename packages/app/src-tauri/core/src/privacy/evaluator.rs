//! Decision engine: "hide wins over everything; a hidden application never
//! leaks its alias either." Ported from PresencePrivacyPolicy.swift.
//!
//! Owned by agent V. Ground truth:
//! `packages/core/src/privacy/evaluator.ts` and
//! `.claude/rewrite/specs/privacy.md` §4. Decisions always use the ORIGINAL
//! (pre-mapping) identifiers; the application and media dimensions are
//! fully independent.

use crate::model::{PrivacyConfig, PrivacyDefault, PrivacyOverride};
use crate::privacy::model::{
    find_rule, find_rule_by_player_name, normalize_text, resolve_override,
};

/// Application-source decision. `shares_window_title` is RULE-LEVEL consent
/// only — the sanitizer additionally requires the global
/// `shareWindowTitles` switch; the `sources.application` gate is applied
/// earlier, in `CaptureService`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessDecision {
    pub shares_application: bool,
    pub shares_window_title: bool,
    /// `None` when hidden (alias never leaks) or blank/absent.
    pub display_alias: Option<String>,
}

/// Media-source decision. `ignore_null_artist` and `sources.media` are NOT
/// evaluated here (CaptureService passes/gates them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaDecision {
    pub shares_media: bool,
    pub display_alias: Option<String>,
}

/// Effective application/windowTitle policy for `app_id`:
/// hidden = effective(application) == Hide (hides app, title AND alias);
/// window shared = !hidden AND effective(windowTitle) == Share.
pub fn process_decision(config: &PrivacyConfig, app_id: &str) -> ProcessDecision {
    let rule = find_rule(config, app_id);
    let hidden = resolve_override(
        rule.map_or(PrivacyOverride::Inherit, |r| r.application),
        config.defaults.application,
    ) == PrivacyDefault::Hide;
    let window_shared = resolve_override(
        rule.map_or(PrivacyOverride::Inherit, |r| r.window_title),
        config.defaults.window_title,
    ) == PrivacyDefault::Share;
    ProcessDecision {
        shares_application: !hidden,
        shares_window_title: !hidden && window_shared,
        display_alias: if hidden {
            None
        } else {
            rule.and_then(|r| normalize_text(r.display_alias.as_deref()))
        },
    }
}

/// Effective media policy. Rule selection is EITHER/OR, not chained: a
/// non-`None` `app_id` consults ONLY `find_rule` (no player-name fallback
/// even on a miss); `app_id == None` consults `find_rule_by_player_name`.
/// Only `rule.media` + `defaults.media` are read — an `application = hide`
/// rule does NOT hide media from the same app.
pub fn media_decision(
    config: &PrivacyConfig,
    app_id: Option<&str>,
    player_name: &str,
) -> MediaDecision {
    let rule = match app_id {
        Some(app_id) => find_rule(config, app_id),
        None => find_rule_by_player_name(config, player_name),
    };
    let hidden = resolve_override(
        rule.map_or(PrivacyOverride::Inherit, |r| r.media),
        config.defaults.media,
    ) == PrivacyDefault::Hide;
    MediaDecision {
        shares_media: !hidden,
        display_alias: if hidden {
            None
        } else {
            rule.and_then(|r| normalize_text(r.display_alias.as_deref()))
        },
    }
}
