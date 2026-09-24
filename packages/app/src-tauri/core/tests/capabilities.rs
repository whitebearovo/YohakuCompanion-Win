//! Ported from `packages/core/test/companion/capabilities.test.ts` (spec
//! §11.4).

use std::cmp::Ordering;

use yohaku_core::model::NegotiatedPresenceConfiguration;
use yohaku_core::protocol::capabilities::{
    compare_semantic_versions, negotiate_presence, parse_semantic_version, PresenceNegotiation,
    SemanticVersion,
};
use yohaku_core::protocol::types::{CapabilitiesData, CapabilitiesFeatures, CapabilitiesLimits};

fn capabilities() -> CapabilitiesData {
    CapabilitiesData {
        minimum_client_version: "1.7.0".to_string(),
        presence_schema_versions: vec![2],
        moment_schema_versions: vec![1],
        features: CapabilitiesFeatures {
            live_desk: true,
            media_timeline: true,
            moments: true,
            reading_sessions: false,
            media_artwork: None,
            media_playback_links: None,
        },
        limits: CapabilitiesLimits {
            presence_payload_bytes: 32768,
            presence_requests_per_minute: 30,
            presence_lease_min_seconds: 30,
            presence_lease_max_seconds: 120,
            recommended_heartbeat_seconds: 45,
            maximum_clock_skew_seconds: 60,
        },
    }
}

mod parse_semantic_version_tests {
    use super::*;

    #[test]
    fn parses_core_prerelease_and_build() {
        assert_eq!(
            parse_semantic_version("1.8.3"),
            Some(SemanticVersion {
                major: 1,
                minor: 8,
                patch: 3,
                prerelease: vec![],
            })
        );
        assert_eq!(
            parse_semantic_version("1.0.0-alpha.1+build.5")
                .unwrap()
                .prerelease,
            vec!["alpha", "1"]
        );
    }

    #[test]
    fn rejects_invalid_inputs() {
        let invalid = [
            "1.8",
            "1.8.3.4",
            "01.0.0",
            "1.0.0-alpha.01",
            "1.0.0-",
            "1.0.0+",
            "v1.0.0",
            "1.0.x",
        ];
        for v in invalid {
            assert_eq!(parse_semantic_version(v), None, "expected None for {v:?}");
        }
    }
}

mod compare_semantic_versions_tests {
    use super::*;

    #[test]
    fn orders_the_canonical_semver_example_chain() {
        let ordered = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "2.0.0",
        ];
        for pair in ordered.windows(2) {
            let a = parse_semantic_version(pair[0]).unwrap();
            let b = parse_semantic_version(pair[1]).unwrap();
            assert_eq!(
                compare_semantic_versions(&a, &b),
                Ordering::Less,
                "{} < {}",
                pair[0],
                pair[1]
            );
            assert_eq!(
                compare_semantic_versions(&b, &a),
                Ordering::Greater,
                "{} > {}",
                pair[1],
                pair[0]
            );
        }
    }

    #[test]
    fn ignores_build_metadata() {
        let a = parse_semantic_version("1.0.0+a").unwrap();
        let b = parse_semantic_version("1.0.0+b").unwrap();
        assert_eq!(compare_semantic_versions(&a, &b), Ordering::Equal);
    }
}

mod negotiate_presence_tests {
    use super::*;

    #[test]
    fn returns_available_with_the_negotiated_configuration() {
        assert_eq!(
            negotiate_presence(&capabilities(), "1.8.3"),
            PresenceNegotiation::Available {
                configuration: NegotiatedPresenceConfiguration {
                    supports_media_timeline: true,
                    maximum_payload_bytes: 32768,
                    requests_per_minute: 30,
                    lease_min_seconds: 30,
                    lease_max_seconds: 120,
                    recommended_heartbeat_seconds: 45,
                    maximum_clock_skew_seconds: 60,
                },
            }
        );
    }

    #[test]
    fn client_update_required_when_below_minimum_client_version() {
        let mut caps = capabilities();
        caps.minimum_client_version = "2.0.0".to_string();
        assert_eq!(
            negotiate_presence(&caps, "1.8.3"),
            PresenceNegotiation::ClientUpdateRequired
        );
    }

    #[test]
    fn schema_unsupported_when_v2_missing() {
        let mut caps = capabilities();
        caps.presence_schema_versions = vec![3];
        assert_eq!(
            negotiate_presence(&caps, "1.8.3"),
            PresenceNegotiation::SchemaUnsupported
        );
    }

    #[test]
    fn feature_unavailable_when_live_desk_off() {
        let mut caps = capabilities();
        caps.features = CapabilitiesFeatures {
            live_desk: false,
            media_timeline: true,
            moments: true,
            reading_sessions: false,
            media_artwork: None,
            media_playback_links: None,
        };
        assert_eq!(
            negotiate_presence(&caps, "1.8.3"),
            PresenceNegotiation::FeatureUnavailable
        );
    }

    #[test]
    fn invalid_capabilities_for_incoherent_limits() {
        type LimitsPatch = fn(&mut CapabilitiesLimits);
        let cases: [(&str, LimitsPatch); 7] = [
            ("zero payload", |l| l.presence_payload_bytes = 0),
            ("zero rpm", |l| l.presence_requests_per_minute = 0),
            ("zero lease min", |l| l.presence_lease_min_seconds = 0),
            ("lease min > max", |l| l.presence_lease_min_seconds = 200),
            ("heartbeat below lease min", |l| {
                l.recommended_heartbeat_seconds = 10
            }),
            ("heartbeat above lease max", |l| {
                l.recommended_heartbeat_seconds = 500
            }),
            ("negative skew", |l| l.maximum_clock_skew_seconds = -1),
        ];
        for (name, patch) in cases {
            let mut caps = capabilities();
            patch(&mut caps.limits);
            assert_eq!(
                negotiate_presence(&caps, "1.8.3"),
                PresenceNegotiation::InvalidCapabilities,
                "invalidCapabilities: {name}"
            );
        }
    }

    #[test]
    fn invalid_capabilities_on_unparseable_versions_or_non_positive_schema_versions() {
        let mut caps = capabilities();
        caps.minimum_client_version = "1.7".to_string();
        assert_eq!(
            negotiate_presence(&caps, "1.8.3"),
            PresenceNegotiation::InvalidCapabilities
        );
        assert_eq!(
            negotiate_presence(&capabilities(), "not-a-version"),
            PresenceNegotiation::InvalidCapabilities
        );
        let mut caps = capabilities();
        caps.presence_schema_versions = vec![0, 2];
        assert_eq!(
            negotiate_presence(&caps, "1.8.3"),
            PresenceNegotiation::InvalidCapabilities
        );
    }
}
