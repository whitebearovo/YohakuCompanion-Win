//! Session lock/unlock + suspend/resume events via a hidden window.
//!
//! Owned by agent C. Ground truth: `capture-stores.md` §4. A hidden ORDINARY
//! top-level window (message-only `HWND_MESSAGE` windows do NOT receive
//! broadcasts) on a dedicated `std::thread` running a Win32 message loop:
//! `WTSRegisterSessionNotification(NOTIFY_FOR_THIS_SESSION)` ->
//! `WM_WTSSESSION_CHANGE` (`WTS_SESSION_LOCK`/`WTS_SESSION_UNLOCK` only);
//! `WM_POWERBROADCAST` with the .NET-parity mapping — suspend:
//! `PBT_APMSUSPEND`, `PBT_APMSTANDBY`; resume: `PBT_APMRESUMECRITICAL`,
//! `PBT_APMRESUMESUSPEND`, `PBT_APMRESUMESTANDBY`;
//! `PBT_APMRESUMEAUTOMATIC` deliberately NOT mapped (unattended wakes are
//! caught by the SuspendDetector instead — strict parity, spec ambiguity
//! §13.3). Dedup state machine: lock/suspend collapse into ONE
//! lock-or-sleep transition while not already suspended; the first of
//! resume/unlock ends it; a resume without prior lock/suspend is swallowed.
//! Teardown: `WTSUnRegisterSessionNotification` + `DestroyWindow` +
//! `PostQuitMessage`; return TRUE from the wndproc for `WM_POWERBROADCAST`.
//!
//! Implementation note: `RegisterClassW` is feature-gated behind
//! `Win32_Graphics_Gdi` (absent from the scaffold-owned Cargo.toml), so the
//! hidden window uses the predefined `"STATIC"` class and is subclassed via
//! `SetWindowLongPtrW(GWLP_WNDPROC)`; the shared state rides in
//! `GWLP_USERDATA`. See integration-notes "## C — Win32_Graphics_Gdi".
//! Callbacks are marshalled onto a dedicated dispatcher thread through a
//! channel so slow listeners can never block the message loop (order
//! preserved).

use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

use parking_lot::Mutex;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, PostMessageW, PostQuitMessage, SetWindowLongPtrW, TranslateMessage,
    GWLP_USERDATA, GWLP_WNDPROC, MSG, PBT_APMRESUMECRITICAL, PBT_APMRESUMESTANDBY,
    PBT_APMRESUMESUSPEND, PBT_APMSTANDBY, PBT_APMSUSPEND, WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY,
    WM_NCDESTROY, WM_POWERBROADCAST, WM_WTSSESSION_CHANGE, WNDPROC, WS_OVERLAPPED,
    WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
};

use crate::model::Unsubscribe;
use crate::runtime::logger;

#[derive(Debug, thiserror::Error)]
pub enum SystemEventsError {
    /// Window/class creation or WTS registration failed; there is no
    /// external process to supervise — log an error and run without the
    /// source (the SuspendDetector remains as the safety net).
    #[error("system events unavailable: {0}")]
    Unavailable(String),
}

/// Which listener set a deduped transition fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventKind {
    LockOrSleep,
    UnlockOrResume,
}

/// `WM_WTSSESSION_CHANGE` wParam -> transition target
/// (`Some(true)` = suspended). Only LOCK (0x7) / UNLOCK (0x8) are mapped;
/// every other WTS code (console/remote connect, logon/logoff, ...) is
/// ignored, matching what the .NET SystemEvents path did for this script.
fn wts_wparam_transition(wparam: u32) -> Option<bool> {
    match wparam {
        WTS_SESSION_LOCK => Some(true),
        WTS_SESSION_UNLOCK => Some(false),
        _ => None,
    }
}

