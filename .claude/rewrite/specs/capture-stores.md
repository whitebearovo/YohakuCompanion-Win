# Spec: Capture Layer, System Events, Runtime Helpers, Persistence

Behavior extracted from `packages/core` (TypeScript, behavioral ground truth)
for the native Rust rewrite. Audience: Rust implementers using the `windows`
crate directly (Win32 + WinRT `Windows.Media.Control`). Observable semantics
and on-disk / credential formats must be preserved exactly.

Source files covered:

- `packages/core/src/capture/foreground/win32.ts`
- `packages/core/src/capture/foreground/ForegroundWatcher.ts`
- `packages/core/src/capture/foreground/displayName.ts`
- `packages/core/src/capture/media/SmtcPowershellProvider.ts` (+ `packages/core/resources/ps/smtc-provider.ps1`)
- `packages/core/src/capture/media/SmtcNpmProvider.ts`
- `packages/core/src/capture/media/NdjsonProcessHost.ts`
- `packages/core/src/capture/media/selectProvider.ts`
- `packages/core/src/capture/system/PsSystemEventsProvider.ts` (+ `packages/core/resources/ps/system-events.ps1`)
- `packages/core/src/capture/types.ts`
- `packages/core/src/runtime/suspendDetector.ts`
- `packages/core/src/runtime/logger.ts`
- `packages/core/src/store/configStore.ts`
- `packages/core/src/store/sequenceStore.ts`
- `packages/core/src/store/credentials.ts`
- `packages/core/src/store/keyringCredentialStore.ts`
- `packages/core/src/store/dpapiCredentialStore.ts`
- `packages/core/package.json`
- Wiring context: `packages/core/src/main.ts`, `packages/core/src/privacy/captureService.ts`,
  `packages/shared/src/{privacy,status}.ts`, `packages/core/test/ipc/server.test.ts`

All timestamps below are epoch milliseconds (`Date.now()`), all media
durations/positions are float seconds, unless stated otherwise.

---

## 1. Raw capture data types (contract with the privacy pipeline)

From `src/capture/types.ts`. These are RAW values: they must never reach the
network, persistence, or the UI without passing the sanitizers.

```ts
interface ForegroundInfo {
  appId: string;              // lowercased executable file name, e.g. "code.exe"
  exePath: string | null;     // full Win32 path, e.g. "C:\\...\\Code.exe"
  displayName: string;        // FileDescription or exe-stem fallback
  windowTitle: string | null; // raw title, may be null
}

interface MediaSnapshot {
  appId: string | null;                // lowercased exe name iff AUMID looks like one, else null
  sourceAppUserModelId: string | null; // trimmed AUMID, null when empty
  playerDisplayName: string | null;
  kind: MediaKind;                     // "music" | "podcast" | "video" | "unknown"
  title: string | null;                // trimmed, empty -> null
  artist: string | null;               // trimmed, empty -> null
  album: string | null;                // trimmed, empty -> null
  playing: boolean;
  durationSeconds: number | null;      // float seconds
  positionSeconds: number | null;      // float seconds (extrapolated, see 3.4)
  sampledAt: number;                   // epoch ms at which positionSeconds was (re)computed
}

interface MediaProvider {
  readonly kind: "npm" | "powershell";   // Rust: single provider, kind "winrt"
  start(): Promise<void>;
  stop(): Promise<void>;
  getSnapshot(options?: { timeoutMs?: number }): Promise<MediaSnapshot | null>;
  onSemanticChange(callback: () => void): () => void;  // returns unsubscribe
  healthy(): boolean;
}

interface SystemEventsProvider {
  start(): Promise<void>;
  stop(): Promise<void>;
  onLockOrSleep(callback: () => void): () => void;
  onUnlockOrResume(callback: () => void): () => void;
}
```

Note: `getSnapshot`'s `options.timeoutMs` parameter is declared but ignored by
both TS providers. The actual timeout is enforced by the caller (section 3.8).

---

## 2. Foreground capture

### 2.1 Win32 sampling (`win32.ts`)

Single FFI convergence point via koffi 3.1.2. Synchronous calls only; no FFI
callbacks anywhere (message-pump-dependent hooks like `SetWinEventHook` are
deliberately avoided; the design polls). The Rust port keeps the polling
design (per ARCHITECTURE.md).

Constants:

- `PROCESS_QUERY_LIMITED_INFORMATION = 0x1000`
- `TITLE_CHARS = 512` (buffer 1024 bytes)
- `PATH_CHARS = 1024` (buffer 2048 bytes)

Exact call sequence of `sampleForeground() -> RawForegroundSample | null`:

1. `hwnd = GetForegroundWindow()` (user32). If `hwnd` is null/0, return `null`
   (the whole sample, not a partial one).
2. Window title: `len = GetWindowTextW(hwnd, titleBuf, TITLE_CHARS)` with a
   512-wide-char buffer. `windowTitle = len > 0 ? decode(titleBuf, len chars) : null`.
   `len` is the count of UTF-16 code units copied (excluding the terminator);
   decode exactly `len` code units as UTF-16LE (lossy — see traps). Any thrown
   error -> `windowTitle = null`.
3. Process id: `GetWindowThreadProcessId(hwnd, &pid)`. Return value (thread id)
   is ignored. `processId = pid > 0 ? pid : null`. Errors -> `null`.
4. Executable path (only when `processId !== null`):
   - `handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid)`.
   - If handle non-null/non-zero:
     `size = PATH_CHARS; ok = QueryFullProcessImageNameW(handle, 0, pathBuf, &size)`
     (`dwFlags = 0` -> Win32 path format, e.g. `C:\...`). On success `size` is
     the number of wide chars written (excluding terminator);
     `exePath = decode(pathBuf, size chars)`. On failure or `size == 0` ->
     `exePath = null`.
   - Errors anywhere -> `exePath = null`.
   - `CloseHandle(handle)` in a `finally`; a CloseHandle failure is swallowed
     (comment: "handle leak is preferable to a crash in the sampler").
5. Return `{ windowTitle, processId, exePath }`.

Failure philosophy: every sub-step degrades to `null` independently; only a
null foreground HWND yields a null sample. Elevated / protected processes
typically succeed for title+pid but may fail at OpenProcess/QueryFullProcessImageNameW
-> `exePath = null` (which makes the watcher fall back to the last known info,
see 2.2). UWP apps usually resolve to `applicationframehost.exe` (host process)
— this artifact is part of the observable behavior; do not "fix" it.

### 2.2 `ForegroundWatcher`

Constants: `POLL_INTERVAL_MS = 1000`, `DEBOUNCE_MS = 500`.

State: `timer`, `debounceTimer`, `lastKey: string | null`,
`lastInfo: ForegroundInfo | null`, `listeners: Set<(info) => void>`.

- `start()`: idempotent (no-op if timer exists). Schedules `poll()` every
  1000 ms (Node timer `unref`ed — must not keep the process alive), then runs
  one immediate `poll()`. Logs `info` `foreground: "watcher started"`.
- `stop()`: clears interval + pending debounce timer, sets both to null,
  resets `lastKey = null` (NOT `lastInfo` — it survives stop, so `current()`
  can still serve the last value, and a restart re-emits the first change
  because `lastKey` was cleared). Logs `info` `foreground: "watcher stopped"`.
- `onChange(cb)`: adds to the listener set; returns an unsubscribe closure.
  Listeners survive stop/start.

`current(): ForegroundInfo | null` (also called directly by the privacy
CaptureService at delivery time — fresh sample per call):

1. `raw = sampleForeground()`.
2. If `raw === null` OR `raw.exePath === null` -> return `this.lastInfo`
   (stale fallback; initially null). This is why transient failures (elevated
   window focused, secure desktop, etc.) keep the previous app visible.
