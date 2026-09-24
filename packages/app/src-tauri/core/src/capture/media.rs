//! WinRT SMTC media provider (`Windows.Media.Control`).
//!
//! Owned by agent M. Ground truth: `capture-stores.md` §3 — authoritative
//! semantics are the PowerShell provider's (`SmtcPowershellProvider.ts` +
//! `smtc-provider.ps1`); do NOT inherit npm-provider divergences or
//! PowerShell-era artifacts (polling, heartbeats, watchdogs, staleness
//! windows). KEEP: fresh WinRT read per snapshot, the null gate (empty
//! source AND empty title -> None), the `.exe`-suffix appId heuristic, the
//! full-AUMID playerDisplayName fallback, position extrapolation
//! (`0 < elapsed < 21600 s` while playing, clamped to duration), kind map
//! {Music(1) -> music, Video(3) -> video, else unknown}, Playing(4) as the
//! only "playing" status, and the semantic-change definition (source app /
//! title / artist / album / playing flips — never timeline ticks).
//! Threading: MTA init on calling threads; event handlers arrive on WinRT
//! threads — marshal into tokio via channels, never block or panic there;
//! re-subscribe per-session events on CurrentSessionChanged and drop old
//! registration tokens.
//!
//! Privacy: raw media text (titles/artists/albums/AUMIDs) lives only in
//! [`crate::model::MediaSnapshot`] (no serde) and in-process state; error
//! strings and log lines carry HRESULT codes and fixed messages only.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::mpsc;

use windows::core::HSTRING;
use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    CurrentSessionChangedEventArgs, GlobalSystemMediaTransportControlsSession,
    GlobalSystemMediaTransportControlsSessionManager, MediaPropertiesChangedEventArgs,
    PlaybackInfoChangedEventArgs,
};

use crate::capture::MediaProvider;
use crate::model::{MediaKind, MediaProviderChoice, MediaSnapshot, Unsubscribe};
use crate::runtime::logger;

/// `MediaProviderHealth.kind` value for this provider.
pub const MEDIA_PROVIDER_KIND: &str = "winrt";

/// Only `Playing (4)` maps to `playing: true`
/// (`GlobalSystemMediaTransportControlsSessionPlaybackStatus`: Closed=0,
/// Opened=1, Changing=2, Stopped=3, Playing=4, Paused=5).
const PLAYBACK_STATUS_PLAYING: i32 = 4;

/// Extrapolation gate: elapsed must be strictly inside `(0, 21600)` seconds
/// (6 h) — SMTC timeline updates are sparse, but a stale `LastUpdatedTime`
/// from before a sleep must not fast-forward the position.
const EXTRAPOLATION_MAX_ELAPSED_SECONDS: f64 = 21_600.0;

/// WinRT `TimeSpan`/`DateTime` tick length: 100 ns.
const TICKS_PER_SECOND: f64 = 10_000_000.0;
const TICKS_PER_MILLISECOND: i64 = 10_000;

/// Ticks between 1601-01-01 (WinRT `DateTime` epoch) and 1970-01-01.
const UNIX_EPOCH_OFFSET_TICKS: i64 = 116_444_736_000_000_000;

/// Bound on internal WinRT awaits outside `get_snapshot` (the PS helper
/// bounded every WinRT await at 5 s; `get_snapshot` uses its caller-supplied
/// timeout instead).
const WINRT_AWAIT_BOUND: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum MediaProviderError {
    /// WinRT/SMTC initialization failed (manager request, COM init, ...).
    /// The message must stay content-free (no titles/media text).
    #[error("media provider unavailable: {0}")]
    Unavailable(String),
}

/// Ensure the current thread is initialized for WinRT as MTA (spec §12
/// threading trap). Tokio tasks migrate across worker threads, so every
/// WinRT call site funnels through this. The initialization is deliberately
/// never undone: tokio workers live for the process, and an extra implicit
/// MTA reference is the documented CoIncrementMTAUsage-style pattern.
/// `RPC_E_CHANGED_MODE` (thread already STA) is tolerated — the SMTC objects
/// are agile. Never STA here: no message pump runs on tokio workers.
fn ensure_winrt_mta() {
    use std::cell::Cell;
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

    thread_local! {
        static WINRT_INITIALIZED: Cell<bool> = const { Cell::new(false) };
    }
    WINRT_INITIALIZED.with(|initialized| {
        if !initialized.get() {
            // SAFETY: RoInitialize is safe to call from any thread; S_FALSE
            // (already initialized) and RPC_E_CHANGED_MODE (STA thread) are
            // both tolerable outcomes, so the Result is ignored.
            unsafe {
                let _ = RoInitialize(RO_INIT_MULTITHREADED);
            }
            initialized.set(true);
        }
    });
}

