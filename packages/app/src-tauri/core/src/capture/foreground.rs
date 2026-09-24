//! Win32 foreground polling watcher + display-name resolution.
//!
//! Owned by agent C. Ground truth:
//! `packages/core/src/capture/foreground/{win32,ForegroundWatcher,displayName}.ts`
//! and `.claude/rewrite/specs/capture-stores.md` §2. The polling design is
//! DELIBERATE (no `SetWinEventHook`): `GetForegroundWindow` +
//! `GetWindowTextW` (512 wide chars) + `GetWindowThreadProcessId` +
//! `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` +
//! `QueryFullProcessImageNameW` (1024 wide chars), every sub-step degrading
//! to null independently; UTF-16 decoded lossily. FileDescription is
//! resolved in-process (`GetFileVersionInfoW`/`VerQueryValueW`) in the
//! background, cached by lowercased-path + mtime, with the capitalized exe
//! stem as immediate fallback.
//!
//! PRIVACY: raw titles and exe paths never reach logs; failure logs are
//! fixed strings.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use parking_lot::{Condvar, Mutex};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_RESOURCE_DATA_NOT_FOUND, ERROR_RESOURCE_NAME_NOT_FOUND,
    ERROR_RESOURCE_TYPE_NOT_FOUND,
};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
};

use crate::capture::ForegroundSource;
use crate::model::{ForegroundInfo, Unsubscribe};
use crate::runtime::logger;

/// Poll cadence (ms).
pub const POLL_INTERVAL_MS: u64 = 1_000;
/// Change-event debounce (ms): rapid changes collapse; only the latest
/// change's info is delivered, once, this long after the last change.
pub const DEBOUNCE_MS: u64 = 500;

/// `GetWindowTextW` buffer capacity in wide chars (matches the TS
/// `TITLE_CHARS = 512`; longer titles truncate identically).
const TITLE_CHARS: usize = 512;
/// `QueryFullProcessImageNameW` buffer capacity in wide chars (TS
/// `PATH_CHARS = 1024`).
const PATH_CHARS: usize = 1024;

// ---------------------------------------------------------------------------
// Raw Win32 sampling (win32.ts)
// ---------------------------------------------------------------------------

/// One raw sample; every field degrades to `None` independently.
struct RawForegroundSample {
    window_title: Option<String>,
    /// Ported shape (win32.ts `RawForegroundSample`); the watcher consumes
    /// only title + path.
    #[allow(dead_code)]
    process_id: Option<u32>,
    exe_path: Option<String>,
}

/// One synchronous sample of the foreground window. Failures degrade to
/// nulls; only a null foreground HWND yields `None` for the whole sample.
/// Elevated / protected processes typically succeed for title+pid but fail
/// at `OpenProcess`/`QueryFullProcessImageNameW` -> `exe_path = None`
/// (which makes the watcher fall back to the last known info). UWP apps
/// usually resolve to `applicationframehost.exe` — that artifact is part of
/// the observable behavior; do not "fix" it.
fn sample_foreground() -> Option<RawForegroundSample> {
    // SAFETY: GetForegroundWindow takes no arguments and returns a plain
    // handle value (possibly null).
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return None;
    }

    // Window title: `len` is the count of UTF-16 code units copied
    // (excluding the terminator); decode exactly `len` code units, lossily
    // (window titles from arbitrary apps DO contain unpaired surrogates).
    // `GetWindowTextW` reads the cached title for other-process windows (no
    // cross-process SendMessage) — safe to call from the poll thread.
    // SAFETY: the buffer outlives the call; the crate derives nMaxCount
    // from the slice length (512).
    let window_title = {
        let mut title_buf = [0u16; TITLE_CHARS];
        let len = unsafe { GetWindowTextW(hwnd, &mut title_buf) };
        if len > 0 {
            Some(String::from_utf16_lossy(&title_buf[..len as usize]))
        } else {
            // A zero return is indistinguishable from "no title" — both map
            // to None (preserved behavior).
            None
        }
    };

    // Process id; the return value (thread id) is ignored.
    // SAFETY: pid points at a live u32 for the duration of the call.
    let process_id = {
        let mut pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid > 0 {
            Some(pid)
        } else {
            None
        }
    };

    // Executable path (only when a pid was obtained).
    let mut exe_path: Option<String> = None;
    if let Some(pid) = process_id {
        // SAFETY: OpenProcess returns an owned handle on success; it is
        // closed below on every path.
        if let Ok(handle) = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
            if !handle.is_invalid() {
                let mut path_buf = [0u16; PATH_CHARS];
                let mut size: u32 = PATH_CHARS as u32;
                // SAFETY: buffer and size stay alive across the call;
                // dwFlags = 0 (PROCESS_NAME_WIN32) -> Win32 path format. On
                // success `size` is the number of wide chars written
                // (excluding the terminator).
                let ok = unsafe {
                    QueryFullProcessImageNameW(
                        handle,
                        PROCESS_NAME_WIN32,
                        PWSTR(path_buf.as_mut_ptr()),
                        &mut size,
                    )
                };
                if ok.is_ok() && size > 0 {
                    exe_path = Some(String::from_utf16_lossy(&path_buf[..size as usize]));
                }
                // SAFETY: handle came from OpenProcess above. A CloseHandle
                // failure is swallowed: a handle leak is preferable to a
                // crash in the sampler.
                let _ = unsafe { CloseHandle(handle) };
            }
        }
    }

    Some(RawForegroundSample {
        window_title,
        process_id,
        exe_path,
    })
}