3. `appId = basename(raw.exePath).toLowerCase()` (Node `path.basename` on
   win32 splits on both `\` and `/`; take the final component). If the
   basename is empty -> return `this.lastInfo`.
4. Build `info = { appId, exePath: raw.exePath, displayName: displayNameFor(raw.exePath, appId, () => this.poll()), windowTitle: raw.windowTitle }`.
5. `this.lastInfo = info`; return it.

`poll()` (change detection + debounce):

1. `info = this.current()`.
2. Change key: `key = info === null ? "" : info.appId + "\\0" + (info.windowTitle ?? "")`.
   IMPORTANT: in the source this is `` `${info.appId}\\0${info.windowTitle ?? ""}` ``
   inside a template literal — the separator is the two-character string
   backslash + digit zero (`\` `0`), NOT a NUL byte. `displayName` and
   `exePath` are deliberately excluded from the key.
3. If `key === lastKey` -> return (no event).
4. `lastKey = key` immediately; cancel any pending debounce timer; schedule a
   new one for 500 ms that notifies every listener with the `info` captured at
   this change (may be null). Timer `unref`ed.
   - Consequence: rapid changes within the debounce window collapse; only the
     latest change's `info` is delivered, once, 500 ms after the last change.
   - A `null` info event is possible (e.g. no valid foreground after
     previously having one -> key transitions to `""`).

What constitutes a change event: the (appId, windowTitle) pair changed —
window-title changes within the same app DO emit; displayName resolution alone
does NOT (see 2.3).

Consumer wiring (main.ts): on each change event, main updates `recentAppIds`
(MRU, most-recent first, dedup by removal, capped at 10 — only when
`info !== null`), calls `service.coordinator.requestFreshSnapshot()` and
broadcasts IPC state. The media provider's `onSemanticChange` also calls
`requestFreshSnapshot()`.

### 2.3 Display name resolution (`displayName.ts`)

Authoritative source: the executable's version-resource `FileDescription`.
Until it resolves (or when it fails), the capitalized exe stem is used.

- `exeStemDisplayName(exePathOrAppId)`:
  1. `base = basename(input)` with the suffix regex `/\.exe$/i` stripped.
  2. If `base` is empty -> return the input unchanged.
  3. Return `base` with the first character upper-cased
     (`base.charAt(0).toUpperCase() + base.slice(1)`), rest untouched.
  - Examples: `"code.exe"` -> `"Code"`, `"spotify.exe"` -> `"Spotify"`,
    `"obs64.exe"` -> `"Obs64"`.

- Cache: module-global `Map<string, string>`, key
  `` `${exePath.toLowerCase()}|${statSync(exePath).mtimeMs}` `` — lowercased
  full path + file mtime in float milliseconds. NO eviction (unbounded map;
  practically bounded by distinct exe paths and their updates). If `statSync`
  throws (file gone, access denied) -> no cache key -> return fallback and do
  not start resolution.
- In-flight dedup: a `pending` set of keys prevents duplicate concurrent
  queries for the same key.

- `displayNameFor(exePath, appId, onResolved?)`:
  1. `fallback = exeStemDisplayName(appId)`.
  2. `exePath === null` -> return fallback.
  3. Compute cache key; on failure return fallback.
  4. Cache hit -> return cached value.
  5. Cache miss and not pending: mark pending; asynchronously resolve
     FileDescription:
     - success with non-empty description: cache it; invoke `onResolved(name)`.
     - success but empty output: cache the FALLBACK; do NOT call `onResolved`.
     - failure/rejection: cache the FALLBACK (no `onResolved`).
     - always: clear the pending mark.
  6. Return `fallback` immediately (resolution is background-only).

- TS resolution mechanism (PowerShell one-shot; the Rust port must NOT
  inherit the child process — read the version resource directly via
  `GetFileVersionInfoSizeW` / `GetFileVersionInfoW` / `VerQueryValueW`):
  `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command
  "& { param([string]$p); (Get-Item -LiteralPath $p).VersionInfo.FileDescription }" <exePath>`
  with `windowsHide: true`, 10 s timeout. Stdout is trimmed; empty -> null
  (meaning "no description"). On error logs `debug`
  `displayName: "FileDescription query failed"` (never the path).
- `onResolved` in the watcher is `() => this.poll()` — it triggers a re-poll
  which refreshes `lastInfo` (so subsequent `current()` calls and snapshots
  see the resolved name), but since the change key excludes displayName, no
  change event fires just because the name resolved.

Rust note on FileDescription: .NET `FileVersionInfo` picks a language block
(first translation from `\VarFileInfo\Translation`, with neutral/en fallbacks).
Match with `VerQueryValueW(L"\\StringFileInfo\\{lang:04x}{cp:04x}\\FileDescription")`
using the first translation pair; treat empty/missing as "no description".

---

## 3. Media / SMTC

The TS core had two providers. For the Rust port there is ONE WinRT provider
(`kind: "winrt"`), but its observable semantics come from the providers below.
Per ARCHITECTURE.md, the AUTHORITATIVE semantics are the PowerShell provider's
(`SmtcPowershellProvider.ts` + `smtc-provider.ps1`); npm-provider divergences
are listed in 3.5 and must NOT be inherited unless noted.

### 3.1 Session access (authoritative, from `smtc-provider.ps1`)

- Manager: `Windows.Media.Control.GlobalSystemMediaTransportControlsSessionManager::RequestAsync()`
  (awaited; the PS script bounds every WinRT await at 5 s and throws
  "WinRT await timeout" past it).
- Current-session selection rule: `manager.GetCurrentSession()` — the single
  session the system believes the user most likely wants to control. No
  enumeration, no custom ranking. `null` -> no session.
- Per session read:
  - `props = session.TryGetMediaPropertiesAsync()` (async) ->
    `props.Title`, `props.Artist`, `props.AlbumTitle`.
  - `timeline = session.GetTimelineProperties()` (sync) ->
    `duration = timeline.EndTime.TotalSeconds` (double; note: `EndTime`, NOT
    `EndTime - StartTime`), `position = timeline.Position.TotalSeconds`
    (double), `updatedAt = timeline.LastUpdatedTime.ToUnixTimeMilliseconds()`
    (i64 epoch ms).
  - `playback = session.GetPlaybackInfo()` (sync) ->
    `playing = ((int)playback.PlaybackStatus == 4)`;
    kind from `playback.PlaybackType` (nullable `IReference<MediaPlaybackType>`):
    value `1` -> `"music"`, `3` -> `"video"`, anything else (incl. null,
    0=Unknown, 2=Image) -> `"unknown"`.
  - `sourceAppId = session.SourceAppUserModelId` (string).
- WinRT enum values (for the Rust port):
  - `GlobalSystemMediaTransportControlsSessionPlaybackStatus`:
    Closed=0, Opened=1, Changing=2, Stopped=3, Playing=4, Paused=5.
    Only `Playing (4)` maps to `playing: true`.
  - `MediaPlaybackType`: Unknown=0, Music=1, Image=2, Video=3.
- `"podcast"` is in the `MediaKind` enum for cross-platform parity but is
  never produced on Windows (no SMTC playback type maps to it).

### 3.2 PS helper cadence and NDJSON shapes (PowerShell-specific; do NOT port)

The helper polls every 1000 ms (`Start-Sleep -Milliseconds 1000`) and writes
one compact JSON line to stdout when the serialized payload changed since the
previous line (position changes count, so at most ~1 line/s while playing):

```
{"type":"media","session":{"sourceAppId":"...","title":"...","artist":"...","album":"...","kind":"music","playing":true,"duration":259.0,"position":217.228,"updatedAt":1740000000000},"at":<epoch ms>}
{"type":"media","session":null,"at":<epoch ms>}
{"type":"heartbeat","at":<epoch ms>}
```

Heartbeat every >= 30 s. On any read error the helper re-acquires the manager
(`RequestAsync` again) and swallows the error. These are polling artifacts;
the Rust provider reads WinRT directly and reacts to events instead.

### 3.3 `SmtcPowershellProvider` (Node side; observable semantics to preserve)

- `kind = "powershell"`. Hosted by `NdjsonProcessHost` with scope `"media-ps"`.
- `start()` spawns the helper, logs `info` `media: "powershell SMTC provider started"`;
  `stop()` kills it, logs `"... stopped"`. `healthy()` = host healthy (3.6).
- Message handling: only `type === "media"` frames are consumed. The frame's
  `session` (or null) is cached with its arrival time.
- Snapshot staleness: `getSnapshot()` returns null when there is no cached
  frame OR the cached frame is older than 10 000 ms ("helper stalled — treat
  as unknown"). Otherwise it converts the cached frame at call time (position
  extrapolation makes it fresh). The Rust provider performs a FRESH WinRT read
  per `get_snapshot` instead; no staleness window needed.
- Semantic-change event: on every incoming media frame compute
  `key = session === null ? "" : JSON.stringify([sourceAppId, title, artist, album, playing])`
  and fire all `onSemanticChange` listeners when the key differs from the
  previous one. So semantic changes are: source app changed, any of
  title/artist/album changed, play/pause boolean changed, session
  appeared/disappeared. Position/duration/kind changes alone are NOT semantic.

### 3.4 Frame -> `MediaSnapshot` mapping (authoritative)

Given a frame `f` and `now = Date.now()`:

1. `source = trim(f.sourceAppId ?? "")`.
2. Null gate: if `source` is empty AND trimmed title is empty -> return null
   (meaningless session).
3. appId heuristic (EXACT): `appId = /\.exe$/i.test(source) ? source.toLowerCase() : null`
   — if the trimmed AUMID ends with `.exe` (case-insensitive), the whole
   trimmed AUMID lowercased is the appId; otherwise null.
   Examples: `"Spotify.exe"` -> `"spotify.exe"`;
   `"Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"` -> null.
4. `playing = (f.playing === true)`.
5. `durationSeconds`: `f.duration` when finite and `> 0`, else null.
6. `positionSeconds`: start from `f.position` when finite and `>= 0`, else null.
   Extrapolation (MUST keep — SMTC timeline updates are sparse): if position
   is non-null AND `playing` AND `f.updatedAt` is finite:
   `elapsed = (now - f.updatedAt) / 1000`; if `0 < elapsed < 21600` (6 h),
   `position += elapsed`; then if duration non-null and `position > duration`,
   clamp to duration.
7. `kind`: pass through when in {"music","podcast","video","unknown"}, else
   `"unknown"`.
8. `sourceAppUserModelId = source.length > 0 ? source : null`.
9. `playerDisplayName`: if `appId !== null` -> `exeStemDisplayName(appId)`
   (e.g. `"spotify.exe"` -> `"Spotify"`); else `toNull(source)` — the full
   trimmed AUMID string, or null when empty.
10. `title` / `artist` / `album`: trimmed; empty -> null.
11. `sampledAt = now` (the epoch ms at which position was computed).

### 3.5 npm provider (`SmtcNpmProvider`) — divergences only

In-process SMTC via `@coooookies/windows-smtc-monitor` 1.0.12 (napi-rs
prebuilt; Windows 10 1809+). Units per its README: timeline position/duration
float seconds; `lastUpdatedTime` epoch ms; `sourceAppId` = exe name or AUMID.
`playbackStatus`/`playbackType` are the raw WinRT enum ints.

- `start()`: dynamic import, then a PROBE native call
  (`SMTCMonitor.getCurrentMediaSession()`) must succeed before the provider is
  considered healthy; then constructs a monitor and subscribes to:
  `session-media-changed`, `session-playback-changed`, `session-added`,
  `session-removed`, `current-session-changed`. Deliberately NOT
  `session-timeline-changed` ("progress ticks are not semantic changes and
  must not trigger refresh storms"). Sets healthy, logs
  `info` `media: "npm SMTC provider started"`.
- On each subscribed event it re-reads `getCurrentMediaSession()` and computes
  the semantic key `JSON.stringify([sourceAppId, media.title, media.artist,
  media.albumTitle, playback.playbackStatus])` — NOTE: the raw playbackStatus
  int, not the `playing` bool (a Paused(5) -> Stopped(3) transition is
  semantic here but not in the PS provider). Errors during the re-read are
  swallowed (no event). Fires listeners on key change (including to/from `""`).
- `getSnapshot()`: fresh native call every time; a thrown error marks the
  provider unhealthy, logs `warn` `media: "npm SMTC snapshot failed: <Error.name>"`,
  returns null.
- Mapping differences vs 3.4:
  - No null gate: even an empty source + empty title still yields a snapshot
    object (fields null). (PS/authoritative returns null.)
  - `duration = info.timeline.duration > 0 ? ... : null` (no finiteness check).
  - `playerDisplayName` fallback for non-exe AUMIDs:
    `toNull(source.split("!")[0]?.split(".").pop() ?? source)` — e.g.
    `"Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"` ->
    `"ZuneMusic_8wekyb3d8bbwe"`. (PS/authoritative: the FULL AUMID.)
  - Kind map identical (`{1: "music", 3: "video"}`, else `"unknown"`);
    `PLAYING = 4`.
- `stop()`: `monitor.destroy()`, unhealthy, logs `"npm SMTC provider stopped"`.
- `healthy()`: internal flag (true after successful start, false after stop or
  a failed snapshot).

Rust event note: `MediaPropertiesChanged` / `PlaybackInfoChanged` are
per-session WinRT events; subscribe on the current session and re-subscribe on
`CurrentSessionChanged` / `SessionsChanged`. Fire the semantic callback only
when the current-session semantic tuple actually changed (dedupe with a key
like 3.3). Never subscribe to `TimelinePropertiesChanged`.

### 3.6 `NdjsonProcessHost` (process supervision; PS-specific, do not port literally)

Documented because its health semantics leak into `MediaProviderHealth`:

- Spawn: `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File <script>`,
  stdio `["ignore","pipe","pipe"]`, `windowsHide: true`.
- Stdout is read line-wise; each line (ANY line) re-arms a watchdog timer
  (default `heartbeatTimeoutMs = 90_000`; both providers use defaults).
  Lines are `JSON.parse`d; non-JSON noise is silently ignored; parsed messages
  fan out to `onMessage` listeners.
- Stderr is consumed and DISCARDED (comment: may contain window titles /
  media text — never logged). Privacy boundary.
- Watchdog fire: log `warn` `<scope>: "helper heartbeat timeout; restarting"`,
  kill child, schedule restart.
- Child exit (not requested): log `warn` `<scope>: "helper exited (code N)"`
  (`?` when null), schedule restart. Spawn error: schedule restart.
- Restart backoff: attempt n (1-based) waits `min(30_000, 1000 * 2^(n-1))` ms
  = 1s, 2s, 4s, 8s, 16s; after `maxRestarts = 5` consecutive attempts are
  exhausted (n > 5): log `error` `<scope>: "helper restart limit reached; giving up"`
  and stay dead. Restart counter resets only on `start()` (not on successful
  lines).
  Each scheduled restart logs `info` `<scope>: "helper restart n/5 in <delay>ms"`.
- `healthy()` = a child process handle currently exists.
- `stop()`: mark stopped, clear watchdog, kill child.

### 3.7 Provider selection (`selectProvider.ts`) and health

`selectMediaProvider(preference, psScriptPath)` with
`preference: "auto" | "npm" | "powershell"` (from `config.media.provider`):

1. If preference != `"powershell"`: construct the npm provider and `start()`
   it (module load + one probing native call). Success -> use it.
   Failure: best-effort `stop()`; if preference == `"npm"` rethrow; else
   (auto) log `warn` `media: "npm SMTC provider unavailable; falling back to PowerShell"`.
2. Fall through (auto-degraded or preference == `"powershell"`): construct the
   PowerShell provider with the script path and `start()` it (spawn always
   "succeeds"; failures surface later through host restarts/health).

main.ts wraps the call: if it throws (only possible with preference `"npm"`),
`mediaProvider = null` and it logs `error` `main: "no media provider available; media capture disabled"`.

Health surface in `CoreStateSnapshot.mediaProvider`
(`packages/shared/src/status.ts` `mediaProviderHealthSchema`):
`{ kind: "npm" | "powershell" | "none", healthy: boolean, detail?: string }` —
main.ts fills `kind = provider?.kind ?? "none"`, `healthy = provider?.healthy() ?? false`;
`detail` is never set by the core. Degraded conditions in practice:
npm provider after a failed snapshot (`healthy=false` until restart), PS
provider while its child is dead/restarting, `kind:"none"` when no provider.
Rust: the union becomes `["winrt","none"]` (UI updated accordingly);
`media.provider` config values `"auto" | "npm" | "powershell"` must still
parse but all select the WinRT provider.

### 3.8 Snapshot timeout (caller-side)

`src/privacy/captureService.ts`: `MEDIA_TIMEOUT_MS = 2000`. The capture
pipeline races `provider.getSnapshot()` against a 2000 ms timer; timeout OR a
rejected promise resolves to `null` (media treated as absent for that
delivery). Providers themselves implement no timeout. Preserve: the Rust
`get_snapshot(timeout)` should return None past 2 s (the facade passes 2 s).
Also note: captureService only uses a snapshot when `raw.playing` is true, and
media is captured BEFORE the foreground application read.

### 3.9 Stop semantics

`main.ts` graceful shutdown calls `mediaProvider.stop()` (bounded overall by a
2 s race with the rest of shutdown). PS: kills the child; npm: destroys the
monitor. After `stop()`, `healthy()` is false and no events fire.

---

## 4. System events (lock/unlock, suspend/resume)

### 4.1 TS/PS implementation (`system-events.ps1`)

A PowerShell helper (hosted by `NdjsonProcessHost`, scope `"system-events"`,
same watchdog/restart behavior as 3.6) registers .NET
`Microsoft.Win32.SystemEvents` handlers (SystemEvents runs its own hidden
broadcast window + message pump thread):

- `SessionSwitch` (SourceIdentifier `"yohaku.session"`):
  `Reason == SessionLock` -> emits `"lock"`; `SessionUnlock` -> `"unlock"`;
  all other reasons ignored.
- `PowerModeChanged` (SourceIdentifier `"yohaku.power"`):
  `Mode == Suspend` -> `"suspend"`; `Resume` -> `"resume"`; `StatusChange`
  ignored.
- Message shapes (one JSON line each):
  `{"type":"event","event":"lock"|"unlock"|"suspend"|"resume","at":<epoch ms>}`
  and `{"type":"heartbeat","at":<epoch ms>}` roughly every 30 s
  (`Wait-Event -Timeout 30` loop; a heartbeat is printed each loop iteration).
- On exit it unregisters both event subscriptions.

### 4.2 Underlying Windows events (what the Rust port listens to directly)

Per ARCHITECTURE.md the Rust port runs a hidden window + Win32 message loop on
a dedicated `std::thread`:

- Session lock/unlock: `WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION)`
  -> `WM_WTSSESSION_CHANGE` (0x02B1) with wParam
  `WTS_SESSION_LOCK = 0x7` -> lock, `WTS_SESSION_UNLOCK = 0x8` -> unlock.
  Ignore all other WTS codes (console/remote connect, logon/logoff...), which
  is what the .NET path effectively did for this script.
  Call `WTSUnRegisterSessionNotification` on teardown.
- Suspend/resume: `WM_POWERBROADCAST` (0x0218). The .NET `SystemEvents`
  mapping the TS behavior inherited is:
  - Suspend: `PBT_APMSUSPEND (0x4)`, `PBT_APMSTANDBY (0x5)`.
  - Resume: `PBT_APMRESUMECRITICAL (0x6)`, `PBT_APMRESUMESUSPEND (0x7)`,
    `PBT_APMRESUMESTANDBY (0x8)`.
  - `PBT_APMRESUMEAUTOMATIC (0x12)` was NOT mapped to Resume by .NET
    SystemEvents — unattended wakes produced no "resume" from this source and
    were caught by the SuspendDetector instead (section 5). See Ambiguities.
- TRAP: a message-ONLY window (`HWND_MESSAGE` parent) does NOT receive
  broadcast messages (`WM_POWERBROADCAST`, `WM_WTSSESSION_CHANGE`). Use a
  hidden ordinary top-level window (never shown) with a normal message loop.

### 4.3 `PsSystemEventsProvider` dedup / callback contract

State: `suspended: boolean = false` (Node-side; survives helper restarts).

- Only messages with `type === "event"` are considered (heartbeats ignored).
- `event in {"lock","suspend"}`: if already `suspended` -> drop (idempotent);
  else set `suspended = true` and fire ALL `onLockOrSleep` listeners.
- `event in {"unlock","resume"}`: if NOT `suspended` -> drop; else set
  `suspended = false` and fire ALL `onUnlockOrResume` listeners.
- Net effect: lock followed by suspend collapses into ONE lock-or-sleep
  transition; the first of resume/unlock ends it; a resume without a prior
  lock/suspend is swallowed (initial state not-suspended).
- `start()`: starts the host, logs `info` `system-events: "provider started"`.
  `stop()`: stops the host (no log).
- Restart-on-crash: inherited from the host (watchdog 90 s, backoff 1/2/4/8/16 s,
  give up after 5 consecutive failures). The Rust port replaces this with an
  in-process window thread; if window creation fails, log an error — there is
  no external process to supervise.

### 4.4 Wiring (main.ts)

`onLockOrSleep -> service.handleSleepOrLock()`;
`onUnlockOrResume -> service.handleWakeOrUnlock()`. The provider is started
(`await systemEvents.start()`) before the IPC server announces readiness, and
stopped during graceful shutdown (within the 2 s bound).

---

## 5. SuspendDetector (`runtime/suspendDetector.ts`)

Safety net beneath the system-events source: detects sleeps that produced no
suspend event via wall-clock jumps.

- Constructor: `(onGapDetected: () => void, gapThresholdMs = 15_000)`.
  main.ts uses the DEFAULT threshold (15 000 ms).
- `start()`: idempotent. `lastTick = Date.now()`. Every 5000 ms (unref'ed
  interval): `now = Date.now(); gap = now - lastTick; lastTick = now;`
  then IF `gap > gapThresholdMs` (strictly greater):
  log `warn` `suspend-detector: "wall-clock gap <N>s"` with
  `N = Math.round(gap / 1000)`, and call `onGapDetected()`.
  Note `lastTick` is updated unconditionally BEFORE the comparison — a single
  callback per gap, no re-fire.
- `stop()`: clears the interval.
- MUST use wall-clock time (SystemTime / epoch ms), never a monotonic clock —
  monotonic clocks may pause during sleep, which would hide exactly the gap
  this detects. (Rust/tokio: also make sure a missed-tick policy cannot fire
  a burst of callbacks after wake; compute the gap from wall clock exactly as
  above.)
- main.ts callback rule: on gap, IF `service.runtimeState()` is `"active"` or
  `"degraded"` -> `service.coordinator.start()` (forces a resume-style
  renegotiation; the server lease already expired the stale presence).
  All other states: do nothing.

---

## 6. Logger (`runtime/logger.ts`)

- Levels and ordering: `debug=10, info=20, warn=30, error=40`. Global minimum
  level, default `"info"`; `setLogLevel(level)` changes it at runtime.
- Filtering: a line below the minimum level is dropped ENTIRELY — it reaches
  neither the console nor the ring buffer.
- Line format (exact):
  `${new Date().toISOString()} [${level}] ${scope}: ${message}`
  e.g. `2026-09-23T12:34:56.789Z [info] foreground: watcher started`.
  Timestamp is UTC ISO-8601 with exactly 3 fractional digits and `Z` suffix.
  Level is the lowercase word; scope is a short module tag.
- Ring buffer: module-global array, capacity `RING_LIMIT = 200`, push + shift
  (FIFO eviction of the oldest). `recentLogLines()` returns a shallow COPY of
  the current contents (oldest first) — surfaced on the Status page.
- Output streams: `error` and `warn` -> stderr (`console.error`);
  `info` and `debug` -> stdout (`console.log`).
- API shape: `logger.debug|info|warn|error(scope, message)` plus `log(level,
  scope, message)`, `setLogLevel`, `recentLogLines`.
- REDACTION (hard privacy boundary, not a formatting choice): log lines never
  include tokens, window titles, media text, exe paths, or any raw capture
  values — only fixed message strings, error names/codes, and counts. Existing
  scopes: `foreground`, `displayName`, `media`, `media-ps`, `system-events`,
  `suspend-detector`, `config`, `credentials`, `main` (plus companion-side
  scopes out of scope here). Keep messages fixed-string in Rust; e.g. the npm
  snapshot failure logs only `error.name`, and the displayName failure logs no
  path.

---

## 7. ConfigStore (`store/configStore.ts`)

### 7.1 Data directory

`dataDirectory()`:
1. `YOHAKU_DATA_DIR` env var, if set AND non-empty -> used verbatim.
2. Else `%APPDATA%` (must be set and non-empty, otherwise throw
   `"APPDATA is not set"`) joined with `yohaku-companion-win`
   -> typically `C:\Users\<u>\AppData\Roaming\yohaku-companion-win`.

Every store constructor (`ConfigStore`, `FileSequenceStore`,
`DpapiCredentialStore`) does `mkdirSync(directory, { recursive: true })`.
Constructors accept an optional directory override (used by tests).

### 7.2 Schema (zod; camelCase; full)

`config.json` holds NON-SECRET configuration only; the device token NEVER
lives here.

```jsonc
{
  "version": 1,                      // literal 1; anything else = corrupt
  "privacy": {
    "defaults": {                    // each: "share" | "hide"
      "application": "share",
      "windowTitle": "hide",
      "media": "share"
    },
    "rules": [                       // array of:
      {
        "appId": "code.exe",         // string, min length 1
        "application": "inherit",    // "inherit" | "share" | "hide"
        "windowTitle": "inherit",
        "media": "inherit",
        "displayAlias": "optional string (key may be absent)"
      }
    ],
    "mappings": [                    // array of:
      {
        // "media_process_name" kept valid for configs from older releases
        "type": "process_name",      // "process_name" | "media_process_name" | "media_player_name"
        "from": "x",                 // min length 1
        "to": "y"                    // min length 1
      }
    ],
    "shareWindowTitles": false,      // boolean
    "ignoreNullArtist": false,       // boolean
    "sources": { "application": true, "media": true }
  },
  "connection": {                    // nullable object
    "baseUrl": "https://...",        // string
    "deviceId": "…",                 // string
    "deviceName": "…",               // string
    "scopes": ["…"],                 // string[]
    "pairingNextSequence": 0,        // integer >= 0
    "liveDeskEnabled": false         // boolean
  },
  "media": { "provider": "auto" },   // "auto" | "npm" | "powershell"
  "credentialBackend": null          // "keyring" | "dpapi" | null
}
```

Defaults (`defaultCoreConfig()` / `defaultPrivacyConfig()`): exactly the
values shown above with `connection: null`, `rules: []`, `mappings: []`.
New installs: window titles hidden unless opted in.

Zod parsing notes for the Rust port:
- Unknown keys are silently STRIPPED (zod object default) — Rust serde must
  NOT use `deny_unknown_fields`.
- Any invalid value (wrong `version`, unknown enum value, wrong type, negative
  `pairingNextSequence`, empty `appId`/`from`/`to`, ...) fails the WHOLE parse
  -> corrupt-file path. There is no partial recovery. Fail-closed: losing
  `connection` means "not paired", never "publishing enabled".
- `media.provider` values `"auto" | "npm" | "powershell"` must still PARSE in
  Rust (config compatibility) but all map to the WinRT provider.

### 7.3 Load / corrupt handling

`load()` at construction:
1. `readFileSync(path, "utf8")`; ANY read error (file missing, access) ->
   return defaults SILENTLY (no log, no backup, and the defaults are NOT
   written to disk — the file first appears on the next `update()`).
2. `JSON.parse` + schema parse; on ANY failure:
   - log `error` `config: "config.json corrupt; backing up and using defaults"`.
   - `copyFileSync(config.json, config.json.bak)` best-effort (errors
     swallowed; an existing `.bak` is overwritten).
   - return defaults (in memory only; corrupt file left in place until the
     next `update()` overwrites it).

### 7.4 API

- `get()` returns the current config (TS returns the internal reference; the
  Rust contract returns a clone).
- `privacy` getter = `get().privacy`.
- `onChange(cb)` -> unsubscribe closure.
- `update(mutate)`: deep-clone current config, apply `mutate`, re-parse with
  the full schema (throws on invalid — nothing written), `atomicWriteJson`,
  swap in memory, then synchronously notify all listeners with the new config.
  Returns the new config.

### 7.5 Atomic write protocol (`atomicWriteJson`) — byte-for-byte

Used by config.json, sequence.json, and credentials.bin.json:

1. `tmp = path + ".tmp"` (fixed name, same directory — e.g. `config.json.tmp`).
2. `openSync(tmp, "w")` (create/truncate).
3. `writeSync(fd, JSON.stringify(value, null, 2))` — UTF-8, no BOM, 2-space
   indent, `\n` line separators (JSON.stringify never emits `\r`), NO trailing
   newline. Key order = object insertion order (see Ambiguities; not
   semantically required).
4. `fsyncSync(fd)`; then `closeSync(fd)` (fsync BEFORE close, in a finally).
5. `renameSync(tmp, path)` — atomic replace of the destination (Windows: this
   must replace an existing file, i.e. MoveFileEx-with-replace semantics /
   `std::fs::rename` which does this on Windows).
6. No directory fsync (accepted risk).

---

## 8. FileSequenceStore (`store/sequenceStore.ts`)

Per-device next-sequence persistence, deliberately a SEPARATE file from
config.json: correctness writes (reserve-before-send) must not race the
chattier settings writes.

- File: `<dataDir>/sequence.json`.
- Layout: a single JSON object mapping deviceId -> number (the next sequence),
  written with the section 7.5 protocol (2-space indent). Example:

```json
{
  "01J8ME9FZW3W7T2C4Y8K5Q6R9S": 42
}
```

- `read()` (internal, per operation — no caching): parse the file; result
  must be a non-null non-array object, else (or on any read/parse error)
  treat as `{}` ("missing or corrupt -> empty; sequencer self-heals via
  reconcile"). No `.bak`, no logging.
- `load(deviceId) -> number | null`: the stored value if it is a JSON number
  (any number accepted by `typeof === "number"`; in practice integers),
  else null.
- `store(deviceId, next)`: read-modify-write, atomic write.
- `remove(deviceId)`: only touches the disk if the key exists; deletes the key
  and atomically writes the remainder — NOTE: writes `{}` rather than
  deleting the file when it was the last key (unlike the DPAPI store).
- Implements the sequencer's `SequencePersistence { load, store }`; `remove`
  is extra (used on unpair).

---

## 9. Credentials

### 9.1 `CredentialStore` contract (`store/credentials.ts`)

```ts
interface CredentialStore {
  readonly backend: "keyring" | "dpapi";
  get(deviceId: string): Promise<string | null>;
  set(deviceId: string, token: string): Promise<void>;
  delete(deviceId: string): Promise<void>;   // idempotent
}
class CredentialStoreUnavailableError extends Error { name = "CredentialStoreUnavailableError" }
```

The stored secret is the RAW companion device token string exactly as
returned by the pairing endpoint (`service.ts`:
`credentialStore.set(result.deviceId, result.deviceToken)`), keyed by
deviceId. Not JSON, no wrapping. Tokens never touch config.json, logs, IPC
payloads, or exports.

### 9.2 Backend selection (`selectCredentialStore(preferred)`)

- Candidate order: `preferred === "dpapi"` -> `[Dpapi, Keyring]`;
  otherwise (null OR `"keyring"`) -> `[Keyring, Dpapi]`.
  `preferred` pins the backend that stored an existing token so a later probe
  cannot orphan it.
- Probe (`roundTrips`): on each candidate, run
  `set("__yohaku_probe__", "probe")` -> `get("__yohaku_probe__")` ->
  `delete("__yohaku_probe__")`; the store qualifies iff no step threw and the
  read value === `"probe"`. (Note: the probe transiently creates a real
  credential named per that backend's convention.)
- First qualifying store wins; log `info` `credentials: "using <backend> backend"`.
  A failing candidate logs `warn` `credentials: "<backend> backend unavailable"`.
- All candidates fail -> throw
  `CredentialStoreUnavailableError("no credential backend available")`
  (surfaces to pairing as IPC error code `credentialStoreUnavailable`).
- main.ts wiring: selection is LAZY (first use) and memoized for the process
  lifetime; after selection, if `config.credentialBackend !== store.backend`,
  the config is updated to pin it. Pairing also re-pins
  `credentialBackend: store.backend` in the same update that stores the
  connection.

### 9.3 Keyring backend (`keyringCredentialStore.ts`) — Windows Credential Manager

TS uses `@napi-rs/keyring` 1.3.0 with `SERVICE = "yohaku-companion-win"` and
account = deviceId:

- `get`: `new Entry(SERVICE, deviceId).getPassword()`, any throw -> null.
- `set`: `new Entry(SERVICE, deviceId).setPassword(token)` (throws propagate).
- `delete`: `deletePassword()`, throws swallowed.

`@napi-rs/keyring` 1.3.0 is a napi binding over the Rust crates
`keyring-core 1.0.0` + `windows-native-keyring-store 1.0.0` (verified from
strings embedded in the shipped
`node_modules/.pnpm/@napi-rs+keyring-win32-x64-msvc@1.3.0/.../keyring.win32-x64-msvc.node`).
It uses the store's DEFAULT configuration: delimiters
`prefix = ""`, `divider = "."`, `suffix = ""`, `service_no_divider = false`,
persistence modifier default `"Enterprise"`. Target name composition (from
the crate source, `cred.rs`):
`target_name = "{prefix}{user}{divider}{service}{suffix}"` = **`{user}.{service}`**.

EXACT Windows Credential Manager entry produced (VERIFIED EMPIRICALLY on this
machine with the shipped binary: wrote service `yohaku-companion-win`, account
`__spec_probe__`, password `probe-secret-123`, then read the raw credential
via `CredReadW`):

| CREDENTIALW field   | Value |
| ---                 | --- |
| `Type`              | `CRED_TYPE_GENERIC` (1) |
| `TargetName`        | `<deviceId>.yohaku-companion-win` (verified: `__spec_probe__.yohaku-companion-win`) |
| `UserName`          | `<deviceId>` (the account string) |
| `Comment`           | `""` (empty) |
| `TargetAlias`       | `""` (empty) |
| `Persist`           | `CRED_PERSIST_ENTERPRISE` (3) |
| `Flags`             | 0 |
| `AttributeCount`    | 0, `Attributes` NULL |
| `CredentialBlob`    | the token encoded as **UTF-16LE, no BOM, no NUL terminator** |

Verified blob for `"probe-secret-123"` (16 chars -> 32 bytes):
`700072006f00620065002d007300650063007200650074002d00310032003300`.

Additional crate semantics to replicate (from
`windows-native-keyring-store-1.0.0/src/{cred,utils}.rs`):

- Write: `CredWriteW(&cred, 0)`. If a credential with the same target already
  EXISTS, the crate first reads it and PRESERVES its existing
  `UserName`/`TargetAlias`/`Comment` (only the blob and Persist are ours);
  for a fresh credential it writes UserName = user specifier, empty
  alias/comment.
- Read: `CredReadW(target, CRED_TYPE_GENERIC, 0, &out)`. Decode blob:
  odd byte count or invalid UTF-16 -> BadEncoding error (binding throws -> TS
  maps to null). `String::from_utf16` on LE u16 pairs.
- Delete: `CredDeleteW(target, CRED_TYPE_GENERIC, 0)`.
- Error mapping: `GetLastError() == ERROR_NOT_FOUND` -> NoEntry;
  `ERROR_NO_SUCH_LOGON_SESSION` -> NoStorageAccess; other -> PlatformFailure.
- Binding behavior at the JS layer (verified empirically, v1.3.0):
  `getPassword()` on a missing entry returns `null` (does not throw);
  `deletePassword()` returns `true`/`false` (false when missing). The TS
  wrapper's try/catch is defensive for other keyring versions.

Rust port requirements (so existing installs keep their credentials):
- Look up with `CredReadW(w!("<deviceId>.yohaku-companion-win"), CRED_TYPE_GENERIC, 0, ...)`.
- Treat ERROR_NOT_FOUND — and any decode failure — as "no token" (parity with
  TS catch -> null).
- Write with the exact field table above (UTF-16LE blob, Persist=Enterprise,
  UserName=deviceId); delete ignoring ERROR_NOT_FOUND.
- The probe sentinel creates/deletes `__yohaku_probe__.yohaku-companion-win`.

### 9.4 DPAPI fallback backend (`dpapiCredentialStore.ts`)

- File: `<dataDir>/credentials.bin.json` (created directories as usual).
- Layout: JSON object `deviceId -> base64 string`, written via the section 7.5
  atomic protocol (2-space indent). Example:

```json
{
  "01J8ME9FZW3W7T2C4Y8K5Q6R9S": "AQAAANCMnd8BFdERjHoAwE/Cl+s..."
}
```

- The base64 payload is the raw output of DPAPI
  `CryptProtectData` over the **UTF-8 bytes of the token**, with:
  - scope: CurrentUser (TS: `ProtectedData.Protect(bytes, $null, "CurrentUser")`;
    i.e. NO `CRYPTPROTECT_LOCAL_MACHINE`; .NET passes
    `CRYPTPROTECT_UI_FORBIDDEN`),
  - optional entropy: NULL (none),
  - standard base64 alphabet, padded, single line
    (`[Convert]::ToBase64String`).
- `get(deviceId)`: missing key -> null; base64-decode + `CryptUnprotectData`
  (entropy NULL, CurrentUser); ANY failure (different user/machine,
  corrupt) -> null ("treat as absent").
- `set(deviceId, token)`: protect; empty protect output -> throw
  `"DPAPI protect produced no output"`; read-modify-write the JSON file
  atomically.
- `delete(deviceId)`: only acts when the key exists; removes it; if the
  object becomes empty, the FILE IS DELETED (`rmSync(path, { force: true })`)
  — otherwise the remainder is written atomically. (Contrast with
  sequence.json which keeps an empty `{}`.)
- Read helper mirrors sequenceStore: parse errors / wrong shape -> `{}`.
- TS transport detail NOT to inherit: the token traveled to a PowerShell
  one-shot via stdin (never on a command line; 15 s timeout, 1 MB buffer) and
  both sides TRIM the strings (`[Console]::In.ReadToEnd().Trim()` on protect
  input and unprotect output; Node trims stdout). Rust calls
  `CryptProtectData`/`CryptUnprotectData` in-process; do not introduce
  trimming of the token itself (tokens contain no whitespace in practice —
  see Ambiguities).
- The DPAPI blob container format is the standard CryptProtectData output;
  .NET `ProtectedData` and Win32 `CryptUnprotectData` are interoperable, so
  Rust can decrypt existing files and vice versa.

---

## 10. Native dependency inventory (`packages/core/package.json`)

- `koffi 3.1.2` — generic synchronous FFI used ONLY by
  `capture/foreground/win32.ts` (user32/kernel32). Replaced by direct
  `windows` crate calls.
- `@napi-rs/keyring 1.3.0` — Windows Credential Manager access; wraps
  `keyring-core 1.0.0` + `windows-native-keyring-store 1.0.0` (see 9.3).
  Replaced by `CredReadW`/`CredWriteW`/`CredDeleteW`.
- `@coooookies/windows-smtc-monitor 1.0.12` (optionalDependency; napi-rs
  prebuilt) — in-process SMTC monitor; provided `getCurrentMediaSession()`
  (single "current" session), session event listeners, timeline in float
  seconds, `lastUpdatedTime` epoch ms, raw playbackStatus/playbackType ints;
  Windows 10 1809+. Replaced by direct WinRT `Windows.Media.Control`.
- `ws 8.21.1` — IPC WebSocket (superseded by Tauri commands/events).
- `zod 4.4.3` — schema validation (superseded by serde + manual checks).
- Build detail: esbuild bundle marks `koffi`, `@napi-rs/keyring`,
  `@coooookies/windows-smtc-monitor` as external (they ship as staged
  node_modules next to main.cjs) — all of this staging disappears in Rust.
- PS helper resolution (`main.ts psDirectory()`): `YOHAKU_PS_DIR` env override
  (the Tauri shell sets it), else `../resources/ps` or `./ps` relative to the
  entry file. Gone in Rust (no helpers), but note `YOHAKU_PS_DIR` becomes
  dead and `YOHAKU_DATA_DIR` (7.1) MUST keep working.

---

## 11. Test vectors

The vitest suites do not cover capture/stores directly (`test/` contains
companion, ipc, privacy). Pinned values that touch this spec:

From `test/ipc/server.test.ts` (shapes only; the WS server itself is
superseded by Tauri commands):

- A fresh `ConfigStore(tempDir)` yields `runtimeState "notPaired"` and
  `privacy` deep-equal to `defaultPrivacyConfig()` (section 7.2 defaults).
- Media provider health placeholder used by the shell when no provider:
  `{ "kind": "none", "healthy": false }` (valid against
  `mediaProviderHealthSchema`).
- `upsertRule` with `appId: "Secret.EXE"` persists as
  `{ appId: "secret.exe", application: "hide", windowTitle: "inherit", media: "inherit" }`
  (appId lowercased by the handler; rules array in config.json).
- `setPrivacy { shareWindowTitles: true }` persists to
  `config.get().privacy.shareWindowTitles === true`.
- `preview.policyFingerprint` matches `/^[0-9a-f]{64}$/` (sha-256 hex,
  lowercase).
- Snapshot fields exercised: `version`, `runtimeState`, `connection: null`,
  `privacy`, `preview`, `mediaProvider`, `recentAppIds: []`,
  `lastPublishAt: null`, `lastError: null`.
- Error codes pinned: `"notPaired"` (consent without pairing),
  `"invalidInput"` (malformed command).

Empirical keyring vectors (measured on Windows 11 with the shipped
`@napi-rs/keyring` 1.3.0 binary — use as Rust interop tests):

- Input: service `yohaku-companion-win`, account `__spec_probe__`, password
  `probe-secret-123`.
- Resulting credential: TargetName `__spec_probe__.yohaku-companion-win`,
  UserName `__spec_probe__`, Type 1, Persist 3, Flags 0, Comment empty,
  TargetAlias empty, AttributeCount 0, CredentialBlobSize 32, blob hex
  `700072006f00620065002d007300650063007200650074002d00310032003300`.
- `getSecret()` -> `[112,0,114,0,111,0,98,0,101,0,45,0,115,0,101,0,99,0,114,0,101,0,116,0,45,0,49,0,50,0,51,0]`.
- `deletePassword()` -> `true`; repeated -> `false`; `getPassword()` after
  delete -> `null`.

Derived behavioral vectors (from code, for unit tests):

- Foreground change key: appId `code.exe`, title `main.rs — repo` ->
  `"code.exe\\0main.rs — repo"` (backslash + `0` separator); null info -> `""`;
  null title -> `"code.exe\\0"`.
- `exeStemDisplayName`: `"code.exe"` -> `"Code"`; `"x"` -> `"X"`;
  `"C:\\Tools\\Some.Tool.exe"` -> `"Some.Tool"` -> capitalized `"Some.Tool"`
  (only first char affected); `".exe"` -> basename stem empty -> input
  returned unchanged (`".exe"`).
- Media appId heuristic: `"Spotify.exe"` -> `"spotify.exe"`; `"MSEdge"` ->
  null; `"Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"` -> null
  (playerDisplayName = the full AUMID per authoritative semantics).
- Position extrapolation: position 100.0, playing, `updatedAt = now - 5000`
  -> 105.0; clamped at duration; elapsed >= 21600 s -> no extrapolation;
  paused -> no extrapolation.
- Logger line: level `info`, scope `foreground`, message `watcher started` ->
  `2026-01-02T03:04:05.678Z [info] foreground: watcher started`.

---

## 12. Traps for the Rust port

Encoding / FFI:

- All Win32 strings are UTF-16. Node decoded with `utf16le` which REPLACES
  unpaired surrogates; use `String::from_utf16_lossy` (window titles from
  arbitrary apps DO contain garbage; never error out).
- `GetWindowTextW` returning 0 is indistinguishable from "no title" — both
  map to `windowTitle = null` (preserved behavior).
- Keyring blob: UTF-16LE WITHOUT BOM or NUL terminator; reject/ignore odd
  lengths on read. `TargetName`/`UserName` passed to CredReadW/WriteW are
  NUL-terminated wide strings.
- JSON files: UTF-8, no BOM, `JSON.stringify(v, null, 2)` formatting, no
  trailing newline (`serde_json::to_string_pretty` matches the 2-space
  indentation; exact key order is not semantically required — see
  Ambiguities).
- koffi buffer trick (`_Out_ uint8*` + wide decode) disappears; use proper
  PWSTR buffers, but keep the SAME capacities (512 chars title, 1024 chars
  path) and the same truncation behavior.

PowerShell-era artifacts the Rust port must NOT inherit:

- 1 s media poll, JSON-line dedupe, 30 s heartbeats, 90 s watchdog,
  exponential restart backoff, 10 s frame-staleness null, "give up after 5
  restarts" — all replaced by direct WinRT reads/events. KEEP: the caller's
  2 s snapshot timeout, position extrapolation, the semantic-change
  definition, and the health flag concept.
- FileDescription via `Get-Item` child process -> read the version resource
  in-process (2.3). Keep the cache keyed by lowercased path + mtime and the
  "background resolution + stem fallback" contract.
- DPAPI via PowerShell stdin one-shot -> in-process CryptProtectData/
  CryptUnprotectData (9.4). Do not inherit the `.Trim()` calls.
- System events via .NET SystemEvents helper -> own hidden window (4.2).
- Media/system helpers wrote raw media text to stderr which was consumed and
  discarded; in-process Rust must simply never log those values.

Win32 / WinRT specifics:

- WinRT `TimeSpan` is 100 ns ticks (`i64`): seconds = ticks / 10_000_000 (as
  f64, matching `.TotalSeconds`). WinRT `DateTime` is 100 ns ticks since
  1601-01-01 UTC: epoch ms = (ticks - 116444736000000000) / 10_000 (matching
  `DateTimeOffset.ToUnixTimeMilliseconds`, which truncates).
- Threading: initialize COM/WinRT as MTA on threads calling
  `GlobalSystemMediaTransportControlsSessionManager::RequestAsync`
  (`RoInitialize(RO_INIT_MULTITHREADED)` or rely on tokio threads +
  explicit init). Do NOT use STA — no message pump on tokio workers. WinRT
  event handlers arrive on WinRT threadpool threads: marshal into tokio via
  channels; never block or panic in handlers.
- Per-session events (`MediaPropertiesChanged`, `PlaybackInfoChanged`) must
  be re-registered when the current session changes; remember to drop old
  registrations (EventRegistrationToken) or handlers leak.
- Hidden window for system events: `HWND_MESSAGE` (message-only) windows do
  NOT receive `WM_POWERBROADCAST` / `WM_WTSSESSION_CHANGE` broadcasts — create
  a hidden ordinary top-level window on a dedicated `std::thread` running
  `GetMessageW`/`DispatchMessageW`; call
  `WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION)` AFTER window
  creation and `WTSUnRegisterSessionNotification` + `DestroyWindow` +
  `PostQuitMessage` on shutdown. Return TRUE from the wndproc for
  `WM_POWERBROADCAST`.
- `GetWindowTextW` on other-process windows reads the cached title (no
  cross-process SendMessage) — safe to call from the 1 s poll thread without
  hang risk.
- OpenProcess with `PROCESS_QUERY_LIMITED_INFORMATION` works across integrity
  levels for most elevated processes; still expect failures (protected
  processes) -> exePath None -> watcher serves last-known info.
- SuspendDetector must use wall-clock (`SystemTime`), NOT `Instant`, and must
  fire at most once per detected gap (5 s tick, `gap > 15_000`).
- Timers in Node were `unref`ed (they never kept the process alive); in Rust
  make all loops shut down promptly on stop/drop so the 2 s bounded shutdown
  holds.

Likely `windows` crate features (trim to what compiles):

- `Win32_Foundation` (HWND, HANDLE, BOOL, FILETIME, CloseHandle, error codes)
- `Win32_UI_WindowsAndMessaging` (GetForegroundWindow, GetWindowTextW,
  GetWindowThreadProcessId, CreateWindowExW/DefWindowProcW/message loop,
  WM_POWERBROADCAST / WM_WTSSESSION_CHANGE constants)
- `Win32_System_Threading` (OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
  QueryFullProcessImageNameW)
- `Win32_System_RemoteDesktop` (WTSRegisterSessionNotification,
  NOTIFY_FOR_THIS_SESSION, WTS_SESSION_LOCK/UNLOCK)
- `Win32_System_Power` (PBT_APMSUSPEND / PBT_APMSTANDBY /
  PBT_APMRESUMESUSPEND / PBT_APMRESUMECRITICAL / PBT_APMRESUMESTANDBY /
  PBT_APMRESUMEAUTOMATIC)
- `Win32_Security_Credentials` (CredReadW/CredWriteW/CredDeleteW/CredFree,
  CREDENTIALW, CRED_TYPE_GENERIC, CRED_PERSIST_ENTERPRISE)
- `Win32_Security_Cryptography` (CryptProtectData/CryptUnprotectData,
  CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN)
- `Win32_Storage_FileSystem` (GetFileVersionInfoSizeW/GetFileVersionInfoW/
  VerQueryValueW for FileDescription)
- `Win32_System_LibraryLoader` (GetModuleHandleW for the window class)
- `Win32_System_WinRT` or `Win32_System_Com` (RoInitialize / CoInitializeEx)
- `Media_Control`, `Foundation` (TimeSpan, DateTime, IReference,
  TypedEventHandler, EventRegistrationToken), `Foundation_Collections`,
  `Storage_Streams` (referenced by media-properties signatures)

---

## 13. Ambiguities found during extraction

1. Keyring target-name convention: VERIFIED empirically against the exact
   binary shipped in this repo (`@napi-rs/keyring` 1.3.0 ->
   `windows-native-keyring-store` 1.0.0, default config, target
   `<user>.<service>`, Persist=Enterprise, UTF-16LE blob) — see section 9.3
   and the measured vector in section 11. What I could NOT verify: the
   keyring-node wrapper's Rust source itself (github fetch blocked in this
   environment), so the conclusion rests on crate-source + on-machine
   measurement rather than the wrapper's lib.rs; and whether any OLDER app
   release shipped a different @napi-rs/keyring version. Mitigation: hwchen
   keyring-rs v2/v3 (the predecessor embedded in older binding versions) used
   the same default `user.service` target and UTF-16 blob; the only known
   difference is that v2/v3 wrote a non-empty metadata `Comment`
   ("keyring-rs v… for target …"), which is irrelevant to CredReadW lookup.
2. `playerDisplayName` fallback for non-exe AUMIDs differs between providers:
   PS/authoritative = full AUMID; npm = `split("!")[0].split(".").pop()`
   (e.g. `"ZuneMusic_8wekyb3d8bbwe"`). ARCHITECTURE.md directs PS semantics
   for the Rust port, but note that in `"auto"` mode real installs preferred
   the npm provider, so users of UWP players actually saw the npm-style
   names (and privacy rules matched against them). Flagged for the
   integrator: privacy rules/mappings keyed on a player name may match
   differently after the port if PS semantics are chosen.
3. `PBT_APMRESUMEAUTOMATIC` (0x12) was not mapped by the .NET SystemEvents
   chain, so unattended wakes produced no `resume` event and were handled by
   the SuspendDetector 15 s gap instead. If the Rust port adds
   RESUMEAUTOMATIC, renegotiation happens at wake instead of at first user
   input — an observable timing change. Strict parity = map only
   PBT_APMSUSPEND/PBT_APMSTANDBY -> suspend and
   PBT_APMRESUMECRITICAL/PBT_APMRESUMESUSPEND/PBT_APMRESUMESTANDBY -> resume.
4. FileDescription language selection: .NET `FileVersionInfo` has its own
   translation-block fallback order; the "first `\VarFileInfo\Translation`
   pair" heuristic recommended in 2.3 can differ for multi-language exes.
   Cosmetic only (display names).
5. JSON key order in written files: TS preserves insertion order
   (defaults order for fresh configs, loaded order after round-trips). Rust
   serde struct-field order will produce a stable but possibly different
   order. All readers parse order-insensitively; byte-identical output is NOT
   required — but if desired, order Rust struct fields as in section 7.2.
6. `ConfigStore.get()` returns the live internal object in TS (callers
   treat it as immutable by convention); the Rust contract clones.
7. `MediaProvider.getSnapshot(options.timeoutMs)` is declared but unused;
   the only timeout is the caller's 2000 ms race (3.8).
8. DPAPI path trims the token (PowerShell `.Trim()` on protect input and
   stdout trim on unprotect); a token with leading/trailing whitespace would
   round-trip differently between backends. Companion tokens contain no
   whitespace in practice; Rust should store the token verbatim.
9. `sequence.json` values are JS numbers; the protocol bounds sequences to
   0..2^53-1 so `u64` in Rust must reject/clamp larger values when writing
   (serde will happily emit >2^53 which old TS could then read imprecisely —
   moot after full migration, noted for mixed-version scenarios).
10. The foreground change-key separator is the two-character literal `\0`
    (backslash, zero) due to `\\0` in a template literal — almost certainly
    an accident (a real NUL was probably intended), but harmless since appIds
    (basenames) cannot contain backslashes. Any injective separator works;
    documenting the literal for byte-parity pedants.