/// Wall-clock epoch milliseconds (`Date.now()` parity).
fn now_epoch_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX),
        Err(before_epoch) => {
            -i64::try_from(before_epoch.duration().as_millis()).unwrap_or(i64::MAX)
        }
    }
}

/// TS `toNull`: trim, empty -> None.
fn to_null(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// appId heuristic (spec §3.4 step 3, EXACT): if the trimmed AUMID ends with
/// `.exe` (case-insensitive, TS `/\.exe$/i`), the whole trimmed AUMID
/// lowercased is the appId; otherwise None. `".exe"` alone matches (parity
/// with the TS regex).
fn app_id_from_source(trimmed_source: &str) -> Option<String> {
    let len = trimmed_source.len();
    if len < 4 {
        return None;
    }
    // `.get` returns None off a char boundary; a string ending in ASCII
    // ".exe" always has that boundary, so this is exactly the TS regex.
    match trimmed_source.get(len - 4..) {
        Some(suffix) if suffix.eq_ignore_ascii_case(".exe") => Some(trimmed_source.to_lowercase()),
        _ => None,
    }
}

/// `MediaPlaybackType` (Unknown=0, Music=1, Image=2, Video=3) -> `MediaKind`.
/// Anything else — including a null `IReference` — is `unknown`; `podcast`
/// is never produced on Windows (cross-platform parity value only).
fn media_kind_from_playback_type(playback_type: Option<i32>) -> MediaKind {
    match playback_type {
        Some(1) => MediaKind::Music,
        Some(3) => MediaKind::Video,
        _ => MediaKind::Unknown,
    }
}

/// WinRT `TimeSpan` ticks (100 ns) -> float seconds (`.TotalSeconds` parity).
fn ticks_to_seconds(ticks: i64) -> f64 {
    ticks as f64 / TICKS_PER_SECOND
}

/// WinRT `DateTime` ticks (100 ns since 1601-01-01 UTC) -> epoch ms,
/// truncating toward zero exactly like `DateTimeOffset.ToUnixTimeMilliseconds`.
fn winrt_datetime_to_epoch_ms(universal_time_ticks: i64) -> i64 {
    (universal_time_ticks - UNIX_EPOCH_OFFSET_TICKS) / TICKS_PER_MILLISECOND
}

/// Position extrapolation (spec §3.4 steps 5–6; privacy spec A2: this math
/// lives in the capture layer, not in the media-session tracker).
///
/// Start from the raw position when finite and `>= 0`, else None. While
/// playing: `elapsed = (now - updated_at) / 1000`; add it only when
/// `0 < elapsed < 21600`; then clamp to duration. The clamp sits inside the
/// playing branch but OUTSIDE the elapsed gate (TS parity: a raw position
/// past the duration is clamped while playing even without extrapolation,
/// and never clamped while paused).
fn extrapolated_position(
    raw_position: f64,
    playing: bool,
    updated_at_ms: i64,
    now_ms: i64,
    duration: Option<f64>,
) -> Option<f64> {
    if !raw_position.is_finite() || raw_position < 0.0 {
        return None;
    }
    let mut position = raw_position;
    // The TS gate also required a finite `updatedAt`; an i64 tick-derived
    // value is always finite, so only the playing gate remains.
    if playing {
        let elapsed = (now_ms - updated_at_ms) as f64 / 1000.0;
        if elapsed > 0.0 && elapsed < EXTRAPOLATION_MAX_ELAPSED_SECONDS {
            position += elapsed;
        }
        if let Some(duration) = duration {
            if position > duration {
                position = duration;
            }
        }
    }
    Some(position)
}

// ---------------------------------------------------------------------------
// Raw frame (one WinRT read of the current session) and pure mapping
// ---------------------------------------------------------------------------

/// One fresh read of the current SMTC session — the Rust equivalent of the
/// PS helper's per-poll `session` object, before any trimming/nulling.
#[derive(Debug, Clone, PartialEq)]
struct RawFrame {
    source_app_id: String,
    title: String,
    artist: String,
    album: String,
    kind: MediaKind,
    playing: bool,
    /// `timeline.EndTime.TotalSeconds` (NOT `EndTime - StartTime`).
    duration_seconds: f64,
    position_seconds: f64,
    /// `timeline.LastUpdatedTime.ToUnixTimeMilliseconds()`.
    updated_at_ms: i64,
}

/// Semantic identity of the current session (spec §3.3, authoritative PS
/// tuple): `None` = no session (PS key `""`), else the RAW (untrimmed)
/// `[sourceAppId, title, artist, album, playing]`. The `playing` component
/// is the boolean — NOT the raw playback-status int the npm provider used.
/// Position, duration, kind and timeline updates are deliberately absent:
/// they must never fire semantic-change events.
type SemanticKey = Option<(String, String, String, String, bool)>;

fn semantic_key(frame: Option<&RawFrame>) -> SemanticKey {
    frame.map(|f| {
        (
            f.source_app_id.clone(),
            f.title.clone(),
            f.artist.clone(),
            f.album.clone(),
            f.playing,
        )
    })
}

/// Frame -> `MediaSnapshot` (spec §3.4, authoritative). `exe_display_name`
/// is injected so tests do not depend on `capture::foreground` internals;
/// production passes [`crate::capture::foreground::exe_stem_display_name`].
fn frame_to_snapshot(
    frame: &RawFrame,
    now_ms: i64,
    exe_display_name: &dyn Fn(&str) -> String,
) -> Option<MediaSnapshot> {
    let source = frame.source_app_id.trim();
    let title = to_null(&frame.title);
    // Null gate: empty source AND empty title -> meaningless session.
    if source.is_empty() && title.is_none() {
        return None;
    }
    let app_id = app_id_from_source(source);
    let playing = frame.playing;

    let duration = if frame.duration_seconds.is_finite() && frame.duration_seconds > 0.0 {
        Some(frame.duration_seconds)
    } else {
        None
    };
    let position = extrapolated_position(
        frame.position_seconds,
        playing,
        frame.updated_at_ms,
        now_ms,
        duration,
    );

    // Exe-attributed players get the capitalized exe stem; everything else
    // falls back to the FULL trimmed AUMID (authoritative PS semantics —
    // spec ambiguity 2; the npm provider's `split("!")[0].split(".").pop()`
    // shortening is NOT inherited).
    let player_display_name = match &app_id {
        Some(app_id) => Some(exe_display_name(app_id)),
        None => to_null(source),
    };

    Some(MediaSnapshot {
        app_id,
        source_app_user_model_id: if source.is_empty() {
            None
        } else {
            Some(source.to_string())
        },
        player_display_name,
        kind: frame.kind,
        title,
        artist: to_null(&frame.artist),
        album: to_null(&frame.album),
        playing,
        duration_seconds: duration,
        position_seconds: position,
        sampled_at: now_ms,
    })
}

/// Fresh WinRT read of the manager's current session (spec §3.1 selection
/// rule: `GetCurrentSession()`, no enumeration, no custom ranking).
///
/// `Ok(None)` = no current session (windows-rs surfaces the null session as
/// an error value, which is folded here). `Err` = a property read failed —
/// callers treat that as "media unknown" (snapshot None / no semantic
/// event), mirroring how PS read errors decayed to null downstream.
async fn read_raw_frame(
    manager: &GlobalSystemMediaTransportControlsSessionManager,
) -> windows::core::Result<Option<RawFrame>> {
    let session: GlobalSystemMediaTransportControlsSession = match manager.GetCurrentSession() {
        Ok(session) => session,
        // Null (no session) and manager failure both read as "no session".
        Err(_) => return Ok(None),
    };

    let props = session.TryGetMediaPropertiesAsync()?.await?;
    let title = props.Title()?.to_string_lossy();
    let artist = props.Artist()?.to_string_lossy();
    let album = props.AlbumTitle()?.to_string_lossy();

    let timeline = session.GetTimelineProperties()?;
    let duration_seconds = ticks_to_seconds(timeline.EndTime()?.Duration);
    let position_seconds = ticks_to_seconds(timeline.Position()?.Duration);
    let updated_at_ms = winrt_datetime_to_epoch_ms(timeline.LastUpdatedTime()?.UniversalTime);

    let playback = session.GetPlaybackInfo()?;
    let playing = playback.PlaybackStatus()?.0 == PLAYBACK_STATUS_PLAYING;
    // PlaybackType is a nullable IReference; null arrives as Err and maps to
    // "unknown" exactly like the PS `$null -ne $playback.PlaybackType` check.
    let kind = media_kind_from_playback_type(
        playback
            .PlaybackType()
            .and_then(|reference| reference.Value())
            .map(|value| value.0)
            .ok(),
    );

    let source_app_id: HSTRING = session.SourceAppUserModelId()?;

    Ok(Some(RawFrame {
        source_app_id: source_app_id.to_string_lossy(),
        title,
        artist,
        album,
        kind,
        playing,
        duration_seconds,
        position_seconds,
        updated_at_ms,
    }))
}

// ---------------------------------------------------------------------------
// Provider state and event plumbing
// ---------------------------------------------------------------------------

/// Notifications marshaled OUT of WinRT handler threads into the tokio
/// event task. Handlers only ever perform a non-blocking channel send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderEvent {
    /// Manager `CurrentSessionChanged` (also the constructor's initial
    /// nudge): re-subscribe per-session handlers, then re-evaluate.
    SessionChanged,
    /// Per-session `MediaPropertiesChanged` / `PlaybackInfoChanged`:
    /// re-evaluate the semantic key only.
    SessionPropsChanged,
    /// `stop()` — exit the event task.
    Stop,
}

