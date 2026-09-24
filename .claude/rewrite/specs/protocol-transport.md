# Companion Protocol v2 — Wire and HTTP Transport Spec

Behavioral ground truth extracted from `packages/core` (TypeScript) for the Rust
rewrite. Audience: Rust implementer + parity reviewer. Every constant, shape,
and assertion below is literal; the protocol must be re-implementable
byte-for-byte from this document.

Source files covered:

- `packages/core/src/companion/protocol/wire.ts`
- `packages/core/src/companion/protocol/types.ts`
- `packages/core/src/companion/protocol/capabilities.ts`
- `packages/core/src/companion/protocol/dtoMapper.ts`
- `packages/core/src/companion/sequencer.ts`
- `packages/core/src/companion/transport/httpClient.ts`
- `packages/core/src/companion/transport/errors.ts`
- `packages/core/src/companion/presenceClient.ts`
- `packages/core/test/companion/{wire,sequencer,dtoMapper,capabilities,presenceClient}.test.ts`
- `packages/core/test/helpers/mockServer.ts`

Endpoint call sites for capabilities/pairing were confirmed in
`src/companion/pairing.ts` and `src/companion/coordinator.ts` (outside this
spec's ownership, referenced only for method/path/auth facts).

---

## 1. Consolidated wire reference

### 1.1 Constants (exact values)

| Constant | Value | Defined in |
| --- | --- | --- |
| `MAXIMUM_SAFE_WIRE_INTEGER` | `9007199254740991` (2^53 − 1) | wire.ts |
| `PRESENCE_SCHEMA` | `"yohaku.companion.presence"` | wire.ts |
| `PRESENCE_SCHEMA_VERSION` | `2` | wire.ts |
| `PROTOCOL_CLIENT_VERSION` | `"1.8.3"` | wire.ts |
| `RFC3339_MS_UTC` (regex) | `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$` | wire.ts |
| `UUID_RE` (regex) | `^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$` | wire.ts |
| `ULID_RE` (regex) | `^[0-9A-HJKMNP-TV-Z]{26}$` | wire.ts |
| `DISPLAY_NAME_LIMIT` | `120` (Unicode scalars) | dtoMapper.ts |
| `WINDOW_TITLE_LIMIT` | `500` (Unicode scalars) | dtoMapper.ts |
| `MEDIA_TEXT_LIMIT` | `300` (Unicode scalars) | dtoMapper.ts |
| `REQUEST_TIMEOUT_MS` | `10000` (whole HTTP request, `AbortSignal.timeout`) | httpClient.ts |
| `DEFAULT_MAX_PAYLOAD_BYTES` | `32768` (`32 * 1024`) | httpClient.ts |
| `VERSION_HEADER` | `"X-Yohaku-Companion-Version"` | httpClient.ts |
| `REQUIRED_PRESENCE_SCOPE` | `"companion:presence:write"` | types.ts |
| `MUTATION_RENEGOTIATE_CODES` | set of `"COMPANION_SCHEMA_UNSUPPORTED"`, `"COMPANION_FEATURE_UNAVAILABLE"` | types.ts |
| Retry count for mutations | exactly `1` retry (2 attempts max) | presenceClient.ts |

Notes:

- `PROTOCOL_CLIENT_VERSION` deliberately tracks the original companion's
  release line (>= 1.7.3 per the Core minimum-client contract), decoupled from
  this application's own version. It is BOTH the `X-Yohaku-Companion-Version`
  header value and the client version fed into capabilities negotiation.
- There is no client-side response-size limit anywhere in the TS transport
  (response bodies are read unbounded). Only request payloads are capped.

### 1.2 Endpoints

| Method | Path (relative to base URL) | Auth | Request body | Response schema | requestId echo checked | Payload cap |
| --- | --- | --- | --- | --- | --- | --- |
| GET | `/companion/capabilities` | none (no Bearer, no version header) | none | `capabilitiesResponseSchema` | no | n/a |
| POST | `/companion/pairings/claim` | none | `PairingClaimRequest` | `pairingClaimResponseSchema` | no | `32768` (default) |
| PUT | `/companion/presence` | Bearer + version header | `PresenceRequest` | `mutationResponseSchema` | yes (`meta.requestId`) | negotiated `maximumPayloadBytes` |
| POST | `/companion/presence/clear` | Bearer + version header | `ClearRequest` | `mutationResponseSchema` | yes (`meta.requestId`) | negotiated `maximumPayloadBytes` |

Paths are appended to any existing base-URL path prefix (section 8.3).
`GET /companion/capabilities` is called from both `pairing.ts` (pairing
preflight) and `coordinator.ts` (session start); neither call attaches a
credential, so capabilities requests carry no `Authorization` and no
`X-Yohaku-Companion-Version` header.

### 1.3 Headers

Request headers set by `CompanionHttpClient.execute`:

- Always: `Accept: application/json`.
- Only when a credential is passed:
  - `Authorization: Bearer <deviceToken>` (literal string `"Bearer "` +
    token, exactly one space, no quoting/encoding of the token).
  - `X-Yohaku-Companion-Version: 1.8.3` (the `PROTOCOL_CLIENT_VERSION`).
- Only when a body is present (raw pre-encoded bytes or JSON-encodable value):
  - `Content-Type: application/json` (no charset parameter).

Header names are sent in this exact spelling; HTTP is case-insensitive and the
mock-server tests read them lowercased (`authorization`,
`x-yohaku-companion-version`).

### 1.4 Key-presence taxonomy (definitions used throughout)

- **REQUIRED**: key must be present, value must be non-null and type-valid.
- **REQUIRED-NULLABLE**: key must be present on the wire; `null` is a legal
  value; a MISSING key is a protocol violation. Absence and null are distinct
  states. Encode side: request bodies are built as fully populated literals so
  `JSON.stringify` keeps every key (it drops only `undefined`, which never
  occurs). Decode side: enforced by zod (`.nullable()` on a required key —
  zod v4.4.3 rejects a missing key and rejects `undefined`, accepts `null`).
- **OPTIONAL**: key may be absent; when present must be type-valid.
- **OMITTED**: key is never emitted by this client (capability-conditional
  keys the client never negotiates).
- **Unknown keys** in RESPONSES: silently accepted and stripped at every
  object level (zod non-strict objects). The Rust decoder must ignore unknown
  fields (do NOT `deny_unknown_fields`). Unknown keys in REQUESTS: never
  produced; the client emits exactly the keys listed here, in the listed
  order.

### 1.5 Canonical presence request example

Input: the dtoMapper test snapshot (section 11.3 fixture), `sequence = 7`,
`requestedLeaseSeconds = 90`, `deviceId = "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b"`,
lease clamp [30, 120]. Wire bytes are compact JSON (no whitespace), keys in
exactly this order (`<uuid>` is a freshly generated lowercase v4 UUID):

```json
{
  "meta": {
    "schema": "yohaku.companion.presence",
    "schemaVersion": 2,
    "requestId": "<uuid>",
    "deviceId": "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b",
    "sequence": 7,
    "observedAt": "2025-07-26T03:20:12.345Z"
  },
  "data": {
    "availability": "active",
    "lease": { "ttlSeconds": 90 },
    "application": {
      "displayName": "Code",
      "activity": null,
      "window": { "title": "file.ts" },
      "icon": null
    },
    "media": {
      "sessionId": "aa6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b",
      "kind": "music",
      "title": "Song",
      "artist": "Artist",
      "album": null,
      "player": { "displayName": "Spotify" },
      "playback": {
        "state": "playing",
        "durationMs": 200500,
        "positionMs": 60250,
        "sampledAt": "2025-07-26T03:20:12.345Z",
        "rate": 1
      }
    }
  }
}
```

Note `"rate": 1` — an integral rate serializes with NO decimal point
(JS number formatting). See traps, section 12.1.

---

## 2. `src/companion/protocol/wire.ts` — wire primitives

Everything is deliberately strict: non-canonical input is a protocol error
(`WireError`), never repaired.

### 2.1 `WireError`

Error class named `"WireError"` (extends `Error`). All wire-primitive
failures throw it. Messages (exact formats, `${...}` interpolated):

- `"non-finite date"`
- `` `non-canonical date: ${encoded}` ``
- `` `invalid wire date: ${value}` ``
- `` `unparseable wire date: ${value}` ``
- `` `non-canonical wire date: ${value}` ``
- `` `integer out of wire range for ${field}: ${value}` ``
- `` `invalid seconds for ${field}: ${seconds}` ``
- `` `missing required key "${key}" in ${context}` `` (from `requireKey`)

### 2.2 Wire date format

Canonical form: RFC3339 UTC with EXACTLY 3 fractional digits and `Z` suffix,
matched by regex `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$`.
4-digit year (0000–9999 only), `T` separator, no offset forms, no more or
fewer fraction digits.

`encodeWireDate(epochMs: number) -> string`:

1. If `epochMs` is not finite (NaN/±Inf) → `WireError("non-finite date")`.
2. `encoded = new Date(epochMs).toISOString()`.
   - JS detail: fractional epochMs is truncated toward zero (moot for the
     Rust `i64` signature).
   - JS detail: finite `|epochMs| > 8_640_000_000_000_000` makes
     `toISOString()` throw a native `RangeError`, NOT a `WireError`
     (ambiguity; see section 13).
3. If `encoded` does not match the regex (years outside 0000–9999 produce
   expanded `+YYYYYY`/`-YYYYYY` forms) → `WireError("non-canonical date: ...")`.
4. Return `encoded`.

Negative epoch milliseconds are legal as long as the year stays in 0000–9999:
`encodeWireDate(-1) === "1969-12-31T23:59:59.999Z"`.
`encodeWireDate(0) === "1970-01-01T00:00:00.000Z"`.
`encodeWireDate(1753500012345) === "2025-07-26T03:20:12.345Z"`.

`decodeWireDate(value: string) -> number` (epoch ms, may be negative):

1. Regex mismatch → `WireError("invalid wire date: ...")`.
2. `parsed = Date.parse(value)`; NaN → `WireError("unparseable wire date: ...")`
   (e.g. month `13`).
3. Round-trip check: `new Date(parsed).toISOString() !== value` →
   `WireError("non-canonical wire date: ...")`. This is what rejects
   date-component overflow that still parses, e.g. `2026-02-30T...` (V8
   parses it as March 2) and `T24:00:00.000Z` (parses as next-day midnight).
4. Return `parsed`.

Net effect the Rust port must reproduce: accept a string iff it matches the
regex AND its components are a real calendar instant (month 01–12, valid
day-of-month incl. leap years, hour 00–23, minute 00–59, second 00–59 — no
leap second `60`, which fails parse), and return its epoch-ms value; the
canonical re-encoding of that value must equal the input string exactly.

Rejection cases (from tests, all → `WireError`):

- `"2026-07-26T09:41:12Z"` (no milliseconds)
- `"2026-07-26T09:41:12.34Z"` (2 fraction digits)
- `"2026-07-26T09:41:12.345678Z"` (6 fraction digits)
- `"2026-07-26T09:41:12.345+00:00"` (offset form)
- `"2026-07-26 09:41:12.345Z"` (space separator)
- `"2026-13-26T09:41:12.345Z"` (invalid month)

### 2.3 Identifier rules

`isValidWireIdentifier(value: string) -> boolean` returns true iff the string
matches `UUID_RE` OR `ULID_RE`:

- UUID: `^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$`
  — any case, hyphenated 8-4-4-4-12; NO version/variant constraint.
- ULID: `^[0-9A-HJKMNP-TV-Z]{26}$` — exactly 26 chars of the Crockford
  Base32 alphabet, UPPERCASE only, excluding `I`, `L`, `O`, `U`. The first
  character is NOT constrained to `0-7` (no timestamp-range check).

### 2.4 Wire integers

`requireWireInteger(value: number, field: string) -> number`: throws
`WireError` unless `Number.isInteger(value)` AND `0 <= value <= 9007199254740991`.
Bounds are inclusive. (JS `Number.isInteger` accepts float-typed integral
values like `3.0`; NaN/±Inf fail it. `-0` passes and is returned.)

`secondsToWireMilliseconds(seconds: number, field: string) -> number`:

1. If `seconds` is not finite OR `seconds < 0` →
   `WireError("invalid seconds for ...")`.
2. Return `requireWireInteger(Math.round(seconds * 1000), field)`.
   JS `Math.round` = round half toward +infinity; inputs are non-negative
   here so this equals round-half-away-from-zero.
   Examples: `1.2345 -> 1235` (1234.5 rounds up), `0 -> 0`,
   `60.2504 -> 60250`.

### 2.5 `requireKey` (decode-side helper)

`requireKey(obj, key, context)`: throws
`WireError('missing required key "<key>" in <context>')` when
`Object.prototype.hasOwnProperty.call(obj, key)` is false, else returns the
value. Exported but UNUSED in this slice (zod enforces decode-side presence);
porting it is optional.

---

## 3. `src/companion/protocol/types.ts` — envelopes and DTO shapes

### 3.1 Enums (exact string values)

- `MediaWireKind`: `"music" | "podcast" | "video" | "unknown"`
- `WirePlaybackState`: `"playing" | "paused"`
- `WireAvailability`: `"idle" | "active"`
- `ClearReason`: `"paused" | "sleep" | "shutdown" | "privacyChanged" | "connectionRemoved"`

### 3.2 Request bodies (client → server)

All request bodies are built as fully populated object literals; every listed
key is serialized, in the listed (insertion) order; `null` is written out
explicitly. Nothing else is ever added.

`PresenceRequestMeta` (used by both mutations):

| Key | Type | Presence |
| --- | --- | --- |
| `schema` | literal `"yohaku.companion.presence"` | REQUIRED |
| `schemaVersion` | literal `2` | REQUIRED |
| `requestId` | string (freshly generated lowercase UUID v4) | REQUIRED |
| `deviceId` | string (the credential's device id) | REQUIRED |
| `sequence` | integer (from sequencer; 0..2^53−2 by construction) | REQUIRED |
| `observedAt` | wire date string | REQUIRED |

`PresenceRequest` — body of `PUT /companion/presence`:

| Key | Type | Presence |
| --- | --- | --- |
| `meta` | `PresenceRequestMeta` | REQUIRED |
| `data.availability` | `"idle"` iff both `application` and `media` are null, else `"active"` | REQUIRED |
| `data.lease` | object `{ ttlSeconds }` | REQUIRED |
| `data.lease.ttlSeconds` | integer (clamped, section 5.4) | REQUIRED |
| `data.application` | `WireApplicationContext` or null | REQUIRED-NULLABLE |
| `data.media` | `WireMediaContext` or null | REQUIRED-NULLABLE |

`WireApplicationContext`:

| Key | Type | Presence |
| --- | --- | --- |
| `displayName` | string (1..120 scalars, non-blank) | REQUIRED |
| `activity` | `{ key: string\|null, customLabel: string\|null }` or null — this client ALWAYS emits `null` | REQUIRED-NULLABLE |
| `window` | `{ title: string }` or null | REQUIRED-NULLABLE |
| `icon` | `{ url: string }` or null — this client ALWAYS emits `null` | REQUIRED-NULLABLE |

`WireMediaContext`:

| Key | Type | Presence |
| --- | --- | --- |
| `sessionId` | string (passed through verbatim from the sanitized snapshot; upstream guarantees a UUID — NOT re-validated here) | REQUIRED |
| `kind` | `MediaWireKind` (passed through) | REQUIRED |
| `title` | string or null | REQUIRED-NULLABLE |
| `artist` | string or null | REQUIRED-NULLABLE |
| `album` | string or null | REQUIRED-NULLABLE |
| `player` | `{ displayName: string }` or null | REQUIRED-NULLABLE |
| `playback` | `WirePlayback` | REQUIRED |
| `artwork` | — capability-conditional; NEVER negotiated by this client | OMITTED (key absent, not null) |
| `link` | — capability-conditional; NEVER negotiated by this client | OMITTED (key absent, not null) |

`WirePlayback`:

| Key | Type | Presence |
| --- | --- | --- |
| `state` | `"playing" \| "paused"` | REQUIRED |
| `durationMs` | integer or null | REQUIRED-NULLABLE |
| `positionMs` | integer or null | REQUIRED-NULLABLE |
| `sampledAt` | wire date string | REQUIRED |
| `rate` | number (0..4; consistency rules in section 5.3) | REQUIRED |

`ClearRequest` — body of `POST /companion/presence/clear`:

| Key | Type | Presence |
| --- | --- | --- |
| `meta` | `PresenceRequestMeta` | REQUIRED |
| `data.reason` | `ClearReason` | REQUIRED |

`PairingClaimRequest` — body of `POST /companion/pairings/claim`
(key order: `deviceName`, `pairingCode`):

| Key | Type | Presence |
| --- | --- | --- |
| `deviceName` | string (NFC-normalized, trimmed, 1..120 scalars — enforced in pairing.ts) | REQUIRED |
| `pairingCode` | string (trimmed, 1..32 chars — enforced in pairing.ts) | REQUIRED |

### 3.3 Response schemas (server → client, zod v4.4.3)

Shared refinements:

- `wireIdentifier`: string satisfying `isValidWireIdentifier` (else decode
  error message `"expected UUID or ULID"`).
- `wireDate`: string for which `decodeWireDate` succeeds (else
  `"expected canonical RFC3339 millisecond UTC date"`).
- `wireInteger`: number, integral, `>= 0`, `<= 9007199254740991`.
- Plain `.int()` fields (capabilities): integral AND a safe integer
  (|v| <= 2^53−1); MAY be negative at decode time — semantic positivity is
  enforced later by `negotiatePresence`, not by the schema.
- All objects: unknown keys accepted and ignored.

`responseMetaSchema` — REQUIRED on every mutation/capabilities/error
envelope. A hard decode constraint: even capabilities and error envelopes
carry the PRESENCE schema constants.

| Key | Type | Presence |
| --- | --- | --- |
| `schema` | literal `"yohaku.companion.presence"` | REQUIRED |
| `schemaVersion` | literal `2` | REQUIRED |
| `requestId` | wireIdentifier | REQUIRED |
| `serverTime` | wireDate | REQUIRED |

`capabilitiesResponseSchema` = `{ meta: responseMetaSchema, data: capabilitiesDataSchema }`, where `capabilitiesDataSchema`:

| Key | Type | Presence |
| --- | --- | --- |
| `minimumClientVersion` | string (any; semver-validated during negotiation) | REQUIRED |
| `presenceSchemaVersions` | array of int | REQUIRED |
| `momentSchemaVersions` | array of int | REQUIRED |
| `features.liveDesk` | boolean | REQUIRED |
| `features.mediaTimeline` | boolean | REQUIRED |
| `features.moments` | boolean | REQUIRED |
| `features.readingSessions` | boolean | REQUIRED |
| `features.mediaArtwork` | boolean | OPTIONAL |
| `features.mediaPlaybackLinks` | boolean | OPTIONAL |
| `limits.presencePayloadBytes` | int | REQUIRED |
| `limits.presenceRequestsPerMinute` | int | REQUIRED |
| `limits.presenceLeaseMinSeconds` | int | REQUIRED |
| `limits.presenceLeaseMaxSeconds` | int | REQUIRED |
| `limits.recommendedHeartbeatSeconds` | int | REQUIRED |
| `limits.maximumClockSkewSeconds` | int | REQUIRED |

`pairingClaimResponseSchema` = `{ data: pairingClaimDataSchema }` — NOTE: no
`meta` key is required on pairing claim responses (and no requestId echo check
is performed for pairing). `pairingClaimDataSchema`:

| Key | Type | Presence |
| --- | --- | --- |
| `deviceId` | wireIdentifier | REQUIRED |
| `deviceToken` | string with `trim().length > 0` (else `"empty token"`) | REQUIRED |
| `scopes` | array of string | REQUIRED |
| `nextSequence` | wireInteger | REQUIRED |

`publicLiveDeskStateSchema`:

| Key | Type | Presence |
| --- | --- | --- |
| `schemaVersion` | literal `2` | REQUIRED |
| `epoch` | wireIdentifier | REQUIRED |
| `revision` | wireInteger | REQUIRED |
| `projection` | any JSON value or null | REQUIRED-NULLABLE (verified: zod v4 rejects a missing key even for `z.unknown().nullable()`) |

`mutationResponseSchema` — success body for both mutations:

| Key | Type | Presence |
| --- | --- | --- |
| `meta` | responseMeta | REQUIRED |
| `data.acceptedSequence` | wireInteger | REQUIRED |
| `data.receivedAt` | wireDate | REQUIRED |
| `data.state` | publicLiveDeskState | REQUIRED |

`errorEnvelopeSchema` — protocol error body (non-2xx):

| Key | Type | Presence |
| --- | --- | --- |
| `meta` | responseMeta | REQUIRED |
| `error.code` | string | REQUIRED |
| `error.message` | string | REQUIRED |
| `error.retryable` | boolean | REQUIRED |
| `error.retryAfterMs` | wireInteger or null | REQUIRED-NULLABLE (decoded but unused in this slice) |
| `error.acceptedSequence` | wireInteger or null | REQUIRED-NULLABLE |
| `error.fields` | array of string | REQUIRED |

`pairingErrorEnvelopeSchema` — simplified pairing rejection body:
`{ error: { code: string } }` (only `error.code` REQUIRED; everything else
ignored).

---

## 4. `src/companion/protocol/capabilities.ts` — negotiation

### 4.1 `SemanticVersion` parsing (strict SemVer 2.0)

`parseSemanticVersion(input: string) -> SemanticVersion | null` where
`SemanticVersion = { major, minor, patch: number, prerelease: string[] }`.
Build metadata is validated then DISCARDED (not stored).

Algorithm (order matters):

1. Find the FIRST `+` in the whole input. If present: the substring after it
   is build metadata — must be non-empty and each `.`-separated identifier
   must be non-empty and match `^[0-9A-Za-z-]+$`; else return null. Strip
   `+...` from the working string.
2. Find the FIRST `-` in the remaining string. If present: the substring
   after it is the prerelease — must be non-empty; split on `.`; each
   identifier must be non-empty, match `^[0-9A-Za-z-]+$`, and if it is purely
   digits (`^\d+$`) it must have no leading zeros (match `^(0|[1-9]\d*)$`,
   so `"0"` is fine, `"01"` is not); else return null. Strip `-...`.
   (Note: only the first `-` splits, so `1.0.0-alpha-beta` yields the single
   prerelease identifier `"alpha-beta"`, which is valid.)
3. The remainder must split on `.` into EXACTLY 3 parts, each matching
   `^(0|[1-9]\d*)$` (no leading zeros, no signs); parse base-10; else null.

Consequences: `"v1.0.0"`, `"1.8"`, `"1.8.3.4"`, `"01.0.0"`,
`"1.0.0-alpha.01"`, `"1.0.0-"`, `"1.0.0+"`, `"1.0.x"` are all null.

### 4.2 `compareSemanticVersions(a, b) -> number` (negative/zero/positive)

1. Compare `major`, then `minor`, then `patch` numerically; first difference
   wins (returned as arithmetic difference).
2. Prerelease: both empty → equal (0). A release (empty prerelease) is
   GREATER than any prerelease of the same core (`a` empty → +1; `b` empty →
   −1).
3. Identifier-by-identifier over the common prefix length:
   - both purely numeric (`^\d+$`): compare as parsed integers;
   - exactly one numeric: numeric < textual;
   - both textual: lexicographic (UTF-16 code-unit `<`; ASCII in practice).
4. All common identifiers equal → shorter prerelease list is smaller
   (`preA.length - preB.length`).

Build metadata never participates (already discarded).

### 4.3 `NegotiatedPresenceConfiguration` (exact shape)

```
{
  supportsMediaTimeline: boolean,      // <- features.mediaTimeline
  maximumPayloadBytes: number,         // <- limits.presencePayloadBytes
  requestsPerMinute: number,           // <- limits.presenceRequestsPerMinute
  leaseMinSeconds: number,             // <- limits.presenceLeaseMinSeconds
  leaseMaxSeconds: number,             // <- limits.presenceLeaseMaxSeconds
  recommendedHeartbeatSeconds: number, // <- limits.recommendedHeartbeatSeconds
  maximumClockSkewSeconds: number      // <- limits.maximumClockSkewSeconds
}
```

### 4.4 `negotiatePresence(capabilities, clientVersion)` decision procedure

Result type `PresenceNegotiation` is one of `{ kind: "available",
configuration }`, `{ kind: "clientUpdateRequired" }`,
`{ kind: "schemaUnsupported" }`, `{ kind: "featureUnavailable" }`,
`{ kind: "invalidCapabilities" }`.

Evaluated strictly in this order:

1. `invalidCapabilities` if ANY of:
   - `clientVersion` fails `parseSemanticVersion`;
   - `capabilities.minimumClientVersion` fails `parseSemanticVersion`;
   - any entry of `presenceSchemaVersions` is `<= 0`;
   - any entry of `momentSchemaVersions` is `<= 0` (yes — moment versions
     are validated even though this client never uses moments);
   - `limitsAreValid` is false. `limitsAreValid(limits)` requires ALL of:
     `presencePayloadBytes > 0`, `presenceRequestsPerMinute > 0`,
     `presenceLeaseMinSeconds > 0`,
     `presenceLeaseMinSeconds <= presenceLeaseMaxSeconds`,
     `presenceLeaseMinSeconds <= recommendedHeartbeatSeconds`,
     `recommendedHeartbeatSeconds <= presenceLeaseMaxSeconds`,
     `maximumClockSkewSeconds >= 0`.
2. `clientUpdateRequired` if `compare(client, minimum) < 0`.
3. `schemaUnsupported` if `presenceSchemaVersions` does not contain `2`
   (`PRESENCE_SCHEMA_VERSION`, exact integer membership).
4. `featureUnavailable` if `features.liveDesk` is false.
5. Otherwise `available` with the configuration mapped 1:1 as in 4.3 (no
   clamping, no defaulting — server values are passed through verbatim once
   they satisfy `limitsAreValid`).

Negotiation is pure; the caller decides error behavior (pairing maps
`clientUpdateRequired` → `PairingError("clientUpdateRequired")`,
`schemaUnsupported`/`featureUnavailable` →
`PairingError("serverFeatureUnavailable")`, `invalidCapabilities` →
`PairingError("invalidCapabilities")`; the coordinator maps them to runtime
states — out of scope here).

---

## 5. `src/companion/protocol/dtoMapper.ts` — snapshot → wire mapping

Input domain type (`src/privacy/types.ts`):

```
SanitizedPresenceSnapshot {
  observedAt: number,                       // epoch ms
  application: { displayName: string, windowTitle: string|null } | null,
  media: {
    sessionId: string,                      // stable per-session UUID (upstream)
    kind: "music"|"podcast"|"video"|"unknown",
    title: string|null, artist: string|null, album: string|null,
    playerDisplayName: string|null,
    playback: {
      state: "playing"|"paused",
      durationSeconds: number|null, positionSeconds: number|null,
      sampledAt: number,                    // epoch ms
      rate: number
    }
  } | null
}
```

### 5.1 Text bounding (`boundedText`)

`truncateScalars(value, limit)`: spread the string into Unicode scalar values
(code points, NOT UTF-16 units, NOT grapheme clusters); if count `<= limit`
return the original string unchanged, else join the first `limit` scalars.

`boundedText(value, limit)`: `null` → `null`; otherwise truncate FIRST, then
`String.prototype.trim()` the result; if the trimmed result is empty →
`null`, else return it. (JS `trim` removes the WhiteSpace+LineTerminator set,
which includes U+00A0 NBSP and U+FEFF — see traps 12.7.)

Limits: displayName/playerDisplayName 120; window title 500;
media title/artist/album 300. Over-limit text is truncated, not rejected
(keeps a publish alive).

### 5.2 `mapApplication(snapshot)`

- `snapshot.application === null` → `null`.
- `displayName = boundedText(application.displayName, 120)`; if `null` →
  `WireError("application displayName empty after bounding")`.
- `title = boundedText(application.windowTitle, 500)`.
- Result: `{ displayName, activity: null, window: title === null ? null : { title }, icon: null }`.

### 5.3 `mapMedia(snapshot)`

- `snapshot.media === null` → `null`.
- `title/artist/album = boundedText(·, 300)` each.
- If `title === null && artist === null` →
  `WireError("media requires title or artist")`.
- `playerDisplayName = boundedText(media.playerDisplayName, 120)`.
- Rate validation (in this order):
  - not finite, or `rate < 0`, or `rate > 4` →
    `WireError("playback rate out of range: <rate>")` (bounds 0 and 4
    inclusive-legal);
  - `state === "paused"` and `rate !== 0` →
    `WireError("paused playback must have rate 0")`;
  - `state === "playing"` and `rate <= 0` →
    `WireError("playing playback must have rate > 0")`.
- `durationMs = durationSeconds === null ? null : secondsToWireMilliseconds(durationSeconds, "durationMs")` (round; field name `"durationMs"` appears in error messages).
- `positionMs` likewise with field `"positionMs"`.
- Clamp: if both non-null and `positionMs > durationMs` →
  `positionMs = durationMs`. Null is NEVER coerced to 0.
- Result: `{ sessionId: media.sessionId, kind: media.kind, title, artist,
  album, player: playerDisplayName === null ? null : { displayName: playerDisplayName },
  playback: { state, durationMs, positionMs,
  sampledAt: encodeWireDate(media.playback.sampledAt), rate } }`.
- `sessionId` and `kind` are passed through VERBATIM (no identifier/enum
  re-validation at this layer).

### 5.4 `makePresenceRequest(snapshot, sequence, requestedLeaseSeconds, options)`

`options = { deviceId, leaseMinSeconds, leaseMaxSeconds }`. Returns
`{ requestId, body }`.

Order of operations (determines which WireError fires first):

1. `application = mapApplication(snapshot)`
2. `media = mapMedia(snapshot)`
3. `ttlSeconds = min(max(Math.round(requestedLeaseSeconds), leaseMinSeconds), leaseMaxSeconds)`
   — round-half-up, then clamp into the negotiated `[leaseMin, leaseMax]`.
4. `meta = makeMeta(options.deviceId, sequence, snapshot.observedAt)`.
5. `body = { meta, data: { availability, lease: { ttlSeconds }, application, media } }`
   with `availability = (application === null && media === null) ? "idle" : "active"`.

`makeMeta(deviceId, sequence, observedAtMs)` returns
`{ schema: "yohaku.companion.presence", schemaVersion: 2,
requestId: randomUUID(), deviceId, sequence,
observedAt: encodeWireDate(observedAtMs) }`.

- `requestId` = Node `crypto.randomUUID()`: RFC 4122 version-4 UUID,
  lowercase hyphenated. A NEW id per mapped request (retries reuse the mapped
  request, hence the same id).
- `sequence` is NOT range-checked here (the sequencer guarantees
  0..2^53−2); `deviceId` is NOT re-validated here.
- `encodeWireDate` may throw `WireError` for out-of-range `observedAt`.

### 5.5 `makeClearRequest(reason, sequence, observedAtMs, options)`

Returns `{ requestId: meta.requestId, body: { meta, data: { reason } } }`
with the same `makeMeta`. `options.leaseMinSeconds/leaseMaxSeconds` are
accepted but unused. `reason` is one of the five `ClearReason` strings,
passed through verbatim. `observedAt` = caller-supplied epoch ms (the
PresenceClient passes its `observedAtMs` argument straight through).

---

## 6. `src/companion/sequencer.ts` — durable monotonic sequence

### 6.1 Persistence interface

```
SequencePersistence {
  load(deviceId) -> number | null        // async
  store(deviceId, next) -> void          // async, may fail
}
```

`CompanionSequencer(persistence, deviceId, pairingNextSequence)`. The stored
value is "the NEXT sequence to hand out". No in-memory cache: EVERY
reserve/reconcile re-loads from persistence.

### 6.2 Serialization

All operations run through an internal FIFO queue (promise-chain mutex):
each task starts only after the previous one settled (fulfilled OR
rejected); a task's failure does not block later tasks. Concurrent
`reserve()` calls therefore return unique, strictly increasing values with no
gaps (absent crashes/persist failures).

### 6.3 `currentNext()` (internal)

`stored = load(deviceId)`. `stored` is VALID iff
`stored !== null && Number.isInteger(stored) && stored >= 0 && stored <= 9007199254740991`.
Result = `max(pairingNextSequence, valid ? stored : pairingNextSequence)` —
i.e. `pairingNextSequence` is a hard floor; invalid/corrupt stored values
self-heal to the pairing base.

### 6.4 `reserve() -> number`

1. `current = currentNext()`.
2. If `current >= 9007199254740991` → throw
   `SequenceExhaustedError("sequence space exhausted")` (error name
   `"SequenceExhaustedError"`). So the maximum value ever RETURNED is
   2^53−2 and the maximum value ever STORED is 2^53−1.
3. `store(deviceId, current + 1)` — persistence is the linearization point.
   A store failure propagates out of `reserve()` and NOTHING is consumed
   (the next reserve returns the same `current`).
4. Return `current`.

Durability invariant: `current + 1` is PERSISTED before `current` is
returned for use, therefore before any network send. A crash between persist
and send produces a legal gap; a sequence number is NEVER reused.

### 6.5 `reconcile(acceptedSequence) -> void`

1. Silent no-op (return, nothing persisted) if
   `!Number.isInteger(acceptedSequence)` or `acceptedSequence < 0` or
   `acceptedSequence >= 9007199254740991` (note `>=`: accepted = 2^53−1 is
   ignored because `accepted + 1` would leave the wire range).
2. `current = currentNext()`; `next = max(current, acceptedSequence + 1)`.
3. Persist ONLY if `next !== current` (monotonic: the stored value never
   moves backwards; a stale/behind `acceptedSequence` is ignored without a
   store call).

### 6.6 Interaction contract with the store

The production backing is `FileSequenceStore`
(`src/store/sequenceStore.ts` / Rust `store/sequence.rs`, out of scope
here); the sequencer only assumes the `SequencePersistence` semantics above:
`load` returns the raw stored number or null; `store` durably writes and may
throw (e.g. disk full), which must fail the surrounding operation.

---

## 7. `src/companion/transport/errors.ts` — error taxonomy

### 7.1 Classes

All extend `CompanionTransportError` (which extends `Error`, name
`"CompanionTransportError"`), except `CompanionServerConfigurationError`
which lives in httpClient.ts and extends plain `Error`.

| Class (name property) | Payload fields | Message format | Thrown when |
| --- | --- | --- | --- |
| `CompanionNetworkError` | `cause_: unknown` | `` `network failure: ${String(cause_)}` `` | fetch rejected (DNS/TCP/TLS/abort-timeout) or reading the response body failed. Ambiguous: server may or may not have committed the mutation. |
| `CompanionEmptyResponseError` | — | `` `empty response (${status})` `` | response body had zero bytes, ANY status (a 204 would hit this too) |
| `CompanionDecodeError` | — | `"response is not JSON"` or `` `response decode failed: ${firstZodIssueMessage ?? "unknown"}` `` | 2xx body not JSON, or 2xx JSON failing the response schema |
| `CompanionRequestIdMismatchError` | — | `"response requestId does not echo the request"` | `meta.requestId` of a decoded success OR error envelope ≠ `expectedRequestId` |
| `CompanionPayloadTooLargeError` | — | `` `payload ${n} bytes exceeds limit ${limit}` `` | encoded request body exceeds the payload cap (checked before sending) |
| `CompanionCredentialDeviceMismatchError` | — | `"request deviceId does not match credential"` | PresenceClient guard, section 9.2 |
| `CompanionServerError` | `status: number`, `envelope: ErrorEnvelope` | `` `server error ${status}: ${envelope.error.code}` `` | non-2xx with a decodable protocol error envelope |
| `CompanionHttpStatusError` | `status: number` | `` `http status ${status} without decodable envelope` `` | non-2xx whose body is not a decodable envelope (incl. non-JSON bodies) |
| `CompanionPairingServerError` | `status: number`, `code: string` | `` `pairing rejected ${status}: ${code}` `` | non-2xx matching the simplified pairing envelope `{error:{code}}` |
| `CompanionServerConfigurationError` | — | `"invalid base URL"` / `"base URL must use HTTPS (HTTP is allowed only for loopback hosts)"` / `"base URL must not embed credentials"` / `"base URL must not contain query or fragment"` | base URL validation, section 8.2 |

Only the TYPE (and `status`/`envelope`/`code` fields) drives logic; message
strings are informational except where tests assert them.

### 7.2 `isSafeForImmediateIdempotentRetry(error) -> boolean` (exact predicate)

Evaluated top to bottom; first match wins:

1. `CompanionPayloadTooLargeError` → `false`
2. `CompanionCredentialDeviceMismatchError` → `false`
3. `CompanionServerError` → `status >= 500 && status <= 599 && envelope.error.retryable`
4. `CompanionHttpStatusError` → `status >= 500 && status <= 599`
5. `CompanionEmptyResponseError` | `CompanionDecodeError` |
   `CompanionRequestIdMismatchError` → `true`
6. `CompanionNetworkError` → `true` (ambiguous transport failure: resend the
   exact request once)
7. anything else (WireError, SequenceExhaustedError, arbitrary errors) →
   `false`

Consequences: 4xx is NEVER retried; 5xx with an envelope is retried only when
the server says `retryable: true`; bare 5xx (no envelope) is always retried;
malformed/mismatched/empty responses and network failures are retried.

### 7.3 `needsRenegotiation(error) -> boolean`

- `CompanionServerError` → `envelope.error.code ∈ {"COMPANION_SCHEMA_UNSUPPORTED", "COMPANION_FEATURE_UNAVAILABLE"}`
- `CompanionHttpStatusError` → `status === 426` (a 426 without a decodable
  envelope is the compatibility signal from servers that cannot encode an
  envelope this client can read)
- else `false`

Schema/feature rejection is not a transport degradation: the caller must
terminate the current authority and re-enter capability negotiation.

### 7.4 `acceptedSequenceOf(error) -> number | null`

- `CompanionServerError` → `envelope.error.acceptedSequence` (which is
  itself `number | null`)
- anything else → `null`

---

## 8. `src/companion/transport/httpClient.ts` — HTTP layer

### 8.1 `isLoopbackHost(host) -> boolean`

Lowercase the host, then:

- `"localhost"`, `"::1"`, `"[::1]"` → true.
- IPv4: split on `"."`; must be EXACTLY 4 parts; `parts[0] === "127"`; every
  part must match `^\d{1,3}$` and parse (base 10) to `<= 255`. Leading zeros
  allowed (`"127.000.000.001"` is loopback); shorthand (`"127.1"`), other
  IPv6 spellings (`"0:0:0:0:0:0:0:1"`), and non-127 addresses are NOT.
- else false.

### 8.2 `CompanionServerConfiguration(baseUrl: string)`

Validation, in order, each failure a `CompanionServerConfigurationError`
(exact messages in 7.1):

1. Must parse as a WHATWG `URL` (this lowercases scheme/host, strips default
   ports, normalizes).
2. Scheme (protocol minus `":"`, lowercased) must be `https`, OR `http` with
   `isLoopbackHost(hostname)` true.
3. `username` and `password` must both be `""`.
4. `search` and `hash` must both be `""` (no query, no fragment).

The validated `URL` is kept as `baseUrl` (any path prefix is preserved).

### 8.3 `endpoint(path) -> URL` (base URL joining)

- Clone the base URL.
- `basePath` = base pathname with AT MOST ONE trailing `/` stripped
  (`slice(0, -1)` iff `endsWith("/")`).
- `suffix` = `path` if it starts with `/`, else `"/" + path`.
- New pathname = `basePath + suffix`.

Examples: base `https://x` (pathname `/`) + `/companion/presence` →
`/companion/presence`; base `https://x/api` or `https://x/api/` →
`/api/companion/presence`.

### 8.4 `encodeBody(body) -> Uint8Array`

UTF-8 bytes of `JSON.stringify(body)`: compact (no whitespace), object keys
in insertion order, `null` kept, `undefined`-valued keys dropped (never
present in our bodies), numbers in ECMAScript shortest form (`1` not `1.0`).

### 8.5 `execute(options)` request construction

`options = { method: "GET"|"POST"|"PUT", path, body?, credential?,
responseSchema, expectedRequestId?, maximumPayloadBytes?, encodedBody? }`.

1. URL = `endpoint(path)`.
2. Headers per section 1.3 (`Accept` always; `Authorization` + version
   header iff `credential`; `Content-Type` iff a body exists).
3. Body bytes: `encodedBody` if provided (idempotent-retry path: resend the
   EXACT bytes), else `encodeBody(body)` if `body !== undefined`, else none.
4. Payload cap (only when body bytes exist):
   `limit = maximumPayloadBytes ?? 32768`; if `byteLength > limit` (STRICTLY
   greater — equal passes) → `CompanionPayloadTooLargeError`. Enforced
   BEFORE any network I/O, on every attempt (deterministic, since retry
   bytes are identical).
5. Send with a 10 000 ms overall timeout (`AbortSignal.timeout(10_000)`);
   timeout surfaces as a fetch rejection.

### 8.6 `execute` response handling (status matrix)

| Step | Condition | Outcome |
| --- | --- | --- |
| fetch call | rejects (network, TLS, abort/timeout) | throw `CompanionNetworkError(cause)` |
| read body text | rejects | throw `CompanionNetworkError(cause)` |
| body text | `length === 0` (any status) | throw `CompanionEmptyResponseError("empty response (<status>)")` |
| `JSON.parse` | fails, status 2xx (`response.ok`) | throw `CompanionDecodeError("response is not JSON")` |
| `JSON.parse` | fails, status non-2xx | throw `CompanionHttpStatusError(status)` |
| 2xx | `responseSchema` parse fails | throw `CompanionDecodeError("response decode failed: <first issue>")` |
| 2xx | schema OK, `expectedRequestId` set, `payload.meta.requestId !== expected` | throw `CompanionRequestIdMismatchError` |
| 2xx | schema OK, echo OK (or no `expectedRequestId`) | RETURN the parsed value |
| non-2xx | body parses as `errorEnvelopeSchema` | echo-check FIRST (mismatch → `CompanionRequestIdMismatchError`), then throw `CompanionServerError(status, envelope)` |
| non-2xx | else body parses as `pairingErrorEnvelopeSchema` | throw `CompanionPairingServerError(status, error.code)` |
| non-2xx | else | throw `CompanionHttpStatusError(status)` |

Echo validation details: performed only when `expectedRequestId` is provided;
loose lookup of `payload.meta.requestId`; strict string `!==` comparison.
Note the ordering subtlety: a non-2xx WITH a valid envelope but a wrong
requestId throws `CompanionRequestIdMismatchError` (retry-safe), NOT
`CompanionServerError`. There is no response size limit and no
`Content-Type` validation on responses.

---

## 9. `src/companion/presenceClient.ts` — ordered presence writer

`PresenceClient(http, credential, sequencer, configuration)` where
`credential = { deviceId, deviceToken }` and `configuration` is the
`NegotiatedPresenceConfiguration` (section 4.3).

### 9.1 FIFO send slot

All mutations are serialized through one promise-chain slot (same pattern as
the sequencer queue): a mutation's ENTIRE lifecycle — sequence reservation,
mapping, device guard, send, single retry, and all reconciliation — completes
before the next queued mutation starts. Failures do not poison the slot.
Concurrent calls are processed strictly in call order (test: sequences
`[5, 6, 7]` for two replaces + one clear issued concurrently).

### 9.2 Public operations

`replacePresence(snapshot, requestedLeaseSeconds) -> MutationResponse`,
inside the slot:

1. `sequence = await sequencer.reserve()` (durable persist BEFORE send).
2. `mapped = makePresenceRequest(snapshot, sequence, requestedLeaseSeconds,
   { deviceId: credential.deviceId, leaseMinSeconds: configuration.leaseMinSeconds,
   leaseMaxSeconds: configuration.leaseMaxSeconds })`.
3. Device guard: if `mapped.body.meta.deviceId !== credential.deviceId` →
   throw `CompanionCredentialDeviceMismatchError` (defense-in-depth; the
   reserved sequence is consumed as a legal gap; never retried, nothing
   sent).
4. `performWithSingleRetry("PUT", "/companion/presence", mapped)`.

`clearPresence(reason, observedAtMs) -> MutationResponse`: identical shape
with `makeClearRequest(reason, sequence, observedAtMs, ...)` and
`performWithSingleRetry("POST", "/companion/presence/clear", mapped)`.

Mapper `WireError`s propagate out of the slot without any HTTP call (the
reserved sequence becomes a legal gap). A `reserve()` failure consumes
nothing.

### 9.3 `performWithSingleRetry` — encode-once, exactly-one-retry

- Encode ONCE: `encodedBody = http.encodeBody(mapped.body)`. Every attempt
  passes this same byte buffer; a retry resends byte-identical content with
  the same `sequence` and `requestId` (idempotent resend). No re-mapping, no
  new UUID, no new sequence.
- Each attempt = `http.execute({ method, path, encodedBody, credential,
  responseSchema: mutationResponseSchema, expectedRequestId: mapped.requestId,
  maximumPayloadBytes: configuration.maximumPayloadBytes })`.
- Control flow:
  1. Attempt 1. Success → `await sequencer.reconcile(response.data.acceptedSequence)`
     → return response.
  2. On error: FIRST `reconcileFromError(error)` — if
     `acceptedSequenceOf(error)` is non-null, `await sequencer.reconcile(it)`
     (this may advance the store even though a subsequent retry still
     resends the ORIGINAL sequence bytes).
  3. If `!isSafeForImmediateIdempotentRetry(error)` → rethrow the original
     error (total attempts: 1).
  4. Attempt 2 (the only retry). Success → reconcile acceptedSequence →
     return.
  5. On retry error: `reconcileFromError(retryError)` → rethrow retryError
     (total attempts: 2, NEVER more).
- Ordering guarantee: reconcile-on-success and reconcile-from-error are
  awaited inside the slot, so the next queued mutation reserves AFTER the
  reconciliation of the previous one has been persisted.

### 9.4 Semantics summary

- Reservation before send: crash after reserve → gap, never reuse.
- Exactly-once-retry rule: at most 2 wire attempts per mutation, and only
  when the failure classifies retry-safe (section 7.2).
- Reconcile sources: `response.data.acceptedSequence` on success;
  `error.envelope.error.acceptedSequence` on `CompanionServerError` (both
  attempts). Reconcile from an error happens BEFORE the retry decision.
- The payload cap uses the NEGOTIATED `maximumPayloadBytes` (not the 32768
  default) for both mutations; `CompanionPayloadTooLargeError` is
  non-retryable and thrown before any bytes hit the network.

---

## 10. `test/helpers/mockServer.ts` — mock server contract (port to `tests/support/mock_server.rs`)

Plain HTTP/1.1 server on `127.0.0.1`, ephemeral port;
`baseUrl = "http://127.0.0.1:<port>"` (loopback HTTP is allowed by the
client's base-URL rules).

Behavior:

- Records EVERY request (in arrival order) as
  `{ method, path, headers (lowercased names), rawBody (utf8 string), json (parsed body or null) }`.
- Handler selection: a FIFO queue of one-shot handlers (`enqueue`), consumed
  in order; when empty, a `fallback` handler (`setFallback`); when neither
  exists → respond `500` with body `{"unexpected":true}`.
- A handler returns either `{ status, body }` (responded with
  `Content-Type: application/json` and `JSON.stringify(body)`) or
  `{ socketDestroy: true }` (destroy the TCP socket without responding —
  the client observes this as a network failure).

Response builders (exact literals):

- `NOW = "2026-07-26T04:00:12.345Z"` (used for `serverTime` and
  `receivedAt`).
- `responseMeta(requestId)` =
  `{ schema: "yohaku.companion.presence", schemaVersion: 2, requestId, serverTime: NOW }`.
- `mutationSuccess(req, acceptedSequence?)` → status `200`, body
  `{ meta: responseMeta(<echo of req.json.meta.requestId, else a random UUID>),
  data: { acceptedSequence: <acceptedSequence ?? req.json.meta.sequence ?? 0>,
  receivedAt: NOW, state: { schemaVersion: 2,
  epoch: "01ARZ3NDEKTSV4RRFFQ69G5FAV", revision: 1, projection: null } } }`.
- `errorEnvelope(req, status, code, { retryable?, acceptedSequence? })` →
  given status, body `{ meta: responseMeta(<echo requestId or random UUID>),
  error: { code, message: "<code> for testing", retryable: <retryable ?? false>,
  retryAfterMs: null, acceptedSequence: <acceptedSequence ?? null>,
  fields: [] } }`.
- `capabilitiesResponse(patch)` → status `200`, body
  `{ meta: responseMeta(<random UUID>), data: {
  minimumClientVersion: patch.minimumClientVersion ?? "1.7.0",
  presenceSchemaVersions: patch.presenceSchemaVersions ?? [2],
  momentSchemaVersions: [1],
  features: { liveDesk: patch.liveDesk ?? true,
  mediaTimeline: patch.mediaTimeline ?? true, moments: true,
  readingSessions: false },
  limits: { presencePayloadBytes: 32768,
  presenceRequestsPerMinute: patch.requestsPerMinute ?? 120,
  presenceLeaseMinSeconds: 30, presenceLeaseMaxSeconds: 120,
  recommendedHeartbeatSeconds: 45, maximumClockSkewSeconds: 60 } } }`.

---

## 11. Test vectors (port 1:1 to cargo tests, keep literal values)

### 11.1 `test/companion/wire.test.ts` → `tests/wire.rs`

wire dates:

1. "encodes epoch ms as RFC3339 with exactly 3 fractional digits + Z":
   `encodeWireDate(1_753_500_012_345)` matches
   `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$` (actual value
   `"2025-07-26T03:20:12.345Z"`); `encodeWireDate(0) === "1970-01-01T00:00:00.000Z"`.
2. "round-trips encode -> decode":
   `decodeWireDate(encodeWireDate(1_753_500_012_345)) === 1_753_500_012_345`.
3. "rejects non-canonical date ..." — each of these throws `WireError`:
   - `"2026-07-26T09:41:12Z"` (no milliseconds)
   - `"2026-07-26T09:41:12.34Z"` (2 digits)
   - `"2026-07-26T09:41:12.345678Z"` (6 digits)
   - `"2026-07-26T09:41:12.345+00:00"` (offset form)
   - `"2026-07-26 09:41:12.345Z"` (space separator)
   - `"2026-13-26T09:41:12.345Z"` (invalid month)

wire identifiers:

4. "accepts UUIDs (any case) and Crockford ULIDs" — true for:
   `"3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b"`,
   `"3F6F6C0A-58A8-4A9D-B0A8-1C2D3E4F5A6B"`,
   `"01ARZ3NDEKTSV4RRFFQ69G5FAV"`.
5. "rejects ULIDs containing I/L/O/U, lowercase, wrong length" — false for:
   `"01ARZ3NDEKTSV4RRFFQ69G5FAI"` (contains `I`),
   `"01arz3ndektsv4rrffq69g5fav"` (lowercase),
   `"01ARZ3NDEKTSV4RRFFQ69G5FA"` (25 chars),
   `"not-an-id"`.

wire integers:

6. "accepts 0..2^53-1": `requireWireInteger(0, "x") === 0`;
   `requireWireInteger(9_007_199_254_740_991, "x") === 9_007_199_254_740_991`.
7. "rejects negatives, floats, and beyond-safe values" — `WireError` for:
   `-1`, `1.5`, `9_007_199_254_740_992`.

secondsToWireMilliseconds:

8. "rounds to integer milliseconds":
   `secondsToWireMilliseconds(1.2345, "x") === 1235`;
   `secondsToWireMilliseconds(0, "x") === 0`.
9. "rejects negative and non-finite" — `WireError` for: `-0.001`, `NaN`,
   `+Infinity`.

### 11.2 `test/companion/sequencer.test.ts` → `tests/sequencer.rs`

Fixture: in-memory persistence (map + op log `{op: "load"|"store", value?}`,
plus a `failNextStore` flag that makes exactly the next `store` throw
`"disk full"` without recording/writing).
`DEVICE = "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b"`.

1. "starts from pairingNextSequence and persists BEFORE returning":
   base 5; `reserve() == 5`; persisted value for DEVICE is `6`; the LAST log
   entry is `{op: "store", value: 6}` (store happened before reserve
   resolved).
2. "is monotonic across reserves": base 0; reserves yield `0, 1, 2`.
3. "a crash after persist produces a legal gap, never reuse": sequencer A
   (base 0) reserves `0` (persisting 1); a NEW sequencer over the same
   persistence (base 0) reserves `1` — gap at 0 is fine, no reuse.
4. "failed persistence fails the reserve (no sequence handed out)":
   `failNextStore = true`; `reserve()` rejects with message `"disk full"`;
   the next `reserve() == 0` — nothing was consumed.
5. "reconcile advances next to accepted+1, never backwards": base 0;
   `reconcile(41)` then `reserve() == 42`; `reconcile(10)` (behind current)
   is ignored; `reserve() == 43`.
6. "invalid stored values are ignored (self-heal from pairing base)":
   stored value `-7`, base 3 → `reserve() == 3`.
7. "pairingNextSequence acts as a floor over stale storage": stored `2`,
   base 10 → `reserve() == 10`.
8. "throws when the sequence space is exhausted": stored
   `9_007_199_254_740_991`, base 0 → `reserve()` rejects with
   `SequenceExhaustedError`.
9. "serializes concurrent reserves (unique, gapless when no crash)": 20
   concurrent `reserve()` calls → 20 UNIQUE values; sorted ascending they
   equal `[0, 1, ..., 19]`.

### 11.3 `test/companion/dtoMapper.test.ts` → `tests/dto_mapper.rs`

Fixtures:

```
OPTS = { deviceId: "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b",
         leaseMinSeconds: 30, leaseMaxSeconds: 120 }

snapshot() = {
  observedAt: 1_753_500_012_345,
  application: { displayName: "Code", windowTitle: "file.ts" },
  media: {
    sessionId: "aa6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b",
    kind: "music", title: "Song", artist: "Artist", album: null,
    playerDisplayName: "Spotify",
    playback: { state: "playing", durationSeconds: 200.5,
                positionSeconds: 60.2504, sampledAt: 1_753_500_012_345,
                rate: 1 }
  }
}
```

makePresenceRequest — wire shape:

1. "serializes every required-nullable key explicitly"
   (`makePresenceRequest(snapshot(), 7, 90, OPTS)`, assertions on the
   JSON-serialized body):
   - `data` owns keys `availability`, `lease`, `application`, `media`;
   - `application` owns keys `displayName`, `activity`, `window`, `icon`,
     with `activity == null` and `icon == null`;
   - `media` owns keys `sessionId`, `kind`, `title`, `artist`, `album`,
     `player`, `playback`, with `album == null`;
   - `playback` owns keys `state`, `durationMs`, `positionMs`, `sampledAt`,
     `rate`.
2. "omits capability-conditional artwork/link keys entirely"
   (`sequence 1, lease 90`): serialized `media` has NO `artwork` key and NO
   `link` key.
3. "null application/media keys still present; availability idle"
   (snapshot with `application: null, media: null`, sequence 1, lease 90):
   `data.availability == "idle"`; `application` key PRESENT with value
   `null`; `media` key PRESENT with value `null`.
4. "availability active when either source present"
   (snapshot with `media: null` only): `data.availability == "active"`.
5. "meta carries schema constants, deviceId, sequence, canonical observedAt"
   (`sequence 42, lease 90`): `meta.schema == "yohaku.companion.presence"`;
   `meta.schemaVersion == 2`;
   `meta.deviceId == "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b"`;
   `meta.sequence == 42`; `meta.requestId` equals the returned `requestId`;
   `meta.observedAt == "2025-07-26T03:20:12.345Z"`.

makePresenceRequest — conversions and limits:

6. "converts seconds to rounded milliseconds and clamps position":
   `playback.durationMs == 200_500`; `playback.positionMs == 60_250`.
7. "keeps null duration/position as null (never 0)": with
   `durationSeconds = null` and `positionSeconds = null` →
   `durationMs == null`, `positionMs == null`.
8. "truncates over-limit text at Unicode scalar boundaries": application
   `displayName` = `"𝒜"` (U+1D49C, a surrogate pair in UTF-16) repeated 200
   times; `windowTitle` = `"t"` repeated 600 times → mapped `displayName`
   has EXACTLY 120 Unicode scalars; `window.title` has EXACTLY 500 scalars.
9. "clamps lease ttl into the negotiated range": requested `5` → `ttlSeconds
   30`; requested `999` → `120`; requested `90` → `90`.
10. "rejects paused-with-rate and playing-without-rate": snapshot with
    `state: "paused", rate: 1` → throws `WireError`; snapshot with
    `state: "playing", rate: 0` → throws `WireError`.
11. "rejects media without title and artist": `title = null` AND
    `artist = null` → throws `WireError`.

makeClearRequest:

12. "carries reason and full meta":
    `makeClearRequest("sleep", 9, 1_753_500_012_345, OPTS)` →
    `data.reason == "sleep"`; `meta.sequence == 9`;
    `meta.schema == "yohaku.companion.presence"`.

### 11.4 `test/companion/capabilities.test.ts` → `tests/capabilities.rs`

Fixture `capabilities(patch)` (defaults, patch shallow-merged):

```
{ minimumClientVersion: "1.7.0",
  presenceSchemaVersions: [2], momentSchemaVersions: [1],
  features: { liveDesk: true, mediaTimeline: true, moments: true,
              readingSessions: false },
  limits: { presencePayloadBytes: 32768, presenceRequestsPerMinute: 30,
            presenceLeaseMinSeconds: 30, presenceLeaseMaxSeconds: 120,
            recommendedHeartbeatSeconds: 45, maximumClockSkewSeconds: 60 } }
```

parseSemanticVersion:

1. "parses core, prerelease and build":
   `parseSemanticVersion("1.8.3") == { major: 1, minor: 8, patch: 3, prerelease: [] }`;
   `parseSemanticVersion("1.0.0-alpha.1+build.5").prerelease == ["alpha", "1"]`.
2. "rejects ..." — null for each of: `"1.8"`, `"1.8.3.4"`, `"01.0.0"`,
   `"1.0.0-alpha.01"`, `"1.0.0-"`, `"1.0.0+"`, `"v1.0.0"`, `"1.0.x"`.

compareSemanticVersions:

3. "orders the canonical semver example chain" — for each ADJACENT pair in
   `["1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta",
   "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0", "1.0.1",
   "1.1.0", "2.0.0"]`: `compare(a, b) < 0` AND `compare(b, a) > 0`.
4. "ignores build metadata": `compare("1.0.0+a", "1.0.0+b") == 0`.

negotiatePresence (client version `"1.8.3"` unless noted):

5. "returns available with the negotiated configuration" — defaults →
   exactly `{ kind: "available", configuration: { supportsMediaTimeline:
   true, maximumPayloadBytes: 32768, requestsPerMinute: 30, leaseMinSeconds:
   30, leaseMaxSeconds: 120, recommendedHeartbeatSeconds: 45,
   maximumClockSkewSeconds: 60 } }`.
6. "clientUpdateRequired when below minimumClientVersion":
   `minimumClientVersion: "2.0.0"` → kind `"clientUpdateRequired"`.
7. "schemaUnsupported when v2 missing": `presenceSchemaVersions: [3]` →
   kind `"schemaUnsupported"`.
8. "featureUnavailable when liveDesk off": features
   `{ liveDesk: false, mediaTimeline: true, moments: true, readingSessions:
   false }` → kind `"featureUnavailable"`.
9. "invalidCapabilities: <name>" — each single-field limits patch (over the
   defaults) yields kind `"invalidCapabilities"`:
   - "zero payload": `presencePayloadBytes: 0`
   - "zero rpm": `presenceRequestsPerMinute: 0`
   - "zero lease min": `presenceLeaseMinSeconds: 0`
   - "lease min > max": `presenceLeaseMinSeconds: 200`
   - "heartbeat below lease min": `recommendedHeartbeatSeconds: 10`
   - "heartbeat above lease max": `recommendedHeartbeatSeconds: 500`
   - "negative skew": `maximumClockSkewSeconds: -1`
10. "invalidCapabilities on unparseable versions or non-positive schema
    versions": `minimumClientVersion: "1.7"` → invalid; client version
    `"not-a-version"` (defaults otherwise) → invalid;
    `presenceSchemaVersions: [0, 2]` → invalid.

### 11.5 `test/companion/presenceClient.test.ts` → `tests/presence_client.rs`

Fixture (fresh per test): mock server (section 10) started on loopback HTTP;
in-memory sequence persistence; sequencer
`CompanionSequencer(persistence, DEVICE, 5)` with
`DEVICE = "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b"`; credential
`{ deviceId: DEVICE, deviceToken: "secret-token" }`; configuration
`CONFIG = { supportsMediaTimeline: true, maximumPayloadBytes: 32768,
requestsPerMinute: 120, leaseMinSeconds: 30, leaseMaxSeconds: 120,
recommendedHeartbeatSeconds: 45, maximumClockSkewSeconds: 60 }`; snapshot
`{ observedAt: 1_753_500_012_345, application: { displayName: "Code",
windowTitle: null }, media: null }`; every `replacePresence` uses
`requestedLeaseSeconds = 90`.

1. "PUTs to /companion/presence with Bearer token and version header":
   fallback = mutationSuccess. After one `replacePresence`: request 0 has
   `method == "PUT"`, `path == "/companion/presence"`, header
   `authorization == "Bearer secret-token"`, header
   `x-yohaku-companion-version == "1.8.3"`, body `meta.sequence == 5`.
2. "retries EXACTLY once with byte-identical body on retryable 5xx":
   enqueue `errorEnvelope(500, "INTERNAL_ERROR", { retryable: true })`;
   fallback mutationSuccess. `replacePresence` succeeds; server saw EXACTLY
   2 requests; `requests[1].rawBody === requests[0].rawBody` (byte-equal);
   both bodies have the SAME `meta.sequence` and SAME `meta.requestId` (no
   new sequence allocated for the retry).
3. "retries once on ambiguous transport failure (socket destroyed)":
   enqueue `{ socketDestroy: true }`; fallback mutationSuccess. Succeeds; 2
   requests; identical `rawBody`.
4. "does not retry non-retryable 5xx": enqueue
   `errorEnvelope(500, "INTERNAL_ERROR", { retryable: false })`.
   `replacePresence` rejects with `CompanionServerError`; server saw exactly
   1 request.
5. "does not retry 4xx and surfaces the envelope": enqueue
   `errorEnvelope(422, "VALIDATION_FAILED")`. Rejects with
   `CompanionServerError`; 1 request.
6. "fails on two consecutive failures (no second retry)": enqueue
   `socketDestroy` twice. Rejects (with the second network error); server
   saw exactly 2 requests.
7. "reconciles acceptedSequence from success responses": enqueue
   `mutationSuccess(req, acceptedSequence = 100)`; fallback mutationSuccess.
   Two sequential `replacePresence` calls; the SECOND request's
   `meta.sequence == 101`.
8. "reconciles acceptedSequence from error envelopes": enqueue
   `errorEnvelope(409, "COMPANION_SEQUENCE_BEHIND", { acceptedSequence: 200 })`;
   fallback mutationSuccess. First `replacePresence` rejects (409 is not
   retried); second `replacePresence` succeeds and its request has
   `meta.sequence == 201`.
9. "rejects a response that does not echo the requestId, then retries":
   enqueue a mutationSuccess whose `meta.requestId` is overwritten with
   `"00000000-0000-4000-8000-000000000000"`; fallback mutationSuccess.
   `replacePresence` succeeds; server saw 2 requests (first response was
   discarded as `CompanionRequestIdMismatchError`, which is retry-safe).
10. "clearPresence consumes a sequence and posts the reason": fallback
    mutationSuccess. `replacePresence` then
    `clearPresence("sleep", 1_753_500_012_345)`: request 1 has
    `path == "/companion/presence/clear"`, body `data.reason == "sleep"`,
    `meta.sequence == 6`.
11. "serializes concurrent mutations through the send slot": fallback
    mutationSuccess. Issue concurrently: `replacePresence`,
    `replacePresence`, `clearPresence("paused", 1_753_500_012_345)`. The
    observed request `meta.sequence` values, in arrival order, are exactly
    `[5, 6, 7]`.
12. "classifies schema rejection and bare 426 as renegotiation signals":
    - enqueue `errorEnvelope(409, "COMPANION_SCHEMA_UNSUPPORTED")` → the
      caught error satisfies `needsRenegotiation == true`;
    - enqueue raw `{ status: 426, body: { upgrade: "required" } }` (JSON
      body that is NOT a protocol envelope → `CompanionHttpStatusError(426)`,
      not retried) → `needsRenegotiation == true`;
    - enqueue `errorEnvelope(422, "VALIDATION_FAILED")` →
      `needsRenegotiation == false`.

---

## 12. Traps for the Rust port (JS-specific behavior to reproduce)

1. **JSON number formatting (byte identity).** `JSON.stringify` emits
   ECMAScript shortest-form numbers: integral values WITHOUT a decimal point
   (`1`, not `1.0`), non-integral shortest round-trip (`1.5`), and `-0` as
   `0`. serde_json serializes `f64 1.0` as `1.0` — a byte difference on
   `playback.rate`. Represent `rate` so integral values serialize without a
   fraction (e.g. custom Serialize: emit as integer when `rate.fract() == 0`,
   else shortest f64 — ryu matches JS for the non-integral case). All other
   numeric wire fields (`schemaVersion`, `sequence`, `ttlSeconds`,
   `durationMs`, `positionMs`, capability limits) are integers — use integer
   types.
2. **JSON key order and omission.** `JSON.stringify` writes keys in object
   insertion order and drops only `undefined`. Rust structs must declare
   fields in the exact orders of section 1.5 / 3.2, serialize `None` for
   required-nullable keys as explicit `null`, and OMIT `artwork`/`link`
   entirely. Compact output (no spaces).
3. **Required-nullable decoding.** serde's default `Option<T>` treats a
   MISSING key as `None`. The protocol distinguishes missing (error) from
   `null` (legal) for: request-side all keys of section 3.2;
   response-side `error.retryAfterMs`, `error.acceptedSequence`,
   `state.projection`. Use a presence-enforcing pattern (e.g.
   `deserialize_with` that errors on missing, or double-Option +
   post-validation).
4. **Unknown response keys.** zod strips unknown keys at every level: the
   Rust decoder must IGNORE unknown fields (default serde behavior; do not
   add `deny_unknown_fields`).
5. **`Number.isInteger` / float-typed integers.** JSON.parse yields f64, so
   zod accepts `3.0` where an int is required and integer bounds are the
   SAFE range (±2^53−1); `.int()` in zod v4 rejects 2^53. Rust `u64`
   deserialization rejects `3.0` (stricter — see ambiguity 13.9) and
   accepts values up to 2^64−1 (LOOSER — explicitly enforce
   `<= 9_007_199_254_740_991` on every wireInteger field: `acceptedSequence`,
   `revision`, `nextSequence`, `retryAfterMs`).
6. **`Math.round`** rounds half toward +infinity; `f64::round` rounds half
   away from zero. Identical for the non-negative inputs used here
   (`secondsToWireMilliseconds` rejects negatives first; lease seconds are
   positive). Do NOT use banker's rounding.
7. **String semantics.** `[...str]` iterates Unicode scalar values — use
   `char` iteration (`chars().take(limit)`), never UTF-16 units or grapheme
   clusters. JS `trim()` removes the WhiteSpace+LineTerminator set including
   U+00A0 NBSP and U+FEFF; Rust `str::trim` (White_Space property) includes
   U+00A0 but NOT U+FEFF — add U+FEFF handling if byte parity on exotic
   input matters.
8. **`Date` handling.** Encode: exactly `YYYY-MM-DDTHH:MM:SS.mmmZ`, 4-digit
   year 0000–9999, error outside; negative epoch ms (pre-1970) is legal.
   Decode: strict format AND real calendar instant AND canonical re-encode
   equality (net rules in section 2.2 — chrono strict parse + reformat +
   compare is sufficient). JS `Date.parse` leniency (day overflow → next
   month, hour 24 → next day) is fully neutralized by the round-trip check,
   so a strict parser is behavior-equivalent.
9. **UUID generation.** `crypto.randomUUID()` = RFC 4122 v4, lowercase,
   hyphenated — `uuid::Uuid::new_v4()` default formatting matches.
10. **Promise-chain FIFO mutexes.** Both the sequencer queue and the
    presence send slot are strict FIFO critical sections; a failed task must
    not poison the queue. The presence slot spans
    reserve → map → guard → send(+retry) → reconcile as ONE critical
    section; the sequencer additionally serializes its own reserve/reconcile
    internally. Tokio's `Mutex` is FIFO-fair and suits both.
11. **`structuredClone`** is not used anywhere in this slice; there is no
    hidden copying. The retry MUST reuse the identical encoded byte buffer
    (encode once, resend same bytes).
12. **Timeout surface.** `AbortSignal.timeout(10_000)` covers the whole
    request; a timeout is indistinguishable from any other network failure
    (`CompanionNetworkError` → retry-safe). `reqwest`'s per-request
    `.timeout(10s)` mapped to the network-error variant is equivalent.
13. **Header handling.** Send headers exactly as spelled in 1.3; matching is
    case-insensitive on the wire (the vitest mock asserts lowercased names).
14. **Error messages.** Logic depends only on error TYPES plus
    `status`/`code`/`envelope` fields. Message strings need parity only
    where tests assert them (`"disk full"` passthrough; error kinds
    elsewhere).

---

## 13. Ambiguities found in the TS source (flag for the parity reviewer)

1. `encodeWireDate` with a FINITE `epochMs` whose magnitude exceeds
   8_640_000_000_000_000 throws a native `RangeError` from
   `Date.toISOString()`, not a `WireError`. Rust should map this to
   `WireError` (documented deviation; unreachable from production callers,
   whose timestamps are current-epoch ms).
2. `encodeWireDate` truncates fractional epoch ms toward zero (JS `Date`
   TimeClip) before formatting. Moot for the Rust `i64` signature.
3. `mapMedia` passes `sessionId` and `kind` through VERBATIM — no UUID or
   enum re-validation at the mapper boundary (upstream sanitizer guarantees
   them). `makeMeta` likewise does not range-check `sequence` or re-validate
   `deviceId`.
4. `makePresenceRequest` computes
   `Math.round(requestedLeaseSeconds)` without a finiteness check: a NaN
   lease would propagate NaN into `ttlSeconds` and `JSON.stringify` would
   emit `null`. Unreachable (callers pass validated numbers); Rust integer
   types close the hole.
5. `PresenceClient.assertDeviceMatches` compares
   `mapped.body.meta.deviceId` against `credential.deviceId`, but the mapper
   received `deviceId` FROM the credential — the guard can only fire on
   internal wiring bugs. When it fires, the reserved sequence is consumed
   (legal gap). It is also unreachable-by-construction dead logic worth
   keeping as defense-in-depth.
6. Empty response bodies raise `CompanionEmptyResponseError` regardless of
   status — a hypothetical 204 success would be treated as a retry-safe
   failure. The protocol never uses 204.
7. There is NO response-size cap and no response `Content-Type` check;
   adding either in Rust would be a behavioral deviation (recommend
   matching TS: none).
8. Required-nullable enforcement for `state.projection` depends on zod v4
   semantics (`z.unknown().nullable()` rejects a missing key in v4.4.3,
   verified; zod v3 would NOT have enforced presence). Spec follows the
   shipped v4 behavior: `projection` key is required.
9. zod accepts float-typed integral JSON numbers (`3.0`) for int fields and
   silently loses precision above 2^53 during `JSON.parse` before
   validation; serde_json integer deserialization rejects `3.0` and handles
   big integers exactly. Divergence only on non-canonical server output;
   stricter Rust decoding is the recommended (flagged) deviation, with the
   explicit `<= 2^53−1` max retained.
10. `capabilitiesDataSchema` allows NEGATIVE integers at decode time for
    schema-version arrays and limits; positivity is enforced only in
    `negotiatePresence` (and `momentSchemaVersions` positivity is enforced
    even though this client never uses moments).
11. `endpoint()` strips at most ONE trailing slash from the base path: a
    base URL whose path ends in `//` yields a double slash in the joined
    path. Unreachable through `CompanionServerConfiguration` normal usage
    but technically representable.
12. `isLoopbackHost` accepts leading-zero IPv4 octets (`"127.000.000.001"`)
    and rejects `"127.1"` shorthand and non-`::1` IPv6 loopback spellings —
    replicate exactly.
13. `error.retryAfterMs` is decoded and required-nullable but UNUSED by any
    logic in this slice (no backoff consumes it).
14. `requireKey` in wire.ts is exported but unused in the covered code;
    porting is optional.
15. In `execute`, the requestId echo check for ERROR envelopes runs BEFORE
    `CompanionServerError` is thrown, so a non-2xx envelope with a wrong
    `meta.requestId` surfaces as retry-safe `CompanionRequestIdMismatchError`
    and its `acceptedSequence` is NOT reconciled. Intentional-looking but
    subtle; preserve exactly.





