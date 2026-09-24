# Companion Service Layer / Live Desk State Machine — Behavior Spec

Ground truth extracted from `packages/core` (TypeScript). Audience: the Rust
implementer of `companion/{service,coordinator,pairing,authority,consent_gate}.rs`
and the parity reviewer. Every number, string, ordering and guard in this
document is literal from the TS source; do not "improve" any of it.

Source files covered:

- `packages/core/src/companion/service.ts` (CompanionService, ServiceError)
- `packages/core/src/companion/coordinator.ts` (LiveDeskCoordinator)
- `packages/core/src/companion/pairing.ts` (claimPairing, PairingError)
- `packages/core/src/companion/authority.ts` (AuthorityRegistry)
- `packages/core/src/companion/consentGate.ts` (ConsentGate, projectionOf)
- `packages/core/src/main.ts` (wiring, triggers, shutdown)
- `packages/core/src/ipc/server.ts`, `src/ipc/handlers.ts` (command routing)
- `packages/core/test/companion/integration.test.ts`
- `packages/core/test/privacy/consentGate.test.ts`
- `packages/core/test/companion/presenceClient.test.ts` (service-relevant parts)

Referenced shapes (already specified elsewhere, do NOT re-derive here):

- `packages/shared/src/status.ts`: `RuntimeState`, `ConnectionSummary`,
  `PreviewProjection`, `Preview`, `MediaProviderHealth`, `CoreStateSnapshot`.
- `packages/shared/src/ipc.ts`: `Command` union, `IpcErrorCode`,
  `ServerMessage`, `PrivacyPatch`, hello message.
- `packages/core/src/privacy/types.ts`: `SanitizedPresenceSnapshot` and nested
  sanitized application/media/playback types.
- `packages/core/src/store/configStore.ts`: `StoredConnection`
  `{ baseUrl, deviceId, deviceName, scopes: string[], pairingNextSequence: int >= 0, liveDeskEnabled: bool }`,
  `CoreConfig` (version 1, privacy, connection nullable, media.provider,
  credentialBackend nullable).
- Protocol/transport internals (`presenceClient.ts`, `sequencer.ts`,
  `httpClient.ts`, `dtoMapper.ts`, `capabilities.ts`, `errors.ts`) are owned by
  agents P and T; this spec cites only the behaviors the service layer
  depends on.

---

## 1. Component ownership and collaboration

`CompanionService` (facade) owns exactly one `ConsentGate` and one
`LiveDeskCoordinator`. The coordinator owns one `AuthorityRegistry`
(`coordinator.registry`, a public readonly field). The service is constructed
with dependencies:

```
CompanionServiceDeps {
  config: ConfigStore                          // get() / update()
  capture: CaptureService                      // captureForDelivery, fingerprint, resetMediaContinuity
  sequenceStore: FileSequenceStore             // load/store/remove per deviceId
  credentials: () => Promise<CredentialStore>  // lazily resolved, memoized by caller
  onChanged: () => void                        // "externally visible change" -> UI snapshot broadcast
  coordinatorTimings?: Partial<CoordinatorTimings>
}
```

Service constructor behavior (order matters):

1. `gate = new ConsentGate(deps.capture.fingerprint())` — the gate's initial
   fingerprint is the policy fingerprint at construction time.
2. `coordinator = new LiveDeskCoordinator(coordinatorDeps, deps.coordinatorTimings ?? {})`
   where coordinatorDeps are:
   - `capture`: same CaptureService
   - `sequenceStore`: same store
   - `getConnection: () => config.get().connection`
   - `getToken: async (deviceId) => (await deps.credentials()).get(deviceId)`
   - `onStateChange: () => deps.onChanged()` (state value ignored by service)
   - `onPublished: () => { lastPublishAt = Date.now(); lastError = null; deps.onChanged(); }`

Invariants enforced by the facade (from its doc comment; all verified in code):

- pairing always lands with `liveDeskEnabled = false`;
- enabling Live Desk validates consent BEFORE and AFTER persisting, and rolls
  back to disabled on any drift (`previewOutOfDate`);
- disabling persists first, then clears remotely;
- a policy change invalidates the preview; when Live Desk is already enabled
  it republishes under the new policy instead of revoking consent.

---

## 2. RuntimeState machine

Enum (from `packages/shared/src/status.ts`, exact serialization strings):
`notPaired`, `disabled`, `connecting`, `active`, `degraded`, `suspended`,
`updateRequired`, `serverFeatureUnavailable`.

### 2.1 Where each state lives

`notPaired` is NOT a coordinator state. It is synthesized at the service level:

```
CompanionService.runtimeState():
  if config.get().connection === null -> "notPaired"
  else -> coordinator.currentState()
```

The coordinator's internal state starts as `"disabled"` and only ever holds
the other seven values.

### 2.2 State meanings and publishing implications

| State | Meaning | Publishing |
| --- | --- | --- |
| `notPaired` | No `connection` in config (service-level view). Coordinator underneath is usually `disabled`. | Never. |
| `disabled` | Coordinator idle: not paired, Live Desk off, or after shutdown(reason). No timers for this generation. | Never. |
| `connecting` | A fresh generation is negotiating: token fetch + GET capabilities + negotiate. | Not yet. `requestFreshSnapshot()` is ignored in this state. |
| `active` | Negotiation succeeded; client built; heartbeat timer running. | Yes: every trigger publishes a FRESH capture. |
| `degraded` | Recoverable failure: missing token, capabilities fetch failure, invalid capabilities, invalid stored baseUrl, or a publish failure. | No proactive publish, but `requestFreshSnapshot()` IS accepted and will attempt a publish if a client exists; success flips straight back to `active`. Heartbeat (if the generation had reached active before degrading) keeps firing and retrying. |
| `suspended` | Lock/sleep observed. Presence was best-effort cleared with reason `sleep`. | Never; triggers ignored until wake. |
| `updateRequired` | Server `minimumClientVersion` > `PROTOCOL_CLIENT_VERSION` ("1.8.3"). Terminal until an app update; NO retry timer. | Never. |
| `serverFeatureUnavailable` | Server does not offer presence schema v2 or `features.liveDesk` is false. Retries every `featureRetryMs`. | Never. |

### 2.3 Full transition table

Guards are exact. "gen check" means `if (gen !== this.generation) return;`
re-checked after every await. `beginNewGeneration(S)` = `generation += 1`;
clear heartbeat interval + reconnect timeout; `refreshRequested = false`;
`capture.resetMediaContinuity()`; `setState(S)`. `setState` is a no-op when
the value is unchanged, otherwise it stores and fires `onStateChange(state)`.

| # | From | Trigger | Guard | To | Side effects |
| --- | --- | --- | --- | --- | --- |
| T1 | any | `start()` | `!stopping` | `connecting` | `beginNewGeneration("connecting")`; spawn `configure(generation)` (fire-and-forget). |
| T2 | `connecting` | `configure`: connection is null OR `!liveDeskEnabled` | — | `disabled` | Nothing else (no timers). |
| T3 | `connecting` | `configure`: `getToken` threw or returned null | gen check after the await | `degraded` | `scheduleRetry(gen, networkRetryMs)`. |
| T4 | `connecting` | `configure`: `new CompanionServerConfiguration(connection.baseUrl)` threw (invalid stored base URL) | — | `degraded` | NO retry timer (dead end until an external `start()`). |
| T5 | `connecting` | `configure`: GET `/companion/capabilities` threw (network / decode / non-2xx) | gen check | `degraded` | `scheduleRetry(gen, networkRetryMs)`. |
| T6 | `connecting` | negotiation kind `clientUpdateRequired` | gen check after capabilities | `updateRequired` | Terminal: no timer. |
| T7 | `connecting` | negotiation kind `schemaUnsupported` or `featureUnavailable` | gen check | `serverFeatureUnavailable` | `scheduleRetry(gen, featureRetryMs)`. |
| T8 | `connecting` | negotiation kind `invalidCapabilities` | gen check | `degraded` | `scheduleRetry(gen, featureRetryMs)` — NOTE featureRetryMs, not networkRetryMs. |
| T9 | `connecting` | negotiation kind `available` | gen check | `active` | Compute negotiated parameters (section 3.1); resolve authority; build new `PresenceClient`; start heartbeat interval; `refreshRequested = true`; run refresh loop. |
| T10 | `active` or `degraded` | `requestFreshSnapshot()` | state is exactly `active` or `degraded` | (unchanged) | `refreshRequested = true`; run refresh loop with current generation. All other states: silently ignored. |
| T11 | `active` (or `degraded` w/ live heartbeat) | heartbeat tick | gen check only (NO state check) | (unchanged) | `refreshRequested = true`; run refresh loop. |
| T12 | loop iteration | publish success | gen check | `active` | `onPublished` callback (service sets `lastPublishAt = Date.now()`, `lastError = null`, fires `onChanged`). |
| T13 | loop iteration | publish error with `needsRenegotiation(error)` (server code `COMPANION_SCHEMA_UNSUPPORTED` / `COMPANION_FEATURE_UNAVAILABLE`, or bare HTTP 426) | gen check | `connecting` | log warn; `discardAuthority()`; `start()`; loop returns. |
| T14 | loop iteration | publish error, any other | gen check | `degraded` | log warn; `scheduleRetry(gen, networkRetryMs)`; loop returns (pending `refreshRequested` is abandoned). |
| T15 | `degraded` / `serverFeatureUnavailable` | reconnect timer fires | `gen === generation && !stopping` | `connecting` | `start()`. |
| T16 | any except `suspended` | `handleSleepOrLock()` | `state !== "suspended" && !stopping` | `suspended` | Capture current client ref; `beginNewGeneration("suspended")`; if client non-null: `cleanupTask = clearBestEffort(client, "sleep", sleepClearTimeoutMs)`; log "suspended (lock/sleep)". |
| T17 | `suspended` | `handleWakeOrUnlock()` | `state === "suspended"` | `connecting` (async) | Take `cleanupTask` (set null); spawn async: await pending clear (bounded promise), then `if (state === "suspended" && !stopping) start()`. |
| T18 | any | `shutdown(reason)` (default `"shutdown"`) | `!stopping` (else immediate return WITHOUT waiting) | `disabled` | `stopping = true`; capture client; `wasSuspended = (state === "suspended")`; `beginNewGeneration("disabled")`; await pending `cleanupTask` (nulled); if client non-null AND `!wasSuspended`: `await clearBestEffort(client, reason, shutdownClearTimeoutMs)`; `client = null`; finally `stopping = false`. |
| T19 | view level | `config.connection` becomes null | — | `notPaired` | Pure function of config; no coordinator involvement. |

