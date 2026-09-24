//! Port of `packages/core/test/privacy/mediaSessionTracker.test.ts`
//! (4 tests). Assertion values are identical to the vitest suite.

use yohaku_core::model::MediaKind;
use yohaku_core::privacy::media_session_tracker::{MediaSemanticIdentity, MediaSessionTracker};

fn identity() -> MediaSemanticIdentity {
    MediaSemanticIdentity {
        kind: MediaKind::Music,
        title: Some("Song".into()),
        artist: Some("Artist".into()),
        album: Some("Album".into()),
        player_display_name: Some("Spotify".into()),
        duration_seconds: Some(200.0),
    }
}

/// The TS regex `/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/`.
fn matches_uuid_shape(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        match index {
            8 | 13 | 18 | 23 => {
                if *byte != b'-' {
                    return false;
                }
            }
            _ => {
                if !matches!(byte, b'0'..=b'9' | b'a'..=b'f') {
                    return false;
                }
            }
        }
    }
    true
}

#[test]
fn keeps_the_same_session_id_for_the_same_semantic_identity() {
    let mut t = MediaSessionTracker::new();
    let a = t.session_id(&identity());
    // A fresh, structurally equal identity keeps the id (progress ticks and
    // repeated captures).
    let b = t.session_id(&identity().clone());
    assert_eq!(a, b);
}

#[test]
fn mints_a_new_session_id_when_identity_changes() {
    let mut t = MediaSessionTracker::new();
    let a = t.session_id(&identity());
    let b = t.session_id(&MediaSemanticIdentity {
        title: Some("Other".into()),
        ..identity()
    });
    assert_ne!(a, b);
}

#[test]
fn mints_a_new_session_id_after_reset_continuity_break() {
    let mut t = MediaSessionTracker::new();
    let a = t.session_id(&identity());
    t.reset();
    assert_ne!(t.session_id(&identity()), a);
}

#[test]
fn session_ids_are_uuids_not_content_hashes() {
    let mut t1 = MediaSessionTracker::new();
    let mut t2 = MediaSessionTracker::new();
    // Independent trackers must mint DIFFERENT ids for the same identity —
    // the id is random, never derived from content.
    assert_ne!(t1.session_id(&identity()), t2.session_id(&identity()));
    assert!(matches_uuid_shape(&t1.session_id(&identity())));
}
