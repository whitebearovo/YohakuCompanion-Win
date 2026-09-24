//! Live Desk coordinator: generation-based connect/publish state machine.
//!
//! Owned by agent K. Ground truth:
//! `packages/core/src/companion/coordinator.ts` and
//! `.claude/rewrite/specs/companion-service.md` §§2-4 (transition table,
//! refresh loop, suspend/resume, shutdown).
//!
//! - A monotonic generation invalidates all in-flight async work on any
//!   state transition (privacy change, sleep, shutdown, reconnect). The TS
//!   code re-checked `gen !== this.generation` after EVERY await; the Rust
//!   port re-checks inside the first locked segment after every `.await`.
//! - All triggers (foreground change, media semantic change, heartbeat,
//!   recovery) coalesce into one single-flight refresh loop; every publish
//!   is a FRESH capture — snapshots are never replayed.
//! - Concurrency model: the TS fields were mutated lock-free between awaits
//!   (single-threaded JS). Here every synchronous segment between awaits
//!   runs under ONE `parking_lot::Mutex<Inner>` hold (never held across an
//!   await); state-change callbacks are collected and fired AFTER the lock
//!   is released because they re-enter the coordinator via snapshot
//!   assembly.
//! - Sleep/lock: new generation (`Suspended`) + bounded best-effort clear;
//!   wake JOINS the bounded race before restarting negotiation. The spawned
//!   clear task is NEVER aborted (TS `Promise.race` does not cancel the
//!   losing request — it may still reconcile its acceptedSequence).
//! - Schema/feature rejection discards the authority and renegotiates;
//!   network failures degrade with a reconnect timer (no path monitor on
//!   Windows — failure-driven recovery plus heartbeat).
//! - `begin_new_generation` clears timers, resets `refresh_requested`,
//!   calls `capture.reset_media_continuity()`, but does NOT null the client
//!   (observable stale-client window, spec trap 11 / ambiguity 7 —
//!   preserved). `last_send_started_at` throttling spans generations.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::companion::authority::{AuthorityRegistry, PublishAuthority};
use crate::companion::http_client::{
    CompanionHttpClient, CompanionServerConfiguration, ExecuteOptions, HttpMethod,
};
use crate::companion::presence_client::PresenceClient;
use crate::model::{BoxFuture, ClearReason, CompanionCredential, RuntimeState, StoredConnection};
use crate::privacy::capture_service::{CaptureOptions, CaptureService};
use crate::protocol::capabilities::{negotiate_presence, PresenceNegotiation};
use crate::protocol::sequencer::{CompanionSequencer, SequenceBacking};
use crate::protocol::types::decode_capabilities_response;
use crate::protocol::wire::PROTOCOL_CLIENT_VERSION;
use crate::runtime::logger;
use crate::store::sequence::FileSequenceStore;

/// Retry/clear timing knobs. MUST stay injectable (the ported integration
/// tests construct the service with `network_retry_ms: 100,
/// feature_retry_ms: 200`); TS `Partial<CoordinatorTimings>` maps to Rust
/// struct-update syntax over `Default::default()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoordinatorTimings {
    pub network_retry_ms: u64,
    pub feature_retry_ms: u64,
    pub sleep_clear_timeout_ms: u64,
    pub shutdown_clear_timeout_ms: u64,
}

impl Default for CoordinatorTimings {
    fn default() -> Self {
        Self {
            network_retry_ms: 30_000,
            feature_retry_ms: 300_000,
            sleep_clear_timeout_ms: 500,
            shutdown_clear_timeout_ms: 800,
        }
    }
}

/// Coordinator dependencies (TS constructor deps object).
/// `get_token` failure and `None` are treated identically (-> degraded,
/// transition T3), so the closure folds errors into `None`.
pub struct CoordinatorDeps {
    pub capture: Arc<CaptureService>,
    pub sequence_store: Arc<FileSequenceStore>,
    pub get_connection: Box<dyn Fn() -> Option<StoredConnection> + Send + Sync>,
    pub get_token: Box<dyn Fn(String) -> BoxFuture<Option<String>> + Send + Sync>,
    pub on_state_change: Box<dyn Fn(RuntimeState) + Send + Sync>,
    /// Fired on every successful `replace_presence` (NOT on clears).
    pub on_published: Box<dyn Fn() + Send + Sync>,
}

