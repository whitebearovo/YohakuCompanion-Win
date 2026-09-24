# Integration notes (append-only)

Implementation agents: append cross-module concerns here (signature drift,
contract gaps, anything you had to adapt locally). The integrator reconciles
these before the verify phase. Format: "## <agent letter> — <topic>" + body.

## Scaffold decisions

Recorded by the scaffolder; binding for implementers unless the integrator
overrules. `lib.rs`, `model.rs`, every `mod.rs` and both `Cargo.toml`s are
scaffold-owned; `model.rs` is COMPLETE (needs no further work).

Conventions
- Callback registration returns `model::Unsubscribe` (guard struct; Drop
  unregisters, `detach()` keeps the callback for the source's lifetime,
  `noop()` for stopped sources). Everyone uses it.
- Errors: per-module `thiserror` types named `<Subject>Error`
  (`WireError`, `SequencerError`, `CompanionTransportError` (one enum
  replacing the TS class family), `PresenceClientError`, `PairingError` /
  `ClaimPairingError`, `ServiceError { code: IpcErrorCode }`,
  `ConfigStoreError`, `CredentialStoreError`, `MediaProviderError`,
  `SystemEventsError`). Logic branches on variants/fields; exact TS message
  strings kept only where ported tests assert them ("disk full"
  passthrough via `SequencerError::Backing(io::Error)`, the four
  `CompanionServerConfigurationError` literals, "sequence space exhausted",
  "no credential backend available").
- Object-safe async traits use the `async-trait` crate: `SequenceBacking`,
  `MediaProvider`, `CredentialStore`.
- Numbers: epoch ms `i64`; seconds `f64`; wire integers `u64` bounded to
  2^53-1 (helpers in `model.rs`); HTTP status `u16`; payload byte counts
  `usize`.
- Dependency closures in deps structs are `Box<dyn Fn(..) -> _ + Send +
  Sync>`; async ones return `model::BoxFuture<T>`.

Signature decisions beyond the ARCHITECTURE contract (conflicts flagged)
- CONFLICT `SequenceBacking`: ARCHITECTURE sketched `load(&self) -> u64` /
  `store(&self, next)`, which cannot express the sequencer spec/tests
  (per-device keys in sequence.json; invalid/negative stored values that
  self-heal to the pairing floor). Kept the name, changed the shape:
  `load(&self, device_id) -> Option<i64>`, `store(&self, device_id, next:
  u64) -> io::Result<()>`. Non-integer JSON stored numbers map to `None` at
  the store layer (behavior-equivalent merge of the TS two-layer
  rejection). `SequenceExhaustedError` is `SequencerError::Exhausted`.
- CONFLICT `CompanionCredential`: ARCHITECTURE's contract line writes field
  `token` but its own comment defers to httpClient.ts, which has
  `deviceToken` → field is `device_token`. Type lives in `model.rs`
  (cross-module) and is re-exported from `companion::http_client` to
  satisfy the ARCHITECTURE placement. No serde derives; `Debug` redacts the
  token.
- `select_credential_store` returns
  `Result<Box<dyn CredentialStore>, CredentialStoreError>` (ARCHITECTURE
  had a bare `Box`; unavailability is a first-class outcome mapping to
  `credentialStoreUnavailable`). `CredentialStore::get` returns
  `Option<String>` (TS get never throws — failures read as "no token");
  `set`/`delete` return `Result`.
- `compare_semantic_versions` returns `std::cmp::Ordering`
  (Less/Equal/Greater ≡ TS negative/zero/positive).
- `CompanionHttpClient::execute<T>` takes the zod-schema equivalent as a
  decode fn pointer `fn(&serde_json::Value) -> Result<T, String>`; the
  decode fns live in `protocol::types` (agent P) so schema strictness stays
  P-owned while transport stays T-owned. `ExecuteOptions` collapses the TS
  `body`/`encodedBody` pair into `encoded_body: Option<&[u8]>` — callers
  always pre-encode via `http_client::encode_body` (what TS did internally),
  making retry bytes identical by construction.
