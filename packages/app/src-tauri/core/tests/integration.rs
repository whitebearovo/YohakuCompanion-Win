//! Port of `packages/core/test/companion/integration.test.ts` — all 14
//! vectors, literal values preserved. Headless end-to-end: real
//! `CompanionService` + `CaptureService` + privacy pipeline + protocol
//! client against a mock Mix Space Core, with fake foreground/media sources.

mod support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use support::mock_server::{
    capabilities_response, error_envelope, mutation_success, response_meta, CapabilitiesPatch,
    ErrorEnvelopeOptions, MockCompanionServer, MockOutcome, MockResponse, RecordedRequest,
};
use yohaku_core::capture::{ForegroundSource, MediaProvider};
use yohaku_core::companion::coordinator::CoordinatorTimings;
use yohaku_core::companion::service::{CompanionService, CompanionServiceDeps};
use yohaku_core::model::{
    ApplicationPrivacyRule, CredentialBackend, ForegroundInfo, IpcErrorCode, MediaKind,
    MediaSnapshot, PrivacyOverride, RuntimeState, Unsubscribe,
};
use yohaku_core::privacy::capture_service::CaptureService;
use yohaku_core::store::config::ConfigStore;
use yohaku_core::store::credentials::{CredentialStore, CredentialStoreError};
use yohaku_core::store::sequence::FileSequenceStore;

const DEVICE_ID: &str = "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b";
const DEVICE_TOKEN: &str = "test-device-token";

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn pairing_claim_response() -> MockResponse {
    MockResponse {
        status: 200,
        body: json!({
            "meta": response_meta(&uuid::Uuid::new_v4().to_string()),
            "data": {
                "deviceId": DEVICE_ID,
                "deviceToken": DEVICE_TOKEN,
                "scopes": ["companion:presence:write"],
                "nextSequence": 10,
            },
        }),
    }
}

// --- fakes ------------------------------------------------------------------

#[derive(Default)]
struct FakeCredentialStore {
    tokens: Mutex<HashMap<String, String>>,
}

#[async_trait]
impl CredentialStore for FakeCredentialStore {
    fn backend(&self) -> CredentialBackend {
        CredentialBackend::Keyring
    }
    async fn get(&self, device_id: &str) -> Option<String> {
        self.tokens.lock().unwrap().get(device_id).cloned()
    }
    async fn set(&self, device_id: &str, token: &str) -> Result<(), CredentialStoreError> {
        self.tokens
            .lock()
            .unwrap()
            .insert(device_id.to_string(), token.to_string());
        Ok(())
    }
    async fn delete(&self, device_id: &str) -> Result<(), CredentialStoreError> {
        self.tokens.lock().unwrap().remove(device_id);
        Ok(())
    }
}

#[derive(Default)]
struct FakeMediaProvider {
    snapshot: Mutex<Option<MediaSnapshot>>,
}

#[async_trait]
impl MediaProvider for FakeMediaProvider {
    fn kind(&self) -> &'static str {
        "npm" // TS fake literal; never asserted, feeds no snapshot field here
    }
    async fn get_snapshot(&self, _timeout: Duration) -> Option<MediaSnapshot> {
        self.snapshot.lock().unwrap().clone()
    }
    fn on_semantic_change(&self, _callback: Box<dyn Fn() + Send + Sync>) -> Unsubscribe {
        Unsubscribe::noop()
    }
    fn healthy(&self) -> bool {
        true
    }
    async fn stop(&self) {}
}

struct FakeForeground {
    info: Mutex<Option<ForegroundInfo>>,
}

impl ForegroundSource for FakeForeground {
    fn current(&self) -> Option<ForegroundInfo> {
        self.info.lock().unwrap().clone()
    }
}

// --- harness ----------------------------------------------------------------

struct Ctx {
    server: MockCompanionServer,
    dir: std::path::PathBuf,
    config: Arc<ConfigStore>,
    credentials: Arc<FakeCredentialStore>,
    media: Arc<FakeMediaProvider>,
    foreground: Arc<FakeForeground>,
    service: CompanionService,
}

