//! Port of `packages/core/test/privacy/consentGate.test.ts` (all 8 vectors,
//! literal values preserved).

use yohaku_core::companion::consent_gate::{projection_of, Confirmation, ConsentGate};
use yohaku_core::model::{
    MediaKind, PlaybackState, SanitizedApplicationPresence, SanitizedMediaPresence,
    SanitizedPlayback, SanitizedPresenceSnapshot,
};

fn snapshot() -> SanitizedPresenceSnapshot {
    SanitizedPresenceSnapshot {
        observed_at: 1_753_500_000_000,
        application: Some(SanitizedApplicationPresence {
            display_name: "Code".to_string(),
            window_title: None,
        }),
        media: Some(SanitizedMediaPresence {
            session_id: "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b".to_string(),
            kind: MediaKind::Music,
            title: Some("Song".to_string()),
            artist: Some("Artist".to_string()),
            album: Some("Album".to_string()),
            player_display_name: Some("Spotify".to_string()),
            playback: SanitizedPlayback {
                state: PlaybackState::Playing,
                duration_seconds: Some(200.0),
                position_seconds: Some(60.0),
                sampled_at: 1_753_500_000_000,
                rate: 1.0,
            },
        }),
    }
}

// --- projectionOf ---

#[test]
fn excludes_observed_at_session_id_position_and_sampled_at() {
    let a = projection_of(&snapshot());
    let mut variant = snapshot();
    variant.observed_at = 9_999_999_999_999;
    {
        let media = variant.media.as_mut().unwrap();
        media.session_id = "00000000-0000-4000-8000-000000000000".to_string();
        media.playback.position_seconds = Some(190.0);
        media.playback.sampled_at = 1;
    }
    let b = projection_of(&variant);
    assert_eq!(a, b);
}

#[test]
fn differs_when_duration_state_rate_track_or_app_changes() {
    let base = projection_of(&snapshot());

    let mut changed_track_snapshot = snapshot();
    changed_track_snapshot.media.as_mut().unwrap().title = Some("Other".to_string());
    let changed_track = projection_of(&changed_track_snapshot);

    let mut changed_state_snapshot = snapshot();
    {
        let playback = &mut changed_state_snapshot.media.as_mut().unwrap().playback;
        playback.state = PlaybackState::Paused;
        playback.rate = 0.0;
    }
    let changed_state = projection_of(&changed_state_snapshot);

    let mut changed_app_snapshot = snapshot();
    changed_app_snapshot.application = Some(SanitizedApplicationPresence {
        display_name: "Другое".to_string(),
        window_title: None,
    });
    let changed_app = projection_of(&changed_app_snapshot);

    assert_ne!(changed_track, base);
    assert_ne!(changed_state, base);
    assert_ne!(changed_app, base);
}

// --- ConsentGate ---

#[test]
fn starts_without_confirmation_and_rejects_unrecorded_candidates() {
    let gate = ConsentGate::new("fp1".to_string());
    let projection = projection_of(&snapshot());
    let candidate = Confirmation {
        policy_fingerprint: "fp1".to_string(),
        projection: projection.clone(),
    };
    assert!(!gate.validates(&candidate, &projection));
}

#[test]
fn validates_recorded_confirmation_against_an_equal_fresh_capture() {
    let mut gate = ConsentGate::new("fp1".to_string());
    let projection = projection_of(&snapshot());
    let confirmation = gate.record(projection);
    // Fresh capture with progressed position — projection unchanged.
    let mut fresh_snapshot = snapshot();
    fresh_snapshot
        .media
        .as_mut()
        .unwrap()
        .playback
        .position_seconds = Some(120.0);
    let fresh = projection_of(&fresh_snapshot);
    assert!(gate.validates(&confirmation, &fresh));
}

#[test]
fn policy_change_invalidates_a_recorded_confirmation() {
    let mut gate = ConsentGate::new("fp1".to_string());
    let projection = projection_of(&snapshot());
    let confirmation = gate.record(projection.clone());
    gate.policy_did_change("fp2".to_string());
    assert!(!gate.validates(&confirmation, &projection));
}

#[test]
fn same_fingerprint_policy_did_change_keeps_confirmation() {
    let mut gate = ConsentGate::new("fp1".to_string());
    let projection = projection_of(&snapshot());
    let confirmation = gate.record(projection.clone());
    gate.policy_did_change("fp1".to_string());
    assert!(gate.validates(&confirmation, &projection));
}

#[test]
fn rejects_when_the_current_capture_drifted_semantically() {
    let mut gate = ConsentGate::new("fp1".to_string());
    let confirmation = gate.record(projection_of(&snapshot()));
    let mut drifted_snapshot = snapshot();
    drifted_snapshot.media.as_mut().unwrap().title = Some("New Track".to_string());
    let drifted = projection_of(&drifted_snapshot);
    assert!(!gate.validates(&confirmation, &drifted));
}

#[test]
fn rejects_a_stale_candidate_after_re_recording() {
    let mut gate = ConsentGate::new("fp1".to_string());
    let old = gate.record(projection_of(&snapshot()));
    let mut second_snapshot = snapshot();
    second_snapshot.media.as_mut().unwrap().title = Some("Second".to_string());
    gate.record(projection_of(&second_snapshot));
    assert!(!gate.validates(&old, &projection_of(&snapshot())));
}