- Decode strictness split: serde derives in `model.rs`/`protocol::types`
  enforce shape, literals (`schema`, `schemaVersion` 2, config `version`
  1), required-nullable key presence (`error.retryAfterMs`,
  `error.acceptedSequence`, `state.projection`) and the 2^53-1 bound;
  wire-date canonicality, UUID/ULID identifier and capability
  safe-integer/token-non-blank refinements happen INSIDE the `decode_*`
  fns after serde. Capabilities ints decode as `i64` (zod allowed
  negatives; positivity is `negotiate_presence`'s job).
- `CoordinatorTimings` is a full Copy struct with `Default`
  (30_000/300_000/500/800); TS `Partial<CoordinatorTimings>` maps to
  struct-update syntax (`CoordinatorTimings { network_retry_ms: 100,
  ..Default::default() }`). Injectable via
  `CompanionServiceDeps::coordinator_timings` (spec trap: ported
  integration tests need 100/200 ms).
- `requested_lease_seconds` is `f64` end-to-end (TS number; the mapper
  rounds half-up then clamps into the negotiated bounds).
- `MediaProvider` trait has no `start()` (per the ARCHITECTURE trait):
  providers are constructed started — `WinRtMediaProvider::new()` is async
  and fallible; `select_media_provider(preference) -> Result<Arc<dyn
  MediaProvider>, _>` and the APP crate maps `Err` to "no provider + error
  log" exactly like main.ts. The capture traits (`MediaProvider`,
  `ForegroundSource`) live in scaffold-owned `capture/mod.rs` so the
  privacy pipeline stays platform-neutral while the impls are
  `#[cfg(windows)]`.
- `exe_stem_display_name` lives in `capture::foreground` (agent C, from
  displayName.ts) and is also called by `capture::media` (agent M) for
  exe-attributed players — single owner, cross-file use.
- `AuthorityRegistry` uses `&mut self` (`resolve` returns
  `&mut PublishAuthority` so the coordinator can overwrite `.client`); the
  coordinator is expected to own it inside its single actor task.
- `coordinator.get_token` folds credential-store errors into `None`
  (TS treats throw and null identically → degraded, T3).
- `ClaimPairingError::Other` covers non-PairingError throws (the
  ensure-credential-store hook) → service maps it to `pairingFailed`,
  mirroring the TS table's catch-all row.
- `ConfigStore::update` takes `impl FnOnce(&mut CoreConfig)` (TS took a
  functional `(config) => CoreConfig` over a deep clone — same semantics).
- `CompanionService::coordinator()` accessor exposes the owned coordinator
  (main wiring calls `service.coordinator().request_fresh_snapshot()` /
  `.start()`); ARCHITECTURE's facade line "request_fresh_snapshot
  (coordinator)" is satisfied through it. `note_error` is ported (dead code
  in TS, safe parity per companion spec ambiguity 3).
- `PrivacyMapping.mapping_type` carries `#[serde(rename = "type")]`
  (`type` is a Rust keyword).
- Raw capture types (`ForegroundInfo`, `MediaSnapshot`) and
  `CompanionCredential` deliberately have NO serde derives — compile-time
  privacy guard.

Dependencies (Cargo.toml is scaffold-owned — implementers cannot add crates;
flag needs here instead)
- Beyond the ARCHITECTURE list, added: `uuid` (v4 — requestId/sessionId),
  `url` (WHATWG base-URL parse/normalize per httpClient.ts), `chrono`
  (wire dates; protocol spec §12.8 endorses strict parse + reformat),
  `unicode-normalization` (NFC; privacy spec §11.3), `parking_lot`
  (allowed by the ARCHITECTURE threading section).
- `reqwest` is `default-features = false, features = ["json",
  "native-tls"]`: reqwest 0.13's default TLS became rustls/aws-lc, but the
  ARCHITECTURE mandates schannel on Windows (native-tls ⇒ schannel); this
  also avoids the cmake/nasm build dependency of aws-lc-sys. No http2/proxy
  features — closest to undici's HTTP/1.1-no-proxy behavior.
- `windows` 0.62.2 is target-gated to `cfg(windows)` with the
  capture-stores §12 feature list (trim unused features in the verify
  phase).
- Dev-deps: tokio gains `net,io-util,test-util` + `tokio-test`. NO
  axum/hyper: the mockServer.ts contract needs per-request `socketDestroy`
  (drop the TCP connection without responding), raw-body byte capture and
  a one-shot handler queue — cleanest as a hand-rolled HTTP/1.1 accept
  loop over `tokio::net::TcpListener` in `tests/support/mock_server.rs`.
  If agent T prefers hyper, append a note and the integrator adds it.

Open items for the integrator
- `core-preview` Tauri event vs snapshot-only previews (companion spec
  ambiguity 12.5) — app-crate decision, no core type impact.
