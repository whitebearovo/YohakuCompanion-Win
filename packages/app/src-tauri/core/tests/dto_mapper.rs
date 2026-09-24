//! Ported from `packages/core/test/companion/dtoMapper.test.ts` (spec
//! §11.3), plus the byte-exact canonical presence request of spec §1.5.

use serde_json::{json, Value};
use yohaku_core::model::{
    ClearReason, MediaKind, PlaybackState, SanitizedApplicationPresence, SanitizedMediaPresence,
    SanitizedPlayback, SanitizedPresenceSnapshot,
};
use yohaku_core::protocol::dto_mapper::{
    make_clear_request, make_presence_request, MakeRequestOptions,
};
use yohaku_core::protocol::types::WireAvailability;

fn opts() -> MakeRequestOptions {
    MakeRequestOptions {
        device_id: "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b".to_string(),
        lease_min_seconds: 30,
        lease_max_seconds: 120,
    }
}

fn snapshot() -> SanitizedPresenceSnapshot {
    SanitizedPresenceSnapshot {
        observed_at: 1_753_500_012_345,
        application: Some(SanitizedApplicationPresence {
            display_name: "Code".to_string(),
            window_title: Some("file.ts".to_string()),
        }),
        media: Some(SanitizedMediaPresence {
            session_id: "aa6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b".to_string(),
            kind: MediaKind::Music,
            title: Some("Song".to_string()),
            artist: Some("Artist".to_string()),
            album: None,
            player_display_name: Some("Spotify".to_string()),
            playback: SanitizedPlayback {
                state: PlaybackState::Playing,
                duration_seconds: Some(200.5),
                position_seconds: Some(60.2504),
                sampled_at: 1_753_500_012_345,
                rate: 1.0,
            },
        }),
    }
}

fn to_json(body: &impl serde::Serialize) -> Value {
    serde_json::to_value(body).unwrap()
}

mod wire_shape {
    use super::*;

    #[test]
    fn serializes_every_required_nullable_key_explicitly() {
        let mapped = make_presence_request(&snapshot(), 7, 90.0, &opts()).unwrap();
        let json = to_json(&mapped.body);
        let data = json["data"].as_object().unwrap();
        for key in ["availability", "lease", "application", "media"] {
            assert!(data.contains_key(key), "data.{key}");
        }
        let app = data["application"].as_object().unwrap();
        for key in ["displayName", "activity", "window", "icon"] {
            assert!(app.contains_key(key), "application.{key}");
        }
        assert_eq!(app["activity"], Value::Null);
        assert_eq!(app["icon"], Value::Null);
        let media = data["media"].as_object().unwrap();
        for key in [
            "sessionId",
            "kind",
            "title",
            "artist",
            "album",
            "player",
            "playback",
        ] {
            assert!(media.contains_key(key), "media.{key}");
        }
        assert_eq!(media["album"], Value::Null);
        let playback = media["playback"].as_object().unwrap();
        for key in ["state", "durationMs", "positionMs", "sampledAt", "rate"] {
            assert!(playback.contains_key(key), "playback.{key}");
        }
    }

    #[test]
    fn omits_capability_conditional_artwork_link_keys_entirely() {
        let mapped = make_presence_request(&snapshot(), 1, 90.0, &opts()).unwrap();
        let json = to_json(&mapped.body);
        let media = json["data"]["media"].as_object().unwrap();
        assert!(!media.contains_key("artwork"));
        assert!(!media.contains_key("link"));
    }

    #[test]
    fn null_application_media_keys_still_present_availability_idle() {
        let mut s = snapshot();
        s.application = None;
        s.media = None;
        let mapped = make_presence_request(&s, 1, 90.0, &opts()).unwrap();
        let json = to_json(&mapped.body);
        let data = json["data"].as_object().unwrap();
        assert_eq!(data["availability"], json!("idle"));
        assert!(data.contains_key("application"));
        assert_eq!(data["application"], Value::Null);
        assert!(data.contains_key("media"));
        assert_eq!(data["media"], Value::Null);
    }

    #[test]
    fn availability_active_when_either_source_present() {
        let mut s = snapshot();
        s.media = None;
        let mapped = make_presence_request(&s, 1, 90.0, &opts()).unwrap();
        assert_eq!(mapped.body.data.availability, WireAvailability::Active);
    }

    #[test]
    fn meta_carries_schema_constants_device_id_sequence_canonical_observed_at() {
        let mapped = make_presence_request(&snapshot(), 42, 90.0, &opts()).unwrap();
        assert_eq!(mapped.body.meta.schema, "yohaku.companion.presence");
        assert_eq!(mapped.body.meta.schema_version, 2);
        assert_eq!(mapped.body.meta.device_id, opts().device_id);
        assert_eq!(mapped.body.meta.sequence, 42);
        assert_eq!(mapped.body.meta.request_id, mapped.request_id);
        assert_eq!(mapped.body.meta.observed_at, "2025-07-26T03:20:12.345Z");
    }

