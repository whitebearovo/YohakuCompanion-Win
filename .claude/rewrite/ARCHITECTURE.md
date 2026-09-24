# Native Rust Rewrite — Architecture Contract

Goal: replace the Node.js TypeScript core (`packages/core`), the Node sidecar,
the PowerShell capture providers, and the localhost WebSocket IPC with a native
Rust implementation living inside the Tauri v2 process. The React settings UI
stays; it talks to the core via Tauri `invoke` commands and events instead of a
WebSocket.

This document is the coordination contract. Implementation agents MUST conform
to the crate layout, file ownership, public type/trait names, and invariants
below. Internals (private functions, exact algorithms) come from the TypeScript
source in `packages/core/src/**`, which is the behavioral ground truth, and
from the spec docs in `.claude/rewrite/specs/`.

## Non-negotiable compatibility invariants

1. **On-disk formats are unchanged.** Existing installations must keep working:
   - `%APPDATA%\yohaku-companion-win\config.json` — exact same JSON schema
     (camelCase keys, `version: 1`, same enums/defaults) as
     `packages/core/src/store/configStore.ts`. Atomic write: tmp + fsync +
     rename; corrupt file → copy to `.bak`, fall back to defaults (fail-closed:
     losing `connection` means "not paired").
   - `%APPDATA%\yohaku-companion-win\sequence.json` — same format as
     `packages/core/src/store/sequenceStore.ts`.
   - `YOHAKU_DATA_DIR` env override honored, else `%APPDATA%\yohaku-companion-win`.
   - Windows Credential Manager entry: same target name / user / secret layout
     as `keyringCredentialStore.ts` (it uses the npm `keyring` conventions —
     replicate the exact target string so existing credentials are found).
   - DPAPI fallback file: same path and byte format as `dpapiCredentialStore.ts`.
2. **Companion Protocol v2 bytes are unchanged.** Same endpoints, headers
   (`X-Yohaku-Companion-Version: 1.8.3` etc.), envelope shapes, RFC3339
   millisecond UTC dates (exactly 3 fraction digits), integers 0..2^53-1,
   UUID/ULID identifiers, sequence reservation/reconciliation semantics,
   single idempotent retry with identical bytes.
3. **Privacy invariants.** Raw capture values (exe paths, un-sanitized titles,
   media text) never reach the network, persistence, or logs. Log lines exclude
   window titles and media text. Snapshots to the UI contain only sanitized
   values, mirroring `CoreStateSnapshot`.
4. **UI data shapes are unchanged.** The snapshot/preview JSON sent to the UI
   must serialize to the same camelCase shapes as `packages/shared/src/*.ts`
   (`CoreStateSnapshot`, `Preview`, `PrivacyConfig`, ...). Rust structs use
   `#[serde(rename_all = "camelCase")]` (plus explicit renames where needed).

## Workspace layout

`packages/app/src-tauri/Cargo.toml` becomes a cargo workspace root:

```
packages/app/src-tauri/
  Cargo.toml            # [workspace] members = [".", "core"] + app crate
  src/                  # tauri app crate (commands, events, lifecycle, state)
    main.rs
    lib.rs              # plugin setup, run loop
    commands.rs         # tauri commands mirroring the IPC Command union
    state.rs            # AppCoreState: core handles + recentAppIds + snapshot assembly
  core/
    Cargo.toml          # crate name: yohaku-core
    src/
      lib.rs            # module declarations + re-exports (scaffold-owned)
      model.rs          # ALL cross-module serde data types (scaffold-owned, complete)
      protocol/
        mod.rs          # scaffold-owned
        wire.rs         # dates, identifiers, integer bounds, WireError
        types.rs        # wire envelopes, MutationResponse, ClearReason, error envelope
        capabilities.rs # negotiation (NegotiatedPresenceConfiguration)
        dto_mapper.rs   # makePresenceRequest / makeClearRequest
        sequencer.rs    # CompanionSequencer + SequenceBacking trait
      companion/
        mod.rs
        http_client.rs  # CompanionHttpClient (reqwest), CompanionCredential
        errors.rs       # transport error taxonomy, retry-safety, acceptedSequenceOf
        presence_client.rs
        pairing.rs
        authority.rs
        consent_gate.rs
        coordinator.rs
        service.rs      # CompanionService facade + ServiceError(IpcErrorCode)
      privacy/
        mod.rs
        model.rs        # rules normalization, effective policy resolution
        evaluator.rs
        sanitize.rs
        fingerprint.rs
        capture_service.rs
        media_session_tracker.rs
      capture/
        mod.rs          # ForegroundInfo/MediaSnapshot re-exports live in model.rs
        foreground.rs   # Win32 polling watcher (GetForegroundWindow et al.)
        media.rs        # WinRT SMTC provider (Windows.Media.Control)
        system_events.rs# WTS session + power broadcast hidden-window listener
      store/
        mod.rs
        config.rs       # ConfigStore + atomic_write_json + data_directory()
        sequence.rs     # FileSequenceStore (implements SequenceBacking)
        credentials.rs  # CredentialStore trait + backend selection
        keyring_store.rs# Windows Credential Manager backend
        dpapi_store.rs  # DPAPI-encrypted file fallback
      runtime/
        mod.rs
        logger.rs       # leveled logger + recent-lines ring buffer
        suspend_detector.rs
    tests/              # ported vitest suites (see Test mapping)
```