/// Final path component, splitting on both `\` and `/` (Node
/// `path.win32.basename` semantics for the full paths
/// `QueryFullProcessImageNameW` produces).
fn win_basename(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

// ---------------------------------------------------------------------------
// Display-name resolution (displayName.ts)
// ---------------------------------------------------------------------------

/// Cache key: `"<lowercased path>|<mtime ms as float>"`. NO eviction —
/// unbounded map, practically bounded by distinct exe paths and their
/// updates (ported invariant).
static DISPLAY_NAME_CACHE: Lazy<Mutex<HashMap<String, String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
/// In-flight dedup: prevents duplicate concurrent queries for one key.
static DISPLAY_NAME_PENDING: Lazy<Mutex<HashSet<String>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

/// Capitalized exe stem: strip a trailing `.exe` (ASCII case-insensitive)
/// from the basename, upper-case the FIRST char only; an empty stem returns
/// the input unchanged. (`"code.exe"` -> `"Code"`, `"obs64.exe"` ->
/// `"Obs64"`, `".exe"` -> `".exe"`.) Also used by the media provider for
/// exe-attributed players.
pub fn exe_stem_display_name(exe_path_or_app_id: &str) -> String {
    let base = win_basename(exe_path_or_app_id);
    let stem = if base.len() >= 4 && base[base.len() - 4..].eq_ignore_ascii_case(".exe") {
        &base[..base.len() - 4]
    } else {
        base
    };
    if stem.is_empty() {
        return exe_path_or_app_id.to_string();
    }
    let mut chars = stem.chars();
    match chars.next() {
        // JS `charAt(0).toUpperCase()` is Unicode-aware and may expand
        // (e.g. `ß` -> `SS`), matching `char::to_uppercase`.
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => stem.to_string(),
    }
}

/// `"<lowercased path>|<statSync(path).mtimeMs>"`; `None` when the file is
/// gone / inaccessible (no cache key -> fallback, no resolution started).
fn display_name_cache_key(exe_path: &str) -> Option<String> {
    let metadata = std::fs::metadata(exe_path).ok()?;
    let modified = metadata.modified().ok()?;
    let mtime_ms = match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_secs_f64() * 1000.0,
        Err(err) => -(err.duration().as_secs_f64() * 1000.0),
    };
    Some(format!("{}|{}", exe_path.to_lowercase(), mtime_ms))
}

/// Callback invoked (from the resolver thread) when a better name lands.
type ResolvedCallback = Box<dyn Fn(&str) + Send + 'static>;