/// Wall-clock epoch milliseconds (TS `Date.now()`).
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Mutable coordinator state; the TS instance fields. Guarded by one mutex
/// that is NEVER held across an await.
struct Inner {
    generation: u64,
    state: RuntimeState,
    stopping: bool,
    refresh_requested: bool,
    refresh_loop_running: bool,
    heartbeat_task: Option<JoinHandle<()>>,
    reconnect_task: Option<JoinHandle<()>>,
    /// The bounded best-effort sleep clear (the TS race promise). Take
    /// semantics: consumed by wake or shutdown, whichever comes first.
    cleanup_task: Option<JoinHandle<()>>,
    /// Epoch ms of the last SEND START. Deliberately NOT reset by
    /// `begin_new_generation` — the min-send interval spans generations
    /// (e.g. wake publish vs pre-sleep publish).
    last_send_started_at: i64,
    client: Option<Arc<PresenceClient>>,
    include_media: bool,
    requested_lease_seconds: f64,
    min_send_interval_ms: u64,
    registry: AuthorityRegistry,
}

struct Shared {
    deps: CoordinatorDeps,
    timings: CoordinatorTimings,
    inner: Mutex<Inner>,
}

impl Shared {
    /// Fire a deferred state-change notification (always after the inner
    /// lock has been released — the callback re-enters via `current_state`).
    fn fire_state_change(&self, change: Option<RuntimeState>) {
        if let Some(state) = change {
            (self.deps.on_state_change)(state);
        }
    }
}

/// The coordinator. Internal state starts as `Disabled` and only ever holds
/// the seven non-`NotPaired` values (`NotPaired` is synthesized by the
/// service facade from `connection == None`).
pub struct LiveDeskCoordinator {
    shared: Arc<Shared>,
}

impl LiveDeskCoordinator {
    pub fn new(deps: CoordinatorDeps, timings: CoordinatorTimings) -> Self {
        Self {
            shared: Arc::new(Shared {
                deps,
                timings,
                inner: Mutex::new(Inner {
                    generation: 0,
                    state: RuntimeState::Disabled,
                    stopping: false,
                    refresh_requested: false,
                    refresh_loop_running: false,
                    heartbeat_task: None,
                    reconnect_task: None,
                    cleanup_task: None,
                    last_send_started_at: 0,
                    client: None,
                    // Pre-negotiation defaults; only relevant before the
                    // first successful configure (client is None until then).
                    include_media: false,
                    requested_lease_seconds: 90.0,
                    min_send_interval_ms: 2000,
                    registry: AuthorityRegistry::new(),
                }),
            }),
        }
    }

    /// T1: (re)start negotiation and publishing under a fresh generation.
    /// No-op while a shutdown is in flight.
    pub fn start(&self) {
        let change = {
            let mut guard = self.shared.inner.lock();
            start_locked(&self.shared, &mut guard)
        };
        self.shared.fire_state_change(change);
    }

    /// Current coordinator state (never `NotPaired`).
    pub fn current_state(&self) -> RuntimeState {
        self.shared.inner.lock().state
    }

    /// T10: external trigger (foreground/media changed). Accepted only in
    /// `Active`/`Degraded`, silently ignored in every other state.
    pub fn request_fresh_snapshot(&self) {
        let mut guard = self.shared.inner.lock();
        if guard.state != RuntimeState::Active && guard.state != RuntimeState::Degraded {
            return;
        }
        let gen = guard.generation;
        trigger_refresh_locked(&self.shared, &mut guard, gen);
    }

    /// T16: new generation in `Suspended`; bounded (sleep_clear_timeout_ms)
    /// best-effort clear with reason `Sleep` through the pre-generation
    /// client; the clear task itself is never cancelled.
    pub fn handle_sleep_or_lock(&self) {
        let change = {
            let mut guard = self.shared.inner.lock();
            let inner = &mut *guard;
            if inner.state == RuntimeState::Suspended || inner.stopping {
                return;
            }
            let client = inner.client.clone();
            let change = begin_new_generation_locked(&self.shared, inner, RuntimeState::Suspended);
            if let Some(client) = client {
                inner.cleanup_task = Some(spawn_clear_best_effort(
                    client,
                    ClearReason::Sleep,
                    self.shared.timings.sleep_clear_timeout_ms,
                ));
            }
            change
        };
        self.shared.fire_state_change(change);
        logger::info("coordinator", "suspended (lock/sleep)");
    }

