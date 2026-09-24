//! Application facade (TS `CompanionService`). Owns the one [`ConsentGate`]
//! and one [`LiveDeskCoordinator`].
//!
//! Owned by agent K. Ground truth:
//! `packages/core/src/companion/service.ts` and
//! `.claude/rewrite/specs/companion-service.md` §§5-7. Invariants enforced
//! here:
//! - pairing always lands with `liveDeskEnabled = false`;
//! - enabling Live Desk validates consent BEFORE and AFTER persisting, and
//!   rolls back to disabled on any drift (`previewOutOfDate`);
//! - disabling persists first, then clears remotely;
//! - a policy change invalidates the preview; when Live Desk is already
//!   enabled it republishes under the new policy instead of revoking
//!   consent.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::companion::consent_gate::{projection_of, Confirmation, ConsentGate};
use crate::companion::coordinator::{CoordinatorDeps, CoordinatorTimings, LiveDeskCoordinator};
use crate::companion::pairing::{claim_pairing, ClaimPairingError, PairingErrorCode};
use crate::model::{BoxFuture, ClearReason, IpcErrorCode, Preview, RuntimeState, StoredConnection};
use crate::privacy::capture_service::{CaptureOptions, CaptureService};
use crate::runtime::logger;
use crate::store::config::ConfigStore;
use crate::store::credentials::{CredentialStore, CredentialStoreError};
use crate::store::sequence::FileSequenceStore;

/// Service failure carrying the IPC error code returned to the UI
/// (`Display` is the exact TS literal, e.g. `"previewOutOfDate"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{code}")]
pub struct ServiceError {
    pub code: IpcErrorCode,
}

impl ServiceError {
    pub fn new(code: IpcErrorCode) -> Self {
        Self { code }
    }
}

/// Lazily resolved credential-store getter (TS `deps.credentials`).
pub type CredentialsFn =
    dyn Fn() -> BoxFuture<Result<Arc<dyn CredentialStore>, CredentialStoreError>> + Send + Sync;

/// Service dependencies (TS `CompanionServiceDeps`).
pub struct CompanionServiceDeps {
    pub config: Arc<ConfigStore>,
    pub capture: Arc<CaptureService>,
    pub sequence_store: Arc<FileSequenceStore>,
    /// Lazily resolved, memoized by the CALLER (app crate): repeated calls
    /// must return the same store selection for the process lifetime.
    pub credentials: Box<CredentialsFn>,
    /// Fired on any externally visible change -> UI snapshot broadcast.
    pub on_changed: Box<dyn Fn() + Send + Sync>,
    /// TS `Partial<CoordinatorTimings>`; use struct-update syntax over
    /// `CoordinatorTimings::default()` for partial overrides.
    pub coordinator_timings: CoordinatorTimings,
}

/// `{ lastPublishAt, lastError }` for the snapshot. `last_publish_at` is
/// NEVER reset (survives disable/unpair/policy changes); `last_error` is
/// set only by the (dead-code, ported for parity) `note_error` and cleared
/// on every successful publish.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublishTelemetry {
    pub last_publish_at: Option<i64>,
    pub last_error: Option<String>,
}

/// Wall-clock epoch milliseconds (TS `Date.now()`).
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub struct CompanionService {
    config: Arc<ConfigStore>,
    capture: Arc<CaptureService>,
    sequence_store: Arc<FileSequenceStore>,
    credentials: Arc<CredentialsFn>,
    on_changed: Arc<dyn Fn() + Send + Sync>,
    gate: Mutex<ConsentGate>,
    preview: Mutex<Option<Preview>>,
    telemetry: Arc<Mutex<PublishTelemetry>>,
    coordinator: LiveDeskCoordinator,
}