/// Returns the best currently-known display name and kicks off background
/// resolution on a cache miss; `on_resolved` fires (from the resolver
/// thread) only when a non-empty FileDescription lands. Resolution results:
/// non-empty description -> cached + `on_resolved`; empty/missing
/// description -> the FALLBACK is cached, no callback; failure -> fallback
/// cached, `debug displayName: "FileDescription query failed"` (never the
/// path).
fn display_name_for(
    exe_path: Option<&str>,
    app_id: &str,
    on_resolved: Option<ResolvedCallback>,
) -> String {
    let fallback = exe_stem_display_name(app_id);
    let Some(exe_path) = exe_path else {
        return fallback;
    };
    let Some(key) = display_name_cache_key(exe_path) else {
        return fallback;
    };
    if let Some(cached) = DISPLAY_NAME_CACHE.lock().get(&key) {
        return cached.clone();
    }

    let newly_pending = DISPLAY_NAME_PENDING.lock().insert(key.clone());
    if newly_pending {
        let exe_path = exe_path.to_string();
        let fallback_for_cache = fallback.clone();
        std::thread::spawn(move || {
            match read_file_description(&exe_path) {
                Ok(Some(description)) => {
                    DISPLAY_NAME_CACHE
                        .lock()
                        .insert(key.clone(), description.clone());
                    if let Some(callback) = on_resolved {
                        callback(&description);
                    }
                }
                Ok(None) => {
                    // Resolved but empty: cache the fallback, do NOT call
                    // on_resolved.
                    DISPLAY_NAME_CACHE
                        .lock()
                        .insert(key.clone(), fallback_for_cache);
                }
                Err(()) => {
                    logger::debug("displayName", "FileDescription query failed");
                    DISPLAY_NAME_CACHE
                        .lock()
                        .insert(key.clone(), fallback_for_cache);
                }
            }
            DISPLAY_NAME_PENDING.lock().remove(&key);
        });
    }
    // Resolution is background-only; the caller always gets the fallback
    // immediately on a miss.
    fallback
}

/// Read the version-resource `FileDescription` in-process (replaces the TS
/// PowerShell one-shot). `Ok(None)` = file has no (usable) description;
/// `Err(())` = the query itself failed.
fn read_file_description(exe_path: &str) -> Result<Option<String>, ()> {
    let wide_path: Vec<u16> = exe_path.encode_utf16().chain(std::iter::once(0)).collect();

    // SAFETY: wide_path is NUL-terminated and outlives the call.
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(wide_path.as_ptr()), None) };
    if size == 0 {
        // Missing version resource == "no description" (the PS path printed
        // an empty string for these); anything else is a query failure.
        let err = unsafe { GetLastError() };
        return if err == ERROR_RESOURCE_DATA_NOT_FOUND
            || err == ERROR_RESOURCE_TYPE_NOT_FOUND
            || err == ERROR_RESOURCE_NAME_NOT_FOUND
        {
            Ok(None)
        } else {
            Err(())
        };
    }

    // u16 backing storage guarantees 2-byte alignment for the wide strings
    // VerQueryValueW points into.
    let mut block = vec![0u16; size as usize / 2 + 1];
    // SAFETY: block is at least `size` bytes; the path is NUL-terminated.
    unsafe {
        GetFileVersionInfoW(
            PCWSTR(wide_path.as_ptr()),
            None,
            size,
            block.as_mut_ptr().cast(),
        )
    }
    .map_err(|_| ())?;

    // Translation-block heuristic (spec §2.3): take the FIRST
    // `\VarFileInfo\Translation` (lang, codepage) pair; .NET-style
    // neutral/en fallbacks follow (cosmetic only, spec ambiguity §13.4).
    let mut candidates: Vec<(u16, u16)> = Vec::new();
    if let Some(first) = first_translation_pair(&block) {
        candidates.push(first);
    }
    for fallback in [(0x0409, 0x04B0), (0x0409, 0x04E4), (0x0409, 0x0000)] {
        if !candidates.contains(&fallback) {
            candidates.push(fallback);
        }
    }

    for (lang, codepage) in candidates {
        let sub_block = format!(
            "\\StringFileInfo\\{:04x}{:04x}\\FileDescription",
            lang, codepage
        );
        if let Some(value) = query_wide_string(&block, &sub_block) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Ok(Some(trimmed.to_string()));
            }
        }
    }
    // Version resource present but no non-empty FileDescription.
    Ok(None)
}

