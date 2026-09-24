//! Port of `packages/core/test/privacy/evaluator.test.ts` (16 tests:
//! 10 processDecision + 6 mediaDecision). Assertion values are identical to
//! the vitest suite.

use yohaku_core::model::{
    default_privacy_config, ApplicationPrivacyRule, PrivacyConfig, PrivacyDefault, PrivacyDefaults,
    PrivacyOverride,
};
use yohaku_core::privacy::evaluator::{
    media_decision, process_decision, MediaDecision, ProcessDecision,
};

fn config() -> PrivacyConfig {
    default_privacy_config()
}

fn rule(
    app_id: &str,
    application: PrivacyOverride,
    window_title: PrivacyOverride,
    media: PrivacyOverride,
) -> ApplicationPrivacyRule {
    ApplicationPrivacyRule {
        app_id: app_id.into(),
        application,
        window_title,
        media,
        display_alias: None,
    }
}

fn rule_with_alias(
    app_id: &str,
    application: PrivacyOverride,
    window_title: PrivacyOverride,
    media: PrivacyOverride,
    alias: &str,
) -> ApplicationPrivacyRule {
    ApplicationPrivacyRule {
        display_alias: Some(alias.into()),
        ..rule(app_id, application, window_title, media)
    }
}

fn config_with_rules(rules: Vec<ApplicationPrivacyRule>) -> PrivacyConfig {
    PrivacyConfig {
        rules,
        ..default_privacy_config()
    }
}

// --- processDecision ---------------------------------------------------------

#[test]
fn shares_by_default_with_new_installation_defaults() {
    let d = process_decision(&config(), "code.exe");
    assert_eq!(
        d,
        ProcessDecision {
            shares_application: true,
            // defaults.windowTitle = hide
            shares_window_title: false,
            display_alias: None,
        }
    );
}

#[test]
fn hide_wins_rule_application_hide_hides_app_title_and_alias() {
    let c = config_with_rules(vec![rule_with_alias(
        "secret.exe",
        PrivacyOverride::Hide,
        PrivacyOverride::Share,
        PrivacyOverride::Inherit,
        "Alias",
    )]);
    let d = process_decision(&c, "secret.exe");
    assert_eq!(
        d,
        ProcessDecision {
            shares_application: false,
            shares_window_title: false,
            display_alias: None,
        }
    );
}

#[test]
fn global_default_application_hide_hides_apps_without_a_rule() {
    let c = PrivacyConfig {
        defaults: PrivacyDefaults {
            application: PrivacyDefault::Hide,
            window_title: PrivacyDefault::Hide,
            media: PrivacyDefault::Share,
        },
        ..default_privacy_config()
    };
    assert!(!process_decision(&c, "anything.exe").shares_application);
}

#[test]
fn rule_share_overrides_a_hide_default() {
    let c = PrivacyConfig {
        defaults: PrivacyDefaults {
            application: PrivacyDefault::Hide,
            window_title: PrivacyDefault::Hide,
            media: PrivacyDefault::Share,
        },
        rules: vec![rule(
            "code.exe",
            PrivacyOverride::Share,
            PrivacyOverride::Inherit,
            PrivacyOverride::Inherit,
        )],
        ..default_privacy_config()
    };
    assert!(process_decision(&c, "code.exe").shares_application);
}

// Window title truth table over (appHidden, ruleTitle resolved) — the third
// switch (global shareWindowTitles) is applied at the sanitize layer.
fn window_title_case(application: PrivacyOverride, window_title: PrivacyOverride) -> bool {
    let c = config_with_rules(vec![rule(
        "x.exe",
        application,
        window_title,
        PrivacyOverride::Inherit,
    )]);
    process_decision(&c, "x.exe").shares_window_title
}

#[test]
fn window_title_app_share_plus_title_share_is_true() {
    assert!(window_title_case(
        PrivacyOverride::Share,
        PrivacyOverride::Share
    ));
}

#[test]
fn window_title_app_share_plus_title_hide_is_false() {
    assert!(!window_title_case(
        PrivacyOverride::Share,
        PrivacyOverride::Hide
    ));
}

#[test]
fn window_title_app_hide_plus_title_share_is_false() {
    assert!(!window_title_case(
        PrivacyOverride::Hide,
        PrivacyOverride::Share
    ));
}

#[test]
fn window_title_app_hide_plus_title_hide_is_false() {
    assert!(!window_title_case(
        PrivacyOverride::Hide,
        PrivacyOverride::Hide
    ));
}

#[test]
fn matches_app_id_case_insensitively_and_trims() {
    let c = config_with_rules(vec![rule(
        "Code.EXE",
        PrivacyOverride::Hide,
        PrivacyOverride::Inherit,
        PrivacyOverride::Inherit,
    )]);
    assert!(!process_decision(&c, "  code.exe ").shares_application);
}

#[test]
fn normalizes_alias_blank_alias_becomes_null() {
    let c = config_with_rules(vec![rule_with_alias(
        "a.exe",
        PrivacyOverride::Share,
        PrivacyOverride::Inherit,
        PrivacyOverride::Inherit,
        "   ",
    )]);
    assert_eq!(process_decision(&c, "a.exe").display_alias, None);
}

// --- mediaDecision -------------------------------------------------------------

#[test]
fn media_shares_by_default() {
    assert_eq!(
        media_decision(&config(), Some("spotify.exe"), "Spotify"),
        MediaDecision {
            shares_media: true,
            display_alias: None,
        }
    );
}

#[test]
fn rule_media_hide_hides_media_and_alias() {
    let c = config_with_rules(vec![rule_with_alias(
        "spotify.exe",
        PrivacyOverride::Inherit,
        PrivacyOverride::Inherit,
        PrivacyOverride::Hide,
        "MyPlayer",
    )]);
    assert_eq!(
        media_decision(&c, Some("spotify.exe"), "Spotify"),
        MediaDecision {
            shares_media: false,
            display_alias: None,
        }
    );
}

#[test]
fn app_hidden_does_not_hide_media_independent_dimensions() {
    let c = config_with_rules(vec![rule(
        "spotify.exe",
        PrivacyOverride::Hide,
        PrivacyOverride::Inherit,
        PrivacyOverride::Inherit,
    )]);
    assert!(media_decision(&c, Some("spotify.exe"), "Spotify").shares_media);
}

#[test]
fn falls_back_to_player_name_matching_when_app_id_is_null() {
    let c = config_with_rules(vec![rule(
        "Spotify",
        PrivacyOverride::Inherit,
        PrivacyOverride::Inherit,
        PrivacyOverride::Hide,
    )]);
    assert!(!media_decision(&c, None, "spotify").shares_media);
    assert!(media_decision(&c, None, "Other Player").shares_media);
}

#[test]
fn app_id_match_takes_precedence_over_player_name_fallback() {
    let c = config_with_rules(vec![rule(
        "spotify",
        PrivacyOverride::Inherit,
        PrivacyOverride::Inherit,
        PrivacyOverride::Hide,
    )]);
    // appId provided and different -> no rule match -> default share
    assert!(media_decision(&c, Some("spotify.exe"), "Spotify").shares_media);
}

#[test]
fn global_media_hide_default() {
    let c = PrivacyConfig {
        defaults: PrivacyDefaults {
            application: PrivacyDefault::Share,
            window_title: PrivacyDefault::Hide,
            media: PrivacyDefault::Hide,
        },
        ..default_privacy_config()
    };
    assert!(!media_decision(&c, Some("x.exe"), "X").shares_media);
}