    /// T17: only from `Suspended`. Join the bounded clear race, then
    /// `start()` (full renegotiation) if still suspended and not stopping.
    pub fn handle_wake_or_unlock(&self) {
        let pending = {
            let mut guard = self.shared.inner.lock();
            if guard.state != RuntimeState::Suspended {
                return;
            }
            guard.cleanup_task.take()
        };
        let shared = self.shared.clone();
        tokio::spawn(async move {
            // Join the in-flight (bounded) clear before restarting so the
            // wake snapshot can never be reordered ahead of it — ordering is
            // then guaranteed by the shared sequencer's serialized reserve,
            // NOT by HTTP send order (a new PresenceClient has a fresh FIFO
            // slot).
            if let Some(pending) = pending {
                let _ = pending.await;
            }
            let change = {
                let mut guard = shared.inner.lock();
                if guard.state != RuntimeState::Suspended || guard.stopping {
                    return;
                }
                start_locked(&shared, &mut guard)
            };
            shared.fire_state_change(change);
        });
    }

    /// T18: bounded final clear + stop. Idempotent while in flight (a second
    /// call returns immediately WITHOUT waiting for the first); `stopping`
    /// resets at the end, so the coordinator is restartable afterwards
    /// (pairing replacement relies on this). Skips the final clear when the
    /// coordinator was suspended (the sleep clear already ran/is running and
    /// is joined via `cleanup_task`). Registry untouched — callers that need
    /// a fresh sequencer call [`Self::discard_authority`] explicitly.
    pub async fn shutdown(&self, reason: ClearReason) {
        let (client, was_suspended, pending, change) = {
            let mut guard = self.shared.inner.lock();
            let inner = &mut *guard;
            if inner.stopping {
                return;
            }
            inner.stopping = true;
            let client = inner.client.clone();
            let was_suspended = inner.state == RuntimeState::Suspended;
            let change = begin_new_generation_locked(&self.shared, inner, RuntimeState::Disabled);
            let pending = inner.cleanup_task.take();
            (client, was_suspended, pending, change)
        };
        self.shared.fire_state_change(change);
        if let Some(pending) = pending {
            let _ = pending.await;
        }
        if !was_suspended {
            if let Some(client) = client {
                // Awaiting the bounded race == TS `await clearBestEffort(...)`.
                let _ = spawn_clear_best_effort(
                    client,
                    reason,
                    self.shared.timings.shutdown_clear_timeout_ms,
                )
                .await;
            }
        }
        let mut guard = self.shared.inner.lock();
        guard.client = None;
        guard.stopping = false;
    }

    /// Capability rejection or unpair: the ordered writer must be rebuilt.
    /// `registry.discard(); client = None;` — called on publish
    /// renegotiation signal, unpair, and pairing replacement only (NOT on
    /// sleep/wake or plain degraded retries — the sequencer must survive
    /// those).
    pub fn discard_authority(&self) {
        let mut guard = self.shared.inner.lock();
        guard.registry.discard();
        guard.client = None;
    }
}

// ---------------------------------------------------------------------------
// Locked helpers. Every function suffixed `_locked` must be called while
// holding the inner lock; each returns any deferred state-change
// notification for the caller to fire AFTER unlocking.
// ---------------------------------------------------------------------------

/// `setState`: no-op when unchanged, else store and defer the notification.
fn set_state_locked(inner: &mut Inner, state: RuntimeState) -> Option<RuntimeState> {
    if inner.state == state {
        return None;
    }
    inner.state = state;
    Some(state)
}

/// `beginNewGeneration(S)`: bump the generation; clear heartbeat + reconnect
/// timers; reset `refresh_requested`; reset media continuity (session ids
/// must not survive a generation boundary); set the state. Deliberately does
/// NOT touch `client` or `last_send_started_at`.
fn begin_new_generation_locked(
    shared: &Shared,
    inner: &mut Inner,
    state: RuntimeState,
) -> Option<RuntimeState> {
    inner.generation += 1;
    if let Some(task) = inner.heartbeat_task.take() {
        task.abort();
    }
    if let Some(task) = inner.reconnect_task.take() {
        task.abort();
    }
    inner.refresh_requested = false;
    shared.deps.capture.reset_media_continuity();
    set_state_locked(inner, state)
}