/// First `(language, codepage)` pair of `\VarFileInfo\Translation`, if any.
fn first_translation_pair(block: &[u16]) -> Option<(u16, u16)> {
    let sub: Vec<u16> = "\\VarFileInfo\\Translation"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut len: u32 = 0;
    // SAFETY: block outlives ptr's use below; VerQueryValueW returns a
    // pointer INTO block.
    let ok = unsafe {
        VerQueryValueW(
            block.as_ptr().cast(),
            PCWSTR(sub.as_ptr()),
            &mut ptr,
            &mut len,
        )
    };
    if !ok.as_bool() || ptr.is_null() || len < 4 {
        return None;
    }
    // SAFETY: the value is an array of DWORD-sized pairs at least `len`
    // bytes long; read unaligned to be independent of interior padding.
    unsafe {
        let words = ptr.cast::<u16>();
        Some((
            std::ptr::read_unaligned(words),
            std::ptr::read_unaligned(words.add(1)),
        ))
    }
}

/// Query one string value out of a version-info block; lossy UTF-16 decode
/// truncated at the first NUL.
fn query_wide_string(block: &[u16], sub_block: &str) -> Option<String> {
    let sub: Vec<u16> = sub_block.encode_utf16().chain(std::iter::once(0)).collect();
    let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut len: u32 = 0;
    // SAFETY: block outlives ptr's use; on success ptr points into block
    // and `len` is the value length in wide characters.
    let ok = unsafe {
        VerQueryValueW(
            block.as_ptr().cast(),
            PCWSTR(sub.as_ptr()),
            &mut ptr,
            &mut len,
        )
    };
    if !ok.as_bool() || ptr.is_null() || len == 0 {
        return None;
    }
    // SAFETY: read `len` u16s one by one (unaligned-safe) from within the
    // block; stop at the terminating NUL if the resource includes it.
    let mut units: Vec<u16> = Vec::with_capacity(len as usize);
    unsafe {
        let words = ptr.cast::<u16>();
        for i in 0..len as usize {
            let unit = std::ptr::read_unaligned(words.add(i));
            if unit == 0 {
                break;
            }
            units.push(unit);
        }
    }
    Some(String::from_utf16_lossy(&units))
}

// ---------------------------------------------------------------------------
// ForegroundWatcher (ForegroundWatcher.ts)
// ---------------------------------------------------------------------------

/// Change key over the (appId, windowTitle) pair. The separator is the
/// TWO-CHARACTER literal backslash + digit zero (`\` `0`) — ported verbatim
/// from the TS template literal `` `${appId}\\0${title ?? ""}` `` (spec
/// ambiguity §13.10; harmless since basenames cannot contain backslashes).
/// `displayName` and `exePath` are deliberately excluded from the key.
fn change_key(info: Option<&ForegroundInfo>) -> String {
    match info {
        None => String::new(),
        Some(info) => format!(
            "{}\\0{}",
            info.app_id,
            info.window_title.as_deref().unwrap_or("")
        ),
    }
}

/// Change-detection state (TS fields `lastKey`/`lastInfo` + the debounce
/// timer expressed as a generation counter: bumping the generation cancels
/// the pending delivery, exactly like `clearTimeout`).
struct WatchState {
    last_key: Option<String>,
    last_info: Option<ForegroundInfo>,
    debounce_generation: u64,
}

/// Registers `key` against the state; `true` means a (re)scheduled
/// debounce delivery (the key changed). Bumps the generation so any
/// pending delivery is cancelled.
fn register_change_key(state: &mut WatchState, key: String) -> bool {
    if state.last_key.as_deref() == Some(key.as_str()) {
        return false;
    }
    state.last_key = Some(key);
    state.debounce_generation += 1;
    true
}