async fn setup() -> Ctx {
    let server = MockCompanionServer::start().await;
    server.set_fallback(|req: &RecordedRequest| {
        if req.path == "/companion/capabilities" {
            return capabilities_response(CapabilitiesPatch::default());
        }
        if req.path == "/companion/pairings/claim" {
            return pairing_claim_response();
        }
        mutation_success(req, None)
    });

    let dir = std::env::temp_dir().join(format!("yohaku-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create temp data dir");
    let config = Arc::new(ConfigStore::new(Some(dir.clone())).expect("config store"));
    let credentials = Arc::new(FakeCredentialStore::default());
    let media = Arc::new(FakeMediaProvider::default());
    let foreground = Arc::new(FakeForeground {
        info: Mutex::new(Some(ForegroundInfo {
            app_id: "code.exe".to_string(),
            exe_path: Some("C:/apps/code.exe".to_string()),
            display_name: "Visual Studio Code".to_string(),
            window_title: Some("secret.ts — project".to_string()),
        })),
    });
    let capture = Arc::new(CaptureService::new(
        foreground.clone() as Arc<dyn ForegroundSource>,
        Box::new({
            let media = media.clone();
            move || Some(media.clone() as Arc<dyn MediaProvider>)
        }),
        Box::new({
            let config = config.clone();
            move || config.get().privacy
        }),
    ));
    let sequence_store =
        Arc::new(FileSequenceStore::new(Some(dir.clone())).expect("sequence store"));
    let service = CompanionService::new(CompanionServiceDeps {
        config: config.clone(),
        capture,
        sequence_store,
        credentials: Box::new({
            let credentials = credentials.clone();
            move || {
                let credentials = credentials.clone();
                Box::pin(async move {
                    Ok::<_, CredentialStoreError>(credentials as Arc<dyn CredentialStore>)
                })
            }
        }),
        on_changed: Box::new(|| {}),
        coordinator_timings: CoordinatorTimings {
            network_retry_ms: 100,
            feature_retry_ms: 200,
            ..Default::default()
        },
    });

    Ctx {
        server,
        dir,
        config,
        credentials,
        media,
        foreground,
        service,
    }
}

async fn teardown(ctx: Ctx) {
    let Ctx {
        server,
        dir,
        service,
        ..
    } = ctx;
    service.shutdown().await;
    server.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// TS `until(cond)`: poll every 15 ms, default timeout 3000 ms.
async fn until(condition: impl Fn() -> bool) {
    until_timeout(condition, 3000).await;
}

async fn until_timeout(condition: impl Fn() -> bool, timeout_ms: u64) {
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    while !condition() {
        if std::time::Instant::now() > deadline {
            panic!("condition timeout");
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

async fn pair_and_consent(ctx: &Ctx) {
    ctx.service
        .pair(&ctx.server.base_url(), "Test PC", "PAIR-CODE")
        .await
        .expect("pair");
    let preview = ctx.service.refresh_preview().await.expect("preview");
    ctx.service
        .confirm_consent(&preview.policy_fingerprint)
        .await
        .expect("consent");
    until(|| ctx.service.runtime_state() == RuntimeState::Active).await;
}

fn requests_to(server: &MockCompanionServer, path: &str) -> Vec<RecordedRequest> {
    server
        .requests()
        .into_iter()
        .filter(|request| request.path == path)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn pairs_with_live_desk_disabled_and_stores_the_token_securely() {
    let ctx = setup().await;
    ctx.service
        .pair(&ctx.server.base_url(), "  Test PC  ", "PAIR-CODE")
        .await
        .expect("pair");
    let connection = ctx.config.get().connection.expect("connection persisted");
    assert!(!connection.live_desk_enabled);
    assert_eq!(connection.device_id, DEVICE_ID);
    assert_eq!(connection.pairing_next_sequence, 10);
    assert_eq!(
        ctx.credentials
            .tokens
            .lock()
            .unwrap()
            .get(DEVICE_ID)
            .map(String::as_str),
        Some(DEVICE_TOKEN)
    );
    // Config file must never contain the token.
    let config_json = serde_json::to_string(&ctx.config.get()).expect("config json");
    assert!(!config_json.contains(DEVICE_TOKEN));
    // Pairing alone must not publish anything.
    assert_eq!(requests_to(&ctx.server, "/companion/presence").len(), 0);
    assert_eq!(ctx.service.runtime_state(), RuntimeState::Disabled);
    teardown(ctx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn capabilities_are_negotiated_before_the_one_time_code_is_consumed() {
    let ctx = setup().await;
    ctx.service
        .pair(&ctx.server.base_url(), "Test PC", "PAIR-CODE")
        .await
        .expect("pair");
    let paths: Vec<String> = ctx
        .server
        .requests()
        .into_iter()
        .map(|request| request.path)
        .collect();
    let capabilities_index = paths
        .iter()
        .position(|path| path == "/companion/capabilities")
        .expect("capabilities requested");
    let claim_index = paths
        .iter()
        .position(|path| path == "/companion/pairings/claim")
        .expect("claim requested");
    assert!(capabilities_index < claim_index);
    teardown(ctx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_pairing_when_the_presence_scope_is_missing() {
    let ctx = setup().await;
    ctx.server
        .enqueue(|_| capabilities_response(CapabilitiesPatch::default()));
    ctx.server.enqueue(|_| MockResponse {
        status: 200,
        body: json!({
            "meta": response_meta(&uuid::Uuid::new_v4().to_string()),
            "data": {
                "deviceId": DEVICE_ID,
                "deviceToken": DEVICE_TOKEN,
                "scopes": ["companion:moment:write"],
                "nextSequence": 10,
            },
        }),
    });
    let error = ctx
        .service
        .pair(&ctx.server.base_url(), "PC", "CODE")
        .await
        .expect_err("pair must fail");
    assert_eq!(error.code, IpcErrorCode::RequiredScopeMissing);
    teardown(ctx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn maps_companion_pairing_expired_to_pairing_expired() {
    let ctx = setup().await;
    ctx.server
        .enqueue(|_| capabilities_response(CapabilitiesPatch::default()));
    ctx.server.enqueue(|_| MockResponse {
        status: 410,
        body: json!({ "error": { "code": "COMPANION_PAIRING_EXPIRED" } }),
    });
    let error = ctx
        .service
        .pair(&ctx.server.base_url(), "PC", "CODE")
        .await
        .expect_err("pair must fail");
    assert_eq!(error.code, IpcErrorCode::PairingExpired);
    teardown(ctx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn consent_with_a_stale_fingerprint_fails_and_never_enables() {
    let ctx = setup().await;
    ctx.service
        .pair(&ctx.server.base_url(), "PC", "CODE")
        .await
        .expect("pair");
    ctx.service.refresh_preview().await.expect("preview");
    let error = ctx
        .service
        .confirm_consent("stale-fingerprint")
        .await
        .expect_err("consent must fail");
    assert_eq!(error.code, IpcErrorCode::PreviewOutOfDate);
    assert!(!ctx.config.get().connection.unwrap().live_desk_enabled);
    teardown(ctx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn consent_fails_when_the_capture_drifts_between_preview_and_click() {
    let ctx = setup().await;
    ctx.service
        .pair(&ctx.server.base_url(), "PC", "CODE")
        .await
        .expect("pair");
    let preview = ctx.service.refresh_preview().await.expect("preview");
    {
        let mut info = ctx.foreground.info.lock().unwrap();
        let current = info.clone().expect("foreground fixture");
        *info = Some(ForegroundInfo {
            app_id: "other.exe".to_string(),
            display_name: "Other".to_string(),
            ..current
        });
    }
    let error = ctx
        .service
        .confirm_consent(&preview.policy_fingerprint)
        .await
        .expect_err("consent must fail");
    assert_eq!(error.code, IpcErrorCode::PreviewOutOfDate);
    assert!(!ctx.config.get().connection.unwrap().live_desk_enabled);
    teardown(ctx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn full_flow_consent_active_sanitized_presence_published() {
    let ctx = setup().await;
    *ctx.media.snapshot.lock().unwrap() = Some(MediaSnapshot {
        app_id: Some("spotify.exe".to_string()),
        source_app_user_model_id: Some("spotify.exe".to_string()),
        player_display_name: Some("Spotify".to_string()),
        kind: MediaKind::Music,
        title: Some("Song".to_string()),
        artist: Some("Artist".to_string()),
        album: Some("Album".to_string()),
        playing: true,
        duration_seconds: Some(200.0),
        position_seconds: Some(60.0),
        sampled_at: now_ms(),
    });
    pair_and_consent(&ctx).await;
    until(|| !requests_to(&ctx.server, "/companion/presence").is_empty()).await;

    let puts = requests_to(&ctx.server, "/companion/presence");
    let put = &puts[0];
    assert_eq!(
        put.headers.get("authorization").map(String::as_str),
        Some("Bearer test-device-token")
    );
    let body = put.json.as_ref().expect("json body");
    assert_eq!(body["meta"]["deviceId"], DEVICE_ID);
    assert!(body["meta"]["sequence"].as_u64().expect("sequence") >= 10);
    assert_eq!(body["data"]["availability"], "active");
    assert_eq!(
        body["data"]["application"]["displayName"],
        "Visual Studio Code"
    );
    // windowTitle: rule default hide + global switch off -> no title on the wire
    assert!(body["data"]["application"]["window"].is_null());
    assert_eq!(body["data"]["media"]["title"], "Song");
    // Raw identifiers never leave the process.
    assert!(!put.raw_body.contains("code.exe"));
    assert!(!put.raw_body.contains("spotify.exe"));
    assert!(!put.raw_body.contains("C:/apps"));
    teardown(ctx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn privacy_change_while_enabled_republishes_under_the_new_policy() {
    let ctx = setup().await;
    pair_and_consent(&ctx).await;
    until(|| !requests_to(&ctx.server, "/companion/presence").is_empty()).await;
    let count_before = requests_to(&ctx.server, "/companion/presence").len();

    ctx.config
        .update(|config| {
            config.privacy.rules = vec![ApplicationPrivacyRule {
                app_id: "code.exe".to_string(),
                application: PrivacyOverride::Share,
                window_title: PrivacyOverride::Inherit,
                media: PrivacyOverride::Inherit,
                display_alias: Some("编辑器".to_string()),
            }];
        })
        .expect("config update");
    ctx.service.policy_maybe_changed();

    until(|| requests_to(&ctx.server, "/companion/presence").len() > count_before).await;
    let puts = requests_to(&ctx.server, "/companion/presence");
    let last = puts.last().expect("presence request");
    let body = last.json.as_ref().expect("json body");
    assert_eq!(body["data"]["application"]["displayName"], "编辑器");
    // Consent stays valid (already enabled), but the recorded preview is gone.
    assert!(ctx.service.current_preview().is_none());
    assert!(ctx.config.get().connection.unwrap().live_desk_enabled);
    teardown(ctx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lock_clears_with_reason_sleep_wake_renegotiates_and_republishes() {
    let ctx = setup().await;
    pair_and_consent(&ctx).await;
    until(|| !requests_to(&ctx.server, "/companion/presence").is_empty()).await;
    let capabilities_before = requests_to(&ctx.server, "/companion/capabilities").len();

    ctx.service.handle_sleep_or_lock();
    until(|| !requests_to(&ctx.server, "/companion/presence/clear").is_empty()).await;
    let clears = requests_to(&ctx.server, "/companion/presence/clear");
    let clear_body = clears[0].json.as_ref().expect("clear body");
    assert_eq!(clear_body["data"]["reason"], "sleep");
    assert_eq!(ctx.service.runtime_state(), RuntimeState::Suspended);

    let puts_before = requests_to(&ctx.server, "/companion/presence").len();
    ctx.service.handle_wake_or_unlock();
    until(|| ctx.service.runtime_state() == RuntimeState::Active).await;
    until(|| requests_to(&ctx.server, "/companion/presence").len() > puts_before).await;
    assert!(requests_to(&ctx.server, "/companion/capabilities").len() > capabilities_before);
    teardown(ctx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sequences_stay_monotonic_across_clear_and_wake_single_sequencer() {
    let ctx = setup().await;
    pair_and_consent(&ctx).await;
    until(|| !requests_to(&ctx.server, "/companion/presence").is_empty()).await;
    ctx.service.handle_sleep_or_lock();
    until(|| !requests_to(&ctx.server, "/companion/presence/clear").is_empty()).await;
    ctx.service.handle_wake_or_unlock();
    until(|| ctx.service.runtime_state() == RuntimeState::Active).await;
    until(|| {
        requests_to(&ctx.server, "/companion/presence").len()
            + requests_to(&ctx.server, "/companion/presence/clear").len()
            >= 3
    })
    .await;

    let sequences: Vec<u64> = ctx
        .server
        .requests()
        .iter()
        .filter(|request| request.path.starts_with("/companion/presence"))
        .map(|request| {
            request.json.as_ref().expect("presence body")["meta"]["sequence"]
                .as_u64()
                .expect("sequence")
        })
        .collect();
    let mut sorted = sequences.clone();
    sorted.sort_unstable();
    assert_eq!(sequences, sorted);
    let unique: std::collections::HashSet<u64> = sequences.iter().copied().collect();
    assert_eq!(unique.len(), sequences.len());
    teardown(ctx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn schema_rejection_triggers_renegotiation_then_publishing_resumes() {
    let ctx = setup().await;
    pair_and_consent(&ctx).await;
    until(|| !requests_to(&ctx.server, "/companion/presence").is_empty()).await;
    let capabilities_before = requests_to(&ctx.server, "/companion/capabilities").len();

    ctx.server.enqueue(|req: &RecordedRequest| {
        error_envelope(
            req,
            409,
            "COMPANION_SCHEMA_UNSUPPORTED",
            ErrorEnvelopeOptions::default(),
        )
    });
    ctx.service.coordinator().request_fresh_snapshot();

    until(|| requests_to(&ctx.server, "/companion/capabilities").len() > capabilities_before).await;
    until_timeout(|| ctx.service.runtime_state() == RuntimeState::Active, 5000).await;
    teardown(ctx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn disable_persists_first_and_clears_with_reason_paused() {
    let ctx = setup().await;
    pair_and_consent(&ctx).await;
    ctx.service.disable_live_desk().await.expect("disable");
    assert!(!ctx.config.get().connection.unwrap().live_desk_enabled);
    let clears = requests_to(&ctx.server, "/companion/presence/clear");
    assert!(!clears.is_empty());
    let last_body = clears.last().unwrap().json.as_ref().expect("clear body");
    assert_eq!(last_body["data"]["reason"], "paused");
    assert_eq!(ctx.service.runtime_state(), RuntimeState::Disabled);
    teardown(ctx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unpair_clears_remotely_removes_credentials_and_connection() {
    let ctx = setup().await;
    pair_and_consent(&ctx).await;
    ctx.service.unpair().await.expect("unpair");
    let clears = requests_to(&ctx.server, "/companion/presence/clear");
    let last_body = clears.last().expect("clear").json.as_ref().expect("body");
    assert_eq!(last_body["data"]["reason"], "connectionRemoved");
    assert_eq!(ctx.credentials.tokens.lock().unwrap().len(), 0);
    assert!(ctx.config.get().connection.is_none());
    assert_eq!(ctx.service.runtime_state(), RuntimeState::NotPaired);
    teardown(ctx).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn degrades_on_network_failure_and_recovers_via_retry() {
    let ctx = setup().await;
    pair_and_consent(&ctx).await;
    until(|| !requests_to(&ctx.server, "/companion/presence").is_empty()).await;
    ctx.server.enqueue(|_| MockOutcome::SocketDestroy);
    // Second: kill the single idempotent retry too.
    ctx.server.enqueue(|_| MockOutcome::SocketDestroy);
    ctx.service.coordinator().request_fresh_snapshot();
    until(|| ctx.service.runtime_state() == RuntimeState::Degraded).await;
    // networkRetryMs=100 -> renegotiation restores active.
    until_timeout(|| ctx.service.runtime_state() == RuntimeState::Active, 5000).await;
    teardown(ctx).await;
}