/// `start()` body (T1): guarded by `stopping`; new generation in
/// `Connecting`; spawn `configure` fire-and-forget.
fn start_locked(shared: &Arc<Shared>, inner: &mut Inner) -> Option<RuntimeState> {
    if inner.stopping {
        return None;
    }
    let change = begin_new_generation_locked(shared, inner, RuntimeState::Connecting);
    let gen = inner.generation;
    let task_shared = shared.clone();
    tokio::spawn(async move {
        configure(task_shared, gen).await;
    });
    change
}

/// `scheduleRetry(gen, delayMs)`: replaces any existing reconnect timer; the
/// callback is a no-op when the generation moved or `stopping` is true.
fn schedule_retry_locked(shared: &Arc<Shared>, inner: &mut Inner, gen: u64, delay_ms: u64) {
    if let Some(task) = inner.reconnect_task.take() {
        task.abort();
    }
    let task_shared = shared.clone();
    inner.reconnect_task = Some(tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        // Belt-and-braces gen/stopping check (the task is also aborted by
        // begin_new_generation) — T15.
        let change = {
            let mut guard = task_shared.inner.lock();
            if gen != guard.generation || guard.stopping {
                return;
            }
            start_locked(&task_shared, &mut guard)
        };
        task_shared.fire_state_change(change);
    }));
}

/// Trigger discipline shared by T9/T10/T11: set `refresh_requested` and
/// ensure exactly one refresh loop runs. The single-flight check-and-set is
/// atomic under the inner lock (the TS check happened synchronously before
/// the first await — trap 3).
fn trigger_refresh_locked(shared: &Arc<Shared>, inner: &mut Inner, gen: u64) {
    inner.refresh_requested = true;
    if !inner.refresh_loop_running {
        inner.refresh_loop_running = true;
        let task_shared = shared.clone();
        tokio::spawn(async move {
            run_refresh_loop(task_shared, gen).await;
        });
    }
}

/// Heartbeat interval (unref'd `setInterval` in TS): the lease-renewal
/// cadence — every tick performs a full fresh publish. The period may be
/// fractional seconds (requested lease / 3); it is NOT cleared on degraded
/// (T14), only by `begin_new_generation`.
fn spawn_heartbeat_locked(
    shared: &Arc<Shared>,
    inner: &mut Inner,
    gen: u64,
    heartbeat_seconds: f64,
) {
    if let Some(task) = inner.heartbeat_task.take() {
        task.abort();
    }
    let period = Duration::from_secs_f64(heartbeat_seconds);
    let task_shared = shared.clone();
    inner.heartbeat_task = Some(tokio::spawn(async move {
        let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            // T11: gen check only (NO state check) — publishes keep being
            // attempted at heartbeat cadence while degraded.
            let mut guard = task_shared.inner.lock();
            if gen != guard.generation {
                return;
            }
            trigger_refresh_locked(&task_shared, &mut guard, gen);
        }
    }));
}

/// TS `clearBestEffort` (`Promise.race`): the underlying clear runs on its
/// OWN detached task that is NEVER aborted — the losing request keeps
/// running and may still reconcile its acceptedSequence into the shared
/// sequencer after the bound (spec trap 5). The returned handle is the
/// bounded JOIN (race of join vs timeout); all failures are swallowed — the
/// server lease expiry is the correctness backstop.
fn spawn_clear_best_effort(
    client: Arc<PresenceClient>,
    reason: ClearReason,
    timeout_ms: u64,
) -> JoinHandle<()> {
    let observed_at = now_ms();
    let clear_task = tokio::spawn(async move {
        let _ = client.clear_presence(reason, observed_at).await;
    });
    tokio::spawn(async move {
        // On timeout the JoinHandle future is dropped, which does NOT abort
        // the spawned clear.
        let _ = tokio::time::timeout(Duration::from_millis(timeout_ms), clear_task).await;
    })
}

// ---------------------------------------------------------------------------
// configure: one negotiation attempt for generation `gen` (T2..T9).
// ---------------------------------------------------------------------------