type ChangeListener = Arc<dyn Fn(Option<ForegroundInfo>) + Send + Sync>;

struct PollControl {
    running: bool,
    /// Bumped on every start; a poll thread exits when its epoch is stale,
    /// so an interleaved stop()+start() can never leave two loops alive or
    /// wedge stop()'s join.
    epoch: u64,
    join: Option<JoinHandle<()>>,
}

struct WatcherInner {
    state: Mutex<WatchState>,
    listeners: Mutex<Vec<(u64, ChangeListener)>>,
    next_listener_id: AtomicU64,
    control: Mutex<PollControl>,
    wake: Condvar,
    /// Serializes whole poll passes (sample -> key compare -> schedule) so
    /// concurrent polls (poll thread + resolver-triggered re-poll) cannot
    /// interleave; TS was single-threaded.
    poll_pass: Mutex<()>,
}

/// Polling foreground watcher.
///
/// Change events fire when the (appId, windowTitle) pair changes —
/// title changes within the same app DO emit; displayName resolution alone
/// does NOT (it refreshes `current()` via a re-poll without an event). A
/// `None` info event is possible (no valid foreground since construction).
/// `stop()` keeps the last info (so `current()` still serves it) but clears
/// the change key (a restart re-emits the first change). Listeners survive
/// stop/start.
pub struct ForegroundWatcher {
    inner: Arc<WatcherInner>,
}

impl ForegroundWatcher {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(WatcherInner {
                state: Mutex::new(WatchState {
                    last_key: None,
                    last_info: None,
                    debounce_generation: 0,
                }),
                listeners: Mutex::new(Vec::new()),
                next_listener_id: AtomicU64::new(1),
                control: Mutex::new(PollControl {
                    running: false,
                    epoch: 0,
                    join: None,
                }),
                wake: Condvar::new(),
                poll_pass: Mutex::new(()),
            }),
        }
    }

    /// Idempotent (no-op while running, without re-logging); spawns the
    /// 1 s poll thread which runs one poll immediately. Logs
    /// `info foreground: "watcher started"`.
    pub fn start(&self) {
        {
            let mut control = self.inner.control.lock();
            if control.join.is_some() {
                return;
            }
            control.running = true;
            control.epoch += 1;
            let epoch = control.epoch;
            let inner = Arc::clone(&self.inner);
            control.join = Some(std::thread::spawn(move || poll_loop(inner, epoch)));
        }
        logger::info("foreground", "watcher started");
    }

    /// Stops the poll thread and cancels any pending debounce delivery
    /// promptly (the 2 s bounded shutdown depends on it). Resets the change
    /// key but NOT the last info — `current()` keeps serving the stale
    /// value and a restart re-emits the first change. Logs
    /// `info foreground: "watcher stopped"` (even when it was not running,
    /// as in TS).
    pub fn stop(&self) {
        let join = {
            let mut control = self.inner.control.lock();
            control.running = false;
            self.inner.wake.notify_all();
            control.join.take()
        };
        if let Some(handle) = join {
            let _ = handle.join();
        }
        {
            let mut state = self.inner.state.lock();
            // Equivalent of clearTimeout(debounceTimer): the pending
            // delivery (if any) sees a newer generation and drops itself.
            state.debounce_generation += 1;
            state.last_key = None;
        }
        logger::info("foreground", "watcher stopped");
    }

    /// Register a change listener (debounced; `None` = no valid
    /// foreground). Listeners survive stop/start.
    pub fn on_change(
        &self,
        callback: Box<dyn Fn(Option<ForegroundInfo>) + Send + Sync>,
    ) -> Unsubscribe {
        let id = self.inner.next_listener_id.fetch_add(1, Ordering::Relaxed);
        self.inner.listeners.lock().push((id, Arc::from(callback)));
        let inner = Arc::clone(&self.inner);
        Unsubscribe::new(move || {
            inner
                .listeners
                .lock()
                .retain(|(entry_id, _)| *entry_id != id);
        })
    }

    /// FRESH sample per call (also used directly by the privacy
    /// CaptureService at delivery time); a failed/pathless sample falls
    /// back to the last known info — why transient failures (elevated
    /// window focused, secure desktop, ...) keep the previous app visible.
    pub fn current(&self) -> Option<ForegroundInfo> {
        current_shared(&self.inner)
    }
}

