//! Port of `packages/core/test/privacy/fingerprint.test.ts` (9 tests) plus
//! the 9 golden hex vectors from the privacy spec §6.6 (spec-added, computed
//! by executing the real TS `policyFingerprint()`; normative for the Rust
//! port per spec A3). The TS tests pin only equality/inequality relations;
//! here each relation ALSO pins the literal digest.

use yohaku_core::model::{
    default_privacy_config, ApplicationPrivacyRule, PrivacyConfig, PrivacyDefault, PrivacyDefaults,
    PrivacyMapping, PrivacyMappingType, PrivacyOverride, PrivacySources,
};
use yohaku_core::privacy::fingerprint::policy_fingerprint;

const DEFAULT_FP: &str = "f59b6fb20c6c4d845f87f24a2700a3b8de6cfab90700728ad71f0a067098adc6";
const SHARE_WINDOW_TITLES_FP: &str =
    "0b9f60f43f35d3910dc720e8fafff62d9e6b00f02a7eec01259127f7a4c11bff";
const IGNORE_NULL_ARTIST_FP: &str =
    "e056264d9861ab7c390e5ae0498c50c9dccb3df668562bb3a716de81215677d9";
const SOURCES_MEDIA_OFF_FP: &str =
    "d1743898a5d1dcb0386052a57e354af8b21cf71ce71af9460cd45febf991b296";
const DEFAULTS_ALL_SHARE_FP: &str =
    "2c1fd331109903db28e0ecd1d37da3e2bdc53047984c32694d22f21d0a1c3cf9";
const RULE_A_EXE_HIDE_FP: &str = "930dd61234871c1092ba3fb5d67c95f54685460cc3029ad1a6c48f096e7ef75b";
const MAPPING_A_TO_B_FP: &str = "b82ed1287ec00b515dde30f19eba257499fb2bbacedf05f2f4baeb13a71aadf8";
const CODE_EXE_ALIAS_FP: &str = "717c3a7c86555b5b86849bf3668eb8282b8b40a2c88564831757e27b3a49a915";
const TWO_RULES_TWO_MAPPINGS_FP: &str =
    "5d914a4d8f22fe0c67a5b16627a5d96c89b97553dc7a38a62a116f29b8937692";

fn config() -> PrivacyConfig {
    default_privacy_config()
}

fn rule(
    app_id: &str,
    application: PrivacyOverride,
    window_title: PrivacyOverride,
    media: PrivacyOverride,
    alias: Option<&str>,
) -> ApplicationPrivacyRule {
    ApplicationPrivacyRule {
        app_id: app_id.into(),
        application,
        window_title,
        media,
        display_alias: alias.map(str::to_owned),
    }
}

fn mapping(mapping_type: PrivacyMappingType, from: &str, to: &str) -> PrivacyMapping {
    PrivacyMapping {
        mapping_type,
        from: from.into(),
        to: to.into(),
    }
}

fn rule_a() -> ApplicationPrivacyRule {
    rule(
        "a.exe",
        PrivacyOverride::Hide,
        PrivacyOverride::Inherit,
        PrivacyOverride::Inherit,
        None,
    )
}

fn rule_b() -> ApplicationPrivacyRule {
    rule(
        "b.exe",
        PrivacyOverride::Inherit,
        PrivacyOverride::Share,
        PrivacyOverride::Inherit,
        None,
    )
}

#[test]
fn is_stable_for_identical_configs() {
    assert_eq!(policy_fingerprint(&config()), policy_fingerprint(&config()));
    assert_eq!(policy_fingerprint(&config()), DEFAULT_FP);
}

#[test]
fn is_order_insensitive_for_rules_and_mappings() {
    let m1 = mapping(PrivacyMappingType::ProcessName, "x", "y");
    let m2 = mapping(PrivacyMappingType::MediaProcessName, "x", "y");
    let ordered = PrivacyConfig {
        rules: vec![rule_a(), rule_b()],
        mappings: vec![m1.clone(), m2.clone()],
        ..config()
    };
    let reversed = PrivacyConfig {
        rules: vec![rule_b(), rule_a()],
        mappings: vec![m2, m1],
        ..config()
    };
    assert_eq!(policy_fingerprint(&ordered), policy_fingerprint(&reversed));
    // Golden value for either ordering (spec §6.6).
    assert_eq!(policy_fingerprint(&ordered), TWO_RULES_TWO_MAPPINGS_FP);
}

#[test]
fn ignores_empty_rules_and_alias_whitespace() {
    let empty = rule(
        "noop.exe",
        PrivacyOverride::Inherit,
        PrivacyOverride::Inherit,
        PrivacyOverride::Inherit,
        Some("   "),
    );
    let with_empty = PrivacyConfig {
        rules: vec![empty],
        ..config()
    };
    assert_eq!(
        policy_fingerprint(&with_empty),
        policy_fingerprint(&config())
    );
    assert_eq!(policy_fingerprint(&with_empty), DEFAULT_FP);
}

// "changes when X changes" — each patched fingerprint differs from the
// default AND matches its spec §6.6 golden literal.