impl CompanionService {
    /// Order matters: build the gate with `capture.fingerprint()` FIRST,
    /// then the coordinator wired to config/credentials/telemetry.
    pub fn new(deps: CompanionServiceDeps) -> Self {
        let gate = ConsentGate::new(deps.capture.fingerprint());
        let credentials: Arc<CredentialsFn> = Arc::from(deps.credentials);
        let on_changed: Arc<dyn Fn() + Send + Sync> = Arc::from(deps.on_changed);
        let telemetry = Arc::new(Mutex::new(PublishTelemetry::default()));

        let coordinator = LiveDeskCoordinator::new(
            CoordinatorDeps {
                capture: deps.capture.clone(),
                sequence_store: deps.sequence_store.clone(),
                get_connection: Box::new({
                    let config = deps.config.clone();
                    move || config.get().connection
                }),
                // Store failures fold into None (TS treats throw and null
                // identically -> degraded, T3).
                get_token: Box::new({
                    let credentials = credentials.clone();
                    move |device_id: String| -> BoxFuture<Option<String>> {
                        let credentials = credentials.clone();
                        Box::pin(async move {
                            match (credentials)().await {
                                Ok(store) => store.get(&device_id).await,
                                Err(_) => None,
                            }
                        })
                    }
                }),
                // State value ignored by the service (parity with TS).
                on_state_change: Box::new({
                    let on_changed = on_changed.clone();
                    move |_state| (on_changed)()
                }),
                on_published: Box::new({
                    let telemetry = telemetry.clone();
                    let on_changed = on_changed.clone();
                    move || {
                        {
                            let mut telemetry = telemetry.lock();
                            telemetry.last_publish_at = Some(now_ms());
                            telemetry.last_error = None;
                        }
                        (on_changed)();
                    }
                }),
            },
            deps.coordinator_timings,
        );

        Self {
            config: deps.config,
            capture: deps.capture,
            sequence_store: deps.sequence_store,
            credentials,
            on_changed,
            gate: Mutex::new(gate),
            preview: Mutex::new(None),
            telemetry,
            coordinator,
        }
    }

    /// Resume publishing after a restart when consent was already given:
    /// starts the coordinator iff `connection != None && liveDeskEnabled`.
    pub fn start(&self) {
        if let Some(connection) = self.config.get().connection {
            if connection.live_desk_enabled {
                self.coordinator.start();
            }
        }
    }

    /// `NotPaired` when `config.connection` is `None`, else the coordinator
    /// state (re-read config on every call; the override lives at the
    /// facade — after unpair the coordinator still reports `Disabled`
    /// internally).
    pub fn runtime_state(&self) -> RuntimeState {
        if self.config.get().connection.is_none() {
            return RuntimeState::NotPaired;
        }
        self.coordinator.current_state()
    }

    /// The recorded preview, if any.
    pub fn current_preview(&self) -> Option<Preview> {
        self.preview.lock().clone()
    }

    pub fn publish_telemetry(&self) -> PublishTelemetry {
        self.telemetry.lock().clone()
    }

    /// Called after ANY privacy-relevant configuration change. Compares
    /// policy fingerprints by value; on change: invalidate consent-preview
    /// binding and republish immediately under the new policy if already
    /// enabled (no clear, no consent revocation).
    pub fn policy_maybe_changed(&self) {
        let fingerprint = self.capture.fingerprint();
        {
            let mut gate = self.gate.lock();
            if fingerprint == gate.fingerprint() {
                return;
            }
            gate.policy_did_change(fingerprint);
        }
        *self.preview.lock() = None;
        self.coordinator.request_fresh_snapshot();
        (self.on_changed)();
    }

    /// Captures a fresh sanitized preview and records it as the consent
    /// basis.
    pub async fn refresh_preview(&self) -> Result<Preview, ServiceError> {
        let config = self.config.get();
        let snapshot = self
            .capture
            .capture_for_delivery(CaptureOptions {
                include_media: config.privacy.sources.media,
            })
            .await;
        let projection = projection_of(&snapshot);
        let preview = {
            let mut gate = self.gate.lock();
            gate.record(projection.clone());
            Preview {
                projection,
                policy_fingerprint: gate.fingerprint().to_string(),
                observed_at: snapshot.observed_at,
            }
        };
        *self.preview.lock() = Some(preview.clone());
        (self.on_changed)();
        Ok(preview)
    }