impl Default for ForegroundWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl ForegroundSource for ForegroundWatcher {
    fn current(&self) -> Option<ForegroundInfo> {
        ForegroundWatcher::current(self)
    }
}

/// `current()` shared by the public method and the poll pass.
fn current_shared(inner: &Arc<WatcherInner>) -> Option<ForegroundInfo> {
    let raw = sample_foreground();
    let Some(raw) = raw else {
        return inner.state.lock().last_info.clone();
    };
    let Some(exe_path) = raw.exe_path else {
        return inner.state.lock().last_info.clone();
    };
    let app_id = win_basename(&exe_path).to_lowercase();
    if app_id.is_empty() {
        return inner.state.lock().last_info.clone();
    }
    // onResolved = re-poll (TS `() => this.poll()`): refreshes lastInfo so
    // later current() calls see the resolved name; no change event fires
    // because the key excludes displayName.
    let poll_target = Arc::clone(inner);
    let display_name = display_name_for(
        Some(&exe_path),
        &app_id,
        Some(Box::new(move |_name: &str| {
            poll_once(&poll_target);
        })),
    );
    let info = ForegroundInfo {
        app_id,
        exe_path: Some(exe_path),
        display_name,
        window_title: raw.window_title,
    };
    inner.state.lock().last_info = Some(info.clone());
    Some(info)
}

/// One change-detection pass: sample, compare the change key, schedule the
/// debounced delivery. Runnable from the poll thread AND from resolver
/// threads (TS poll() was plain re-entrant single-threaded code).
fn poll_once(inner: &Arc<WatcherInner>) {
    let _pass = inner.poll_pass.lock();
    let info = current_shared(inner);
    let key = change_key(info.as_ref());
    let generation = {
        let mut state = inner.state.lock();
        if !register_change_key(&mut state, key) {
            return;
        }
        state.debounce_generation
    };
    // Debounce timer: a short-lived thread that self-cancels when a newer
    // change (or stop()) bumped the generation. Delivers the info captured
    // at THIS change, exactly once.
    let inner = Arc::clone(inner);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(DEBOUNCE_MS));
        if inner.state.lock().debounce_generation != generation {
            return;
        }
        let listeners: Vec<ChangeListener> = inner
            .listeners
            .lock()
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect();
        for listener in listeners {
            listener(info.clone());
        }
    });
}

