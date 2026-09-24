//! Core wiring for the Tauri process: constructs the native `yohaku-core`
//! service graph (port of `packages/core/src/main.ts`), owns the pieces the
//! commands need, and assembles/broadcasts the UI snapshot.
//!
//! Privacy invariant: everything emitted from here is built exclusively from
//! sanitized/service-level values (`CoreStateSnapshot`); raw capture types
//! never reach this module's outputs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{Emitter, Manager};
use yohaku_core::capture::{ForegroundSource, ForegroundWatcher, MediaProvider, SystemEvents};
use yohaku_core::companion::coordinator::CoordinatorTimings;
use yohaku_core::companion::service::{CompanionService, CompanionServiceDeps, CredentialsFn};
use yohaku_core::model::{
    ConnectionSummary, CoreStateSnapshot, MediaProviderHealth, MediaProviderKind,
};
use yohaku_core::privacy::capture_service::CaptureService;
use yohaku_core::runtime::logger;
use yohaku_core::runtime::suspend_detector::SuspendDetector;
use yohaku_core::store::config::ConfigStore;
use yohaku_core::store::credentials::{
    select_credential_store, CredentialStore, CredentialStoreError,
};
use yohaku_core::store::sequence::FileSequenceStore;

/// Reported in the snapshot; tracks the crate version (main.ts APP_VERSION).
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Bounded shutdown budget (main.ts gracefulExit 2000 ms race).
const SHUTDOWN_BUDGET: Duration = Duration::from_millis(2_000);

/// recentAppIds MRU capacity (main.ts).
const RECENT_APP_IDS_MAX: usize = 10;

pub struct AppCoreState {
    pub config: Arc<ConfigStore>,
    pub service: Arc<CompanionService>,
    foreground: Arc<ForegroundWatcher>,
    media_provider: Arc<Mutex<Option<Arc<dyn MediaProvider>>>>,
    system_events: Arc<SystemEvents>,
    suspend_detector: Arc<SuspendDetector>,
    recent_app_ids: Arc<Mutex<Vec<String>>>,
    exiting: AtomicBool,
}

impl AppCoreState {
    /// Full non-sensitive snapshot (main.ts `getSnapshot`).
    pub fn snapshot(&self) -> CoreStateSnapshot {
        let config = self.config.get();
        let telemetry = self.service.publish_telemetry();
        let media_provider = self.media_provider.lock().unwrap().clone();
        CoreStateSnapshot {
            version: APP_VERSION.to_string(),
            runtime_state: self.service.runtime_state(),
            connection: config.connection.map(|connection| ConnectionSummary {
                base_url: connection.base_url,
                device_id: connection.device_id,
                device_name: connection.device_name,
                scopes: connection.scopes,
                live_desk_enabled: connection.live_desk_enabled,
            }),
            privacy: config.privacy,
            preview: self.service.current_preview(),
            media_provider: MediaProviderHealth {
                kind: if media_provider.is_some() {
                    MediaProviderKind::Winrt
                } else {
                    MediaProviderKind::None
                },
                healthy: media_provider.map(|p| p.healthy()).unwrap_or(false),
                detail: None,
            },
            recent_app_ids: self.recent_app_ids.lock().unwrap().clone(),
            last_publish_at: telemetry.last_publish_at,
            last_error: telemetry.last_error,
        }
    }

    /// Bounded graceful shutdown (main.ts `gracefulExit`): detector and
    /// watcher stop synchronously, then the remote clear + async teardown
    /// race a 2 s budget. Idempotent.
    pub fn shutdown_blocking(&self) {
        if self.exiting.swap(true, Ordering::SeqCst) {
            return;
        }
        logger::info("main", "shutting down");
        self.suspend_detector.stop();
        self.foreground.stop();
        let service = self.service.clone();
        let system_events = self.system_events.clone();
        let media_provider = self.media_provider.lock().unwrap().clone();
        tauri::async_runtime::block_on(async move {
            let _ = tokio::time::timeout(SHUTDOWN_BUDGET, async move {
                service.shutdown().await;
                system_events.stop().await;
                if let Some(provider) = media_provider {
                    provider.stop().await;
                }
            })
            .await;
        });
    }
}

/// Emit the current snapshot to the main window (`ipc.broadcastState()`).
/// Safe to call before the state is managed (no-op).
pub fn broadcast_state(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<AppCoreState>() else {
        return;
    };
    let _ = app.emit_to(
        tauri::EventTarget::labeled("main"),
        "core-state",
        state.snapshot(),
    );
}

