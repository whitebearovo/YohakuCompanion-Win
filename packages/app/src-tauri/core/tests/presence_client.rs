//! Ported 1:1 from `packages/core/test/companion/presenceClient.test.ts`
//! (spec `protocol-transport.md` section 11.5), literal values preserved.
//!
//! Fixture per test: mock Companion server on loopback HTTP; in-memory
//! sequence persistence; `CompanionSequencer(persistence, DEVICE, 5)`;
//! credential `{ deviceId: DEVICE, deviceToken: "secret-token" }`; the
//! negotiated CONFIG below; every replace uses requestedLeaseSeconds = 90.

mod support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;

use support::mock_server::{
    error_envelope, mutation_success, ErrorEnvelopeOptions, MockCompanionServer, MockOutcome,
    MockResponse, RecordedRequest,
};
use yohaku_core::companion::errors::CompanionTransportError;
use yohaku_core::companion::http_client::{CompanionHttpClient, CompanionServerConfiguration};
use yohaku_core::companion::presence_client::{PresenceClient, PresenceClientError};
use yohaku_core::model::{
    ClearReason, CompanionCredential, NegotiatedPresenceConfiguration,
    SanitizedApplicationPresence, SanitizedPresenceSnapshot,
};
use yohaku_core::protocol::sequencer::{CompanionSequencer, SequenceBacking};

const DEVICE: &str = "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b";

struct MemoryPersistence {
    values: Mutex<HashMap<String, i64>>,
}

impl MemoryPersistence {
    fn new() -> Self {
        Self {
            values: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl SequenceBacking for MemoryPersistence {
    async fn load(&self, device_id: &str) -> Option<i64> {
        self.values.lock().unwrap().get(device_id).copied()
    }

    async fn store(&self, device_id: &str, next: u64) -> std::io::Result<()> {
        self.values
            .lock()
            .unwrap()
            .insert(device_id.to_string(), next as i64);
        Ok(())
    }
}

fn config() -> NegotiatedPresenceConfiguration {
    NegotiatedPresenceConfiguration {
        supports_media_timeline: true,
        maximum_payload_bytes: 32768,
        requests_per_minute: 120,
        lease_min_seconds: 30,
        lease_max_seconds: 120,
        recommended_heartbeat_seconds: 45,
        maximum_clock_skew_seconds: 60,
    }
}

fn snapshot() -> SanitizedPresenceSnapshot {
    SanitizedPresenceSnapshot {
        observed_at: 1_753_500_012_345,
        application: Some(SanitizedApplicationPresence {
            display_name: "Code".to_string(),
            window_title: None,
        }),
        media: None,
    }
}

struct Fixture {
    server: MockCompanionServer,
    client: PresenceClient,
}

async fn fixture() -> Fixture {
    let server = MockCompanionServer::start().await;
    let persistence = Arc::new(MemoryPersistence::new());
    let sequencer = Arc::new(CompanionSequencer::new(persistence, DEVICE, 5));
    let configuration =
        CompanionServerConfiguration::new(&server.base_url()).expect("loopback HTTP base URL");
    let http = Arc::new(CompanionHttpClient::new(configuration));
    let client = PresenceClient::new(
        http,
        CompanionCredential {
            device_id: DEVICE.to_string(),
            device_token: "secret-token".to_string(),
        },
        sequencer,
        config(),
    );
    Fixture { server, client }
}

fn body_json(req: &RecordedRequest) -> &Value {
    req.json.as_ref().expect("request carries a JSON body")
}

fn meta_sequence(req: &RecordedRequest) -> u64 {
    body_json(req)["meta"]["sequence"]
        .as_u64()
        .expect("meta.sequence is a wire integer")
}

#[tokio::test]
async fn puts_to_companion_presence_with_bearer_token_and_version_header() {
    let f = fixture().await;
    f.server.set_fallback(|req| mutation_success(req, None));

    f.client
        .replace_presence(&snapshot(), 90.0)
        .await
        .expect("publish succeeds");

    let requests = f.server.requests();
    let req = &requests[0];
    assert_eq!(req.method, "PUT");
    assert_eq!(req.path, "/companion/presence");
    assert_eq!(
        req.headers.get("authorization").map(String::as_str),
        Some("Bearer secret-token"),
    );
    assert_eq!(
        req.headers
            .get("x-yohaku-companion-version")
            .map(String::as_str),
        Some("1.8.3"),
    );
    assert_eq!(meta_sequence(req), 5);
}

#[tokio::test]
async fn retries_exactly_once_with_byte_identical_body_on_retryable_5xx() {
    let f = fixture().await;
    f.server.enqueue(|req| {
        error_envelope(
            req,
            500,
            "INTERNAL_ERROR",
            ErrorEnvelopeOptions {
                retryable: Some(true),
                ..Default::default()
            },
        )
    });
    f.server.set_fallback(|req| mutation_success(req, None));

    f.client
        .replace_presence(&snapshot(), 90.0)
        .await
        .expect("retry succeeds");

    let requests = f.server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].raw_body, requests[0].raw_body);
    // Same sequence + requestId, no new sequence allocated for the retry.
    let first = body_json(&requests[0]);
    let second = body_json(&requests[1]);
    assert_eq!(second["meta"]["sequence"], first["meta"]["sequence"]);
    assert_eq!(second["meta"]["requestId"], first["meta"]["requestId"]);
}

#[tokio::test]
async fn retries_once_on_ambiguous_transport_failure_socket_destroyed() {
    let f = fixture().await;
    f.server.enqueue(|_req| MockOutcome::SocketDestroy);
    f.server.set_fallback(|req| mutation_success(req, None));

    f.client
        .replace_presence(&snapshot(), 90.0)
        .await
        .expect("retry succeeds");

    let requests = f.server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].raw_body, requests[0].raw_body);
}