- Stale-client publish window (coordinator trap 11) ported as observable
  behavior; revisit only with an explicit deviation note.

## K — companion service layer landed

- All five modules implemented; `tests/consent_gate.rs` (8) and
  `tests/integration.rs` (14) ported and PASSING against T's
  `tests/support/mock_server.rs` (consumed its documented builder API as-is
  — no drift to reconcile).
- Concurrency port (coordinator.rs), for the parity reviewer:
  - TS's lock-free between-await mutation maps to ONE
    `parking_lot::Mutex<Inner>`; every synchronous segment between awaits is
    one lock hold, never held across `.await`. State-change callbacks are
    DEFERRED and fired after unlock (they re-enter via snapshot ->
    `current_state()`); under thread contention two notifications can arrive
    out of order — benign (the callback carries no payload the service
    uses; snapshots read current state), but noted.
  - `configure()` gained a generation check at the top of its body: in TS
    the pre-first-await prefix (T2 pairing gate) ran synchronously inside
    `start()` and could never see a moved generation; the spawned Rust task
    can. The check makes Rust identical in all TS-reachable schedules.
  - Trap 5 port: the underlying clear is spawned DETACHED (never aborted);
    `cleanup_task` stores the bounded JOIN (`tokio::time::timeout` over the
    JoinHandle) — timeout drops the handle, not the request.
  - Refresh-loop single flight: `refresh_loop_running` is claimed/released
    only under the inner lock; the T13 renegotiation exit clears it
    ATOMICALLY with discard+start so the new generation's configure can
    never observe a stale flag.
- service.rs: added `pub type CredentialsFn` alias; the
  `CompanionServiceDeps::credentials` field type is byte-identical to the
  scaffold signature, just named (clippy type_complexity).
- Non-ServiceError TS throws that the IPC handler mapped to `"internal"`
  (credential `set` during pair, `config.update` failures, sequence `remove`
  during unpair) are mapped to `ServiceError(Internal)` inside the facade —
  Rust's typed `Result` absorbs the handler's catch-all at this boundary.
- Preserved verbatim (spec §12 ambiguities): `note_error` dead code;
  `last_publish_at` never reset; T4 degraded dead-end with NO retry timer;
  stale-client publish window (`begin_new_generation` does not null the
  client); `suspended` shown for paired-but-disabled sleep; pairing skips
  requestId echo + claim needs no meta; ANY pairing capabilities failure ->
  `network`; unpair error-tolerance asymmetry (credential delete tolerated,
  sequence remove aborts); `alreadyPaired` never produced.
- Test fakes: `FakeMediaProvider::kind()` returns the TS literal `"npm"`
  (trait returns `&'static str`; never asserted, `MediaProviderKind` enum
  not involved).


## C — Win32_Graphics_Gdi feature gap (hidden-window workaround)

`RegisterClassW`/`RegisterClassExW` and `WNDCLASSW`/`WNDCLASSEXW` in
`windows` 0.62.2 are `#[cfg(feature = "Win32_Graphics_Gdi")]` (the structs
carry an `HBRUSH` field), and that feature is not in the scaffold-owned
Cargo.toml list. Adapted locally: `capture::system_events` creates its
hidden ordinary top-level window with the PREDEFINED `"STATIC"` class
(no registration needed, `hInstance` null) and subclasses it via
`GetWindowLongPtrW`/`SetWindowLongPtrW(GWLP_WNDPROC)` (ungated), keeping
the shared state in `GWLP_USERDATA` and forwarding unhandled messages to
the original class proc with `CallWindowProcW`. Behavior is identical
(the window receives `WM_POWERBROADCAST` broadcasts and the registered
`WM_WTSSESSION_CHANGE`); no Cargo.toml change is required. If the
integrator prefers an own window class, add `Win32_Graphics_Gdi` and it
can be swapped in the verify phase.

Related trim hint for the verify phase: agent C's modules do NOT use
`Win32_System_LibraryLoader` (no `GetModuleHandleW`; `hInstance` is null
for the predefined class) nor `Win32_System_ProcessStatus` — drop them
only if agents M/S do not need them either.

Note: C's task briefing said "message-only window (HWND_MESSAGE)" in
passing; implemented per the binding spec §4.2 TRAP and the scaffold stub
instead (hidden ORDINARY top-level window — message-only windows do not
receive `WM_POWERBROADCAST`).

## S — base64 helper (no crate)