#[test]
fn changes_when_share_window_titles_changes() {
    let c = PrivacyConfig {
        share_window_titles: true,
        ..config()
    };
    let fp = policy_fingerprint(&c);
    assert_ne!(fp, policy_fingerprint(&config()));
    assert_eq!(fp, SHARE_WINDOW_TITLES_FP);
}

#[test]
fn changes_when_ignore_null_artist_changes() {
    let c = PrivacyConfig {
        ignore_null_artist: true,
        ..config()
    };
    let fp = policy_fingerprint(&c);
    assert_ne!(fp, policy_fingerprint(&config()));
    assert_eq!(fp, IGNORE_NULL_ARTIST_FP);
}

#[test]
fn changes_when_sources_media_changes() {
    let c = PrivacyConfig {
        sources: PrivacySources {
            application: true,
            media: false,
        },
        ..config()
    };
    let fp = policy_fingerprint(&c);
    assert_ne!(fp, policy_fingerprint(&config()));
    assert_eq!(fp, SOURCES_MEDIA_OFF_FP);
}

#[test]
fn changes_when_defaults_window_title_changes() {
    let c = PrivacyConfig {
        defaults: PrivacyDefaults {
            application: PrivacyDefault::Share,
            window_title: PrivacyDefault::Share,
            media: PrivacyDefault::Share,
        },
        ..config()
    };
    let fp = policy_fingerprint(&c);
    assert_ne!(fp, policy_fingerprint(&config()));
    assert_eq!(fp, DEFAULTS_ALL_SHARE_FP);
}

#[test]
fn changes_when_a_rule_changes() {
    let c = PrivacyConfig {
        rules: vec![rule_a()],
        ..config()
    };
    let fp = policy_fingerprint(&c);
    assert_ne!(fp, policy_fingerprint(&config()));
    assert_eq!(fp, RULE_A_EXE_HIDE_FP);
}

#[test]
fn changes_when_a_mapping_changes() {
    let c = PrivacyConfig {
        mappings: vec![mapping(PrivacyMappingType::ProcessName, "a", "b")],
        ..config()
    };
    let fp = policy_fingerprint(&c);
    assert_ne!(fp, policy_fingerprint(&config()));
    assert_eq!(fp, MAPPING_A_TO_B_FP);
}

// --- spec §6.6 golden vectors not covered by a TS relation test ---------------

#[test]
fn golden_vector_normalized_rule_with_alias() {
    // appId "  Code.EXE " -> "code.exe"; alias "  VS Code  " -> "VS Code"
    // (displayAlias key present in the sorted rule serialization).
    let c = PrivacyConfig {
        rules: vec![rule(
            "  Code.EXE ",
            PrivacyOverride::Share,
            PrivacyOverride::Share,
            PrivacyOverride::Inherit,
            Some("  VS Code  "),
        )],
        ..config()
    };
    assert_eq!(policy_fingerprint(&c), CODE_EXE_ALIAS_FP);
}

#[test]
fn golden_vectors_all_nine_pinned() {
    let table: [(&str, PrivacyConfig); 9] = [
        ("default", config()),
        (
            "shareWindowTitles",
            PrivacyConfig {
                share_window_titles: true,
                ..config()
            },
        ),
        (
            "ignoreNullArtist",
            PrivacyConfig {
                ignore_null_artist: true,
                ..config()
            },
        ),
        (
            "sources.media off",
            PrivacyConfig {
                sources: PrivacySources {
                    application: true,
                    media: false,
                },
                ..config()
            },
        ),
        (
            "defaults all share",
            PrivacyConfig {
                defaults: PrivacyDefaults {
                    application: PrivacyDefault::Share,
                    window_title: PrivacyDefault::Share,
                    media: PrivacyDefault::Share,
                },
                ..config()
            },
        ),
        (
            "rule a.exe hide",
            PrivacyConfig {
                rules: vec![rule_a()],
                ..config()
            },
        ),
        (
            "mapping a->b",
            PrivacyConfig {
                mappings: vec![mapping(PrivacyMappingType::ProcessName, "a", "b")],
                ..config()
            },
        ),
        (
            "Code.EXE alias rule",
            PrivacyConfig {
                rules: vec![rule(
                    "  Code.EXE ",
                    PrivacyOverride::Share,
                    PrivacyOverride::Share,
                    PrivacyOverride::Inherit,
                    Some("  VS Code  "),
                )],
                ..config()
            },
        ),
        (
            "two rules + two mappings",
            PrivacyConfig {
                rules: vec![rule_a(), rule_b()],
                mappings: vec![
                    mapping(PrivacyMappingType::ProcessName, "x", "y"),
                    mapping(PrivacyMappingType::MediaProcessName, "x", "y"),
                ],
                ..config()
            },
        ),
    ];
    let expected = [
        DEFAULT_FP,
        SHARE_WINDOW_TITLES_FP,
        IGNORE_NULL_ARTIST_FP,
        SOURCES_MEDIA_OFF_FP,
        DEFAULTS_ALL_SHARE_FP,
        RULE_A_EXE_HIDE_FP,
        MAPPING_A_TO_B_FP,
        CODE_EXE_ALIAS_FP,
        TWO_RULES_TWO_MAPPINGS_FP,
    ];
    for ((name, cfg), want) in table.iter().zip(expected) {
        assert_eq!(policy_fingerprint(cfg), want, "golden vector: {name}");
    }
}
