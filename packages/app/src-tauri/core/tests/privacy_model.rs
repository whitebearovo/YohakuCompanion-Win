//! Port of `packages/core/test/privacy/model.test.ts` (4 tests), plus the
//! Unicode parity pins the privacy spec (§§11.1-11.2) asks the Rust port to
//! add for `normalize_text` / `normalize_app_id` (JS trim set, U+0130,
//! final sigma).

use yohaku_core::model::{
    PrivacyConfig, PrivacyDefault, PrivacyDefaults, PrivacyMapping, PrivacyMappingType,
    PrivacySources,
};
use yohaku_core::privacy::model::{apply_mapping, normalize_app_id, normalize_text, scalar_length};

fn fixture_config() -> PrivacyConfig {
    PrivacyConfig {
        defaults: PrivacyDefaults {
            application: PrivacyDefault::Share,
            window_title: PrivacyDefault::Hide,
            media: PrivacyDefault::Share,
        },
        rules: Vec::new(),
        mappings: vec![
            PrivacyMapping {
                mapping_type: PrivacyMappingType::ProcessName,
                from: "code.exe".into(),
                to: "Visual Studio Code".into(),
            },
            PrivacyMapping {
                mapping_type: PrivacyMappingType::MediaProcessName,
                from: "Spotify.EXE".into(),
                to: "Spotify".into(),
            },
            PrivacyMapping {
                mapping_type: PrivacyMappingType::MediaPlayerName,
                from: "YouTube Music".into(),
                to: "YouTube Music Desktop".into(),
            },
        ],
        share_window_titles: false,
        ignore_null_artist: false,
        sources: PrivacySources {
            application: true,
            media: true,
        },
    }
}

#[test]
fn replaces_an_app_display_name_using_the_executable_app_id() {
    let config = fixture_config();
    assert_eq!(
        apply_mapping(&config, PrivacyMappingType::ProcessName, "CODE.EXE"),
        Some("Visual Studio Code")
    );
}

#[test]
fn normalizes_media_process_names_with_the_same_matching_rules() {
    let config = fixture_config();
    assert_eq!(
        apply_mapping(&config, PrivacyMappingType::MediaProcessName, "spotify.exe"),
        Some("Spotify")
    );
}

#[test]
fn does_not_cross_mapping_types() {
    let config = fixture_config();
    assert_eq!(
        apply_mapping(&config, PrivacyMappingType::MediaProcessName, "code.exe"),
        None
    );
}

#[test]
fn maps_the_captured_media_player_name_explicitly() {
    let config = fixture_config();
    assert_eq!(
        apply_mapping(
            &config,
            PrivacyMappingType::MediaPlayerName,
            " youtube music "
        ),
        Some("YouTube Music Desktop")
    );
}

// --- spec-added Unicode parity pins (privacy spec §§11.1-11.2) --------------

#[test]
fn normalize_text_uses_the_ecmascript_trim_set() {
    // U+FEFF is JS whitespace (trimmed); U+0085 NEL is NOT (JS keeps it).
    assert_eq!(
        normalize_text(Some("\u{FEFF} x \u{FEFF}")),
        Some("x".into())
    );
    assert_eq!(normalize_text(Some("\u{FEFF}\u{3000}\t\n")), None);
    assert_eq!(
        normalize_text(Some("\u{0085}x\u{0085}")),
        Some("\u{0085}x\u{0085}".into())
    );
    // Empty result means "value not present": None, never Some("").
    assert_eq!(normalize_text(Some("   ")), None);
    assert_eq!(normalize_text(None), None);
    // NFC: decomposed e + U+0301 composes to U+00E9.
    assert_eq!(normalize_text(Some("Cafe\u{0301}")), Some("Café".into()));
}

#[test]
fn normalize_app_id_matches_ecmascript_to_lower_case() {
    // U+0130 LATIN CAPITAL LETTER I WITH DOT ABOVE -> "i" + U+0307
    // (locale-independent Default Case Conversion, no Turkish special case).
    assert_eq!(normalize_app_id("\u{0130}"), "i\u{0307}");
    // Final sigma: U+03A3 lowercases to U+03C2 word-finally, U+03C3 medially.
    assert_eq!(normalize_app_id("ΟΔΟΣ"), "οδο\u{03C2}");
    assert_eq!(normalize_app_id("ΣΟ"), "\u{03C3}ο");
    // NFC + trim + lowercase, in that order; may return "".
    assert_eq!(normalize_app_id("  Code.EXE "), "code.exe");
    assert_eq!(normalize_app_id(" \u{FEFF} "), "");
}

#[test]
fn scalar_length_counts_code_points() {
    // Dead export parity (spec A1): surrogate pairs count as 1.
    assert_eq!(scalar_length(""), 0);
    assert_eq!(scalar_length("abc"), 3);
    assert_eq!(scalar_length("a\u{10000}b"), 3);
    assert_eq!(scalar_length("é"), 1);
}