/// Per-session event registrations. The tokens MUST be removed from the
/// exact session object they were registered on (spec §12: stale
/// registrations leak handlers), so the session travels with its tokens.
struct SessionSubscription {
    session: GlobalSystemMediaTransportControlsSession,
    media_properties_token: i64,
    playback_info_token: i64,
}

impl SessionSubscription {
    /// Best-effort detach (errors are unobservable and unactionable here).
    fn detach(self) {
        let _ = self
            .session
            .RemoveMediaPropertiesChanged(self.media_properties_token);
        let _ = self
            .session
            .RemovePlaybackInfoChanged(self.playback_info_token);
    }
}

struct Inner {
    manager: GlobalSystemMediaTransportControlsSessionManager,
    event_tx: mpsc::UnboundedSender<ProviderEvent>,
    /// True after `stop()`: `healthy()` false, no callbacks fire, snapshots
    /// return None.
    stopped: AtomicBool,
    listeners: Mutex<HashMap<u64, Arc<dyn Fn() + Send + Sync>>>,
    next_listener_id: AtomicU64,
    session_subscription: Mutex<Option<SessionSubscription>>,
    manager_token: Mutex<Option<i64>>,
    last_semantic_key: Mutex<SemanticKey>,
    event_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Defensive detach for the drop-without-stop path; `stop()` has
        // normally taken both already.
        if let Some(token) = self.manager_token.get_mut().take() {
            let _ = self.manager.RemoveCurrentSessionChanged(token);
        }
        if let Some(subscription) = self.session_subscription.get_mut().take() {
            subscription.detach();
        }
    }
}