    /// Spec §1.5: exact wire bytes — compact JSON, exact key order, and an
    /// integral `rate` serialized WITHOUT a decimal point.
    #[test]
    fn canonical_presence_request_serializes_byte_exact() {
        const TEMPLATE: &str = r#"{"meta":{"schema":"yohaku.companion.presence","schemaVersion":2,"requestId":"<uuid>","deviceId":"3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b","sequence":7,"observedAt":"2025-07-26T03:20:12.345Z"},"data":{"availability":"active","lease":{"ttlSeconds":90},"application":{"displayName":"Code","activity":null,"window":{"title":"file.ts"},"icon":null},"media":{"sessionId":"aa6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b","kind":"music","title":"Song","artist":"Artist","album":null,"player":{"displayName":"Spotify"},"playback":{"state":"playing","durationMs":200500,"positionMs":60250,"sampledAt":"2025-07-26T03:20:12.345Z","rate":1}}}}"#;
        let mapped = make_presence_request(&snapshot(), 7, 90.0, &opts()).unwrap();
        let encoded = serde_json::to_string(&mapped.body).unwrap();
        assert_eq!(encoded, TEMPLATE.replace("<uuid>", &mapped.request_id));
    }
}

mod conversions_and_limits {
    use super::*;

    #[test]
    fn converts_seconds_to_rounded_milliseconds_and_clamps_position() {
        let mapped = make_presence_request(&snapshot(), 1, 90.0, &opts()).unwrap();
        let media = mapped.body.data.media.unwrap();
        assert_eq!(media.playback.duration_ms, Some(200_500));
        assert_eq!(media.playback.position_ms, Some(60_250));
    }

    #[test]
    fn keeps_null_duration_position_as_null_never_0() {
        let mut s = snapshot();
        let media = s.media.as_mut().unwrap();
        media.playback.duration_seconds = None;
        media.playback.position_seconds = None;
        let mapped = make_presence_request(&s, 1, 90.0, &opts()).unwrap();
        let media = mapped.body.data.media.unwrap();
        assert_eq!(media.playback.duration_ms, None);
        assert_eq!(media.playback.position_ms, None);
    }

    #[test]
    fn truncates_over_limit_text_at_unicode_scalar_boundaries() {
        // U+1D49C is a surrogate pair in UTF-16; scalar counting must not
        // split it.
        let mut s = snapshot();
        s.application = Some(SanitizedApplicationPresence {
            display_name: "\u{1D49C}".repeat(200),
            window_title: Some("t".repeat(600)),
        });
        let mapped = make_presence_request(&s, 1, 90.0, &opts()).unwrap();
        let app = mapped.body.data.application.unwrap();
        assert_eq!(app.display_name.chars().count(), 120);
        assert_eq!(app.window.unwrap().title.chars().count(), 500);
    }

    #[test]
    fn clamps_lease_ttl_into_the_negotiated_range() {
        let ttl = |requested: f64| {
            make_presence_request(&snapshot(), 1, requested, &opts())
                .unwrap()
                .body
                .data
                .lease
                .ttl_seconds
        };
        assert_eq!(ttl(5.0), 30);
        assert_eq!(ttl(999.0), 120);
        assert_eq!(ttl(90.0), 90);
    }

    #[test]
    fn rejects_paused_with_rate_and_playing_without_rate() {
        let mut paused = snapshot();
        {
            let playback = &mut paused.media.as_mut().unwrap().playback;
            playback.state = PlaybackState::Paused;
            playback.rate = 1.0;
        }
        assert!(make_presence_request(&paused, 1, 90.0, &opts()).is_err());

        let mut playing = snapshot();
        playing.media.as_mut().unwrap().playback.rate = 0.0;
        assert!(make_presence_request(&playing, 1, 90.0, &opts()).is_err());
    }

    #[test]
    fn rejects_media_without_title_and_artist() {
        let mut s = snapshot();
        {
            let media = s.media.as_mut().unwrap();
            media.title = None;
            media.artist = None;
        }
        assert!(make_presence_request(&s, 1, 90.0, &opts()).is_err());
    }
}

mod clear_request {
    use super::*;

    #[test]
    fn carries_reason_and_full_meta() {
        let mapped = make_clear_request(ClearReason::Sleep, 9, 1_753_500_012_345, &opts()).unwrap();
        assert_eq!(mapped.body.data.reason, ClearReason::Sleep);
        assert_eq!(mapped.body.meta.sequence, 9);
        assert_eq!(mapped.body.meta.schema, "yohaku.companion.presence");
        // Wire literal check plus the meta echo of request_id.
        let json = to_json(&mapped.body);
        assert_eq!(json["data"]["reason"], json!("sleep"));
        assert_eq!(json["meta"]["requestId"], json!(mapped.request_id));
        assert_eq!(
            json["meta"]["observedAt"],
            json!("2025-07-26T03:20:12.345Z")
        );
    }
}