Additional notes:

- `scheduleRetry(gen, delayMs)` replaces any existing reconnect timer; the
  callback is a no-op when the generation moved or `stopping` is true. Timers
  are `unref()`d (must not keep the process alive).
- `discardAuthority()` = `registry.discard(); client = null;`. It is called on:
  publish renegotiation signal (T13), unpair, and pairing replacement. It is
  NOT called on sleep/wake or plain degraded retries — the sequencer must
  survive those.
- `beginNewGeneration` does NOT null `this.client`. Only `shutdown()` and
  `discardAuthority()` do. See trap 11.
- `handleSleepOrLock` from `disabled`/`connecting` is legal and yields
  `suspended` (with no clear when client is null). On wake, `configure`
  re-derives the correct terminal state (e.g. back to `disabled`).

---

## 3. Coordinator publish loop

### 3.1 Timings and negotiated parameters

```
CoordinatorTimings (defaults):
  networkRetryMs        = 30_000
  featureRetryMs        = 300_000
  sleepClearTimeoutMs   = 500
  shutdownClearTimeoutMs= 800
```

Pre-negotiation instance defaults (only relevant before the first successful
configure; the client is null until then so no publish can use them):
`includeMedia = false`, `requestedLeaseSeconds = 90`, `minSendIntervalMs = 2000`,
`lastSendStartedAt = 0`.

On negotiation kind `available` with configuration `config`:

```
includeMedia          = config.supportsMediaTimeline
requestedLeaseSeconds = min(max(90, config.leaseMinSeconds), config.leaseMaxSeconds)
heartbeatSeconds      = min(config.recommendedHeartbeatSeconds,
                            max(1, requestedLeaseSeconds / 3))   // float division
minSendIntervalMs     = ceil(60_000 / config.requestsPerMinute)
```

With the test-suite capabilities (leaseMin 30, leaseMax 120, heartbeat 45,
rpm 120): requestedLeaseSeconds = 90, heartbeatSeconds = min(45, 30) = 30,
minSendIntervalMs = 500.

Heartbeat: `setInterval(heartbeatSeconds * 1000)` (float ms allowed), unref'd,
callback: `if (gen !== generation) return; refreshRequested = true; runRefreshLoop(gen)`.
The heartbeat is the lease-renewal cadence — every tick performs a full fresh
publish, which renews the lease (there is no separate renewal call).

Authority resolution (see section 4) then:

```
authority.client = new PresenceClient(http, { deviceId, deviceToken: token },
                                      authority.sequencer, config)
this.client = authority.client
setState("active")
start heartbeat
refreshRequested = true; runRefreshLoop(gen)     // immediate first publish
```

The `http` client used for capability GET and handed to the PresenceClient is
built fresh per configure from `connection.baseUrl`. The capabilities GET is
UNauthenticated (no Authorization, no `X-Yohaku-Companion-Version` header —
the version header is attached only alongside a Bearer credential).

### 3.2 Refresh loop algorithm (exact)

Single-flight: guarded by `refreshLoopRunning` (checked and set synchronously
before the first await; reset in `finally`). Triggers only set
`refreshRequested = true` and call the loop; a running loop absorbs them as
one extra iteration (coalescing/debounce).

```
runRefreshLoop(gen):
  if refreshLoopRunning: return
  refreshLoopRunning = true
  try:
    while gen == generation && refreshRequested:
      refreshRequested = false
      wait = lastSendStartedAt + minSendIntervalMs - now()
      if wait > 0: sleep(wait)                    # throttle from SEND START
      if gen != generation: return
      client = this.client
      if client == null: return
      lastSendStartedAt = now()
      snapshot = await capture.captureForDelivery({ includeMedia })
      if gen != generation: return
      try:
        await client.replacePresence(snapshot, requestedLeaseSeconds)
        if gen != generation: return
        setState("active")                        # degraded -> active on success
        onPublished?()
      catch error:
        if gen != generation: return
        if needsRenegotiation(error):
          log warn "schema/feature rejected; renegotiating"
          discardAuthority(); start(); return
        log warn "publish failed; degraded"
        setState("degraded"); scheduleRetry(gen, networkRetryMs); return
  finally:
    refreshLoopRunning = false
```

Key semantics:

- Every publish is a FRESH capture; snapshots are never cached or replayed.
- Throttle: at most one send start per `minSendIntervalMs`, measured from the
  previous send's start (`lastSendStartedAt` is set before capture, after the
  wait). `lastSendStartedAt` is NOT reset by `beginNewGeneration` — it
  throttles across generations (e.g. wake publish vs pre-sleep publish).
- On publish failure the loop RETURNS; a `refreshRequested` set during the
  failed attempt is dropped (recovery comes from the retry timer or the still-
  running heartbeat).
- `replacePresence(snapshot, requestedLeaseSeconds)` (transport layer): FIFO
  send slot, durable sequence reservation before send, single idempotent retry
  with identical bytes, acceptedSequence reconciliation. The wire
  `lease.ttlSeconds = min(max(round(requestedLeaseSeconds), leaseMinSeconds), leaseMaxSeconds)`.
  When both application and media sanitize to null the request is still sent
  with `availability: "idle"` — an empty state is a publish, not a clear.

### 3.3 Degraded entry/exit summary

Entry: T3 (no token), T4 (invalid baseUrl, no retry), T5 (capabilities fetch
failed), T8 (invalidCapabilities), T14 (publish failure).

Exit paths:
1. Reconnect timer (`networkRetryMs` for T3/T5/T14; `featureRetryMs` for T8)
   fires -> `start()` -> full renegotiation.
2. `requestFreshSnapshot()` while degraded with a non-null client -> publish
   attempt -> on success `setState("active")` directly (no renegotiation).
3. Heartbeat tick (if this generation reached active before degrading; the
   heartbeat interval is NOT cleared by T14) -> same as (2). Every failed
   attempt re-arms the reconnect timer (scheduleRetry replaces it).

There is NO hysteresis: a single success flips degraded -> active; a single
non-renegotiation failure flips active -> degraded. There is no jitter and no
exponential backoff — fixed delays only.

### 3.4 Suspend / resume

`handleSleepOrLock()` (T16): new generation in `suspended`, then a BOUNDED
best-effort clear using the pre-generation client:

```
clearBestEffort(client, reason, timeoutMs):
  try:
    await Promise.race([ client.clearPresence(reason, Date.now()), sleep(timeoutMs) ])
  catch: /* swallowed; server lease expiry is the correctness backstop */
```

Reason is `"sleep"`, timeout `sleepClearTimeoutMs` (500 ms). The race does NOT
cancel the underlying HTTP request — it may complete (and reconcile its
acceptedSequence) in the background after the bound (see trap 5). The race
promise is stored in `cleanupTask`.

`handleWakeOrUnlock()` (T17): only from `suspended`. Takes `cleanupTask`, then
asynchronously: awaits it (i.e. joins the BOUNDED race, max ~500 ms more),
then `if (state === "suspended" && !stopping) start()`. The join exists so the
wake snapshot's sequence reservation cannot be reordered ahead of the clear's
(they also share the sequencer via the authority — section 4).

### 3.5 `shutdown(reason)` (T18)

- Idempotency: while a shutdown is in flight, a second call returns
  immediately (it does NOT await the first). `stopping` resets to false in
  `finally`, so the coordinator is restartable afterwards (pairing replacement
  relies on this).