/// Tokio-side event task: serializes all reactions to WinRT notifications.
/// Holds only a `Weak` so a provider dropped without `stop()` lets `Inner`
/// unwind (its `Drop` detaches the handlers, which closes the channel).
async fn run_event_loop(inner: Weak<Inner>, mut event_rx: mpsc::UnboundedReceiver<ProviderEvent>) {
    while let Some(event) = event_rx.recv().await {
        let Some(inner) = inner.upgrade() else { break };
        if inner.stopped.load(Ordering::SeqCst) {
            break;
        }
        match event {
            ProviderEvent::Stop => break,
            ProviderEvent::SessionChanged => {
                resubscribe_session_events(&inner);
                recompute_semantic_key(&inner).await;
            }
            ProviderEvent::SessionPropsChanged => {
                recompute_semantic_key(&inner).await;
            }
        }
    }
}

/// Move the per-session registrations to the CURRENT session (session
/// identity is whatever `GetCurrentSession()` returns now — never cached
/// across changes). Runs entirely under the subscription lock so a
/// concurrent `stop()` (which sets `stopped` before taking the lock) can
/// never leave freshly-added handlers behind.
fn resubscribe_session_events(inner: &Arc<Inner>) {
    ensure_winrt_mta();
    let mut subscription = inner.session_subscription.lock();
    if let Some(old) = subscription.take() {
        old.detach();
    }
    if inner.stopped.load(Ordering::SeqCst) {
        return;
    }
    let Ok(session) = inner.manager.GetCurrentSession() else {
        // No current session: nothing to subscribe until the next
        // CurrentSessionChanged.
        return;
    };
    let media_tx = inner.event_tx.clone();
    let media_token = session.MediaPropertiesChanged(&TypedEventHandler::<
        GlobalSystemMediaTransportControlsSession,
        MediaPropertiesChangedEventArgs,
    >::new(move |_, _| {
        // WinRT thread: non-blocking send only; never panic here.
        let _ = media_tx.send(ProviderEvent::SessionPropsChanged);
        Ok(())
    }));
    let playback_tx = inner.event_tx.clone();
    let playback_token = session.PlaybackInfoChanged(&TypedEventHandler::<
        GlobalSystemMediaTransportControlsSession,
        PlaybackInfoChangedEventArgs,
    >::new(move |_, _| {
        let _ = playback_tx.send(ProviderEvent::SessionPropsChanged);
        Ok(())
    }));
    match (media_token, playback_token) {
        (Ok(media_properties_token), Ok(playback_info_token)) => {
            *subscription = Some(SessionSubscription {
                session,
                media_properties_token,
                playback_info_token,
            });
        }
        // Partial subscription failure: roll back so no token leaks.
        (Ok(token), Err(_)) => {
            let _ = session.RemoveMediaPropertiesChanged(token);
        }
        (Err(_), Ok(token)) => {
            let _ = session.RemovePlaybackInfoChanged(token);
        }
        (Err(_), Err(_)) => {}
    }
}