async fn configure(shared: Arc<Shared>, gen: u64) {
    // In TS this body ran synchronously inside start() up to the first
    // await, so it could never observe a moved generation before T2. The
    // spawned Rust task can, hence the entry check.
    {
        let guard = shared.inner.lock();
        if gen != guard.generation {
            return;
        }
    }

    // T2: pairing gate — refuse unless paired AND Live Desk enabled.
    let connection = match (shared.deps.get_connection)() {
        Some(connection) if connection.live_desk_enabled => connection,
        _ => {
            let change = {
                let mut guard = shared.inner.lock();
                if gen != guard.generation {
                    return;
                }
                set_state_locked(&mut guard, RuntimeState::Disabled)
            };
            shared.fire_state_change(change);
            return; // nothing else — no timers
        }
    };

    // T3: token fetch. The deps closure folds store errors into None.
    let token = (shared.deps.get_token)(connection.device_id.clone()).await;

    // Post-await sync segment (token check + base URL build) in one hold.
    let http = {
        let mut guard = shared.inner.lock();
        if gen != guard.generation {
            return;
        }
        if token.is_none() {
            let change = set_state_locked(&mut guard, RuntimeState::Degraded);
            schedule_retry_locked(&shared, &mut guard, gen, shared.timings.network_retry_ms);
            drop(guard);
            shared.fire_state_change(change);
            return;
        }
        match CompanionServerConfiguration::new(&connection.base_url) {
            Ok(configuration) => Arc::new(CompanionHttpClient::new(configuration)),
            Err(_) => {
                // T4: invalid stored base URL — degraded with NO retry timer
                // (dead end until an external start(); preserved as-is, spec
                // ambiguity 6).
                let change = set_state_locked(&mut guard, RuntimeState::Degraded);
                drop(guard);
                shared.fire_state_change(change);
                return;
            }
        }
    };
    let token = token.expect("token checked above");

    // T5: unauthenticated capabilities GET (no Bearer, no version header).
    let capabilities = match http
        .execute(
            ExecuteOptions {
                method: HttpMethod::Get,
                path: "/companion/capabilities",
                credential: None,
                expected_request_id: None,
                maximum_payload_bytes: None,
                encoded_body: None,
            },
            decode_capabilities_response,
        )
        .await
    {
        Ok(capabilities) => capabilities,
        Err(_) => {
            let change = {
                let mut guard = shared.inner.lock();
                if gen != guard.generation {
                    return;
                }
                let change = set_state_locked(&mut guard, RuntimeState::Degraded);
                schedule_retry_locked(&shared, &mut guard, gen, shared.timings.network_retry_ms);
                change
            };
            shared.fire_state_change(change);
            return;
        }
    };

    // T6..T9: negotiation dispatch (one post-await sync segment).
    let change = {
        let mut guard = shared.inner.lock();
        let inner = &mut *guard;
        if gen != inner.generation {
            return;
        }
        match negotiate_presence(&capabilities.data, PROTOCOL_CLIENT_VERSION) {
            PresenceNegotiation::ClientUpdateRequired => {
                // T6: terminal until an app update — no timer.
                set_state_locked(inner, RuntimeState::UpdateRequired)
            }
            PresenceNegotiation::SchemaUnsupported | PresenceNegotiation::FeatureUnavailable => {
                // T7
                let change = set_state_locked(inner, RuntimeState::ServerFeatureUnavailable);
                schedule_retry_locked(&shared, inner, gen, shared.timings.feature_retry_ms);
                change
            }
            PresenceNegotiation::InvalidCapabilities => {
                // T8 — NOTE featureRetryMs, not networkRetryMs.
                let change = set_state_locked(inner, RuntimeState::Degraded);
                schedule_retry_locked(&shared, inner, gen, shared.timings.feature_retry_ms);
                change
            }
            PresenceNegotiation::Available { configuration } => {
                // T9: negotiated parameters (§3.1).
                inner.include_media = configuration.supports_media_timeline;
                inner.requested_lease_seconds = 90.0f64
                    .max(configuration.lease_min_seconds as f64)
                    .min(configuration.lease_max_seconds as f64);
                let heartbeat_seconds = (configuration.recommended_heartbeat_seconds as f64)
                    .min((inner.requested_lease_seconds / 3.0).max(1.0));
                // requests_per_minute > 0 is guaranteed by limitsAreValid.
                inner.min_send_interval_ms = 60_000u64.div_ceil(configuration.requests_per_minute);

                // The sequencer survives renegotiation for the same
                // (baseUrl, deviceId); the client (and its mapper limits) is
                // rebuilt from the fresh negotiation — deliberately avoiding
                // the stale-mapper reuse the macOS implementation exhibits.
                let client = {
                    let backing: Arc<dyn SequenceBacking> = shared.deps.sequence_store.clone();
                    let device_id = connection.device_id.clone();
                    let pairing_next_sequence = connection.pairing_next_sequence;
                    let authority = inner.registry.resolve(
                        &connection.base_url,
                        &connection.device_id,
                        move || PublishAuthority {
                            sequencer: Arc::new(CompanionSequencer::new(
                                backing,
                                device_id,
                                pairing_next_sequence,
                            )),
                            client: None,
                        },
                    );
                    let client = Arc::new(PresenceClient::new(
                        http.clone(),
                        CompanionCredential {
                            device_id: connection.device_id.clone(),
                            device_token: token.clone(),
                        },
                        authority.sequencer.clone(),
                        configuration,
                    ));
                    authority.client = Some(client.clone());
                    client
                };
                inner.client = Some(client);

                let change = set_state_locked(inner, RuntimeState::Active);
                spawn_heartbeat_locked(&shared, inner, gen, heartbeat_seconds);
                // Immediate first publish.
                trigger_refresh_locked(&shared, inner, gen);
                change
            }
        }
    };
    shared.fire_state_change(change);
}