- If the coordinator was `suspended`, the final clear is SKIPPED (the sleep
  clear already ran/is running; it is joined via `cleanupTask`).
- Otherwise, if a client exists: bounded clear with the given reason and
  `shutdownClearTimeoutMs` (800 ms).
- `client = null` at the end; registry untouched (callers that need a fresh
  sequencer call `discardAuthority()` explicitly).

Reasons used per call site (`ClearReason` wire values):

| Call site | Reason |
| --- | --- |
| `service.disableLiveDesk()` | `"paused"` |
| `service.unpair()` and `service.pair()` (replacing an existing pairing) | `"connectionRemoved"` |
| `service.shutdown()` (process exit) | `"shutdown"` |
| lock/sleep best-effort clear | `"sleep"` |
| (defined but NEVER used by this codebase) | `"privacyChanged"` |

---

## 4. AuthorityRegistry

One ordered writer per paired `(baseUrl, deviceId)`:

```
key = `${baseUrl}|${deviceId}`
resolve(baseUrl, deviceId, factory):
  if current != null && current.key == key: return current.authority
  current = { key, authority: factory() }; return it
peek(): current?.authority ?? null
discard(): current = null
```

Coordinator usage in `configure`:

- factory builds `{ sequencer: new CompanionSequencer(sequenceStore, deviceId, connection.pairingNextSequence), client: <placeholder> }`;
- the client field is ALWAYS overwritten after resolve with a brand-new
  `PresenceClient` built from the fresh negotiation (deliberate: avoids the
  stale-mapper reuse bug the macOS implementation exhibits — payload limits
  and lease bounds always come from the newest capabilities).

Consequences:

- The SEQUENCER survives renegotiation, sleep/wake, and degraded retries for
  the same (baseUrl, deviceId) — this is what keeps sequences monotonic across
  a sleep clear and a wake publish (integration test "sequences stay
  monotonic...").
- The PresenceClient (and its FIFO send slot) does NOT survive renegotiation:
  ordering across renegotiation is guaranteed only by sequence numbers (via
  the shared sequencer's internal serialization), not by HTTP submission
  order.
- Discard (forcing a fresh sequencer object on next resolve — still seeded by
  the same persistence file + `pairingNextSequence`) happens on: capability
  rejection during publish, `unpair`, pairing replacement.

### 4.1 Publish authority decision (policy + consent + sources + runtime)

What may go on the wire is decided in layers; the Rust port must keep each
check at its layer:

1. Pairing gate: `configure` refuses (-> `disabled`) unless
   `connection != null && connection.liveDeskEnabled`.
2. Consent gate: enforced only at the ENABLE boundary
   (`confirmConsent`, section 6.4). Once `liveDeskEnabled` is persisted true,
   individual publishes do NOT re-check consent; a policy change republishes
   under the new policy instead of pausing (service doc invariant).
3. Sources + privacy policy: applied inside
   `capture.captureForDelivery({ includeMedia })` at capture time. Media is
   captured only when `includeMedia` (negotiated `mediaTimeline`) AND
   `privacy.sources.media` is true at the moment of capture (re-read after
   each await, fail-closed). Application is included only when
   `privacy.sources.application` is true.
4. Runtime state: loop only runs toward the wire from `active`/`degraded`
   generations that completed negotiation (client non-null).

A CLEAR (instead of a publish) is required exactly on the lifecycle
transitions listed in section 3.5's reason table. A privacy change never
clears; a fully-hidden capture publishes `availability: "idle"`.

---

## 5. Pairing

### 5.1 `claimPairing(baseUrl, deviceName, pairingCode, ensureCredentialStore)`

Free function. The one-time pairing code is consumed only AFTER (1) the
credential store proved writable and (2) capabilities negotiated successfully.

Steps, in order:

1. `code = pairingCode.trim()`; require `1 <= code.length <= 32` (UTF-16
   length) else `PairingError("invalidPairingCode")`.
2. `name = deviceName.normalize("NFC").trim()`; require non-empty and at most
   120 Unicode scalar values (`[...name].length`) else
   `PairingError("invalidDeviceName")`.
3. `configuration = new CompanionServerConfiguration(baseUrl.trim())`; any
   constructor throw -> `PairingError("invalidServerUrl")`. Constructor rules
   (httpClient.ts): URL must parse; scheme `https`, or `http` only for
   loopback hosts (`localhost`, `::1`, `[::1]`, `127.x.x.x` with each octet
   <= 255); no embedded username/password; no query; no fragment.
4. Preflight 1: `await ensureCredentialStore()` — protected storage must be
   writable before consuming the code. (In `service.pair` this closure is
   `async () => { await deps.credentials(); }`. A throw here propagates out of
   claimPairing as a non-PairingError and maps to `pairingFailed` — but it is
   effectively unreachable because `service.pair` resolves the store first.)
5. Preflight 2: `GET /companion/capabilities` (unauthenticated; validated
   against `capabilitiesResponseSchema` incl. envelope meta). ANY failure
   (network, decode, non-2xx, even a decodable server error envelope) ->
   `PairingError("network")`.
6. `negotiatePresence(capabilities.data, PROTOCOL_CLIENT_VERSION /* "1.8.3" */)`:
   - `clientUpdateRequired` -> `PairingError("clientUpdateRequired")`
   - `schemaUnsupported` | `featureUnavailable` -> `PairingError("serverFeatureUnavailable")`
   - `invalidCapabilities` -> `PairingError("invalidCapabilities")`
7. `POST /companion/pairings/claim` with body
   `{ deviceName: name, pairingCode: code }` (unauthenticated). Response
   schema `pairingClaimResponseSchema` = `{ data: { deviceId: UUID/ULID,
   deviceToken: string (non-blank after trim), scopes: string[],
   nextSequence: int 0..2^53-1 } }` — note: NO `meta` envelope required and no
   requestId echo check for this call.
   On failure: `extractPairingServerCode(error)`:
   - `CompanionPairingServerError` (simplified `{error:{code}}` envelope) -> its `code`;
   - `CompanionServerError` (full envelope) -> `envelope.error.code`;
   - anything else -> null.
   Non-null code -> `PairingError("pairingRejected", serverCode)`;
   null -> `PairingError("network")`.
8. Scope check: `claim.data.scopes` must include the REQUIRED scope
   `"companion:presence:write"` else `PairingError("requiredScopeMissing")`.
   No other scope is required; all returned scopes are preserved verbatim into
   the stored connection (optional/unknown scopes tolerated).
9. Return `{ ...claim.data, baseUrl: configuration.baseUrl.toString(), deviceName: name }`.
   IMPORTANT: `baseUrl` is the URL object's serialization, i.e. normalized
   (an origin-only input like `http://127.0.0.1:4938` becomes
   `http://127.0.0.1:4938/` with a trailing slash). This exact string is
   persisted and later feeds the authority key.

### 5.2 `service.pair(baseUrl, deviceName, pairingCode)`

1. `credentialStore = await deps.credentials()`; on throw ->
   `ServiceError("credentialStoreUnavailable")`.
2. `result = await claimPairing(...)`; on throw -> `mapPairingError(error)`
   (table in 5.4).
3. Replacing an existing pairing (unconditional, also when not previously
   paired): `await coordinator.shutdown("connectionRemoved")` — issues the old
   pairing's bounded final clear if an old client existed and the coordinator
   was not suspended. Then `coordinator.discardAuthority()`.
4. `await credentialStore.set(result.deviceId, result.deviceToken)` — the
   token goes into protected storage BEFORE any non-secret metadata is
   committed. (A throw here propagates raw; the IPC handler maps it to
   `"internal"`.)
5. `config.update`: set `credentialBackend = credentialStore.backend` and
   `connection = { baseUrl: result.baseUrl, deviceId, deviceName: result.deviceName,
   scopes: result.scopes, pairingNextSequence: result.nextSequence,
   liveDeskEnabled: false }`. Pairing NEVER enables publishing.
6. `logger.info("service", "paired (live desk disabled)")`.
7. `await refreshPreview()` — captures a fresh preview, records it as the new
   consent basis, fires `onChanged`.

The config file must never contain the device token (asserted in tests via
`JSON.stringify(config.get())` not containing the token).

`pairingNextSequence` handling: stored once at pair time and never updated.
It is the floor passed to every `CompanionSequencer` constructed for this
device: `currentNext = max(pairingNextSequence, validStoredValue ?? pairingNextSequence)`
where the stored value comes from `sequence.json` keyed by deviceId (valid =
integer in `0..=2^53-1`). The moving counter lives in `sequence.json`;
`config.json` keeps the original claim value.

There is no `alreadyPaired` path: pairing while already paired REPLACES the
existing pairing (step 3). The `alreadyPaired` IpcErrorCode exists in the
shared enum (and has a UI label) but is never produced by the core.

### 5.3 `service.unpair()`

1. `connection = config.get().connection`; if null -> return (success, no
   error; idempotent).
2. Durable disable first: `config.update` sets `liveDeskEnabled = false`
   (connection kept).
3. `await coordinator.shutdown("connectionRemoved")` — bounded (800 ms) remote
   clear with reason `connectionRemoved`, skipped if suspended or no client.
4. `coordinator.discardAuthority()`.
5. Best-effort credential deletion: `try { (await deps.credentials()).delete(connection.deviceId) }`
   catch -> `logger.warn("service", "credential deletion failed during unpair")`
   — the unpair CONTINUES (error tolerance is local-only here).
6. `await sequenceStore.remove(connection.deviceId)` — NOT wrapped in try; a
   throw propagates (handler -> `"internal"`) leaving the connection present
   but disabled.
7. `config.update` sets `connection = null`.
8. `gate.clear(); preview = null;` log
   "unpaired; credentials and sequence removed"; `onChanged()`.

There is no remote "unpair"/revoke API call — unpair is local state removal
plus the best-effort presence clear. Remote failures of the clear are
swallowed by `clearBestEffort`.

### 5.4 Failure -> IpcErrorCode mapping

`ServiceError` codes (service.ts) are a subset of `IpcErrorCode` (shared
ipc.ts). The IPC handler returns `error.code` for `ServiceError` and
`"internal"` for anything else thrown.

`mapPairingError(error)`:

| Input | ServiceError code |
| --- | --- |
| `PairingError("clientUpdateRequired")` | `clientUpdateRequired` |
| `PairingError("serverFeatureUnavailable")` | `serverFeatureUnavailable` |
| `PairingError("invalidCapabilities")` | `serverFeatureUnavailable` |
| `PairingError("requiredScopeMissing")` | `requiredScopeMissing` |
| `PairingError("invalidPairingCode")` | `invalidInput` |
| `PairingError("invalidDeviceName")` | `invalidInput` |
| `PairingError("invalidServerUrl")` | `invalidInput` |
| `PairingError("pairingRejected", serverCode = "COMPANION_PAIRING_EXPIRED")` | `pairingExpired` |
| `PairingError("pairingRejected", serverCode = "RATE_LIMITED")` | `rateLimited` |
| `PairingError("pairingRejected", serverCode = "VALIDATION_FAILED")` | `validationFailed` |
| `PairingError("pairingRejected", any other serverCode)` | `pairingFailed` |
| `PairingError("network")` | `network` |
| any non-PairingError | `pairingFailed` |

Other producers of `IpcErrorCode`:

| Code | Produced by |
| --- | --- |
| `notPaired` | `confirmConsent` when `connection === null` (only producer). |
| `previewOutOfDate` | `confirmConsent` (stale fingerprint / no preview / drift before or after persist). |
| `credentialStoreUnavailable` | `service.pair` step 1 (only producer). |
| `invalidInput` | pairing input validation (above) AND the IPC frame layer when an authenticated client sends a schema-invalid command that still has a string `id`. |
| `internal` | handler catch-all for non-ServiceError throws; WS server catch around `onCommand`. |
| `alreadyPaired` | NEVER produced (enum + UI label only). |

---

## 6. Consent gate

### 6.1 Policy fingerprint coverage

`capture.fingerprint()` = `policyFingerprint(privacyConfig)`
(privacy/fingerprint.ts): SHA-256 hex (64 lowercase hex chars) of the JSON of
a canonicalized projection:

```
{
  sources,                    // { application: bool, media: bool }
  shareWindowTitles,
  ignoreNullArtist,
  defaults,                   // { application, windowTitle, media }
  rules,                      // normalizedRule() each, empty rules dropped, sorted by appId asc
  mappings,                   // sorted by "type\0from" asc
}
```

canonicalize = recursively sort object keys ascending and drop
undefined-valued keys; arrays keep order (after the explicit sorts above).
Rule normalization: appId NFC + trim + lowercase; displayAlias NFC + trim,
omitted when empty; an all-inherit rule with no alias is "empty" and excluded.
Because `sources` is included, a source toggle changes the fingerprint.

### 6.2 ConsentGate semantics (consentGate.ts)

State: `currentFingerprint: string` (constructor arg),
`confirmation: { policyFingerprint, projection } | null` (starts null).

- `fingerprint` getter -> currentFingerprint.
- `policyDidChange(fp)`: if `fp === currentFingerprint` -> NO-OP (confirmation
  kept). Else set currentFingerprint = fp and `confirmation = null`.
- `record(projection)`: `confirmation = { policyFingerprint: currentFingerprint, projection }`;
  returns the confirmation object.
- `clear()`: confirmation = null.
- `validates(candidate, currentProjection)` — true iff ALL of:
  1. `confirmation !== null`,
  2. `deepEqual(candidate, confirmation)` (candidate IS the latest recorded
     confirmation by value — a stale candidate recorded earlier fails),
  3. `candidate.policyFingerprint === currentFingerprint`,
  4. `deepEqual(candidate.projection, currentProjection)` (fresh capture still
     matches what was confirmed).

deepEqual: `Object.is` fast path; both non-null objects; identical key COUNT;
recursive per key. Key-order-insensitive. In Rust, `PartialEq` on the typed
`PreviewProjection` structs is equivalent.

### 6.3 `projectionOf(snapshot)` — what consent binds

From `SanitizedPresenceSnapshot`, keeps:

- `application`: null, or `{ displayName, windowTitle }`.
- `media`: null, or `{ kind, title, artist, album, playerDisplayName,
  playback: { state, durationSeconds, rate } }`.

Deliberately EXCLUDED: `observedAt`, media `sessionId`, `playback.positionSeconds`,
`playback.sampledAt`. Natural playback progress is continuity, not new
disclosure; track changes, pause/play flips, duration and rate changes all
invalidate.

### 6.4 `confirmConsent(uiFingerprint)` — exact algorithm

`uiFingerprint` is the fingerprint the user was looking at when they clicked;
a stale click can never enable publishing.

1. If `config.connection === null` -> throw `ServiceError("notPaired")`.
2. If `this.preview === null` OR `uiFingerprint !== gate.fingerprint`:
   `await refreshPreview()` (re-records a fresh basis for the UI), then throw
   `ServiceError("previewOutOfDate")`.
3. `candidate = { policyFingerprint: uiFingerprint, projection: preview.projection }`.
4. Validation 1 (pre-persist): fresh capture
   `before = capture.captureForDelivery({ includeMedia: config.privacy.sources.media })`;
   if `!gate.validates(candidate, projectionOf(before))` ->
   `await refreshPreview()`; throw `previewOutOfDate`.
5. Persist the enable: `config.update` sets `connection.liveDeskEnabled = true`
   (no-op mapper when connection is null).
6. Validation 2 (post-persist): fresh capture `after` (same options, config
   re-read); `fingerprintNow = capture.fingerprint()`; if
   `fingerprintNow !== uiFingerprint || !gate.validates(candidate, projectionOf(after))`:
   ROLL BACK — `config.update` sets `liveDeskEnabled = false`;
   `await refreshPreview()`; throw `previewOutOfDate`. (Anything that drifted
   during the awaits above rolls the persisted state back — a stale write can
   never start publishing.)
7. `logger.info("service", "live desk enabled by explicit consent")`;
   `coordinator.start()`; `onChanged()`.

Note the asymmetry: validation 1 does not compare `capture.fingerprint()`
explicitly (gate.validates check 3 covers the gate's view); validation 2
additionally compares the live fingerprint against `uiFingerprint`.

### 6.5 `disableLiveDesk()` / liveDeskEnabled persistence

`disableLiveDesk`: persist-first — `config.update` sets
`liveDeskEnabled = false`; then `await coordinator.shutdown("paused")`; then
`onChanged()`. A crash mid-way must never resume publishing (which is why the
flag is written before the network clear).

`liveDeskEnabled` is flipped TRUE only in confirmConsent step 5. It is
flipped FALSE by: pairing (initial value), confirmConsent rollback (step 6),
`disableLiveDesk`, and `unpair` step 2. `service.start()` (process startup)
resumes the coordinator only when `connection != null && liveDeskEnabled`.

### 6.6 `policyMaybeChanged()`

Called after ANY privacy-relevant configuration change (every privacy mutation
command in the IPC handler calls it after `config.update`).

```
fingerprint = capture.fingerprint()
if fingerprint === gate.fingerprint: return          // value compare, no-op
gate.policyDidChange(fingerprint)                    // clears confirmation
this.preview = null                                  // preview invalidated
coordinator.requestFreshSnapshot()                   // republish under new policy
                                                     // (no-op unless active/degraded)
onChanged()
```

When Live Desk is enabled this REPUBLISHES under the new policy; it does NOT
revoke consent, disable, or clear. When not enabled it merely drops the
preview so the UI must request a new one.

---

## 7. Service facade reference

All methods of `CompanionService`; error mapping per section 5.4.

| Method | Preconditions | Effects / persisted changes | Events (`onChanged`) | Errors |
| --- | --- | --- | --- | --- |
| `constructor(deps)` | — | Builds gate (fingerprint at construction) + coordinator. | — | — |
| `start()` | — | If `connection != null && liveDeskEnabled`: `coordinator.start()`. Else nothing. Used to resume publishing after a restart when consent was already given. | via coordinator state changes | — |
| `runtimeState()` | — | Pure: `notPaired` when connection null, else coordinator state. | — | — |
| `currentPreview()` | — | Returns `preview` (nullable). | — | — |
| `publishTelemetry()` | — | Returns `{ lastPublishAt, lastError }`. | — | — |
| `policyMaybeChanged()` | — | Section 6.6. No persistence of its own. | yes, iff fingerprint changed | — |
| `refreshPreview()` | — | Fresh capture with `includeMedia = privacy.sources.media`; `gate.record(projection)`; `preview = { projection, policyFingerprint: gate.fingerprint, observedAt: snapshot.observedAt }`; returns the preview. | yes | capture failures propagate (-> `internal`) |
| `pair(baseUrl, deviceName, pairingCode)` | — | Section 5.2 (credential set + config connection with `liveDeskEnabled:false` + refreshPreview). | via refreshPreview + coordinator | 5.4 table; `credentialStoreUnavailable` |
| `confirmConsent(uiFingerprint)` | paired | Section 6.4; persists `liveDeskEnabled = true` (with rollback). Starts coordinator. | yes on success (also via refreshPreview on failure paths) | `notPaired`, `previewOutOfDate` |
| `disableLiveDesk()` | — | Persists `liveDeskEnabled = false` (no-op when unpaired), then bounded clear reason `paused`. | yes | — |
| `unpair()` | — | Section 5.3; ends with `connection = null`, gate cleared, preview null. | yes (and via coordinator) | non-ServiceError throws -> `internal` |
| `handleSleepOrLock()` | — | Delegates to coordinator (T16). | via coordinator | — |
| `handleWakeOrUnlock()` | — | Delegates to coordinator (T17). | via coordinator | — |
| `shutdown()` | — | `coordinator.shutdown("shutdown")` (bounded 800 ms clear). | via coordinator | — |
| `noteError(code)` | — | `lastError = code`. NOTE: never called anywhere in the current codebase. | yes | — |

`publishTelemetry` field lifecycle:

- `lastPublishAt`: starts null; set to `Date.now()` inside the coordinator's
  `onPublished` callback on EVERY successful `replacePresence`. NEVER cleared
  or reset — it survives disable, unpair, policy changes and renegotiations.
- `lastError`: starts null; SET only by `noteError(code)` (dead code in the
  current wiring, so in production it is always null); CLEARED (set null) on
  every successful publish. Successful clears do NOT touch either field
  (`onPublished` fires only from `replacePresence` success in the refresh
  loop).

`currentPreview` lifecycle:

- Set by `refreshPreview()` (also called at the end of `pair()` and on every
  `confirmConsent` failure path).
- Nulled by `policyMaybeChanged()` (on real fingerprint change) and
  `unpair()`.
- NOT nulled by `disableLiveDesk`, sleep/wake, or coordinator state changes.
- The UI receives it inside every `CoreStateSnapshot` (`preview` field); the
  dedicated `{type:"preview"}` ServerMessage variant exists in the schema but
  is never sent.

`onChanged` (-> snapshot broadcast) fires from: every coordinator state
change, every successful publish, `policyMaybeChanged` (on change),
`refreshPreview`, `confirmConsent` success, `disableLiveDesk`, `unpair`,
`noteError`.

---

## 8. IPC layer (WS server + command handler)

Per ARCHITECTURE.md the WebSocket transport DIES in the rewrite: listener on
127.0.0.1:random-port, hello/token handshake, 3 s hello timeout, origin
whitelist, close codes 4001/4002/4003, `{type:"ready", port}` stdout
announcement, and the dev-token stdout line are all replaced by Tauri
`invoke` commands + a `core-state` event. The COMMAND SEMANTICS, error codes,
snapshot assembly and broadcast triggers below survive 1:1 (commands become
`Result<(), String>` where `Err` is the `IpcErrorCode` literal).

### 8.1 Legacy handshake (documented for completeness; superseded)

- Server binds `127.0.0.1`, port 0 (ephemeral). Token: `YOHAKU_IPC_TOKEN` env
  or 24 random bytes hex; when the env var is unset a
  `{"type":"dev-token","token":...}` JSON line is printed to stdout.
- On connection: if an `Origin` header is present and not in
  `{tauri://localhost, http://tauri.localhost, https://tauri.localhost,
  http://localhost:5173, http://localhost:1420}` -> close `4003 "origin rejected"`.
  Native clients send no Origin and pass.
- First frame must arrive and authenticate within 3000 ms or the socket is
  closed `4001 "hello timeout"` (timer unref'd).
- Frame handling: JSON parse failure -> close `4002 "invalid frame"`.
  zod `clientMessageSchema` (hello | command) failure: if the socket is
  already authenticated AND the raw object has a string `id` -> reply
  `{type:"result", id, ok:false, error:"invalidInput"}`; otherwise close
  `4002 "invalid frame"`.
- Hello (`{type:"hello", token}`): token match -> mark authenticated, clear
  timeout, immediately send a full `{type:"state", snapshot}`; mismatch ->
  close `4001 "bad token"`.
- Any command before authentication -> close `4001 "not authenticated"`.
- Command dispatch: `error = await onCommand(command)` with a catch mapping
  any throw to `"internal"`. Reply `{type:"result", id, ok:true}` or
  `{type:"result", id, ok:false, error}`. After EVERY command (success or
  failure) call `broadcastState()`.
- `broadcastState()`: trailing coalescing — at most one snapshot push per
  100 ms (a pending timer absorbs further requests; timer unref'd). Push sends
  the current snapshot to all authenticated OPEN sockets.
- `stop()`: clear pending broadcast timer, terminate all clients, close.

### 8.2 Command routing (handlers.ts) — survives into Tauri commands

Handler contract: `(command) -> IpcErrorCode | null` (null = ok). `ServiceError`
-> its `code`; any other throw -> `"internal"`.

| Command | Action |
| --- | --- |
| `getState` | No-op success (client relies on the post-command broadcast; in Tauri, `get_state` returns the snapshot directly). |
| `pair { baseUrl, deviceName, pairingCode }` | `service.pair(...)`. |
| `unpair` | `service.unpair()`. |
| `requestPreview` | `service.refreshPreview()` (result travels in the snapshot). |
| `confirmConsent { policyFingerprint }` | `service.confirmConsent(policyFingerprint)`. |
| `disableLiveDesk` | `service.disableLiveDesk()`. |
| `setSources { application?, media? }` | `config.update`: merge into `privacy.sources`, missing fields keep the current value; then `service.policyMaybeChanged()`. |
| `setPrivacy { patch }` | `config.update`: merge `patch.defaults.{application,windowTitle,media}`, `patch.shareWindowTitles`, `patch.ignoreNullArtist`, each falling back to the current value when absent; then `policyMaybeChanged()`. |
| `upsertRule { rule }` | `rule = normalizedRule(rule)` (appId NFC+trim+lowercase; alias normalized/omitted); `config.update`: drop any existing rule with the same appId, push unless `isEmptyRule(rule)` (upsert of an empty rule = delete), sort rules by appId ascending (`a.appId < b.appId ? -1 : 1`); then `policyMaybeChanged()`. |
| `deleteRule { appId }` | `config.update`: filter out rules whose `appId !== command.appId.toLowerCase()` — NOTE: lowercase only, no NFC/trim (differs from upsert normalization). Then `policyMaybeChanged()`. |
| `setMappings { mappings }` | `config.update`: replace `privacy.mappings` wholesale; then `policyMaybeChanged()`. |
| `shutdown` | `deps.requestShutdown()` (in main.ts: `void gracefulExit(0)` — fire-and-forget), returns success BEFORE the process exits. |

Every privacy mutation goes through `ConfigStore.update` (zod-validated,
atomic tmp+fsync+rename write) FIRST, then `policyMaybeChanged()`, so the
consent gate and Live Desk always observe the new fingerprint.

### 8.3 Snapshot assembly (main.ts `getSnapshot`)

```
CoreStateSnapshot {
  version:      APP_VERSION ("0.3.0" in main.ts)
  runtimeState: service.runtimeState()
  connection:   null | { baseUrl, deviceId, deviceName, scopes, liveDeskEnabled }
                // ConnectionSummary — pairingNextSequence deliberately omitted
  privacy:      config.get().privacy
  preview:      service.currentPreview()
  mediaProvider:{ kind: mediaProvider?.kind ?? "none", healthy: mediaProvider?.healthy() ?? false }
  recentAppIds: [...recentAppIds]                    // defensive copy
  lastPublishAt / lastError: service.publishTelemetry()
}
```

No secrets, no un-sanitized capture text, no exe paths, no appIds other than
the `recentAppIds` MRU (appIds are shown to the local UI only, never on the
wire).

---

## 9. Wiring and lifecycle (main.ts)

### 9.1 Startup order

1. Resolve IPC token (env or random); maybe print dev-token line.
2. `config = new ConfigStore()`, `sequenceStore = new FileSequenceStore()`.
3. `credentials()`: memoized promise around
   `selectCredentialStore(config.credentialBackend)`; when the selected
   store's backend differs from the configured one, persist
   `credentialBackend = store.backend`.
4. `foreground = new ForegroundWatcher(); foreground.start()`.
5. `mediaProvider = await selectMediaProvider(config.media.provider, <ps script>)`;
   failure -> null + `logger.error("main", "no media provider available; media capture disabled")`.
6. `capture = new CaptureService(foreground, () => mediaProvider, () => config.get().privacy)`.
7. `recentAppIds: string[] = []`; `ipc = new IpcServer({ token, getSnapshot, onCommand: handler })`.
8. `service = new CompanionService({ config, capture, sequenceStore, credentials, onChanged: () => ipc.broadcastState() })`.
9. `handler = createCommandHandler({ config, service, requestShutdown: () => void gracefulExit(0) })`.
10. Register capture triggers (9.2), system events, suspend detector.
11. `port = await ipc.start()`; print `{"type":"ready","port"}`.
12. `service.start()` — LAST: publishing resumes only after IPC is up.

### 9.2 Triggers

| Event | Handler |
| --- | --- |
| `foreground.onChange(info)` | If `info !== null`: recentAppIds MRU update (below). ALWAYS (even for null info): `service.coordinator.requestFreshSnapshot()` AND `ipc.broadcastState()`. |
| `mediaProvider.onSemanticChange` | `service.coordinator.requestFreshSnapshot()` only — NO broadcast (semantic media changes reach the UI only if a publish/state change follows). |
| `systemEvents.onLockOrSleep` | `service.handleSleepOrLock()`. |
| `systemEvents.onUnlockOrResume` | `service.handleWakeOrUnlock()`. |
| Suspend detector gap | see 9.4. |
| Every IPC command | result reply + `broadcastState()` (server layer). |
| `service.onChanged` | `ipc.broadcastState()` (100 ms coalesced). |

### 9.3 recentAppIds MRU semantics

On each non-null foreground info:

```
existing = recentAppIds.indexOf(info.appId)
if existing !== -1: remove that single occurrence   // dedupe
recentAppIds.unshift(info.appId)                    // most-recent-first
recentAppIds.length = min(recentAppIds.length, 10)  // cap 10, drop oldest
```

Case-sensitive exact string match on `appId` (already normalized lowercase by
the foreground watcher). Not persisted; process-lifetime only. Per
ARCHITECTURE.md this moves to the Rust app crate state, fed by
`ForegroundWatcher::on_change`.

### 9.4 SuspendDetector

`new SuspendDetector(callback)` with default `gapThresholdMs = 15_000`,
sampling `setInterval` of 5000 ms (unref'd). When a wall-clock gap between
ticks exceeds the threshold (missed suspend — the lease already expired
remotely), the main.ts callback runs:

```
state = service.runtimeState()
if state === "active" || state === "degraded": service.coordinator.start()
```

i.e. a forced renegotiation ONLY from active/degraded. All other states
(including `suspended` — the real system-events path owns that) are left
alone.

### 9.5 Bounded shutdown (`gracefulExit(code)`)

Triggered by: IPC `shutdown` command, or signals SIGINT / SIGTERM / SIGBREAK /
SIGHUP. Re-entrancy guarded by an `exiting` flag (subsequent calls return
immediately).

```
1. logger.info("main", "shutting down")
2. suspendDetector.stop()          // sync, unconditional, outside the bound
3. foreground.stop()               // sync, unconditional, outside the bound
4. await Promise.race([
     (async () => {
        await service.shutdown();      // coordinator.shutdown("shutdown"):
                                       //   final remote clear, reason "shutdown",
                                       //   bounded at 800 ms internally
        await systemEvents.stop();
        await mediaProvider?.stop();
        await ipc.stop();
     })(),
     sleep(2000)                       // overall 2000 ms bound
   ])
5. process.exit(code)              // hard exit; losing work is abandoned
```

Ordering inside the bound is strictly sequential: service (remote clear) ->
system events -> media provider -> IPC. If the chain exceeds 2000 ms total,
the process exits anyway; the server lease expiry is the backstop for a lost
final clear. Rust: `RunEvent::Exit` -> `service.shutdown()` bounded to 2 s,
same ordering.

---

## 10. TEST VECTORS

These become `core/tests/integration.rs` and `core/tests/consent_gate.rs`.
Keep every literal value.

### 10.1 integration.test.ts — "CompanionService end-to-end (mock Core)"

Common setup (beforeEach):

- `MockCompanionServer` on 127.0.0.1:ephemeral. Fallback handler:
  - `/companion/capabilities` -> 200 `capabilitiesResponse()`:
    `meta = { schema: "yohaku.companion.presence", schemaVersion: 2, requestId: <uuid>, serverTime: "2026-07-26T04:00:12.345Z" }`,
    `data = { minimumClientVersion: "1.7.0", presenceSchemaVersions: [2],
    momentSchemaVersions: [1], features: { liveDesk: true, mediaTimeline: true,
    moments: true, readingSessions: false }, limits: { presencePayloadBytes: 32768,
    presenceRequestsPerMinute: 120, presenceLeaseMinSeconds: 30,
    presenceLeaseMaxSeconds: 120, recommendedHeartbeatSeconds: 45,
    maximumClockSkewSeconds: 60 } }`.
  - `/companion/pairings/claim` -> 200
    `{ meta: responseMeta(<uuid>), data: { deviceId: "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b",
    deviceToken: "test-device-token", scopes: ["companion:presence:write"], nextSequence: 10 } }`.
  - anything else -> `mutationSuccess(req)`: 200, echoes the request's
    `meta.requestId`, `acceptedSequence = request meta.sequence`,
    `receivedAt = "2026-07-26T04:00:12.345Z"`,
    `state = { schemaVersion: 2, epoch: "01ARZ3NDEKTSV4RRFFQ69G5FAV", revision: 1, projection: null }`.
  - `server.enqueue(handler)` pushes one-shot handlers consumed before the
    fallback, in order.
- Fresh temp data dir; `ConfigStore(dir)` (defaults: not paired, default
  privacy); `FileSequenceStore(dir)`.
- `FakeCredentialStore` (backend "keyring", in-memory map).
- `FakeMediaProvider` (kind "npm", healthy, programmable `snapshot`, no
  semantic events).
- Foreground fixture (mutable):
  `{ appId: "code.exe", exePath: "C:/apps/code.exe", displayName: "Visual Studio Code", windowTitle: "secret.ts — project" }`.
- `CompanionService` with `coordinatorTimings: { networkRetryMs: 100, featureRetryMs: 200 }`
  and `onChanged: () => undefined`.
- Helpers: `pairAndConsent()` = pair(baseUrl, "Test PC", "PAIR-CODE") ->
  `refreshPreview()` -> `confirmConsent(preview.policyFingerprint)` -> wait
  until runtimeState() === "active". `until(cond)` polls every 15 ms, default
  timeout 3000 ms. `requestsTo(path)` filters recorded requests by exact path.
- afterEach: `service.shutdown()`, server stop, dir removal.

Test cases:

1. "pairs with live desk disabled and stores the token securely"
   - Action: `pair(baseUrl, "  Test PC  ", "PAIR-CODE")` (note surrounding
     whitespace in the name).
   - Assert: `config.connection.liveDeskEnabled === false`;
     `connection.deviceId === "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b"`;
     `connection.pairingNextSequence === 10`;
     credential store has `DEVICE_ID -> "test-device-token"`;
     `JSON.stringify(config.get())` does NOT contain `"test-device-token"`;
     zero requests to `/companion/presence`; `runtimeState() === "disabled"`.

2. "capabilities are negotiated BEFORE the one-time code is consumed"
   - Action: `pair(baseUrl, "Test PC", "PAIR-CODE")`.
   - Assert: in the recorded request order,
     `indexOf("/companion/capabilities") < indexOf("/companion/pairings/claim")`.

3. "rejects pairing when the presence scope is missing"
   - Setup: enqueue capabilities OK, then a claim response whose
     `data.scopes = ["companion:moment:write"]`.
   - Action: `pair(baseUrl, "PC", "CODE")`.
   - Assert: rejects with `code: "requiredScopeMissing"` (ServiceError).

4. "maps COMPANION_PAIRING_EXPIRED to pairingExpired"
   - Setup: enqueue capabilities OK, then claim ->
     status 410, body `{ error: { code: "COMPANION_PAIRING_EXPIRED" } }`
     (simplified pairing envelope, no meta).
   - Assert: pair rejects with `code: "pairingExpired"`.

5. "consent with a stale fingerprint fails and never enables"
   - Setup: pair; `refreshPreview()`.
   - Action: `confirmConsent("stale-fingerprint")`.
   - Assert: rejects `code: "previewOutOfDate"`;
     `config.connection.liveDeskEnabled === false`.

6. "consent fails when the capture drifts between preview and click"
   - Setup: pair; `preview = refreshPreview()`; then mutate foreground to
     `{ ...same, appId: "other.exe", displayName: "Other" }`.
   - Action: `confirmConsent(preview.policyFingerprint)`.
   - Assert: rejects `code: "previewOutOfDate"`; liveDeskEnabled stays false.

7. "full flow: consent -> active -> sanitized presence published"
   - Setup: media snapshot =
     `{ appId: "spotify.exe", sourceAppUserModelId: "spotify.exe",
     playerDisplayName: "Spotify", kind: "music", title: "Song",
     artist: "Artist", album: "Album", playing: true, durationSeconds: 200,
     positionSeconds: 60, sampledAt: Date.now() }`.
   - Action: `pairAndConsent()`; wait for >= 1 PUT `/companion/presence`.
   - Assert on the first presence PUT:
     `headers.authorization === "Bearer test-device-token"`;
     `body.meta.deviceId === DEVICE_ID`; `body.meta.sequence >= 10`;
     `body.data.availability === "active"`;
     `body.data.application.displayName === "Visual Studio Code"`;
     `body.data.application.window === null` (default rule hides titles +
     global shareWindowTitles off);
     `body.data.media.title === "Song"`;
     rawBody contains NONE of `"code.exe"`, `"spotify.exe"`, `"C:/apps"`.

8. "privacy change while enabled republishes under the new policy"
   - Setup: `pairAndConsent()`; wait >= 1 publish; record count.
   - Action: `config.update` adds rule
     `{ appId: "code.exe", application: "share", windowTitle: "inherit",
     media: "inherit", displayAlias: "编辑器" }` (direct config write, not via
     IPC), then `service.policyMaybeChanged()`.
   - Assert: presence count grows; the LAST presence body has
     `data.application.displayName === "编辑器"`;
     `service.currentPreview() === null` (recorded preview gone);
     `config.connection.liveDeskEnabled === true` (consent stays valid).

9. "lock clears with reason sleep; wake renegotiates and republishes"
   - Setup: `pairAndConsent()`; wait >= 1 publish; record capabilities count.
   - Action: `service.handleSleepOrLock()`.
   - Assert: >= 1 POST `/companion/presence/clear`; the first clear body has
     `data.reason === "sleep"`; `runtimeState() === "suspended"`.
   - Action: record presence count; `service.handleWakeOrUnlock()`.
   - Assert: runtimeState reaches "active"; presence count exceeds the
     pre-wake count; capabilities request count INCREASED (renegotiation
     happened).

10. "sequences stay monotonic across clear and wake (single sequencer)"
    - Action: `pairAndConsent()`; >= 1 publish; sleep; wait for clear; wake;
      wait active; wait until (presence + clear request count) >= 3.
    - Assert: taking `meta.sequence` of every request whose path starts with
      `/companion/presence` in arrival order: the list equals its sorted copy
      (non-decreasing) AND all values are unique (strictly increasing, no
      reuse).

11. "schema rejection triggers renegotiation, then publishing resumes"
    - Setup: `pairAndConsent()`; >= 1 publish; record capabilities count;
      enqueue one-shot: next mutation -> 409 error envelope, code
      `COMPANION_SCHEMA_UNSUPPORTED` (full envelope, retryable false,
      requestId echoed).
    - Action: `service.coordinator.requestFreshSnapshot()`.
    - Assert: capabilities count increases (authority discarded, renegotiated)
      and runtimeState returns to "active" (timeout 5000 ms).

12. "disable persists first and clears with reason paused"
    - Action: `pairAndConsent()`; `service.disableLiveDesk()`.
    - Assert: `config.connection.liveDeskEnabled === false`;
      clear count >= 1; LAST clear body `data.reason === "paused"`;
      `runtimeState() === "disabled"`.

13. "unpair clears remotely, removes credentials and connection"
    - Action: `pairAndConsent()`; `service.unpair()`.
    - Assert: LAST clear body `data.reason === "connectionRemoved"`;
      credential store empty (`tokens.size === 0`);
      `config.connection === null`; `runtimeState() === "notPaired"`.

14. "degrades on network failure and recovers via retry"
    - Setup: `pairAndConsent()`; >= 1 publish; enqueue TWO one-shot
      `socketDestroy` handlers (first kills the publish attempt, second kills
      the single idempotent retry).
    - Action: `service.coordinator.requestFreshSnapshot()`.
    - Assert: runtimeState reaches "degraded"; then (networkRetryMs = 100)
      renegotiation restores "active" within 5000 ms.

### 10.2 consentGate.test.ts

Fixture `snapshot(patch)` (SanitizedPresenceSnapshot):

```
observedAt: 1_753_500_000_000
application: { displayName: "Code", windowTitle: null }
media: {
  sessionId: "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b", kind: "music",
  title: "Song", artist: "Artist", album: "Album", playerDisplayName: "Spotify",
  playback: { state: "playing", durationSeconds: 200, positionSeconds: 60,
              sampledAt: 1_753_500_000_000, rate: 1 }
}
```

`projectionOf` cases:

1. "excludes observedAt, sessionId, position and sampledAt": projection of the
   base snapshot EQUALS projection of a variant with
   `observedAt = 9_999_999_999_999`,
   `media.sessionId = "00000000-0000-4000-8000-000000000000"`,
   `playback.positionSeconds = 190`, `playback.sampledAt = 1`.
2. "differs when duration, state, rate, track or app changes":
   - changedTrack: `media.title = "Other"` -> projection != base.
   - changedState: `playback.state = "paused"`, `playback.rate = 0` -> != base.
   - changedApp: `application = { displayName: "Другое", windowTitle: null }`
     -> != base.

`ConsentGate` cases (all constructed as `new ConsentGate("fp1")`):

3. "starts without confirmation and rejects unrecorded candidates":
   without `record`, `validates({ policyFingerprint: "fp1", projection }, projection) === false`.
4. "validates recorded confirmation against an equal fresh capture":
   `confirmation = gate.record(projection)`; fresh projection built from a
   snapshot with `positionSeconds = 120` (progress only);
   `validates(confirmation, fresh) === true`.
5. "policy change invalidates a recorded confirmation":
   record, then `policyDidChange("fp2")`;
   `validates(confirmation, projection) === false`.
6. "same-fingerprint policyDidChange keeps confirmation":
   record, then `policyDidChange("fp1")`;
   `validates(confirmation, projection) === true`.
7. "rejects when the current capture drifted semantically":
   record base; drifted projection from `media.title = "New Track"`;
   `validates(confirmation, drifted) === false`.
8. "rejects a stale candidate after re-recording":
   `old = record(projection(base))`; then record a second projection built
   from `media.title = "Second"`;
   `validates(old, projection(base)) === false`.

### 10.3 presenceClient.test.ts — service-relevant clarifications only

(Owned by agent T for the full port; the service/coordinator spec depends on
these behaviors.)

- Publish PUTs `/companion/presence` with `Authorization: Bearer <token>` and
  `x-yohaku-companion-version: 1.8.3`; first sequence equals the sequencer
  floor (pairingNextSequence 5 -> `meta.sequence === 5`).
- Retry-safe failures (retryable 5xx envelope, destroyed socket, requestId
  echo mismatch) are retried EXACTLY once with byte-identical rawBody, same
  sequence, same requestId. Two consecutive failures reject (exactly 2
  requests). Non-retryable 5xx and 4xx are not retried.
- `acceptedSequence` reconciliation: success `acceptedSequence: 100` makes the
  NEXT mutation use sequence 101; an error envelope with
  `acceptedSequence: 200` (e.g. 409 COMPANION_SEQUENCE_BEHIND) makes the next
  use 201 — even though the failed call itself rejected.
- `clearPresence("sleep", ...)` consumes a sequence like any mutation
  (5 then 6) and POSTs `{ data: { reason: "sleep" } }` to
  `/companion/presence/clear`.
- Concurrent mutations submitted in call order serialize FIFO through the
  send slot: sequences `[5, 6, 7]`.
- Renegotiation classification: 409 envelope code
  `COMPANION_SCHEMA_UNSUPPORTED` -> true; bare HTTP 426 with a non-envelope
  body -> true; 422 `VALIDATION_FAILED` -> false.

### 10.4 server.test.ts semantics worth preserving (transport superseded)

The WS transport tests die with the WS IPC, but these routed-behavior
assertions remain valid against the Tauri command layer:

- `confirmConsent` without pairing -> `{ ok: false, error: "notPaired" }`.
- Malformed command (e.g. `pair` with missing fields) -> `invalidInput`.
- `requestPreview` yields a snapshot whose preview has
  `projection.application.displayName === "Code"`, `windowTitle === null`
  (default privacy), and a `policyFingerprint` matching `/^[0-9a-f]{64}$/`.
- `upsertRule` with `appId: "Secret.EXE"` persists
  `{ appId: "secret.exe", application: "hide", windowTitle: "inherit", media: "inherit" }`.
- `shutdown` command triggers the exit hook and still replies ok.

---

## 11. Traps for the Rust port

1. Generation discipline: TS re-checks `gen !== this.generation` after EVERY
   await (token fetch, capabilities, throttle sleep, capture, publish). Rust
   must re-load the generation atomically after every `.await` and bail
   identically — including the subtle spot where a stale generation returns
   from inside the refresh loop's `while` without touching state.
2. Single-threaded interleaving: all coordinator fields
   (`state`, `refreshRequested`, `refreshLoopRunning`, `client`, timers) are
   mutated without locks, safe only because JS never preempts between awaits.
   In Rust either run the coordinator as a single actor task (recommended) or
   guard every field group with one mutex; never hold it across `.await`.
3. `runRefreshLoop` single-flight: the `refreshLoopRunning` check-and-set is
   atomic in JS because it happens synchronously before the first await. A
   naive Rust translation with an async lock introduces a race; use a
   dedicated task + `Notify`/channel, or an `AtomicBool` compare-exchange.
4. Fire-and-forget promises: `void this.configure(gen)`,
   `void this.runRefreshLoop(gen)`, the wake IIFE. If `captureForDelivery`
   ever threw, `runRefreshLoop` would reject unobserved (Node >= 15 would
   crash the process). The capture pipeline is written never to throw; in
   Rust make capture return `Result`/never-panic and decide explicitly (log +
   treat as failed iteration) rather than aborting.
5. `clearBestEffort` uses `Promise.race` — the losing clear REQUEST IS NOT
   CANCELLED. It keeps running in the background and may still reconcile its
   acceptedSequence into the shared sequencer after the bound. A
   `tokio::select!` that drops the future changes semantics (cancels the HTTP
   call and the reconcile). Port as: spawn the clear on its own task; race
   only the JOIN against the timeout; never abort the task.
6. Wake join is bounded: `handleWakeOrUnlock` awaits the RACE promise (max
   `sleepClearTimeoutMs`), not the underlying clear. Ordering of wake
   publishes vs a still-in-flight clear is guaranteed only by the shared
   sequencer's serialized reserve (clear reserved first -> lower sequence),
   NOT by HTTP send order — a new PresenceClient has a fresh FIFO slot.
7. Coordinator `shutdown` concurrency: a second call while `stopping` returns
   IMMEDIATELY without awaiting the first. `stopping` resets in `finally`,
   making the coordinator restartable (pair() depends on it). Also
   `wasSuspended` skips the final clear entirely.
8. Timers: heartbeat `setInterval` and retry `setTimeout` are `unref()`d and
   also gen-checked in their callbacks. `beginNewGeneration` clears both. In
   Rust, aborting the timer tasks on generation change is the equivalent, but
   KEEP the gen check inside the tick as belt-and-braces (a tick already
   dequeued can still run after clearInterval in Node).
9. Throttle bookkeeping: `lastSendStartedAt` is set BEFORE the capture (after
   the wait) and is NOT reset by `beginNewGeneration` — the min-send interval
   spans generations. `minSendIntervalMs`, `includeMedia`,
   `requestedLeaseSeconds` are plain fields overwritten by each configure;
   an in-flight loop reads the newest values (no per-iteration snapshot).
10. Heartbeat survives degraded: T14 does not clear the interval, so while
    degraded (same generation) publishes keep being attempted at heartbeat
    cadence AND the reconnect timer is re-armed on every failure. Both
    recovery paths must exist in Rust.
11. `beginNewGeneration` does NOT null `this.client`. Consequence: after a
    retry-triggered `start()` whose configure fails (e.g. capabilities down),
    the coordinator sits in `degraded` with the PREVIOUS generation's client
    still set — a `requestFreshSnapshot` then publishes through that stale
    client (old negotiated limits/token) under the new generation. This is
    observable behavior; replicate it (or consciously document a deviation in
    integration-notes.md).
12. Promise-chain mutexes (PresenceClient send slot, sequencer queue)
    guarantee FIFO in CALL order because chaining is synchronous. Rust:
    `tokio::sync::Mutex` is FIFO-fair and equivalent IF lock acquisition
    happens in submission order; an actor/mpsc design preserves this
    naturally.
13. `confirmConsent` ordering is load-bearing: validate-1 (fresh capture) ->
    persist enable -> validate-2 (fresh capture + live fingerprint compare) ->
    rollback on drift. Each validation is a REAL fresh capture (awaits
    included); do not cache or reorder, and roll back the persisted flag
    before throwing.
14. `deleteRule` matches `appId.toLowerCase()` while `upsertRule` uses full
    normalization (NFC + trim + lowercase). Keep the asymmetry byte-exact.
15. Base URL normalization: the persisted `connection.baseUrl` is
    `URL.toString()` output (adds trailing slash to origin-only URLs, strips
    default ports, lowercases host). The authority key is
    `"${baseUrl}|${deviceId}"` of that string. Verify the Rust `url` crate
    serializes identically for the inputs users can enter, or normalize
    through the same rules before persisting.
16. Snapshot broadcast coalescing: trailing 100 ms timer in the WS server. If
    the Tauri layer keeps coalescing, keep it trailing-edge (first request
    starts the window; the push reads the LATEST snapshot at fire time).
17. Shutdown is a hard bound: `Promise.race` + `process.exit` abandons the
    losing shutdown chain mid-await. Rust must likewise not block exit on the
    remote clear beyond 2 s total (and 800 ms for the clear itself).
18. `capture.resetMediaContinuity()` is called on EVERY `beginNewGeneration`
    (start, sleep, shutdown, renegotiation) — media session IDs must not
    survive a generation boundary.
19. The heartbeat interval value `heartbeatSeconds * 1000` can be fractional
    (requestedLeaseSeconds / 3); use a Duration, do not round to seconds.
20. `service.runtimeState()` reads config on every call — after `unpair` the
    coordinator still reports "disabled" internally but the facade reports
    "notPaired" because connection is null. Keep the override at the facade.

---

## 12. Ambiguities / underspecified behaviors found

1. `alreadyPaired` (`IpcErrorCode`) is never produced: `pair()` silently
   replaces an existing pairing. The UI has a label for it
   (`packages/app/src/labels.ts`). Rust should keep the enum variant but will
   never emit it unless the product decides otherwise.
2. `ClearReason::"privacyChanged"` is defined on the wire type but has no call
   site — privacy changes republish instead of clearing. Keep the variant for
   wire compatibility.
3. `service.noteError(code)` is dead code: nothing calls it, so `lastError`
   in the snapshot is always null in production (it is only ever CLEARED on
   publish). Decide whether to port the method (contract lists it via
   `publish_telemetry`; porting it as-is is the safe parity choice).
4. `lastPublishAt` is never reset — it survives disable, unpair and policy
   changes, so the UI can show a stale "last published" from a previous
   pairing. Intentional-looking but undocumented.
5. `ServerMessage` variant `{type:"preview"}` exists in the shared schema but
   is never sent; previews travel only inside state snapshots. ARCHITECTURE
   nonetheless names a `core-preview` event for the Tauri layer — the
   integrator must pick one behavior.
6. Invalid stored `baseUrl` (T4) leaves the coordinator in `degraded` with NO
   retry timer and no user-visible distinction from network degradation; only
   an external trigger (suspend-detector gap, disable/enable, restart) can
   retry.
7. Stale-client publish window (trap 11): while a renegotiation attempt is
   failing, `requestFreshSnapshot` can publish through the previous
   generation's client. Almost certainly an accident of not clearing
   `client` in `beginNewGeneration`, but it is test-invisible and observable;
   parity vs cleanup needs an explicit decision.
8. `deleteRule` lowercase-only matching (vs full NFC+trim+lowercase in
   upsert): a rule whose appId round-tripped through NFC normalization could
   be undeletable by the raw string. Bug-compatible port recommended.
9. `unpair`: credential deletion failure is tolerated (warn + continue) but a
   `sequenceStore.remove` failure aborts unpair AFTER the credential may
   already be gone, leaving connection present (disabled) with no token.
   Error-tolerance boundary looks unintentional.
10. `ensureCredentialStore` failure inside `claimPairing` maps to
    `pairingFailed`, not `credentialStoreUnavailable` (unreachable in practice
    because `service.pair` resolves the store first — but the Rust pairing fn
    should keep the same mapping if it keeps the same hook).
11. `credentialStore.set` failure during pair and `sequenceStore.remove`
    failure during unpair surface as generic `internal`, not a specific code.
12. `handleSleepOrLock` from a paired-but-disabled coordinator reports
    `suspended` until wake (then settles back to `disabled` via a transient
    `connecting`). Harmless but visible in the UI.
13. Pairing responses skip the requestId echo check (`expectedRequestId` is
    not passed for capabilities or claim), and `pairingClaimResponseSchema`
    requires no `meta` at all. The mock's 410 test relies on the simplified
    `{error:{code}}` envelope being accepted.
14. In pairing, ANY capabilities-fetch failure (including a well-formed server
    error envelope) maps to `network`; only the claim step distinguishes
    server codes.
15. Timer semantics around `until()`-style tests: integration tests rely on
    real time with `networkRetryMs: 100` / `featureRetryMs: 200`; the Rust
    port should keep coordinator timings injectable exactly as
    `Partial<CoordinatorTimings>` to make the ported tests deterministic-ish
    without a mock clock (the TS suite has none).