/// `WM_POWERBROADCAST` wParam -> transition target. The .NET
/// `SystemEvents` mapping the TS behavior inherited: suspend =
/// PBT_APMSUSPEND (0x4) / PBT_APMSTANDBY (0x5); resume =
/// PBT_APMRESUMECRITICAL (0x6) / PBT_APMRESUMESUSPEND (0x7) /
/// PBT_APMRESUMESTANDBY (0x8). PBT_APMRESUMEAUTOMATIC (0x12) is
/// deliberately unmapped (spec ambiguity §13.3): unattended wakes produce
/// no resume from this source and are caught by the SuspendDetector.
fn power_wparam_transition(wparam: u32) -> Option<bool> {
    match wparam {
        PBT_APMSUSPEND | PBT_APMSTANDBY => Some(true),
        PBT_APMRESUMECRITICAL | PBT_APMRESUMESUSPEND | PBT_APMRESUMESTANDBY => Some(false),
        _ => None,
    }
}

/// Dedup state machine (spec §4.3): returns whether listeners fire.
/// `lock`/`suspend` while already suspended -> dropped (idempotent);
/// `unlock`/`resume` while not suspended -> dropped (a resume without a
/// prior lock/suspend is swallowed; the initial state is not-suspended).
fn dedupe_transition(suspended: &mut bool, to_suspended: bool) -> bool {
    if to_suspended {
        if *suspended {
            return false;
        }
        *suspended = true;
        true
    } else {
        if !*suspended {
            return false;
        }
        *suspended = false;
        true
    }
}

type EventListener = Arc<dyn Fn() + Send + Sync>;

/// State shared between the public handle, the window thread (via
/// `GWLP_USERDATA`) and the dispatcher thread.
struct SysShared {
    /// Dedup flag; survives stop/start (TS field survived helper
    /// restarts).
    suspended: Mutex<bool>,
    lock_listeners: Mutex<Vec<(u64, EventListener)>>,
    resume_listeners: Mutex<Vec<(u64, EventListener)>>,
    next_listener_id: AtomicU64,
    /// Sender for the dispatcher thread; present while started.
    tx: Mutex<Option<mpsc::Sender<EventKind>>>,
    /// Original `"STATIC"` wndproc (subclassing), stored as a raw isize.
    prev_proc: AtomicIsize,
}

impl SysShared {
    /// Runs the dedupe state machine and, when it fires, queues the event
    /// for the dispatcher thread — the message loop never runs listener
    /// code (an unbounded channel send cannot block).
    fn transition(&self, to_suspended: bool) {
        let fired = {
            let mut suspended = self.suspended.lock();
            dedupe_transition(&mut suspended, to_suspended)
        };
        if !fired {
            return;
        }
        let kind = if to_suspended {
            EventKind::LockOrSleep
        } else {
            EventKind::UnlockOrResume
        };
        let sender = self.tx.lock().clone();
        if let Some(sender) = sender {
            let _ = sender.send(kind);
        }
    }
}

/// Handles owned while the hidden window runs.
struct Running {
    /// HWND stored as isize so it can cross threads (only used with the
    /// thread-safe `PostMessageW`).
    hwnd: isize,
    window_join: Option<JoinHandle<()>>,
    dispatcher_join: Option<JoinHandle<()>>,
}

/// Hidden-window WTS/power listener.
pub struct SystemEvents {
    shared: Arc<SysShared>,
    running: tokio::sync::Mutex<Option<Running>>,
}