/// Dedicated poll thread: one immediate poll, then every 1000 ms until
/// stopped (the condvar makes stop() prompt; a stale epoch means a newer
/// start() superseded this loop).
fn poll_loop(inner: Arc<WatcherInner>, epoch: u64) {
    loop {
        poll_once(&inner);
        let mut control = inner.control.lock();
        let deadline = Instant::now() + Duration::from_millis(POLL_INTERVAL_MS);
        loop {
            if !control.running || control.epoch != epoch {
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let _ = inner.wake.wait_for(&mut control, deadline - now);
        }
        if !control.running || control.epoch != epoch {
            return;
        }
        drop(control);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(app_id: &str, title: Option<&str>) -> ForegroundInfo {
        ForegroundInfo {
            app_id: app_id.to_string(),
            exe_path: Some(format!("C:\\apps\\{app_id}")),
            display_name: exe_stem_display_name(app_id),
            window_title: title.map(str::to_string),
        }
    }

    #[test]
    fn exe_stem_display_name_vectors() {
        // Pinned vectors from capture-stores.md §11.
        assert_eq!(exe_stem_display_name("code.exe"), "Code");
        assert_eq!(exe_stem_display_name("spotify.exe"), "Spotify");
        assert_eq!(exe_stem_display_name("obs64.exe"), "Obs64");
        assert_eq!(exe_stem_display_name("x"), "X");
        assert_eq!(
            exe_stem_display_name("C:\\Tools\\Some.Tool.exe"),
            "Some.Tool"
        );
        // Empty stem -> the INPUT unchanged (not the basename).
        assert_eq!(exe_stem_display_name(".exe"), ".exe");
        assert_eq!(exe_stem_display_name("C:\\Tools\\.exe"), "C:\\Tools\\.exe");
        assert_eq!(exe_stem_display_name(""), "");
    }

    #[test]
    fn exe_stem_strip_is_ascii_case_insensitive_and_single() {
        assert_eq!(exe_stem_display_name("CODE.EXE"), "CODE");
        assert_eq!(exe_stem_display_name("code.eXe"), "Code");
        // Only the trailing `.exe` is stripped, once.
        assert_eq!(exe_stem_display_name("tool.exe.exe"), "Tool.exe");
        // Forward slashes split too (Node win32 basename).
        assert_eq!(exe_stem_display_name("C:/x/y.exe"), "Y");
        // `.exe` elsewhere is untouched.
        assert_eq!(exe_stem_display_name("notexe"), "Notexe");
    }

    #[test]
    fn change_key_uses_literal_backslash_zero_separator() {
        // "code.exe" + "\" + "0" + title — a TWO-character separator, not
        // a NUL byte (spec ambiguity §13.10).
        let key = change_key(Some(&info("code.exe", Some("main.rs — repo"))));
        assert_eq!(key, "code.exe\\0main.rs — repo");
        assert_eq!(&key[8..10], "\\0");
        assert_eq!(key.as_bytes()[8], b'\\');
        assert_eq!(key.as_bytes()[9], b'0');

        // Null info -> "".
        assert_eq!(change_key(None), "");
        // Null title -> appId + separator.
        assert_eq!(change_key(Some(&info("code.exe", None))), "code.exe\\0");
    }

    #[test]
    fn register_change_key_transitions() {
        let mut state = WatchState {
            last_key: None,
            last_info: None,
            debounce_generation: 0,
        };
        // First poll emits even for the empty (null-info) key: lastKey was
        // null, "" differs.
        assert!(register_change_key(&mut state, String::new()));
        assert_eq!(state.debounce_generation, 1);
        // Same key again -> no event.
        assert!(!register_change_key(&mut state, String::new()));
        assert_eq!(state.debounce_generation, 1);
        // App/title change -> event; generation bump cancels the pending
        // delivery (debounce collapse).
        assert!(register_change_key(&mut state, "code.exe\\0a".to_string()));
        assert!(register_change_key(&mut state, "code.exe\\0b".to_string()));
        assert_eq!(state.debounce_generation, 3);
        // Title-only change within the same app DOES emit.
        assert!(register_change_key(&mut state, "code.exe\\0c".to_string()));
        // stop() equivalent: lastKey cleared -> the same key re-emits after
        // a restart.
        state.last_key = None;
        state.debounce_generation += 1;
        assert!(register_change_key(&mut state, "code.exe\\0c".to_string()));
    }

    #[test]
    fn debounce_generation_cancels_stale_delivery() {
        let mut state = WatchState {
            last_key: None,
            last_info: None,
            debounce_generation: 0,
        };
        assert!(register_change_key(&mut state, "a\\0".to_string()));
        let scheduled = state.debounce_generation;
        // A rapid second change within the window invalidates the first.
        assert!(register_change_key(&mut state, "b\\0".to_string()));
        assert_ne!(state.debounce_generation, scheduled);
        // Only the latest scheduled generation may deliver.
        assert_eq!(state.debounce_generation, 2);
    }

    #[test]
    fn win_basename_splits_on_both_separators() {
        assert_eq!(win_basename("C:\\a\\b\\Code.exe"), "Code.exe");
        assert_eq!(win_basename("C:/a/b/code.exe"), "code.exe");
        assert_eq!(win_basename("code.exe"), "code.exe");
        // Trailing separator -> empty basename -> the watcher falls back to
        // lastInfo (appId empty guard).
        assert_eq!(win_basename("C:\\dir\\"), "");
    }
}