/// Memoized credential-store selection (main.ts `credentials`): one
/// selection per process lifetime — including a failed one; on success the
/// pinned backend is persisted into the config when it drifted.
fn credentials_fn(config: Arc<ConfigStore>) -> Box<CredentialsFn> {
    let cell: Arc<tokio::sync::OnceCell<Option<Arc<dyn CredentialStore>>>> =
        Arc::new(tokio::sync::OnceCell::new());
    Box::new(move || {
        let cell = cell.clone();
        let config = config.clone();
        Box::pin(async move {
            let selected = cell
                .get_or_init(|| async move {
                    match select_credential_store(config.get().credential_backend).await {
                        Ok(store) => {
                            let store: Arc<dyn CredentialStore> = Arc::from(store);
                            let backend = store.backend();
                            if config.get().credential_backend != Some(backend) {
                                // Pin failure is non-fatal: the store still
                                // works this session; log and continue.
                                if config
                                    .update(|c| c.credential_backend = Some(backend))
                                    .is_err()
                                {
                                    logger::warn(
                                        "credentials",
                                        "failed to persist credential backend pin",
                                    );
                                }
                            }
                            Some(store)
                        }
                        // select_credential_store only fails as Unavailable.
                        Err(_) => None,
                    }
                })
                .await;
            selected.clone().ok_or(CredentialStoreError::Unavailable)
        })
    })
}

/// Build and start the core (port of main.ts `main()`), then manage the
/// resulting [`AppCoreState`]. Called once from `setup`; blocks on the async
/// pieces (media provider selection, system events start) exactly where the
/// TS main awaited them.
pub fn init(app: &tauri::AppHandle) -> Result<(), String> {
    let config = Arc::new(ConfigStore::new(None).map_err(|e| e.to_string())?);
    let sequence_store = Arc::new(FileSequenceStore::new(None).map_err(|e| e.to_string())?);

    let foreground = Arc::new(ForegroundWatcher::new());
    foreground.start();

    let media_provider: Arc<Mutex<Option<Arc<dyn MediaProvider>>>> = Arc::new(Mutex::new(
        match tauri::async_runtime::block_on(yohaku_core::capture::select_media_provider(
            config.get().media.provider,
        )) {
            Ok(provider) => Some(provider),
            Err(_) => {
                logger::error(
                    "main",
                    "no media provider available; media capture disabled",
                );
                None
            }
        },
    ));

    let capture = Arc::new(CaptureService::new(
        foreground.clone() as Arc<dyn ForegroundSource>,
        Box::new({
            let media_provider = media_provider.clone();
            move || media_provider.lock().unwrap().clone()
        }),
        Box::new({
            let config = config.clone();
            move || config.get().privacy
        }),
    ));

    let service = Arc::new(CompanionService::new(CompanionServiceDeps {
        config: config.clone(),
        capture,
        sequence_store,
        credentials: credentials_fn(config.clone()),
        on_changed: Box::new({
            let app = app.clone();
            move || broadcast_state(&app)
        }),
        coordinator_timings: CoordinatorTimings::default(),
    }));

    let recent_app_ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let system_events = Arc::new(SystemEvents::new());
    let suspend_detector = Arc::new(SuspendDetector::new(Box::new({
        // Missed a suspend: the lease already expired remotely; renegotiate.
        let service = service.clone();
        move || {
            let state = service.runtime_state();
            if matches!(
                state,
                yohaku_core::model::RuntimeState::Active
                    | yohaku_core::model::RuntimeState::Degraded
            ) {
                service.coordinator().start();
            }
        }
    })));

    app.manage(AppCoreState {
        config,
        service: service.clone(),
        foreground: foreground.clone(),
        media_provider: media_provider.clone(),
        system_events: system_events.clone(),
        suspend_detector: suspend_detector.clone(),
        recent_app_ids: recent_app_ids.clone(),
        exiting: AtomicBool::new(false),
    });

    // --- capture triggers (registered post-manage so broadcasts resolve) ---
    foreground
        .on_change(Box::new({
            let app = app.clone();
            let service = service.clone();
            move |info| {
                if let Some(info) = info {
                    let mut recent = recent_app_ids.lock().unwrap();
                    recent.retain(|id| *id != info.app_id);
                    recent.insert(0, info.app_id);
                    recent.truncate(RECENT_APP_IDS_MAX);
                }
                service.coordinator().request_fresh_snapshot();
                broadcast_state(&app);
            }
        }))
        .detach();
    if let Some(provider) = media_provider.lock().unwrap().as_ref() {
        provider
            .on_semantic_change(Box::new({
                let service = service.clone();
                move || service.coordinator().request_fresh_snapshot()
            }))
            .detach();
    }

    // --- system events -----------------------------------------------------
    system_events
        .on_lock_or_sleep(Box::new({
            let service = service.clone();
            move || service.handle_sleep_or_lock()
        }))
        .detach();
    system_events
        .on_unlock_or_resume(Box::new({
            let service = service.clone();
            move || service.handle_wake_or_unlock()
        }))
        .detach();
    // Unavailable is non-fatal (already logged); the SuspendDetector remains
    // as the safety net.
    let _ = tauri::async_runtime::block_on(system_events.start());
    suspend_detector.start();

    service.start();
    Ok(())
}