#[tokio::test]
async fn does_not_retry_non_retryable_5xx() {
    let f = fixture().await;
    f.server.enqueue(|req| {
        error_envelope(
            req,
            500,
            "INTERNAL_ERROR",
            ErrorEnvelopeOptions {
                retryable: Some(false),
                ..Default::default()
            },
        )
    });

    let error = f
        .client
        .replace_presence(&snapshot(), 90.0)
        .await
        .expect_err("non-retryable 5xx rejects");
    assert!(matches!(
        error,
        PresenceClientError::Transport(CompanionTransportError::Server { .. })
    ));
    assert_eq!(f.server.requests().len(), 1);
}

#[tokio::test]
async fn does_not_retry_4xx_and_surfaces_the_envelope() {
    let f = fixture().await;
    f.server.enqueue(|req| {
        error_envelope(
            req,
            422,
            "VALIDATION_FAILED",
            ErrorEnvelopeOptions::default(),
        )
    });

    let error = f
        .client
        .replace_presence(&snapshot(), 90.0)
        .await
        .expect_err("4xx rejects");
    assert!(matches!(
        error,
        PresenceClientError::Transport(CompanionTransportError::Server { .. })
    ));
    assert_eq!(f.server.requests().len(), 1);
}

#[tokio::test]
async fn fails_on_two_consecutive_failures_no_second_retry() {
    let f = fixture().await;
    f.server.enqueue(|_req| MockOutcome::SocketDestroy);
    f.server.enqueue(|_req| MockOutcome::SocketDestroy);

    let error = f
        .client
        .replace_presence(&snapshot(), 90.0)
        .await
        .expect_err("two consecutive failures reject");
    // Surfaces the SECOND network error; total attempts capped at 2.
    assert!(matches!(
        error,
        PresenceClientError::Transport(CompanionTransportError::Network { .. })
    ));
    assert_eq!(f.server.requests().len(), 2);
}

#[tokio::test]
async fn reconciles_accepted_sequence_from_success_responses() {
    let f = fixture().await;
    f.server.enqueue(|req| mutation_success(req, Some(100)));
    f.server.set_fallback(|req| mutation_success(req, None));

    f.client
        .replace_presence(&snapshot(), 90.0)
        .await
        .expect("first publish succeeds");
    f.client
        .replace_presence(&snapshot(), 90.0)
        .await
        .expect("second publish succeeds");

    let requests = f.server.requests();
    assert_eq!(meta_sequence(&requests[1]), 101);
}