impl SystemEvents {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(SysShared {
                suspended: Mutex::new(false),
                lock_listeners: Mutex::new(Vec::new()),
                resume_listeners: Mutex::new(Vec::new()),
                next_listener_id: AtomicU64::new(1),
                tx: Mutex::new(None),
                prev_proc: AtomicIsize::new(0),
            }),
            running: tokio::sync::Mutex::new(None),
        }
    }

    /// Spawn the window + dispatcher threads and register notifications.
    /// Idempotent while running. Logs `info system-events: "provider
    /// started"` on success; on failure logs an error and returns
    /// [`SystemEventsError::Unavailable`] (the caller runs without the
    /// source; the SuspendDetector remains as the safety net).
    pub async fn start(&self) -> Result<(), SystemEventsError> {
        let mut running = self.running.lock().await;
        if running.is_some() {
            return Ok(());
        }

        let (tx, rx) = mpsc::channel();
        *self.shared.tx.lock() = Some(tx);
        let dispatcher_join = {
            let shared = Arc::clone(&self.shared);
            std::thread::spawn(move || dispatcher_loop(shared, rx))
        };

        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let window_join = {
            let shared = Arc::clone(&self.shared);
            std::thread::spawn(move || window_thread(shared, ready_tx))
        };

        let detail = match ready_rx.await {
            Ok(Ok(hwnd)) => {
                *running = Some(Running {
                    hwnd,
                    window_join: Some(window_join),
                    dispatcher_join: Some(dispatcher_join),
                });
                logger::info("system-events", "provider started");
                return Ok(());
            }
            Ok(Err(detail)) => detail,
            Err(_) => "window thread terminated before initialization".to_string(),
        };

        // Failure: the window thread has already unwound; retire the
        // dispatcher (dropping the sender ends its receive loop).
        *self.shared.tx.lock() = None;
        let _ = tokio::task::spawn_blocking(move || {
            let _ = window_join.join();
            let _ = dispatcher_join.join();
        })
        .await;
        logger::error(
            "system-events",
            "hidden window unavailable; session/power events disabled",
        );
        Err(SystemEventsError::Unavailable(detail))
    }

    /// Tear the window down promptly (within the 2 s bounded shutdown):
    /// `WM_CLOSE` -> unregister + `DestroyWindow` -> `WM_DESTROY` ->
    /// `PostQuitMessage` -> thread exit; then the dispatcher drains and
    /// exits. No log on stop (TS parity). The dedup flag is NOT reset.
    pub async fn stop(&self) {
        let mut running = self.running.lock().await;
        let Some(mut active) = running.take() else {
            return;
        };
        // SAFETY: PostMessageW is thread-safe; the HWND stays valid until
        // the window thread processes this WM_CLOSE.
        unsafe {
            let _ = PostMessageW(
                Some(HWND(active.hwnd as *mut core::ffi::c_void)),
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            );
        }
        if let Some(join) = active.window_join.take() {
            let _ = tokio::task::spawn_blocking(move || {
                let _ = join.join();
            })
            .await;
        }
        *self.shared.tx.lock() = None;
        if let Some(join) = active.dispatcher_join.take() {
            let _ = tokio::task::spawn_blocking(move || {
                let _ = join.join();
            })
            .await;
        }
    }

    /// First of lock/suspend while not suspended.
    pub fn on_lock_or_sleep(&self, callback: Box<dyn Fn() + Send + Sync>) -> Unsubscribe {
        register_listener(&self.shared, EventKind::LockOrSleep, callback)
    }

    /// First of unlock/resume while suspended.
    pub fn on_unlock_or_resume(&self, callback: Box<dyn Fn() + Send + Sync>) -> Unsubscribe {
        register_listener(&self.shared, EventKind::UnlockOrResume, callback)
    }
}

impl Default for SystemEvents {
    fn default() -> Self {
        Self::new()
    }
}

fn listener_set(shared: &SysShared, kind: EventKind) -> &Mutex<Vec<(u64, EventListener)>> {
    match kind {
        EventKind::LockOrSleep => &shared.lock_listeners,
        EventKind::UnlockOrResume => &shared.resume_listeners,
    }
}

fn register_listener(
    shared: &Arc<SysShared>,
    kind: EventKind,
    callback: Box<dyn Fn() + Send + Sync>,
) -> Unsubscribe {
    let id = shared.next_listener_id.fetch_add(1, Ordering::Relaxed);
    listener_set(shared, kind)
        .lock()
        .push((id, Arc::from(callback)));
    let shared = Arc::clone(shared);
    Unsubscribe::new(move || {
        listener_set(&shared, kind)
            .lock()
            .retain(|(entry_id, _)| *entry_id != id);
    })
}

/// Dispatcher thread: fires listeners sequentially, in event order, off
/// the message loop. Exits when the sender is dropped (stop/cleanup).
fn dispatcher_loop(shared: Arc<SysShared>, rx: mpsc::Receiver<EventKind>) {
    while let Ok(kind) = rx.recv() {
        let listeners: Vec<EventListener> = listener_set(&shared, kind)
            .lock()
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect();
        for listener in listeners {
            listener();
        }
    }
}