The scaffold Cargo.toml carries no base64 crate, so `store/dpapi_store.rs`
contains a local RFC 4648 standard-alphabet codec (encode padded single-line,
strict decode) matching `[Convert]::To/FromBase64String` for the files we
write/read. Pinned by RFC 4648 test vectors in-module. If the integrator
prefers the `base64` crate, the two private fns swap out cleanly.

## S — DPAPI token stored verbatim (deviation-by-spec)

The PS-era transport trimmed the token (`.Trim()` on protect input and
unprotect output); per capture-stores §9.4 and ambiguity §13.8 the Rust port
stores/returns the token VERBATIM (UTF-8 bytes, in-process
CryptProtectData/CryptUnprotectData, CurrentUser, UI_FORBIDDEN, no entropy).
A whitespace-padded token now round-trips unchanged where the legacy path
would have trimmed it — companion tokens contain no whitespace in practice.
Pinned by `token_with_surrounding_whitespace_round_trips_verbatim`.

## S — credential backend construction failures

TS `selectCredentialStore` constructed BOTH candidates eagerly, so a
`DpapiCredentialStore` constructor throw (APPDATA unset / mkdir denied)
escaped selection uncaught even when keyring would have qualified. Rust
constructs candidates lazily and treats a construction failure as that
candidate's probe failure (`warn credentials: "<backend> backend
unavailable"`, continue); all candidates failing still yields the exact
`Unavailable` ("no credential backend available"). Deliberate edge-case
deviation in favor of the spec's probe semantics.

## S — keyring delete swallows all errors (TS parity)

`KeyringCredentialStore::delete` returns `Ok(())` for EVERY CredDeleteW
outcome, not just ERROR_NOT_FOUND — the TS wrapper's catch swallowed all
deletePassword throws, and a keyring probe therefore can only fail via
set/get. The raw `delete_credential` helper still surfaces the win32 error
for the ignored manual test.

## S — shared lenient JSON-object reader

The duplicated private TS `read()` helpers of sequenceStore/dpapiCredentialStore
exist once as `pub(crate) store::config::read_json_object` (next to
`atomic_write_json`, same shared-file-IO role). Non-object/missing/corrupt ->
empty map, no logging, no `.bak`.
## T — mock server landed; API for K

`core/tests/support/mock_server.rs` (declare `mod support;` in your test
crate, then `use support::mock_server::*`). Full doc at the top of the file.
- `MockCompanionServer::start().await` / `.base_url()` / `.requests()`
  (snapshot, arrival order) / `.stop().await` (Drop also shuts down).
- `.enqueue(|req| ...)` one-shot FIFO handlers; `.set_fallback(|req| ...)`
  when the queue is empty; neither -> `500 {"unexpected":true}`. Handlers
  return anything `Into<MockOutcome>`: `MockResponse { status, body:
  serde_json::Value }` or `MockOutcome::SocketDestroy`.
- Builders (mockServer.ts literals): `response_meta(request_id)`,
  `mutation_success(req, accepted_sequence: Option<u64>)`,
  `error_envelope(req, status, code, ErrorEnvelopeOptions { retryable,
  accepted_sequence })`, `capabilities_response(CapabilitiesPatch {
  minimum_client_version, live_desk, media_timeline,
  presence_schema_versions, requests_per_minute })` — patch/options structs
  derive `Default`.
- `RecordedRequest { method, path, headers (lowercased names), raw_body,
  json: Option<Value> }`.

Deviations (contract-invisible, documented in the file header): every mock
response sends `Connection: close` (one TCP connection per request — keeps
hyper's pooled-connection retry away so "exactly N requests" assertions stay
exact; the Node mock kept connections alive). `SocketDestroy` closes without
writing any response byte (same observable as Node `destroy()` after the
request was read: the client surfaces a network error).

## T — transport implementation notes

- `CompanionTransportError::Server` keeps the scaffold's inline
  `envelope: ErrorEnvelope` (clippy `result_large_err` waived at the two
  flagged sync helpers with comments). If the integrator prefers
  `Box<ErrorEnvelope>`, only errors.rs + http_client.rs construction sites
  change; matching via field access stays source-compatible.
- `CompanionHttpClient::execute` echo-checks `meta.requestId` on the RAW
  decoded JSON value (loose lookup, strict string compare) — equivalent to
  the TS check on the zod-parsed payload for every schema-passing body, and
  it keeps `execute<T>` generic over the decode fn pointer.