## File ownership (parallel implementation)

Each agent edits ONLY its owned files (plus its test files). `mod.rs`,
`lib.rs`, `model.rs`, and both `Cargo.toml`s are scaffold-owned; implementers
must not restructure them (adding `use` statements inside owned files is fine).
If a contract signature proves wrong, the implementer notes it in
`.claude/rewrite/integration-notes.md` (append-only) and adapts locally; the
integrator reconciles.

- **P protocol**: protocol/{wire,types,capabilities,dto_mapper,sequencer}.rs,
  tests/{wire,sequencer,dto_mapper,capabilities}.rs
- **T transport**: companion/{http_client,errors,presence_client}.rs,
  tests/presence_client.rs (+ tests/support/mock_server.rs)
- **V privacy**: privacy/*.rs (except mod.rs),
  tests/{sanitize,evaluator,fingerprint,privacy_model,consent_gate is K's}.rs,
  tests/media_session_tracker.rs
- **C capture**: capture/foreground.rs, capture/system_events.rs,
  runtime/suspend_detector.rs
- **M media**: capture/media.rs
- **S stores**: store/*.rs (except mod.rs), runtime/logger.rs
- **K companion service**: companion/{service,coordinator,pairing,authority,
  consent_gate}.rs, tests/{consent_gate,integration}.rs

## Cross-module contract (key public items)

Names below are fixed; signatures may gain `&self`/generics as needed but the
shapes and semantics must hold. All fallible cross-module calls return
`Result<T, E>` with module-local `thiserror` errors.

```rust
// model.rs (scaffold provides COMPLETE definitions, serde-derived, mirroring
// packages/shared and capture/privacy types):
PrivacyDefault, PrivacyOverride, ApplicationPrivacyRule, PrivacyMapping,
PrivacyConfig, RuntimeState, ConnectionSummary, PreviewProjection, Preview,
MediaProviderHealth, CoreStateSnapshot, StoredConnection, CoreConfig,
MediaProviderChoice, CredentialBackend, IpcErrorCode,
ForegroundInfo, MediaSnapshot, MediaKind,
SanitizedPresenceSnapshot (+ nested sanitized app/media types, per privacy/types.ts)

// protocol
pub fn encode_wire_date(epoch_ms: i64) -> Result<String, WireError>;
pub fn decode_wire_date(value: &str) -> Result<i64, WireError>;
pub fn is_valid_wire_identifier(value: &str) -> bool;
pub const PROTOCOL_CLIENT_VERSION: &str = "1.8.3";
pub struct CompanionSequencer { /* reserve() -> u64, reconcile(accepted: u64) */ }
#[async_trait] pub trait SequenceBacking: Send + Sync { async fn load(&self) -> u64; async fn store(&self, next: u64) -> std::io::Result<()>; }
// (exact reserve/reconcile semantics per sequencer.ts + its tests)

// companion transport
pub struct CompanionCredential { pub device_id: String, pub token: String /* per httpClient.ts */ }
pub struct CompanionHttpClient { /* base_url, reqwest client; execute(...) validates envelope, request-id echo, payload caps */ }
pub struct PresenceClient { /* replace_presence(), clear_presence(); FIFO slot; single idempotent retry */ }

// capture
pub struct ForegroundWatcher { /* start/stop; on_change(callback); current() */ }
pub trait MediaProvider: Send + Sync {
    fn kind(&self) -> &'static str;              // "winrt" (see Media note)
    async fn get_snapshot(&self, timeout: Duration) -> Option<MediaSnapshot>;
    fn on_semantic_change(&self, cb: Box<dyn Fn() + Send + Sync>) -> Unsubscribe;
    fn healthy(&self) -> bool;
    async fn stop(&self);
}
pub struct SystemEvents { /* start/stop; on_lock_or_sleep, on_unlock_or_resume */ }