/// Re-read the current session and fire registered callbacks iff the
/// semantic key changed (spec §3.3). Read errors are swallowed — no key
/// update, no event — matching both TS providers.
async fn recompute_semantic_key(inner: &Arc<Inner>) {
    ensure_winrt_mta();
    let frame = match tokio::time::timeout(WINRT_AWAIT_BOUND, read_raw_frame(&inner.manager)).await
    {
        Ok(Ok(frame)) => frame,
        Ok(Err(_)) | Err(_) => return,
    };
    let key = semantic_key(frame.as_ref());
    let changed = {
        let mut last = inner.last_semantic_key.lock();
        if *last == key {
            false
        } else {
            *last = key;
            true
        }
    };
    if !changed || inner.stopped.load(Ordering::SeqCst) {
        return;
    }
    // Snapshot the callbacks, then invoke without holding the lock so a
    // callback may (un)register listeners without deadlocking.
    let callbacks: Vec<Arc<dyn Fn() + Send + Sync>> =
        inner.listeners.lock().values().cloned().collect();
    for callback in callbacks {
        callback();
    }
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// The single production media provider (`kind() == "winrt"`).
pub struct WinRtMediaProvider {
    inner: Arc<Inner>,
}

impl WinRtMediaProvider {
    /// Construct AND start (subsumes the TS `start()`): request the session
    /// manager, subscribe session events, mark healthy. Logs
    /// `info media: "winrt SMTC provider started"`.
    pub async fn new() -> Result<Self, MediaProviderError> {
        ensure_winrt_mta();
        let operation =
            GlobalSystemMediaTransportControlsSessionManager::RequestAsync().map_err(|error| {
                MediaProviderError::Unavailable(format!(
                    "session manager request failed ({:#010X})",
                    error.code().0
                ))
            })?;
        let manager = match tokio::time::timeout(WINRT_AWAIT_BOUND, operation).await {
            Ok(Ok(manager)) => manager,
            Ok(Err(error)) => {
                return Err(MediaProviderError::Unavailable(format!(
                    "session manager request failed ({:#010X})",
                    error.code().0
                )));
            }
            Err(_elapsed) => {
                return Err(MediaProviderError::Unavailable(
                    "WinRT await timeout".to_string(),
                ));
            }
        };

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let manager_tx = event_tx.clone();
        let manager_token = manager
            .CurrentSessionChanged(&TypedEventHandler::<
                GlobalSystemMediaTransportControlsSessionManager,
                CurrentSessionChangedEventArgs,
            >::new(move |_, _| {
                // WinRT thread: non-blocking send only; never panic here.
                let _ = manager_tx.send(ProviderEvent::SessionChanged);
                Ok(())
            }))
            .map_err(|error| {
                MediaProviderError::Unavailable(format!(
                    "session event subscription failed ({:#010X})",
                    error.code().0
                ))
            })?;

        let inner = Arc::new(Inner {
            manager,
            event_tx,
            stopped: AtomicBool::new(false),
            listeners: Mutex::new(HashMap::new()),
            next_listener_id: AtomicU64::new(0),
            session_subscription: Mutex::new(None),
            manager_token: Mutex::new(Some(manager_token)),
            last_semantic_key: Mutex::new(None),
            event_task: Mutex::new(None),
        });
        let task = tokio::spawn(run_event_loop(Arc::downgrade(&inner), event_rx));
        *inner.event_task.lock() = Some(task);
        // Initial nudge: attach per-session handlers to the current session
        // and seed the semantic key (a present session fires the first
        // change, like the PS provider's first frame did).
        let _ = inner.event_tx.send(ProviderEvent::SessionChanged);

        logger::info("media", "winrt SMTC provider started");
        Ok(Self { inner })
    }
}

#[async_trait::async_trait]
impl MediaProvider for WinRtMediaProvider {
    fn kind(&self) -> &'static str {
        MEDIA_PROVIDER_KIND
    }

    async fn get_snapshot(
        &self,
        timeout: std::time::Duration,
    ) -> Option<crate::model::MediaSnapshot> {
        if self.inner.stopped.load(Ordering::SeqCst) {
            return None;
        }
        ensure_winrt_mta();
        // Fresh WinRT read per call (no cached frames, no staleness window —
        // spec §3.3); the caller-supplied timeout bounds the whole read
        // (the capture facade passes 2 s, spec §3.8). Timeout and read
        // errors both mean "media unknown" for this delivery.
        let frame = tokio::time::timeout(timeout, read_raw_frame(&self.inner.manager))
            .await
            .ok()?
            .ok()??;
        frame_to_snapshot(
            &frame,
            now_epoch_ms(),
            &crate::capture::foreground::exe_stem_display_name,
        )
    }

    fn on_semantic_change(&self, callback: Box<dyn Fn() + Send + Sync>) -> Unsubscribe {
        if self.inner.stopped.load(Ordering::SeqCst) {
            return Unsubscribe::noop();
        }
        let id = self.inner.next_listener_id.fetch_add(1, Ordering::Relaxed);
        self.inner.listeners.lock().insert(id, Arc::from(callback));
        let weak = Arc::downgrade(&self.inner);
        Unsubscribe::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.listeners.lock().remove(&id);
            }
        })
    }

    fn healthy(&self) -> bool {
        // PS-provider semantics: healthy from successful construction until
        // stop(); individual read failures do NOT degrade health (the npm
        // provider's failed-snapshot flag is a documented divergence).
        !self.inner.stopped.load(Ordering::SeqCst)
    }

    async fn stop(&self) {
        if self.inner.stopped.swap(true, Ordering::SeqCst) {
            return; // idempotent
        }
        // Detach the manager-level handler, then the per-session handlers.
        if let Some(token) = self.inner.manager_token.lock().take() {
            let _ = self.inner.manager.RemoveCurrentSessionChanged(token);
        }
        if let Some(subscription) = self.inner.session_subscription.lock().take() {
            subscription.detach();
        }
        // Stop the event task promptly (abort covers a task parked inside a
        // bounded WinRT await; the 2 s shutdown budget must hold). The lock
        // guard is dropped before awaiting the handle.
        let _ = self.inner.event_tx.send(ProviderEvent::Stop);
        let task = self.inner.event_task.lock().take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        self.inner.listeners.lock().clear();
        logger::info("media", "winrt SMTC provider stopped");
    }
}