    /// §5.2: claim + store token + persist connection
    /// (`liveDeskEnabled: false`) + refresh preview. Replaces any existing
    /// pairing (bounded clear with reason `ConnectionRemoved`, authority
    /// discarded). Error mapping per §5.4.
    pub async fn pair(
        &self,
        base_url: &str,
        device_name: &str,
        pairing_code: &str,
    ) -> Result<(), ServiceError> {
        let credential_store = match (self.credentials)().await {
            Ok(store) => store,
            Err(_) => return Err(ServiceError::new(IpcErrorCode::CredentialStoreUnavailable)),
        };

        let ensure = {
            let credentials = self.credentials.clone();
            async move {
                (credentials)()
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
        };
        let result = match claim_pairing(base_url, device_name, pairing_code, ensure).await {
            Ok(result) => result,
            Err(error) => return Err(map_pairing_error(error)),
        };

        // Replacing an existing pairing: stop the old lifecycle first
        // (bounded final clear if an old client existed and the coordinator
        // was not suspended).
        self.coordinator
            .shutdown(ClearReason::ConnectionRemoved)
            .await;
        self.coordinator.discard_authority();

        // Token into protected storage BEFORE non-secret metadata is
        // committed. Failures surface as generic `internal` (ambiguity 11).
        credential_store
            .set(&result.device_id, &result.device_token)
            .await
            .map_err(|_| ServiceError::new(IpcErrorCode::Internal))?;
        let backend = credential_store.backend();
        self.config
            .update(|config| {
                config.credential_backend = Some(backend);
                config.connection = Some(StoredConnection {
                    base_url: result.base_url.clone(),
                    device_id: result.device_id.clone(),
                    device_name: result.device_name.clone(),
                    scopes: result.scopes.clone(),
                    pairing_next_sequence: result.next_sequence,
                    live_desk_enabled: false, // pairing NEVER enables publishing
                });
            })
            .map_err(|_| ServiceError::new(IpcErrorCode::Internal))?;
        logger::info("service", "paired (live desk disabled)");
        self.refresh_preview().await?;
        Ok(())
    }

    /// Consent boundary (§6.4). `ui_fingerprint` is the policy fingerprint
    /// the user was looking at when they clicked — a stale click can never
    /// enable publishing.
    pub async fn confirm_consent(&self, ui_fingerprint: &str) -> Result<(), ServiceError> {
        if self.config.get().connection.is_none() {
            return Err(ServiceError::new(IpcErrorCode::NotPaired));
        }
        let preview = self.preview.lock().clone();
        let gate_fingerprint = self.gate.lock().fingerprint().to_string();
        let preview = match preview {
            Some(preview) if ui_fingerprint == gate_fingerprint => preview,
            _ => {
                // Re-record a fresh basis for the UI, then reject.
                self.refresh_preview().await?;
                return Err(ServiceError::new(IpcErrorCode::PreviewOutOfDate));
            }
        };
        let candidate = Confirmation {
            policy_fingerprint: ui_fingerprint.to_string(),
            projection: preview.projection.clone(),
        };

        // Validation 1: fresh capture still matches what the user confirmed.
        let before = self
            .capture
            .capture_for_delivery(CaptureOptions {
                include_media: self.config.get().privacy.sources.media,
            })
            .await;
        if !self
            .gate
            .lock()
            .validates(&candidate, &projection_of(&before))
        {
            self.refresh_preview().await?;
            return Err(ServiceError::new(IpcErrorCode::PreviewOutOfDate));
        }

        // Persist the enable.
        self.config
            .update(|config| {
                if let Some(connection) = config.connection.as_mut() {
                    connection.live_desk_enabled = true;
                }
            })
            .map_err(|_| ServiceError::new(IpcErrorCode::Internal))?;

        // Validation 2 (post-persist): anything drifted during the awaits
        // above rolls the persisted state back — a stale write can never
        // start publishing.
        let after = self
            .capture
            .capture_for_delivery(CaptureOptions {
                include_media: self.config.get().privacy.sources.media,
            })
            .await;
        let fingerprint_now = self.capture.fingerprint();
        if fingerprint_now != ui_fingerprint
            || !self
                .gate
                .lock()
                .validates(&candidate, &projection_of(&after))
        {
            self.config
                .update(|config| {
                    if let Some(connection) = config.connection.as_mut() {
                        connection.live_desk_enabled = false;
                    }
                })
                .map_err(|_| ServiceError::new(IpcErrorCode::Internal))?;
            self.refresh_preview().await?;
            return Err(ServiceError::new(IpcErrorCode::PreviewOutOfDate));
        }

        logger::info("service", "live desk enabled by explicit consent");
        self.coordinator.start();
        (self.on_changed)();
        Ok(())
    }

    /// Persist-first disable: a crash mid-way must never resume publishing
    /// (the flag is written before the network clear).
    pub async fn disable_live_desk(&self) -> Result<(), ServiceError> {
        self.config
            .update(|config| {
                if let Some(connection) = config.connection.as_mut() {
                    connection.live_desk_enabled = false;
                }
            })
            .map_err(|_| ServiceError::new(IpcErrorCode::Internal))?;
        self.coordinator.shutdown(ClearReason::Paused).await;
        (self.on_changed)();
        Ok(())
    }

    /// §5.3: idempotent when unpaired; durable disable first; bounded clear
    /// (`ConnectionRemoved`); discard authority; best-effort credential
    /// delete (warn + continue); sequence remove (failure propagates as
    /// `Internal`, leaving the connection present but disabled — preserved
    /// error-tolerance asymmetry, ambiguity 9); `connection = None`; gate
    /// cleared; preview dropped.
    pub async fn unpair(&self) -> Result<(), ServiceError> {
        let connection = match self.config.get().connection {
            None => return Ok(()),
            Some(connection) => connection,
        };
        // Disable durably first, then final clear with the in-memory
        // credential.
        self.config
            .update(|config| {
                if let Some(connection) = config.connection.as_mut() {
                    connection.live_desk_enabled = false;
                }
            })
            .map_err(|_| ServiceError::new(IpcErrorCode::Internal))?;
        self.coordinator
            .shutdown(ClearReason::ConnectionRemoved)
            .await;
        self.coordinator.discard_authority();
        let deleted = match (self.credentials)().await {
            Ok(store) => store.delete(&connection.device_id).await.is_ok(),
            Err(_) => false,
        };
        if !deleted {
            logger::warn("service", "credential deletion failed during unpair");
        }
        self.sequence_store
            .remove(&connection.device_id)
            .await
            .map_err(|_| ServiceError::new(IpcErrorCode::Internal))?;
        self.config
            .update(|config| config.connection = None)
            .map_err(|_| ServiceError::new(IpcErrorCode::Internal))?;
        self.gate.lock().clear();
        *self.preview.lock() = None;
        logger::info("service", "unpaired; credentials and sequence removed");
        (self.on_changed)();
        Ok(())
    }

    /// Delegates to the coordinator (T16).
    pub fn handle_sleep_or_lock(&self) {
        self.coordinator.handle_sleep_or_lock();
    }

    /// Delegates to the coordinator (T17).
    pub fn handle_wake_or_unlock(&self) {
        self.coordinator.handle_wake_or_unlock();
    }

    /// `coordinator.shutdown(ClearReason::Shutdown)` — bounded 800 ms final
    /// clear; the app crate additionally bounds the whole exit at 2 s.
    pub async fn shutdown(&self) {
        self.coordinator.shutdown(ClearReason::Shutdown).await;
    }

    /// Dead code in the TS wiring (nothing calls it; `last_error` stays
    /// `None` in production) — ported as-is for parity (ambiguity 3).
    pub fn note_error(&self, code: &str) {
        self.telemetry.lock().last_error = Some(code.to_string());
        (self.on_changed)();
    }

    /// The owned coordinator (main wiring calls
    /// `service.coordinator().request_fresh_snapshot()` / `.start()`).
    pub fn coordinator(&self) -> &LiveDeskCoordinator {
        &self.coordinator
    }
}

/// §5.4 `mapPairingError`, including the catch-all rows: any
/// non-`PairingError` throw (`ClaimPairingError::Other`) maps to
/// `pairingFailed`.
fn map_pairing_error(error: ClaimPairingError) -> ServiceError {
    let code = match error {
        ClaimPairingError::Pairing(pairing) => match pairing.code {
            PairingErrorCode::ClientUpdateRequired => IpcErrorCode::ClientUpdateRequired,
            PairingErrorCode::ServerFeatureUnavailable | PairingErrorCode::InvalidCapabilities => {
                IpcErrorCode::ServerFeatureUnavailable
            }
            PairingErrorCode::RequiredScopeMissing => IpcErrorCode::RequiredScopeMissing,
            PairingErrorCode::InvalidPairingCode
            | PairingErrorCode::InvalidDeviceName
            | PairingErrorCode::InvalidServerUrl => IpcErrorCode::InvalidInput,
            PairingErrorCode::PairingRejected => match pairing.server_code.as_deref() {
                Some("COMPANION_PAIRING_EXPIRED") => IpcErrorCode::PairingExpired,
                Some("RATE_LIMITED") => IpcErrorCode::RateLimited,
                Some("VALIDATION_FAILED") => IpcErrorCode::ValidationFailed,
                _ => IpcErrorCode::PairingFailed,
            },
            PairingErrorCode::Network => IpcErrorCode::Network,
        },
        ClaimPairingError::Other(_) => IpcErrorCode::PairingFailed,
    };
    ServiceError::new(code)
}