// stores
pub struct ConfigStore { /* get() -> CoreConfig (clone), update(f), on_change */ }
pub trait CredentialStore: Send + Sync { /* backend(), read/write/delete of device credential, per credentials.ts */ }
pub async fn select_credential_store(preferred: Option<CredentialBackend>) -> Box<dyn CredentialStore>;

// privacy
pub struct CaptureService { /* observe() -> (raw capture → evaluator → sanitize) pipeline, per captureService.ts */ }
pub fn policy_fingerprint(...) -> String;        // exact algorithm from fingerprint.ts

// companion service (facade used by the app crate)
pub struct CompanionService { /* start, shutdown, pair, unpair, refresh_preview,
    confirm_consent, disable_live_desk, policy_maybe_changed, handle_sleep_or_lock,
    handle_wake_or_unlock, runtime_state, current_preview, publish_telemetry,
    request_fresh_snapshot (coordinator), on_changed callback */ }
pub struct ServiceError { pub code: IpcErrorCode, ... }
```

Callback registration returns an `Unsubscribe` guard (`Box<dyn FnOnce()>` or a
guard struct — scaffold picks one; everyone uses it consistently).

## Media note

The TS core had two providers (npm NDJSON child process, PowerShell script).
Native Rust talks to SMTC directly via WinRT `Windows.Media.Control`
(`GlobalSystemMediaTransportControlsSessionManager`). One provider, `kind:
"winrt"`. `MediaProviderHealth.kind` gains `"winrt"`; the UI union is updated
accordingly (shared schema + labels). Config `media.provider` values
`"auto" | "npm" | "powershell"` must still PARSE (config compat) but all map to
the WinRT provider. Semantics to preserve from the TS providers: semantic-change
events fire on track/session/playback-state changes, not on position ticks;
snapshot fields incl. `positionSeconds`/`sampledAt` computation; appId derived
from the SMTC SourceAppUserModelId when it looks like an exe name, else null
with `playerDisplayName` fallback (per `SmtcPowershellProvider.ts` +
`smtc-provider.ps1`).

## Threading / async

- Tokio (Tauri's runtime). Core logic is async; locks via `parking_lot` or std
  `Mutex` — no lock held across `.await` (clippy `await_holding_lock` clean).
- Win32 pieces that need a message pump (`SetWinEventHook` alternative not used;
  we PORT the TS polling design for foreground; `system_events.rs` runs a
  hidden message-only window + `WTSRegisterSessionNotification` +
  `WM_POWERBROADCAST` on a dedicated `std::thread` with a Win32 message loop).
- WinRT SMTC event handlers arrive on WinRT threads: marshal into tokio via
  channels; no blocking in handlers.
- Everything Windows-specific compiles under `#[cfg(windows)]`; pure-logic
  modules stay platform-neutral. Non-Windows builds are out of scope; the crate
  may be windows-only (document in core/Cargo.toml).

## Dependencies (core crate)

tokio (rt-multi-thread, sync, time, macros), reqwest (json; default TLS =
schannel on Windows), serde + serde_json, thiserror, async-trait,
windows (pin the latest stable version; features for: Win32_Foundation,
Win32_UI_WindowsAndMessaging, Win32_System_Threading,
Win32_System_ProcessStatus, Win32_Storage_FileSystem,
Win32_System_RemoteDesktop, Win32_System_Power, Win32_Security_Credentials,
Win32_Security_Cryptography, Media_Control, Foundation, Foundation_Collections,
Storage_Streams — trim to what compiles), sha2 (fingerprint, if fingerprint.ts
uses sha-256 — match its algorithm exactly), rand, once_cell.
Dev-deps: axum or hyper for the mock server (mirror test/helpers/mockServer.ts),
tokio-test utilities as needed. Do NOT add zod-like validation crates; serde +
manual checks replicate the strictness.

App crate keeps: tauri (tray-icon), tauri-plugin-single-instance,
tauri-plugin-autostart, tauri-plugin-process, tauri-plugin-updater, url, rand,
serde, serde_json, tokio; DROPS tauri-plugin-shell (no sidecar).

## Tauri surface (app crate)

Commands (names/args mirror `packages/shared/src/ipc.ts` Command union; return
`Result<(), String>` where the `Err` string is the `IpcErrorCode` literal):
`get_state` (returns full `CoreStateSnapshot`), `pair{baseUrl,deviceName,pairingCode}`,
`unpair`, `request_preview`, `confirm_consent{policyFingerprint}`,
`disable_live_desk`, `set_sources{application?,media?}`, `set_privacy{patch}`,
`upsert_rule{rule}`, `delete_rule{appId}`, `set_mappings{mappings}`, `shutdown`.
Plus existing `check_and_install_update`, `is_hidden_launch`.

