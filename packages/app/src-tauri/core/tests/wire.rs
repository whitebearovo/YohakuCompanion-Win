//! Ported from `packages/core/test/companion/wire.test.ts` (spec §11.1),
//! plus decode-strictness vectors for the `protocol::types` envelope
//! decoders (the TS suite covered those only via zod types).

use yohaku_core::protocol::wire::{
    decode_wire_date, encode_wire_date, is_valid_wire_identifier, require_wire_integer,
    seconds_to_wire_milliseconds, MAXIMUM_SAFE_WIRE_INTEGER,
};

/// The TS `RFC3339_MS_UTC` regex:
/// `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$`.
fn matches_wire_date_shape(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 24
        && bytes.iter().enumerate().all(|(i, &c)| match i {
            4 | 7 => c == b'-',
            10 => c == b'T',
            13 | 16 => c == b':',
            19 => c == b'.',
            23 => c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

mod wire_dates {
    use super::*;

    #[test]
    fn encodes_epoch_ms_as_rfc3339_with_exactly_3_fractional_digits_and_z() {
        let encoded = encode_wire_date(1_753_500_012_345).unwrap();
        assert!(matches_wire_date_shape(&encoded), "{encoded}");
        assert_eq!(encoded, "2025-07-26T03:20:12.345Z");
        assert_eq!(encode_wire_date(0).unwrap(), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn round_trips_encode_decode() {
        let ms = 1_753_500_012_345;
        assert_eq!(
            decode_wire_date(&encode_wire_date(ms).unwrap()).unwrap(),
            ms
        );
    }

    #[test]
    fn rejects_non_canonical_dates() {
        let rejected = [
            "2026-07-26T09:41:12Z",          // no milliseconds
            "2026-07-26T09:41:12.34Z",       // 2 digits
            "2026-07-26T09:41:12.345678Z",   // 6 digits
            "2026-07-26T09:41:12.345+00:00", // offset form
            "2026-07-26 09:41:12.345Z",      // space separator
            "2026-13-26T09:41:12.345Z",      // invalid month
        ];
        for value in rejected {
            assert!(
                decode_wire_date(value).is_err(),
                "expected WireError for {value:?}"
            );
        }
    }
}

mod wire_identifiers {
    use super::*;

    #[test]
    fn accepts_uuids_any_case_and_crockford_ulids() {
        assert!(is_valid_wire_identifier(
            "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b"
        ));
        assert!(is_valid_wire_identifier(
            "3F6F6C0A-58A8-4A9D-B0A8-1C2D3E4F5A6B"
        ));
        assert!(is_valid_wire_identifier("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
    }

    #[test]
    fn rejects_ulids_containing_i_l_o_u_lowercase_wrong_length() {
        assert!(!is_valid_wire_identifier("01ARZ3NDEKTSV4RRFFQ69G5FAI"));
        assert!(!is_valid_wire_identifier("01arz3ndektsv4rrffq69g5fav"));
        assert!(!is_valid_wire_identifier("01ARZ3NDEKTSV4RRFFQ69G5FA"));
        assert!(!is_valid_wire_identifier("not-an-id"));
    }
}

mod wire_integers {
    use super::*;

    #[test]
    fn accepts_0_to_2_pow_53_minus_1() {
        assert_eq!(require_wire_integer(0.0, "x").unwrap(), 0);
        assert_eq!(
            require_wire_integer(MAXIMUM_SAFE_WIRE_INTEGER as f64, "x").unwrap(),
            MAXIMUM_SAFE_WIRE_INTEGER
        );
    }

    #[test]
    fn rejects_negatives_floats_and_beyond_safe_values() {
        assert!(require_wire_integer(-1.0, "x").is_err());
        assert!(require_wire_integer(1.5, "x").is_err());
        assert!(require_wire_integer((MAXIMUM_SAFE_WIRE_INTEGER + 1) as f64, "x").is_err());
    }
}

mod seconds_to_wire_milliseconds_tests {
    use super::*;

    #[test]
    fn rounds_to_integer_milliseconds() {
        assert_eq!(seconds_to_wire_milliseconds(1.2345, "x").unwrap(), 1235);
        assert_eq!(seconds_to_wire_milliseconds(0.0, "x").unwrap(), 0);
    }

    #[test]
    fn rejects_negative_and_non_finite() {
        assert!(seconds_to_wire_milliseconds(-0.001, "x").is_err());
        assert!(seconds_to_wire_milliseconds(f64::NAN, "x").is_err());
        assert!(seconds_to_wire_milliseconds(f64::INFINITY, "x").is_err());
    }
}

/// Decode-strictness vectors (zod-only coverage in TS): required-nullable
/// key presence, schema literals, wire refinements, unknown-key tolerance.
/// Body literals mirror `test/helpers/mockServer.ts` (spec §10).
mod envelope_decode_strictness {
    use serde_json::json;
    use yohaku_core::protocol::types::{
        decode_capabilities_response, decode_error_envelope, decode_mutation_response,
        decode_pairing_claim_response, decode_pairing_error_envelope,
    };

    fn meta() -> serde_json::Value {
        json!({
            "schema": "yohaku.companion.presence",
            "schemaVersion": 2,
            "requestId": "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b",
            "serverTime": "2026-07-26T04:00:12.345Z"
        })
    }

    fn mutation_body() -> serde_json::Value {
        json!({
            "meta": meta(),
            "data": {
                "acceptedSequence": 7,
                "receivedAt": "2026-07-26T04:00:12.345Z",
                "state": {
                    "schemaVersion": 2,
                    "epoch": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    "revision": 1,
                    "projection": null
                }
            }
        })
    }

    #[test]
    fn accepts_the_mock_server_mutation_success_shape_and_ignores_unknown_keys() {
        let mut body = mutation_body();
        body["data"]["extra"] = json!({ "ignored": true });
        let decoded = decode_mutation_response(&body).unwrap();
        assert_eq!(decoded.data.accepted_sequence, 7);
        assert_eq!(decoded.data.received_at, "2026-07-26T04:00:12.345Z");
        assert_eq!(decoded.data.state.epoch, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
        assert_eq!(decoded.data.state.revision, 1);
        assert_eq!(decoded.data.state.projection, None);
        assert_eq!(
            decoded.meta.request_id,
            "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b"
        );
    }

    #[test]
    fn missing_required_nullable_projection_key_fails_decode() {
        let mut body = mutation_body();
        body["data"]["state"]
            .as_object_mut()
            .unwrap()
            .remove("projection");
        assert!(decode_mutation_response(&body).is_err());
    }

    #[test]
    fn accepted_sequence_beyond_the_safe_range_fails_decode() {
        let mut body = mutation_body();
        body["data"]["acceptedSequence"] = json!(9_007_199_254_740_992u64);
        assert!(decode_mutation_response(&body).is_err());
    }

    #[test]
    fn rejects_wrong_schema_literal_and_non_canonical_server_time() {
        let mut body = mutation_body();
        body["meta"]["schema"] = json!("other.schema");
        assert!(decode_mutation_response(&body).is_err());

        let mut body = mutation_body();
        body["meta"]["serverTime"] = json!("2026-07-26T04:00:12Z");
        assert_eq!(
            decode_mutation_response(&body).unwrap_err(),
            "expected canonical RFC3339 millisecond UTC date"
        );
    }

    fn error_body() -> serde_json::Value {
        json!({
            "meta": meta(),
            "error": {
                "code": "INTERNAL_ERROR",
                "message": "INTERNAL_ERROR for testing",
                "retryable": false,
                "retryAfterMs": null,
                "acceptedSequence": null,
                "fields": []
            }
        })
    }

    #[test]
    fn error_envelope_requires_the_nullable_keys_to_be_present() {
        let decoded = decode_error_envelope(&error_body()).unwrap();
        assert_eq!(decoded.error.code, "INTERNAL_ERROR");
        assert_eq!(decoded.error.retry_after_ms, None);
        assert_eq!(decoded.error.accepted_sequence, None);

        let mut body = error_body();
        body["error"]
            .as_object_mut()
            .unwrap()
            .remove("acceptedSequence");
        assert!(decode_error_envelope(&body).is_err());
    }

    #[test]
    fn pairing_claim_rejects_blank_tokens_and_invalid_device_ids() {
        let body = json!({
            "data": {
                "deviceId": "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b",
                "deviceToken": "secret-token",
                "scopes": ["companion:presence:write"],
                "nextSequence": 5
            }
        });
        assert!(decode_pairing_claim_response(&body).is_ok());

        let mut blank = body.clone();
        blank["data"]["deviceToken"] = json!("   ");
        assert_eq!(
            decode_pairing_claim_response(&blank).unwrap_err(),
            "empty token"
        );

        let mut bad_id = body.clone();
        bad_id["data"]["deviceId"] = json!("not-an-id");
        assert_eq!(
            decode_pairing_claim_response(&bad_id).unwrap_err(),
            "expected UUID or ULID"
        );
    }

    #[test]
    fn capabilities_decode_allows_negative_integers_positivity_is_negotiations_job() {
        let body = json!({
            "meta": meta(),
            "data": {
                "minimumClientVersion": "1.7.0",
                "presenceSchemaVersions": [-2],
                "momentSchemaVersions": [1],
                "features": {
                    "liveDesk": true,
                    "mediaTimeline": true,
                    "moments": true,
                    "readingSessions": false
                },
                "limits": {
                    "presencePayloadBytes": -1,
                    "presenceRequestsPerMinute": 120,
                    "presenceLeaseMinSeconds": 30,
                    "presenceLeaseMaxSeconds": 120,
                    "recommendedHeartbeatSeconds": 45,
                    "maximumClockSkewSeconds": 60
                }
            }
        });
        let decoded = decode_capabilities_response(&body).unwrap();
        assert_eq!(decoded.data.presence_schema_versions, vec![-2]);
        assert_eq!(decoded.data.limits.presence_payload_bytes, -1);
    }

    #[test]
    fn pairing_error_envelope_needs_only_error_code() {
        let decoded = decode_pairing_error_envelope(
            &json!({ "error": { "code": "PAIRING_CODE_INVALID" }, "junk": 1 }),
        )
        .unwrap();
        assert_eq!(decoded.error.code, "PAIRING_CODE_INVALID");
        assert!(decode_pairing_error_envelope(&json!({ "error": {} })).is_err());
    }
}