#[tokio::test]
async fn reconciles_accepted_sequence_from_error_envelopes() {
    let f = fixture().await;
    f.server.enqueue(|req| {
        error_envelope(
            req,
            409,
            "COMPANION_SEQUENCE_BEHIND",
            ErrorEnvelopeOptions {
                accepted_sequence: Some(200),
                ..Default::default()
            },
        )
    });
    f.server.set_fallback(|req| mutation_success(req, None));

    f.client
        .replace_presence(&snapshot(), 90.0)
        .await
        .expect_err("409 is not retried");
    f.client
        .replace_presence(&snapshot(), 90.0)
        .await
        .expect("second publish succeeds");

    let requests = f.server.requests();
    assert_eq!(meta_sequence(&requests[1]), 201);
}

#[tokio::test]
async fn rejects_a_response_that_does_not_echo_the_request_id_then_retries() {
    let f = fixture().await;
    f.server.enqueue(|req| {
        let mut ok = mutation_success(req, None);
        ok.body["meta"]["requestId"] =
            Value::String("00000000-0000-4000-8000-000000000000".to_string());
        ok
    });
    f.server.set_fallback(|req| mutation_success(req, None));

    f.client
        .replace_presence(&snapshot(), 90.0)
        .await
        .expect("retry succeeds after the mismatched response is discarded");
    assert_eq!(f.server.requests().len(), 2);
}

#[tokio::test]
async fn clear_presence_consumes_a_sequence_and_posts_the_reason() {
    let f = fixture().await;
    f.server.set_fallback(|req| mutation_success(req, None));

    f.client
        .replace_presence(&snapshot(), 90.0)
        .await
        .expect("publish succeeds");
    f.client
        .clear_presence(ClearReason::Sleep, 1_753_500_012_345)
        .await
        .expect("clear succeeds");

    let requests = f.server.requests();
    let clear = &requests[1];
    assert_eq!(clear.path, "/companion/presence/clear");
    assert_eq!(body_json(clear)["data"]["reason"], "sleep");
    assert_eq!(meta_sequence(clear), 6);
}

#[tokio::test]
async fn serializes_concurrent_mutations_through_the_send_slot() {
    let f = fixture().await;
    f.server.set_fallback(|req| mutation_success(req, None));

    // join! polls in declaration order, so the FIFO slot queues the three
    // mutations in call order — the TS test issued them synchronously.
    let snapshot_a = snapshot();
    let snapshot_b = snapshot();
    let (a, b, c) = tokio::join!(
        f.client.replace_presence(&snapshot_a, 90.0),
        f.client.replace_presence(&snapshot_b, 90.0),
        f.client
            .clear_presence(ClearReason::Paused, 1_753_500_012_345),
    );
    a.expect("first replace succeeds");
    b.expect("second replace succeeds");
    c.expect("clear succeeds");

    let sequences: Vec<u64> = f.server.requests().iter().map(meta_sequence).collect();
    assert_eq!(sequences, vec![5, 6, 7]);
}

#[tokio::test]
async fn classifies_schema_rejection_and_bare_426_as_renegotiation_signals() {
    let f = fixture().await;

    f.server.enqueue(|req| {
        error_envelope(
            req,
            409,
            "COMPANION_SCHEMA_UNSUPPORTED",
            ErrorEnvelopeOptions::default(),
        )
    });
    let error1 = f
        .client
        .replace_presence(&snapshot(), 90.0)
        .await
        .expect_err("schema rejection surfaces");
    assert!(error1.needs_renegotiation());

    // JSON body that is NOT a protocol envelope -> HttpStatus(426), not
    // retried.
    f.server.enqueue(|_req| MockResponse {
        status: 426,
        body: serde_json::json!({ "upgrade": "required" }),
    });
    let error2 = f
        .client
        .replace_presence(&snapshot(), 90.0)
        .await
        .expect_err("bare 426 surfaces");
    assert!(error2.needs_renegotiation());

    f.server.enqueue(|req| {
        error_envelope(
            req,
            422,
            "VALIDATION_FAILED",
            ErrorEnvelopeOptions::default(),
        )
    });
    let error3 = f
        .client
        .replace_presence(&snapshot(), 90.0)
        .await
        .expect_err("validation failure surfaces");
    assert!(!error3.needs_renegotiation());
}