Events emitted to the `main` window: `core-state` (full snapshot, on every
change — same triggers as `ipc.broadcastState()` in main.ts), `core-preview`
(fresh preview after `request_preview`), `updater-progress` (unchanged).
`get_core_endpoint`, sidecar supervision, `core-ready`/`core-dead` are removed;
the UI treats the core as always-present.

recentAppIds (max 10, MRU) tracking moves into the app crate state, fed by
`ForegroundWatcher::on_change`, same as main.ts.

Shutdown: on `RunEvent::Exit`, run `service.shutdown()` bounded to 2s (bounded
remote clear), mirroring main.ts `gracefulExit` + the old TS-side WS shutdown.

## UI changes (packages/app/src)

- `wsClient.ts` → `coreClient.ts`: same exported API surface the store consumes
  (connect/status callbacks, sendCommand), implemented over
  `@tauri-apps/api/core` `invoke` + `@tauri-apps/api/event` `listen`. Commands
  resolve/reject from the invoke Result; keep zod parsing of incoming snapshots
  via `@yohaku/shared` (validation stays).
- `store.ts`: drop core-ready/port/token handshake; core is available at start.
- `labels.ts` / status page: media provider kind `"winrt"` label; remove
  npm/powershell wording.
- `packages/shared/src/status.ts`: `mediaProviderHealth.kind` enum becomes
  `["winrt","none"]` (UI-only type — keep in sync with Rust serialization).
- Tray (`tray.ts`), background, updater UI: unchanged.

## Build pipeline

- `package.json`: `dist` = `pnpm --filter @yohaku/app tauri build` (no
  build:core / stage:core / fetch:node). `dev:app` = `tauri dev` only.
  Keep `pnpm -r test` (TS tests in packages/core remain as reference and must
  still pass untouched) and add cargo test instructions to README.
- `tauri.conf.json`: remove `bundle.externalBin` and `bundle.resources`;
  everything else (updater, NSIS currentUser, installer hooks) unchanged.
- Delete usage of `scripts/stage-core.mjs` / `scripts/fetch-node-sidecar.mjs`
  from flows and CI; leave `packages/core` sources in place (legacy reference,
  noted in README).
- `.github/workflows/*`: build job = pnpm install, pnpm -r typecheck/test,
  cargo test (workspace), tauri build; no node-sidecar fetch/verify steps.
- `capabilities/default.json`: drop shell permissions; keep updater/process/
  autostart as needed by the remaining JS API usage.

## Test mapping (vitest → cargo)

| vitest | cargo test file |
| --- | --- |
| test/companion/wire.test.ts | core/tests/wire.rs |
| test/companion/sequencer.test.ts | core/tests/sequencer.rs |
| test/companion/dtoMapper.test.ts | core/tests/dto_mapper.rs |
| test/companion/capabilities.test.ts | core/tests/capabilities.rs |
| test/companion/presenceClient.test.ts | core/tests/presence_client.rs |
| test/companion/integration.test.ts | core/tests/integration.rs |
| test/privacy/sanitize.test.ts | core/tests/sanitize.rs |
| test/privacy/evaluator.test.ts | core/tests/evaluator.rs |
| test/privacy/fingerprint.test.ts | core/tests/fingerprint.rs |
| test/privacy/model.test.ts | core/tests/privacy_model.rs |
| test/privacy/consentGate.test.ts | core/tests/consent_gate.rs |
| test/privacy/mediaSessionTracker.test.ts | core/tests/media_session_tracker.rs |
| test/ipc/server.test.ts | superseded by tauri commands (no port) |

Every ported test keeps the original assertion values (byte-exact wire
expectations). Add Rust-side tests for anything the TS suite covered only via
types (zod) — e.g., config parsing edge cases.

## Conventions

- Rust 2021, rustc 1.97 stable, MSVC target only.
- `cargo fmt` formatting; `cargo clippy` clean for real lints (allow pedantic).
- Comment style follows the existing repo: doc comments explain invariants and
  privacy reasoning (port the meaningful TS comments, not line-by-line noise).
- No `unwrap()` on external input paths; `expect()` only for provable
  invariants. No panics in event/callback threads.
- Do not commit; leave all changes in the working tree.
- Logger: `logger::info(scope, msg)` etc. + `recent_log_lines()` ring buffer
  (cap per logger.ts); never log titles/media text/credentials.