// ---------------------------------------------------------------------------
// Refresh loop (§3.2, exact port). The caller has already claimed
// `refresh_loop_running` under the inner lock; EVERY exit path resets it
// inside its final locked segment, atomically with any renegotiation start,
// so a newly spawned configure can never observe a stale flag.
// ---------------------------------------------------------------------------

async fn run_refresh_loop(shared: Arc<Shared>, gen: u64) {
    loop {
        // Head: while (gen == generation && refresh_requested); compute the
        // throttle wait from SEND START of the previous publish.
        let wait_ms = {
            let mut guard = shared.inner.lock();
            if gen != guard.generation || !guard.refresh_requested {
                guard.refresh_loop_running = false;
                return;
            }
            guard.refresh_requested = false;
            guard.last_send_started_at + guard.min_send_interval_ms as i64 - now_ms()
        };
        if wait_ms > 0 {
            tokio::time::sleep(Duration::from_millis(wait_ms as u64)).await;
        }

        // Post-throttle segment: gen check, client grab, send-start stamp.
        // include_media is read HERE (the newest configure's value — no
        // per-iteration snapshot, trap 9).
        let (client, include_media) = {
            let mut guard = shared.inner.lock();
            if gen != guard.generation {
                guard.refresh_loop_running = false;
                return;
            }
            let Some(client) = guard.client.clone() else {
                guard.refresh_loop_running = false;
                return;
            };
            guard.last_send_started_at = now_ms();
            (client, guard.include_media)
        };

        // Every publish is a FRESH capture; never cached or replayed.
        let snapshot = shared
            .deps
            .capture
            .capture_for_delivery(CaptureOptions { include_media })
            .await;

        let requested_lease_seconds = {
            let mut guard = shared.inner.lock();
            if gen != guard.generation {
                guard.refresh_loop_running = false;
                return;
            }
            guard.requested_lease_seconds
        };

        let result = client
            .replace_presence(&snapshot, requested_lease_seconds)
            .await;

        // Post-publish segment (T12/T13/T14).
        let (change, published, done) = {
            let mut guard = shared.inner.lock();
            let inner = &mut *guard;
            if gen != inner.generation {
                inner.refresh_loop_running = false;
                return;
            }
            match result {
                Ok(_) => {
                    // T12: a single success flips degraded -> active.
                    (set_state_locked(inner, RuntimeState::Active), true, false)
                }
                Err(error) => {
                    if error.needs_renegotiation() {
                        // T13: terminate the authority, renegotiate.
                        logger::warn("coordinator", "schema/feature rejected; renegotiating");
                        inner.registry.discard();
                        inner.client = None;
                        inner.refresh_loop_running = false;
                        (start_locked(&shared, inner), false, true)
                    } else {
                        // T14: degrade; the pending refresh_requested is
                        // abandoned (recovery = retry timer + heartbeat).
                        logger::warn("coordinator", "publish failed; degraded");
                        let change = set_state_locked(inner, RuntimeState::Degraded);
                        schedule_retry_locked(&shared, inner, gen, shared.timings.network_retry_ms);
                        inner.refresh_loop_running = false;
                        (change, false, true)
                    }
                }
            }
        };
        shared.fire_state_change(change);
        if published {
            (shared.deps.on_published)();
        }
        if done {
            return;
        }
    }
}