/// Window thread body: create the hidden window, subclass it, register WTS
/// notifications, report readiness, then pump messages until `WM_QUIT`.
fn window_thread(
    shared: Arc<SysShared>,
    ready: tokio::sync::oneshot::Sender<Result<isize, String>>,
) {
    // Hidden ORDINARY top-level window: no parent (a `HWND_MESSAGE` child
    // would miss `WM_POWERBROADCAST` broadcasts), never shown. The
    // predefined `"STATIC"` class avoids `RegisterClassW` (Gdi-gated, see
    // module docs); `hInstance` may be null for system classes.
    // SAFETY: plain window creation with constant class/title literals.
    let hwnd = match unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("yohaku-companion system events"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        )
    } {
        Ok(hwnd) if !hwnd.is_invalid() => hwnd,
        Ok(_) => {
            let _ = ready.send(Err("CreateWindowExW returned a null handle".to_string()));
            return;
        }
        Err(err) => {
            let _ = ready.send(Err(format!("CreateWindowExW failed ({})", err.code())));
            return;
        }
    };

    // Subclass: remember the original proc FIRST so the custom proc can
    // always forward, then attach the shared state, then swap the proc.
    // SAFETY: hwnd was just created on this thread; no messages are
    // dispatched between these calls (no pump runs here).
    unsafe {
        let previous = GetWindowLongPtrW(hwnd, GWLP_WNDPROC);
        shared.prev_proc.store(previous, Ordering::SeqCst);
        let shared_ptr = Arc::into_raw(Arc::clone(&shared));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, shared_ptr as isize);
        let proc_ptr: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT = wndproc;
        SetWindowLongPtrW(hwnd, GWLP_WNDPROC, proc_ptr as usize as isize);
    }

    // SAFETY: hwnd is valid; NOTIFY_FOR_THIS_SESSION scopes notifications
    // to the interactive session (spec §4.2).
    if let Err(err) = unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) } {
        // SAFETY: destroying the window releases the userdata Arc through
        // our WM_NCDESTROY handler.
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        let _ = ready.send(Err(format!(
            "WTSRegisterSessionNotification failed ({})",
            err.code()
        )));
        return;
    }

    let _ = ready.send(Ok(hwnd.0 as isize));

    // Message loop; ends when the WM_CLOSE-triggered teardown posts
    // WM_QUIT. `> 0` also exits on the (practically impossible) -1 error.
    // SAFETY: standard pump for a window owned by this thread.
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Subclass wndproc. Runs listener-free (transitions are queued to the
/// dispatcher thread); unhandled messages forward to the original
/// `"STATIC"` proc. No panicking operations (unwinding out of an extern
/// "system" callback aborts).
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: GWLP_USERDATA holds an Arc::into_raw pointer between
    // subclass installation and WM_NCDESTROY; null before/after.
    let shared_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const SysShared;
    if shared_ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    // SAFETY: the pointed-to SysShared stays alive: the Arc reference held
    // through GWLP_USERDATA is only released in WM_NCDESTROY below.
    let shared = unsafe { &*shared_ptr };
    let previous_raw = shared.prev_proc.load(Ordering::SeqCst);
    // SAFETY: WNDPROC is a pointer-sized Option<fn>; previous_raw came from
    // GetWindowLongPtrW(GWLP_WNDPROC) on this window (0 becomes None).
    let previous: WNDPROC = unsafe { std::mem::transmute::<isize, WNDPROC>(previous_raw) };

    match msg {
        WM_WTSSESSION_CHANGE => {
            if let Some(to_suspended) = wts_wparam_transition(wparam.0 as u32) {
                shared.transition(to_suspended);
            }
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            if let Some(to_suspended) = power_wparam_transition(wparam.0 as u32) {
                shared.transition(to_suspended);
            }
            // Return TRUE for WM_POWERBROADCAST (spec §12).
            LRESULT(1)
        }
        WM_CLOSE => {
            // Teardown order per spec §4.2: unregister, then destroy (the
            // destroy cascade posts the quit from WM_DESTROY).
            // SAFETY: hwnd is still valid inside WM_CLOSE.
            unsafe {
                let _ = WTSUnRegisterSessionNotification(hwnd);
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // Let the original proc clean up, then end the message loop.
            let result = match previous {
                // SAFETY: forwarding to the original class proc.
                Some(_) => unsafe { CallWindowProcW(previous, hwnd, msg, wparam, lparam) },
                None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
            };
            unsafe { PostQuitMessage(0) };
            result
        }
        WM_NCDESTROY => {
            // Last message: restore the original proc, detach and release
            // the shared-state reference taken at subclass time.
            // SAFETY: balances the Arc::into_raw in window_thread; nothing
            // touches `shared` after the final forward below because the
            // local Arc keeps the allocation alive until scope end.
            let arc = unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                SetWindowLongPtrW(hwnd, GWLP_WNDPROC, previous_raw);
                Arc::from_raw(shared_ptr)
            };
            let result = match previous {
                // SAFETY: forwarding the final message to the restored proc.
                Some(_) => unsafe { CallWindowProcW(previous, hwnd, msg, wparam, lparam) },
                None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
            };
            drop(arc);
            result
        }
        _ => match previous {
            // SAFETY: default path forwards to the original class proc.
            Some(_) => unsafe { CallWindowProcW(previous, hwnd, msg, wparam, lparam) },
            None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wts_mapping_is_lock_and_unlock_only() {
        assert_eq!(wts_wparam_transition(WTS_SESSION_LOCK), Some(true));
        assert_eq!(wts_wparam_transition(WTS_SESSION_UNLOCK), Some(false));
        // Console/remote connect, logon/logoff etc. are ignored.
        for other in [0x1u32, 0x2, 0x3, 0x4, 0x5, 0x6, 0x9, 0xA, 0xB] {
            assert_eq!(wts_wparam_transition(other), None, "wparam {other:#x}");
        }
    }

    #[test]
    fn power_mapping_matches_dotnet_systemevents() {
        // Suspend family.
        assert_eq!(power_wparam_transition(PBT_APMSUSPEND), Some(true));
        assert_eq!(power_wparam_transition(PBT_APMSTANDBY), Some(true));
        // Resume family.
        assert_eq!(power_wparam_transition(PBT_APMRESUMECRITICAL), Some(false));
        assert_eq!(power_wparam_transition(PBT_APMRESUMESUSPEND), Some(false));
        assert_eq!(power_wparam_transition(PBT_APMRESUMESTANDBY), Some(false));
        // PBT_APMRESUMEAUTOMATIC (0x12) deliberately NOT mapped (spec
        // ambiguity §13.3): unattended wakes fall to the SuspendDetector.
        assert_eq!(power_wparam_transition(0x12), None);
        // Unrelated power messages (status change, query) are ignored.
        for other in [0x0u32, 0x1, 0x2, 0x3, 0x9, 0xA, 0xB, 0x8013] {
            assert_eq!(power_wparam_transition(other), None, "wparam {other:#x}");
        }
    }

    #[test]
    fn dedupe_collapses_lock_then_suspend() {
        let mut suspended = false;
        // lock fires, the following suspend is idempotent.
        assert!(dedupe_transition(&mut suspended, true));
        assert!(!dedupe_transition(&mut suspended, true));
        // first of resume/unlock ends the transition; the second drops.
        assert!(dedupe_transition(&mut suspended, false));
        assert!(!dedupe_transition(&mut suspended, false));
        assert!(!suspended);
    }

    #[test]
    fn dedupe_swallows_resume_without_prior_suspend() {
        let mut suspended = false;
        // Initial state is not-suspended: a bare resume/unlock is dropped.
        assert!(!dedupe_transition(&mut suspended, false));
        assert!(!suspended);
        // Full cycle still works afterwards.
        assert!(dedupe_transition(&mut suspended, true));
        assert!(dedupe_transition(&mut suspended, false));
    }
}
