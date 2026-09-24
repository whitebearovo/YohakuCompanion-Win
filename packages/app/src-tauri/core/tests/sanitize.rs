//! Port of `packages/core/test/privacy/sanitize.test.ts` (11 tests:
//! 4 sanitizeApplication + 7 sanitizeMedia). Assertion values are identical
//! to the vitest suite.

use yohaku_core::model::{MediaKind, PlaybackState};
use yohaku_core::privacy::evaluator::{MediaDecision, ProcessDecision};
use yohaku_core::privacy::sanitize::{
    sanitize_application, sanitize_media, ApplicationSanitizeInput, MediaSanitizeInput,
    SanitizeMediaOptions,
};

fn share() -> ProcessDecision {
    ProcessDecision {
        shares_application: true,
        shares_window_title: true,
        display_alias: None,
    }
}

fn app_input() -> ApplicationSanitizeInput<'static> {
    ApplicationSanitizeInput {
        captured_display_name: "Visual Studio Code",
        mapped_display_name: None,
        window_title: Some("secret.ts - project"),
    }
}

// --- sanitizeApplication -------------------------------------------------------

#[test]
fn returns_null_when_application_is_not_shared() {
    let decision = ProcessDecision {
        shares_application: false,
        ..share()
    };
    assert_eq!(sanitize_application(&app_input(), &decision, true), None);
}

#[test]
fn display_name_precedence_alias_over_mapping_over_raw() {
    let mapped = ApplicationSanitizeInput {
        mapped_display_name: Some("Mapped"),
        ..app_input()
    };
    let alias_decision = ProcessDecision {
        display_alias: Some("Alias".into()),
        ..share()
    };
    assert_eq!(
        sanitize_application(&mapped, &alias_decision, true)
            .expect("shared")
            .display_name,
        "Alias"
    );
    assert_eq!(
        sanitize_application(&mapped, &share(), true)
            .expect("shared")
            .display_name,
        "Mapped"
    );
    assert_eq!(
        sanitize_application(&app_input(), &share(), true)
            .expect("shared")
            .display_name,
        "Visual Studio Code"
    );
}

#[test]
fn window_title_requires_all_three_switches() {
    // rule says share + global on -> title present
    assert_eq!(
        sanitize_application(&app_input(), &share(), true)
            .expect("shared")
            .window_title
            .as_deref(),
        Some("secret.ts - project")
    );
    // global off -> no title
    assert_eq!(
        sanitize_application(&app_input(), &share(), false)
            .expect("shared")
            .window_title,
        None
    );
    // rule says hide -> no title even with global on
    let rule_hide = ProcessDecision {
        shares_window_title: false,
        ..share()
    };
    assert_eq!(
        sanitize_application(&app_input(), &rule_hide, true)
            .expect("shared")
            .window_title,
        None
    );
}

#[test]
fn normalizes_text_nfc_plus_trim_empty_title_becomes_null() {
    let input = ApplicationSanitizeInput {
        captured_display_name: "  Café  ",
        window_title: Some("   "),
        ..app_input()
    };
    let out = sanitize_application(&input, &share(), true).expect("shared");
    assert_eq!(out.display_name, "Café");
    assert_eq!(out.window_title, None);
}

// --- sanitizeMedia ---------------------------------------------------------------

fn share_media() -> MediaDecision {
    MediaDecision {
        shares_media: true,
        display_alias: None,
    }
}

fn media_input() -> MediaSanitizeInput<'static> {
    MediaSanitizeInput {
        kind: MediaKind::Music,
        title: Some("Song"),
        artist: Some("Artist"),
        album: Some("Album"),
        captured_player_name: Some("Spotify"),
        mapped_player_name: None,
        playing: true,
        duration_seconds: Some(200.0),
        position_seconds: Some(60.0),
        sampled_at: 1_753_500_000_000,
    }
}

fn no_artist() -> SanitizeMediaOptions {
    SanitizeMediaOptions {
        requires_artist: false,
    }
}

#[test]
fn returns_null_when_media_is_not_shared() {
    let decision = MediaDecision {
        shares_media: false,
        display_alias: None,
    };
    assert_eq!(
        sanitize_media(&media_input(), &decision, &no_artist()),
        None
    );
}

#[test]
fn requires_artist_drops_media_without_artist() {
    let input = MediaSanitizeInput {
        artist: None,
        ..media_input()
    };
    assert_eq!(
        sanitize_media(
            &input,
            &share_media(),
            &SanitizeMediaOptions {
                requires_artist: true
            }
        ),
        None
    );
    assert!(sanitize_media(&input, &share_media(), &no_artist()).is_some());
}

#[test]
fn requires_title_or_artist_after_normalization() {
    let input = MediaSanitizeInput {
        title: Some("  "),
        artist: None,
        ..media_input()
    };
    assert_eq!(sanitize_media(&input, &share_media(), &no_artist()), None);
}

#[test]
fn clamps_position_to_duration_and_nulls_invalid_values() {
    let out = sanitize_media(
        &MediaSanitizeInput {
            duration_seconds: Some(100.0),
            position_seconds: Some(150.0),
            ..media_input()
        },
        &share_media(),
        &no_artist(),
    )
    .expect("shared");
    assert_eq!(out.playback.position_seconds, Some(100.0));

    let bad = sanitize_media(
        &MediaSanitizeInput {
            duration_seconds: Some(f64::NAN),
            position_seconds: Some(-5.0),
            ..media_input()
        },
        &share_media(),
        &no_artist(),
    )
    .expect("shared");
    assert_eq!(bad.playback.duration_seconds, None);
    assert_eq!(bad.playback.position_seconds, None);
}

#[test]
fn preserves_a_real_zero_position_null_means_unavailable_zero_means_start() {
    let out = sanitize_media(
        &MediaSanitizeInput {
            position_seconds: Some(0.0),
            ..media_input()
        },
        &share_media(),
        &no_artist(),
    )
    .expect("shared");
    assert_eq!(out.playback.position_seconds, Some(0.0));
}

#[test]
fn derives_rate_and_state_from_playing_flag() {
    let playing = sanitize_media(
        &MediaSanitizeInput {
            playing: true,
            ..media_input()
        },
        &share_media(),
        &no_artist(),
    )
    .expect("shared")
    .playback;
    assert_eq!(playing.state, PlaybackState::Playing);
    assert_eq!(playing.rate, 1.0);

    // Paused branch is unreachable through captureForDelivery (playing gate)
    // but pinned here regardless (privacy spec A4).
    let paused = sanitize_media(
        &MediaSanitizeInput {
            playing: false,
            ..media_input()
        },
        &share_media(),
        &no_artist(),
    )
    .expect("shared")
    .playback;
    assert_eq!(paused.state, PlaybackState::Paused);
    assert_eq!(paused.rate, 0.0);
}

#[test]
fn player_display_name_precedence_alias_over_mapping_over_raw() {
    let input = MediaSanitizeInput {
        mapped_player_name: Some("Mapped"),
        ..media_input()
    };
    let alias_decision = MediaDecision {
        shares_media: true,
        display_alias: Some("Alias".into()),
    };
    assert_eq!(
        sanitize_media(&input, &alias_decision, &no_artist())
            .expect("shared")
            .player_display_name
            .as_deref(),
        Some("Alias")
    );
    assert_eq!(
        sanitize_media(&input, &share_media(), &no_artist())
            .expect("shared")
            .player_display_name
            .as_deref(),
        Some("Mapped")
    );
}