/// Legacy `config.media.provider` values (`auto`/`npm`/`powershell`) all
/// select the WinRT provider (config compat). `Err` means "no media
/// provider available; media capture disabled" — the app crate maps it to
/// `None` plus an error log, like main.ts did.
pub async fn select_media_provider(
    preference: MediaProviderChoice,
) -> Result<Arc<dyn MediaProvider>, MediaProviderError> {
    // "auto" | "npm" | "powershell" are all satisfied by the one WinRT
    // implementation; the preference carries no other information.
    let _ = preference;
    let provider = WinRtMediaProvider::new().await?;
    Ok(Arc::new(provider))
}

// ---------------------------------------------------------------------------
// Tests (pure helpers only; live-WinRT paths run in the verify phase)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> RawFrame {
        RawFrame {
            source_app_id: "Spotify.exe".to_string(),
            title: "Song".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            kind: MediaKind::Music,
            playing: true,
            duration_seconds: 259.0,
            position_seconds: 100.0,
            updated_at_ms: 1_740_000_000_000,
        }
    }

    fn stub_display(app_id: &str) -> String {
        format!("resolved:{app_id}")
    }

    // ----- appId heuristic (spec §3.4 step 3; §11 vectors) -----

    #[test]
    fn app_id_lowercases_exe_suffixed_aumid() {
        assert_eq!(
            app_id_from_source("Spotify.exe"),
            Some("spotify.exe".to_string())
        );
    }

    #[test]
    fn app_id_suffix_match_is_case_insensitive() {
        assert_eq!(
            app_id_from_source("FooBar.EXE"),
            Some("foobar.exe".to_string())
        );
        assert_eq!(app_id_from_source("baz.eXe"), Some("baz.exe".to_string()));
    }

    #[test]
    fn app_id_none_for_non_exe_source() {
        assert_eq!(app_id_from_source("MSEdge"), None);
    }

    #[test]
    fn app_id_none_for_uwp_aumid() {
        assert_eq!(
            app_id_from_source("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"),
            None
        );
    }

    #[test]
    fn app_id_bare_dot_exe_matches_like_the_ts_regex() {
        assert_eq!(app_id_from_source(".exe"), Some(".exe".to_string()));
    }

    #[test]
    fn app_id_short_empty_and_multibyte_sources() {
        assert_eq!(app_id_from_source(""), None);
        assert_eq!(app_id_from_source("exe"), None);
        // len >= 4 but no ASCII ".exe" boundary at the tail — must not panic.
        assert_eq!(app_id_from_source("日本語"), None);
    }

    // ----- kind mapping (spec §3.1 enum values) -----

    #[test]
    fn kind_maps_music_video_and_everything_else_unknown() {
        assert_eq!(media_kind_from_playback_type(Some(1)), MediaKind::Music);
        assert_eq!(media_kind_from_playback_type(Some(3)), MediaKind::Video);
        assert_eq!(media_kind_from_playback_type(Some(0)), MediaKind::Unknown);
        assert_eq!(media_kind_from_playback_type(Some(2)), MediaKind::Unknown);
        assert_eq!(media_kind_from_playback_type(Some(99)), MediaKind::Unknown);
        assert_eq!(media_kind_from_playback_type(None), MediaKind::Unknown);
    }

    // ----- unit conversions (spec §12) -----

    #[test]
    fn timespan_ticks_convert_to_total_seconds() {
        assert_eq!(ticks_to_seconds(0), 0.0);
        assert_eq!(ticks_to_seconds(10_000_000), 1.0);
        assert_eq!(ticks_to_seconds(2_592_500_000), 259.25);
        assert_eq!(ticks_to_seconds(-10_000_000), -1.0);
    }

    #[test]
    fn winrt_datetime_converts_to_unix_ms_with_truncation() {
        assert_eq!(winrt_datetime_to_epoch_ms(UNIX_EPOCH_OFFSET_TICKS), 0);
        assert_eq!(
            winrt_datetime_to_epoch_ms(UNIX_EPOCH_OFFSET_TICKS + 10_000),
            1
        );
        // ToUnixTimeMilliseconds truncates sub-millisecond ticks.
        assert_eq!(
            winrt_datetime_to_epoch_ms(UNIX_EPOCH_OFFSET_TICKS + 19_999),
            1
        );
        assert_eq!(
            winrt_datetime_to_epoch_ms(UNIX_EPOCH_OFFSET_TICKS - 10_000),
            -1
        );
    }

    // ----- position extrapolation (spec §3.4 steps 5–6; §11 vectors) -----

    const NOW: i64 = 1_740_000_000_000;

    #[test]
    fn playing_position_extrapolates_elapsed_seconds() {
        assert_eq!(
            extrapolated_position(100.0, true, NOW - 5_000, NOW, None),
            Some(105.0)
        );
    }

    #[test]
    fn extrapolated_position_clamps_to_duration() {
        assert_eq!(
            extrapolated_position(100.0, true, NOW - 5_000, NOW, Some(102.0)),
            Some(102.0)
        );
    }

    #[test]
    fn elapsed_at_or_past_six_hours_is_not_extrapolated() {
        assert_eq!(
            extrapolated_position(100.0, true, NOW - 21_600_000, NOW, None),
            Some(100.0)
        );
        assert_eq!(
            extrapolated_position(100.0, true, NOW - 21_599_000, NOW, None),
            Some(100.0 + 21_599.0)
        );
    }

    #[test]
    fn paused_position_is_not_extrapolated() {
        assert_eq!(
            extrapolated_position(100.0, false, NOW - 5_000, NOW, None),
            Some(100.0)
        );
    }

    #[test]
    fn zero_or_negative_elapsed_adds_nothing() {
        assert_eq!(
            extrapolated_position(100.0, true, NOW, NOW, None),
            Some(100.0)
        );
        assert_eq!(
            extrapolated_position(100.0, true, NOW + 5_000, NOW, None),
            Some(100.0)
        );
    }

    #[test]
    fn invalid_raw_positions_are_none() {
        assert_eq!(extrapolated_position(-1.0, true, NOW, NOW, None), None);
        assert_eq!(extrapolated_position(f64::NAN, true, NOW, NOW, None), None);
        assert_eq!(
            extrapolated_position(f64::INFINITY, false, NOW, NOW, None),
            None
        );
    }

    #[test]
    fn clamp_applies_while_playing_even_without_elapsed_gate() {
        // elapsed == 0 -> no extrapolation, but the clamp still runs (TS
        // parity: clamp is inside the playing branch, outside the gate).
        assert_eq!(
            extrapolated_position(500.0, true, NOW, NOW, Some(300.0)),
            Some(300.0)
        );
        // Paused: never clamped.
        assert_eq!(
            extrapolated_position(500.0, false, NOW - 5_000, NOW, Some(300.0)),
            Some(500.0)
        );
    }

    // ----- semantic key (spec §3.3, authoritative PS tuple) -----

    #[test]
    fn no_session_key_differs_from_all_empty_session() {
        let empty = RawFrame {
            source_app_id: String::new(),
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            kind: MediaKind::Unknown,
            playing: false,
            duration_seconds: 0.0,
            position_seconds: 0.0,
            updated_at_ms: 0,
        };
        assert_eq!(semantic_key(None), None);
        assert_ne!(semantic_key(None), semantic_key(Some(&empty)));
    }

    #[test]
    fn timeline_kind_and_duration_changes_are_not_semantic() {
        let a = frame();
        let mut b = frame();
        b.position_seconds = 217.228;
        b.duration_seconds = 300.0;
        b.updated_at_ms += 60_000;
        b.kind = MediaKind::Video;
        assert_eq!(semantic_key(Some(&a)), semantic_key(Some(&b)));
    }

    #[test]
    fn text_source_and_playing_changes_are_semantic() {
        let base = frame();
        let mut paused = frame();
        paused.playing = false;
        assert_ne!(semantic_key(Some(&base)), semantic_key(Some(&paused)));

        let mut retitled = frame();
        retitled.title = "Other".to_string();
        assert_ne!(semantic_key(Some(&base)), semantic_key(Some(&retitled)));

        let mut other_app = frame();
        other_app.source_app_id = "vlc.exe".to_string();
        assert_ne!(semantic_key(Some(&base)), semantic_key(Some(&other_app)));

        assert_eq!(semantic_key(Some(&base)), semantic_key(Some(&frame())));
    }

    // ----- frame -> snapshot mapping (spec §3.4) -----

    #[test]
    fn null_gate_drops_empty_source_and_title_sessions() {
        let mut f = frame();
        f.source_app_id = "   ".to_string();
        f.title = " ".to_string();
        // Artist/album alone cannot save the session (gate checks only
        // source + title).
        f.artist = "Somebody".to_string();
        assert!(frame_to_snapshot(&f, NOW, &stub_display).is_none());
    }

    #[test]
    fn exe_attributed_player_maps_app_id_and_display_name() {
        let mut f = frame();
        f.source_app_id = " Spotify.exe ".to_string();
        let snapshot = frame_to_snapshot(&f, NOW, &stub_display).expect("snapshot");
        assert_eq!(snapshot.app_id.as_deref(), Some("spotify.exe"));
        assert_eq!(
            snapshot.source_app_user_model_id.as_deref(),
            Some("Spotify.exe")
        );
        // The resolver receives the LOWERCASED appId, not the raw AUMID.
        assert_eq!(
            snapshot.player_display_name.as_deref(),
            Some("resolved:spotify.exe")
        );
        assert_eq!(snapshot.sampled_at, NOW);
    }

    #[test]
    fn non_exe_player_falls_back_to_full_aumid() {
        let mut f = frame();
        f.source_app_id = "Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic".to_string();
        let snapshot = frame_to_snapshot(&f, NOW, &stub_display).expect("snapshot");
        assert_eq!(snapshot.app_id, None);
        // Authoritative PS semantics: the FULL AUMID (the npm provider's
        // "ZuneMusic_8wekyb3d8bbwe" shortening is not inherited).
        assert_eq!(
            snapshot.player_display_name.as_deref(),
            Some("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic")
        );
        assert_eq!(snapshot.kind, MediaKind::Music);
    }

    #[test]
    fn title_only_session_survives_the_gate_with_null_player() {
        let mut f = frame();
        f.source_app_id = String::new();
        f.title = "Lonely Song".to_string();
        let snapshot = frame_to_snapshot(&f, NOW, &stub_display).expect("snapshot");
        assert_eq!(snapshot.app_id, None);
        assert_eq!(snapshot.source_app_user_model_id, None);
        assert_eq!(snapshot.player_display_name, None);
        assert_eq!(snapshot.title.as_deref(), Some("Lonely Song"));
    }

    #[test]
    fn text_fields_trim_and_duration_gate_applies() {
        let mut f = frame();
        f.title = "  Song  ".to_string();
        f.artist = "   ".to_string();
        f.album = String::new();
        f.duration_seconds = 0.0;
        f.playing = false;
        let snapshot = frame_to_snapshot(&f, NOW, &stub_display).expect("snapshot");
        assert_eq!(snapshot.title.as_deref(), Some("Song"));
        assert_eq!(snapshot.artist, None);
        assert_eq!(snapshot.album, None);
        assert_eq!(snapshot.duration_seconds, None);
        assert!(!snapshot.playing);
        assert_eq!(snapshot.position_seconds, Some(100.0));
    }

    #[test]
    fn negative_duration_is_none_and_position_extrapolates_in_mapping() {
        let mut f = frame();
        f.duration_seconds = -3.0;
        f.updated_at_ms = NOW - 5_000;
        let snapshot = frame_to_snapshot(&f, NOW, &stub_display).expect("snapshot");
        assert_eq!(snapshot.duration_seconds, None);
        // §11 vector: position 100.0, playing, updatedAt = now - 5000 -> 105.0.
        assert_eq!(snapshot.position_seconds, Some(105.0));
    }
}