- reqwest is built with only the whole-request 10 s timeout; base-URL
  validation replicates JS `URL` edge semantics (empty query/fragment and
  empty username+password pass, per httpClient.ts `!== ""` checks).

## V — scalar_length dead export (spec A1 vs task directive)

The implement-phase directive said "do NOT port dead code; note it", but the
privacy spec (A1) and the scaffolded stub both keep `scalar_length` in
`privacy::model`'s public surface. Kept it implemented (three lines, doc
comment marks it as a dead export; only `tests/privacy_model.rs` calls it).
Integrator may delete the fn + its test together if the directive wins.

## V — js_trim / is_js_whitespace made pub in privacy::model

`protocol::types` (agent P) currently references a not-yet-existing
`dto_mapper::is_js_whitespace` for the wire re-trim. The ECMAScript
whitespace table (trims U+FEFF, keeps U+0085) now exists once as
`privacy::model::{is_js_whitespace, js_trim}` (pub, spec §11.1) — P/the
integrator can point the dto_mapper re-trim at it instead of duplicating
the table.

## V — fingerprint canonical JSON is hand-emitted

`policy_fingerprint` does NOT build a `serde_json::Value` (a
`preserve_order` feature unification from any workspace dependency would
silently reorder keys). The canonical projection is emitted directly with
the spec §6.4 sorted key order; user strings are escaped via
`serde_json::to_string` (matches V8 `JSON.stringify` for well-formed
strings). Rule/mapping sorts use a UTF-16 code-unit comparator; the mapping
sort key reproduces the literal backslash+zero separator (A8). All 9 golden
vectors from spec §6.6 are pinned in `tests/fingerprint.rs`.

## V — capture_service media timeout placement

`capture_for_delivery` wraps `provider.get_snapshot(timeout)` in an OUTER
`tokio::time::timeout` of `MEDIA_TIMEOUT_MS` (2000) — the TS caller-side
race — and also passes the same `Duration` into the trait method (the
ARCHITECTURE signature requires one). Providers may bound internally;
either way a hung provider yields "no media" after ~2 s and the losing
future is dropped. Provider errors must already be folded to `None` inside
`MediaProvider::get_snapshot` per the trait contract (agent M).

## I — integration phase landed (app crate + UI + pipeline)

- App crate rewired to the native core: `src/state.rs` (service graph port of
  main.ts, snapshot assembly, recentAppIds MRU-10, bounded 2 s shutdown on
  `RunEvent::Exit`), `src/commands.rs` (Command union as Tauri commands,
  handlers.ts routing incl. normalized_rule/is_empty_rule + UTF-16 rule sort),
  `src/lib.rs` (sidecar supervision, get_core_endpoint, tauri-plugin-shell,
  rand all removed; updater + is_hidden_launch kept).
- Open item 12.5 RESOLVED: snapshot-only previews. Ground truth: the TS ipc
  server never emitted the standalone "preview" message; refreshPreview
  reached the UI via onChanged -> state broadcast. No `core-preview` event.
- Credentials memoization: one `tokio::sync::OnceCell` selection per process,
  failure cached as `Unavailable` (TS cached the rejected promise). Deviation:
  a config-write failure while persisting the backend pin logs a warning and
  keeps the selected store (TS would have failed the whole credentials
  promise); selection still never re-runs.
- UI: wsClient.ts -> coreClient.ts (invoke + core-state listen, same send()
  surface, zod snapshot validation kept); store drops coreDead/handshake;
  shared `mediaProviderHealth.kind` -> ["winrt","none"] (legacy
  packages/core/src/main.ts gained a cast to keep the reference typecheck
  green); labels: winrt = "系统媒体控制 (SMTC)". SendErrorCode keeps
  timeout/disconnected as never-produced legacy codes (labels stay total).
- Pipeline: package.json dist/dev:app are pure tauri build/dev
  (build:core/stage:core/fetch:node/dev:core removed; both staging scripts
  deleted); tauri.conf.json drops externalBin + resources; CI + release run
  `cargo test --workspace`; installer PREINSTALL keeps the legacy
  yohaku-core-node.exe taskkill for upgrades from sidecar-era installs.
- Verify phase: trimmed `Win32_System_LibraryLoader` +
  `Win32_System_ProcessStatus` (no references; C's hint held for M/S too).
  cargo test --workspace green (225 core tests), clippy --workspace
  --all-targets clean, pnpm -r typecheck/test green (144 legacy TS tests),
  vite production build green.
