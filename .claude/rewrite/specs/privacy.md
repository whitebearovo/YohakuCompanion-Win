# Privacy Pipeline — Behavior Specification

Audience: the Rust implementer of `core/src/privacy/*` (agent V per
`.claude/rewrite/ARCHITECTURE.md`) and the parity reviewer. This document is a
transcription of the TypeScript ground truth; where it and the TS source ever
disagree, the TS source wins.

Ground-truth files (all paths relative to repo root):

- `packages/core/src/privacy/types.ts` — sanitized domain types
- `packages/core/src/privacy/model.ts` — normalization, rules, mappings
- `packages/core/src/privacy/evaluator.ts` — decision engine
- `packages/core/src/privacy/sanitize.ts` — sanitization boundary
- `packages/core/src/privacy/fingerprint.ts` — policy fingerprint
- `packages/core/src/privacy/captureService.ts` — capture pipeline
- `packages/core/src/privacy/mediaSessionTracker.ts` — session identity
- `packages/shared/src/privacy.ts` — config schema (reference only)
- `packages/shared/src/status.ts` — `PreviewProjection` / `Preview` schema
- `packages/core/src/capture/types.ts` — raw capture types (`ForegroundInfo`,
  `MediaSnapshot`) consumed by this pipeline
- `packages/core/src/companion/consentGate.ts` — `projectionOf()` (section 9;
  the gate itself is agent K's scope)
- Tests: `packages/core/test/privacy/{model,evaluator,sanitize,fingerprint,mediaSessionTracker}.test.ts`

Verification performed while writing this spec (2026-09-23, Node v24.15.0):

- `vitest run test/privacy` — all 52 tests pass (44 in the five files listed
  above + 8 in `consentGate.test.ts`, which is out of this spec's scope).
- The golden fingerprint vectors in section 6.6 were computed by executing the
  real `policyFingerprint()` from `fingerprint.ts` via `tsx`. The canonical
  JSON preimages shown there were verified by re-hashing them with SHA-256 and
  comparing against the real function's output (all matched).
- The mapping-sort separator bytes were verified with a hex dump of
  `fingerprint.ts` (see 6.3).

## 0. Dataflow overview

```
ForegroundWatcher ──► ForegroundInfo ─┐
                                      │   CaptureService.captureForDelivery
MediaProvider ──► MediaSnapshot ──────┤   ┌──────────────────────────────────┐
                                      └──►│ gates (sources.*, playing)       │
PrivacyConfig (re-read at each step) ────►│ evaluator (process/mediaDecision)│
                                          │ mappings (display names only)    │
                                          │ sanitize (normalize, clamp)      │
                                          │ MediaSessionTracker (sessionId)  │
                                          └──────────────┬───────────────────┘
                                                         ▼
                                           SanitizedPresenceSnapshot
                                            ├─► projectionOf() ─► PreviewProjection (consent UI)
                                            └─► dtoMapper ─► wire request (protocol spec)
```

Security invariant (from `types.ts` header comment): the sanitized types are
the ONLY shapes allowed to flow toward the network, the preview UI, or
persistence. By construction they have no field for executable paths, appIds,
process IDs, or raw capture objects. Raw capture types exist only between the
capture layer and this pipeline.

## 1. Type glossary

All "string" fields are JS strings (UTF-16, potentially ill-formed in theory;
in practice well-formed). "epoch ms" means milliseconds since the Unix epoch
as produced by `Date.now()` (an integer-valued IEEE-754 double; use `i64`/`u64`
in Rust). "seconds" values are IEEE-754 doubles and may be fractional.

### 1.1 Raw capture types (`packages/core/src/capture/types.ts`)

These must never reach the network, persistence, or the UI without passing the
sanitizers.

`ForegroundInfo`:

| field | type | semantics |
| --- | --- | --- |
| `appId` | `string` | Lowercased executable file name, e.g. `"code.exe"`. Produced lowercase by the foreground watcher. |
| `exePath` | `string \| null` | Full executable path. Never consumed by the privacy pipeline; must never leave the process. |
| `displayName` | `string` | Friendly name (PE `FileDescription`) or exe-stem fallback (stem with first letter uppercased, `.exe` stripped). |
| `windowTitle` | `string \| null` | Raw foreground window title. |

`MediaSnapshot`:

| field | type | semantics |
| --- | --- | --- |
| `appId` | `string \| null` | Lowercased exe name when the SMTC `SourceAppUserModelId` matches `/\.exe$/i` (then `toLowerCase()`), else `null`. |
| `sourceAppUserModelId` | `string \| null` | Trimmed SMTC source id; `null` when empty. |
| `playerDisplayName` | `string \| null` | Exe-stem display name when `appId` is non-null, else a best-effort name derived from the AUMID; `null` when empty. |
| `kind` | `MediaKind` | `"music" \| "podcast" \| "video" \| "unknown"`. |
| `title` | `string \| null` | Raw title; provider trims and maps empty to `null`. |
| `artist` | `string \| null` | Raw artist; same trimming. |
| `album` | `string \| null` | Raw album; same trimming. |
| `playing` | `boolean` | `true` iff SMTC playback status is Playing. |
| `durationSeconds` | `number \| null` | Float seconds; providers emit `null` unless `> 0`. |
| `positionSeconds` | `number \| null` | Float seconds; providers extrapolate while playing (section 7.5). |
| `sampledAt` | `number` | Epoch ms at which `positionSeconds` was (re)computed — always `Date.now()` at snapshot build time in both providers. |

`MediaProvider` (the part the privacy pipeline uses):
`getSnapshot(options?: { timeoutMs?: number }): Promise<MediaSnapshot | null>`
— bounded fresh lookup; `null` when nothing plays. NOTE: `CaptureService`
calls `getSnapshot()` with NO arguments and applies its own outer 2000 ms
race (section 8.4); the `timeoutMs` option is unused by this pipeline.

### 1.2 Sanitized types (`packages/core/src/privacy/types.ts`)

```ts
type MediaKind = "music" | "podcast" | "video" | "unknown";
type PlaybackState = "playing" | "paused";
```

`SanitizedApplicationPresence`:

| field | type | semantics |
| --- | --- | --- |
| `displayName` | `string` | Non-empty by construction (NFC + trim, empty rejected — the whole application becomes `null` instead). |
| `windowTitle` | `string \| null` | NFC + trim; `null` unless all three title switches allow (section 5.1). |

`SanitizedPlayback`:

| field | type | semantics |
| --- | --- | --- |
| `state` | `PlaybackState` | `"playing"` iff input `playing === true`, else `"paused"`. |
| `durationSeconds` | `number \| null` | `null`, or a finite value `>= 0` (may be fractional). |
| `positionSeconds` | `number \| null` | `null`, or finite `>= 0`; clamped to `durationSeconds` when duration is non-null. `0` is meaningful ("at start"); `null` means "unavailable". |
| `sampledAt` | `number` | Epoch ms at which position was sampled. Passed through unvalidated from `MediaSnapshot.sampledAt`. |
| `rate` | `number` | Derived: `playing ? 1 : 0`. SMTC exposes no reliable rate (comment in `sanitize.ts`). |

`SanitizedMediaPresence`:

| field | type | semantics |
| --- | --- | --- |
| `sessionId` | `string` | Stable per-semantic-session UUID v4, lowercase hyphenated. Random — never derived from content (section 7). |
| `kind` | `MediaKind` | Preserved verbatim from `MediaSnapshot.kind`; no transformation. |
| `title` | `string \| null` | NFC + trim, empty to `null`. |
| `artist` | `string \| null` | NFC + trim, empty to `null`. Invariant: `title` and `artist` are never both `null` (dropped instead). |
| `album` | `string \| null` | NFC + trim, empty to `null`. |
| `playerDisplayName` | `string \| null` | Alias > mapping > raw precedence (section 5.2); may be `null`. |
| `playback` | `SanitizedPlayback` | See above. |

`SanitizedPresenceSnapshot`:

| field | type | semantics |
| --- | --- | --- |
| `observedAt` | `number` | Epoch ms captured at the START of `captureForDelivery` (before the media await). |
| `application` | `SanitizedApplicationPresence \| null` | `null` when source off, no foreground info, hidden, or blank display name. |
| `media` | `SanitizedMediaPresence \| null` | `null` when source off, no/paused media, hidden, artist policy, or no meaningful text. |

### 1.3 Privacy configuration (`packages/shared/src/privacy.ts` — reference)

Do not re-derive; the zod schema is authoritative. Shapes:

- `PrivacyDefault = "share" | "hide"`
- `PrivacyOverride = "inherit" | "share" | "hide"`
- `ApplicationPrivacyRule = { appId: string (min length 1), application: PrivacyOverride, windowTitle: PrivacyOverride, media: PrivacyOverride, displayAlias?: string }`
  — `displayAlias` is an OPTIONAL KEY (absent vs present, never `null`).
- `PrivacyMapping = { type: "process_name" | "media_process_name" | "media_player_name", from: string (min 1), to: string (min 1) }`
  — `media_process_name` is kept valid only for configs written by older
  releases (schema comment).
- `PrivacyConfig = { defaults: { application, windowTitle, media: PrivacyDefault }, rules: ApplicationPrivacyRule[], mappings: PrivacyMapping[], shareWindowTitles: boolean, ignoreNullArtist: boolean, sources: { application: boolean, media: boolean } }`

`defaultPrivacyConfig()` (new-installation defaults; window titles private
unless opted in):

```json
{
  "defaults": { "application": "share", "windowTitle": "hide", "media": "share" },
  "rules": [],
  "mappings": [],
  "shareWindowTitles": false,
  "ignoreNullArtist": false,
  "sources": { "application": true, "media": true }
}
```

### 1.4 Preview types (`packages/shared/src/status.ts` — reference)

- `PreviewProjection = { application: { displayName: string, windowTitle: string | null } | null, media: { kind, title, artist, album, playerDisplayName, playback: { state, durationSeconds, rate } } | null }` — details and exclusions in section 9.
- `Preview = { projection: PreviewProjection, policyFingerprint: string, observedAt: number }` — the wrapper does carry `observedAt` (for display), but consent comparison uses only `projection` + fingerprint.

## 2. Text primitives and rule model (`privacy/model.ts`)

### 2.1 `normalizeText(value: string | null | undefined): string | null`

The single normalization primitive used across the pipeline:

1. `null` or `undefined` input returns `null`.
2. Otherwise: `value.normalize("NFC").trim()` — Unicode NFC first, then
   ECMAScript trim (exact whitespace set in section 11.1).
3. If the result has length 0, return `null`; else return it.

"Empty result means value not present" — `null`, never `""`, flows onward.
Mirrors macOS `precomposedStringWithCanonicalMapping` (source comment).

### 2.2 `scalarLength(value: string): number`

Counts Unicode scalar values (code points) via `for..of` iteration; a
surrogate pair counts as 1. Doc comment: "the unit all wire limits use".
NOTE: no caller exists anywhere in `packages/core/src` — the wire truncation
in `protocol/dtoMapper.ts` defines its own private `truncateScalars`. Dead
export; see ambiguity A1. Port it (it is part of `model.rs`'s contract), but
nothing in the privacy pipeline calls it.

### 2.3 `normalizeAppId(appId: string): string`

`appId.normalize("NFC").trim().toLowerCase()` — exactly this order: NFC, then
trim, then full-Unicode locale-independent lowercase. Never returns `null`;
may return `""`. Applied to BOTH sides of every rule and mapping lookup.

### 2.4 appId semantics

- An `appId` is the lowercased executable file name of a process, e.g.
  `"code.exe"` (Windows replacement for macOS bundle identifiers; schema
  header comment).
- Media sessions that cannot be attributed to an executable (`MediaSnapshot.appId === null`)
  are matched against rules by the player's NORMALIZED DISPLAY NAME instead
  (the "legacy hidden media names" fallback, Windows adaptation). The exact
  normalization for this fallback is `normalizeAppId` (2.3) applied to the
  player name — i.e. rule `appId` values may legitimately hold player display
  names like `"Spotify"`, and matching is NFC + trim + lowercase on both
  sides.

### 2.5 `resolveOverride(override: PrivacyOverride, fallback: PrivacyDefault): PrivacyDefault`

`override === "inherit" ? fallback : override`. Full table:

| override | fallback | result |
| --- | --- | --- |
| `inherit` | `share` | `share` |
| `inherit` | `hide` | `hide` |
| `share` | any | `share` |
| `hide` | any | `hide` |

### 2.6 `isEmptyRule(rule): boolean`

`true` iff `rule.application === "inherit"` AND `rule.windowTitle === "inherit"`
AND `rule.media === "inherit"` AND `normalizeText(rule.displayAlias) === null`.
(A rule with all-inherit overrides and no effective alias carries no
information; such rules are excluded from the fingerprint, section 6.)
Note the alias check uses `normalizeText`: a whitespace-only alias counts as
absent.

### 2.7 `normalizedRule(rule): ApplicationPrivacyRule`

Builds a new rule:

- `appId`: `normalizeAppId(rule.appId)`
- `application`, `windowTitle`, `media`: copied verbatim
- `displayAlias`: `normalizeText(rule.displayAlias)`; the key is INCLUDED only
  when the normalized alias is non-null, otherwise the key is OMITTED entirely
  (not set to `null`/`undefined`). Object literal insertion order: `appId`,
  `application`, `windowTitle`, `media`, then optionally `displayAlias` —
  irrelevant to the fingerprint (canonicalize re-sorts) but stated for
  completeness.

### 2.8 `findRule(config, appId): ApplicationPrivacyRule | null`

`key = normalizeAppId(appId)`; return the FIRST rule in `config.rules` array
order with `normalizeAppId(rule.appId) === key`, else `null`. First match
wins when duplicate normalized appIds exist. The returned rule is the
ORIGINAL config object (not normalized) — callers normalize `displayAlias`
themselves at decision time.

### 2.9 `findRuleByPlayerName(config, playerName): ApplicationPrivacyRule | null`

`key = normalizeAppId(playerName)`; if `key.length === 0` return `null`
(blank player names never match anything); else identical to `findRule`
(first match on normalized `appId` vs normalized player name).

### 2.10 Rule ordering and the precedence chain

- Lookup: first match in config array order (2.8/2.9).
- Fingerprint: rules are normalized, empty-filtered, and sorted (section 6) —
  config order does not affect the fingerprint.
- Per-dimension effective policy, for `d` in {application, windowTitle, media}:
  `effective(d) = resolveOverride(rule ? rule[d] : "inherit", defaults[d])`.
  A missing rule behaves exactly like a rule with all three set to `"inherit"`
  and no alias.
- Cross-dimension precedence is defined by the evaluator (section 4): within
  the application source, `application = hide` overrides everything (title
  and alias); the media dimension is fully independent of the application
  dimension.

## 3. Mappings (`applyMapping` in `privacy/model.ts` + call sites in `captureService.ts`)

### 3.1 Matching rules

`applyMapping(config, type, from): string | null`:

1. `key = normalizeAppId(from)`.
2. Scan `config.mappings` in array order; the FIRST entry `m` with
   `m.type === type` (exact string equality on the type tag, no
   normalization) AND `normalizeAppId(m.from) === key` wins.
3. Return `m.to` VERBATIM (no trim, no NFC, no case change) — normalization
   of the mapped value happens later inside the sanitizer via `normalizeText`.
   A whitespace-only `to` (schema allows `" "`) therefore normalizes to
   `null` in the sanitizer and falls through to the next precedence tier.
4. No hit: return `null`.

Properties: `from` matching is case-insensitive, trimmed, NFC (both sides);
types never cross (a `process_name` entry can never satisfy a
`media_process_name` lookup); first match wins; duplicates are allowed in
config.

### 3.2 Where each mapping type applies in the pipeline

| type | applied in | lookup key (`from` argument) | result destination |
| --- | --- | --- | --- |
| `process_name` | application branch of `captureForDelivery` | `ForegroundInfo.appId` (the exe name, NOT the captured display name) | `ApplicationSanitizeInput.mappedDisplayName` |
| `media_player_name` | media branch | `MediaSnapshot.playerDisplayName` (only when non-null) | `MediaSanitizeInput.mappedPlayerName` (first choice) |
| `media_process_name` (legacy) | media branch | `MediaSnapshot.playerDisplayName` (same key!) | `MediaSanitizeInput.mappedPlayerName` (fallback when `media_player_name` misses) |

Exact media lookup expression (from `captureService.ts`):

```ts
mappedPlayerName:
  raw.playerDisplayName === null
    ? null
    : applyMapping(config, "media_player_name", raw.playerDisplayName) ??
      // Compatibility with mappings saved before media_player_name.
      applyMapping(config, "media_process_name", raw.playerDisplayName),
```

Notes:

- When `raw.playerDisplayName` is `null`, NO mapping lookup happens at all
  (`mappedPlayerName = null`) — `sourceAppUserModelId` is never used as a
  mapping key (asymmetry with rule matching, see 4.2/A5).
- The legacy `media_process_name` fallback is keyed by the CURRENT player
  display name, not by a process name; a legacy entry only matches when its
  stored `from` normalizes equal to today's player display name (ambiguity
  A6).
- `??` is nullish coalescing: only a `null` first lookup falls through.

### 3.3 Interaction with rules

Mappings NEVER change the identifier used for rule lookup. The evaluator
header comment is normative: "Decisions always use the ORIGINAL (pre-mapping)
identifiers." Mappings affect display names only, and only inside the
sanitizer, and only when the corresponding presence is shared at all (a
hidden application/media never reaches the sanitizer's name computation).

## 4. Evaluator (`privacy/evaluator.ts`)

Ported from `PresencePrivacyPolicy.swift`. "Hide wins over everything; a
hidden application never leaks its alias either."

### 4.1 `processDecision(config, appId): ProcessDecision`

```ts
const rule = findRule(config, appId);
const hidden =
  resolveOverride(rule?.application ?? "inherit", config.defaults.application) === "hide";
const windowShared =
  resolveOverride(rule?.windowTitle ?? "inherit", config.defaults.windowTitle) === "share";
return {
  sharesApplication: !hidden,
  sharesWindowTitle: !hidden && windowShared,
  displayAlias: hidden ? null : normalizeText(rule?.displayAlias),
};
```

- `ProcessDecision = { sharesApplication: boolean, sharesWindowTitle: boolean, displayAlias: string | null }`.
- `sharesWindowTitle` is RULE-LEVEL consent only. The doc comment is
  normative: the sanitizer additionally requires the global
  `shareWindowTitles` switch — a title leaves the process only when the app
  is not hidden AND the rule resolves to share AND the global switch is on.
  The `sources.application` gate is applied earlier, in `CaptureService`.
- `displayAlias`: `null` when hidden (alias never leaks), else the
  NFC-trimmed alias or `null` (blank alias ≡ absent). When no rule matches,
  `rule?.displayAlias` is `undefined` → `normalizeText` → `null`.

Window-title truth table over (effective application, effective windowTitle),
pinned by tests:

| effective application | effective windowTitle | `sharesWindowTitle` |
| --- | --- | --- |
| share | share | `true` |
| share | hide | `false` |
| hide | share | `false` |
| hide | hide | `false` |

### 4.2 `mediaDecision(config, appId, playerName): MediaDecision`

```ts
const rule =
  appId !== null ? findRule(config, appId) : findRuleByPlayerName(config, playerName);
const hidden =
  resolveOverride(rule?.media ?? "inherit", config.defaults.media) === "hide";
return {
  sharesMedia: !hidden,
  displayAlias: hidden ? null : normalizeText(rule?.displayAlias),
};
```

- `MediaDecision = { sharesMedia: boolean, displayAlias: string | null }`.
- Rule selection is EITHER/OR, not chained: when `appId` is non-null, only
  `findRule(appId)` is consulted — even if it misses, there is NO fallback to
  player-name matching (pinned by the test "appId match takes precedence over
  player-name fallback": rule `appId: "spotify"`, call with
  `("spotify.exe", "Spotify")` → no match → default share).
- When `appId` is `null`, `findRuleByPlayerName(playerName)` is used; an
  empty/blank `playerName` matches nothing (2.9), so defaults apply.
- The media dimension only reads `rule.media` + `defaults.media`: an
  `application = "hide"` rule does NOT hide media from the same app
  (independent dimensions; pinned by test). Conversely a `media = "hide"`
  rule does not hide the application.
- `displayAlias` here feeds `playerDisplayName` precedence in the media
  sanitizer; hidden media yields `null` alias.
- `ignoreNullArtist` is NOT evaluated here — it is passed to `sanitizeMedia`
  as `options.requiresArtist` by `CaptureService` (sections 5.2, 8.3).
- `sources.media` is NOT evaluated here — `CaptureService` gates it.

### 4.3 Caller-supplied `playerName` (context from `captureService.ts`)

The `playerName` argument for `mediaDecision` is
`raw.playerDisplayName ?? raw.sourceAppUserModelId ?? ""` — i.e. rule
fallback matching MAY use the raw `sourceAppUserModelId` when no display name
exists, whereas mapping lookup (3.2) never does. Flagged as A5.

## 5. Sanitizers (`privacy/sanitize.ts`)

Ported from `CompanionApplicationPresenceSanitizer` /
`CompanionMediaPresenceSanitizer`. Header comment is normative: "These
functions deliberately do not accept appIds or executable paths — original
process identity cannot pass this boundary by construction. Display name
precedence: alias > mapping > raw."

### 5.1 `sanitizeApplication(input, decision, globalShareWindowTitles)`

Input `ApplicationSanitizeInput = { capturedDisplayName: string, mappedDisplayName: string | null, windowTitle: string | null }`.
Output `SanitizedApplicationPresence | null`. Algorithm:

1. If `!decision.sharesApplication` → return `null`.
2. `displayName = decision.displayAlias ?? normalizeText(input.mappedDisplayName) ?? normalizeText(input.capturedDisplayName)`.
   - `decision.displayAlias` is already normalized (evaluator) and non-empty
     when non-null; a non-null alias wins even when a mapping exists.
   - `??` fallthrough happens only on `null` (normalizeText never yields `""`).
3. If `displayName === null` → return `null` (the WHOLE application presence
   is dropped when every name source is blank, even though the app is shared).
4. `windowTitle = (decision.sharesWindowTitle && globalShareWindowTitles) ? normalizeText(input.windowTitle) : null`.
   Combined with the evaluator this is the full three-switch chain: title
   present ⇔ app not hidden AND rule/default windowTitle resolves to share
   AND global `shareWindowTitles` is `true` (AND the title is non-blank).
5. Return `{ displayName, windowTitle }`.

### 5.2 `sanitizeMedia(input, decision, options)`

Input `MediaSanitizeInput = { kind, title, artist, album, capturedPlayerName, mappedPlayerName, playing, durationSeconds, positionSeconds, sampledAt }`;
`options = { requiresArtist: boolean }` (the global `ignoreNullArtist`
switch: drop media that has no artist).
Output `Omit<SanitizedMediaPresence, "sessionId"> | null` — the `sessionId`
is assigned by the caller via `MediaSessionTracker` AFTER sanitization
succeeds. Algorithm, in exact order:

1. If `!decision.sharesMedia` → return `null`.
2. `title = normalizeText(input.title)`; `artist = normalizeText(input.artist)`;
   `album = normalizeText(input.album)`.
3. If `options.requiresArtist && artist === null` → return `null`.
4. If `title === null && artist === null` → return `null` (album alone is
   not meaningful text).
5. `playerDisplayName = decision.displayAlias ?? normalizeText(input.mappedPlayerName) ?? normalizeText(input.capturedPlayerName)`
   — may end up `null` (allowed; media is still shared).
6. `duration = normalizedSeconds(input.durationSeconds)` where
   `normalizedSeconds(v) = (v === null || !Number.isFinite(v) || v < 0) ? null : v`
   — `null`, `NaN`, `±Infinity`, and negatives become `null`; `0` and
   fractional values pass through unchanged (no rounding).
7. `position = normalizedSeconds(input.positionSeconds)`; then, if
   `position !== null && duration !== null && position > duration`, clamp
   `position = duration`. When `duration` is `null`, position is NOT clamped.
8. `playback = { state: playing ? "playing" : "paused", durationSeconds: duration, positionSeconds: position, sampledAt: input.sampledAt, rate: playing ? 1 : 0 }`.
   `sampledAt` passes through with NO validation. Rate is derived from state
   because SMTC exposes no reliable playback rate (matches the macOS
   sanitizer: playing → 1, paused → 0).
9. Return `{ kind: input.kind, title, artist, album, playerDisplayName, playback }`
   — `kind` is preserved verbatim (all four values including `"unknown"`).

### 5.3 What sanitization does NOT do

- No length caps, no control-character stripping, no ellipsis — the ONLY
  string transforms in this layer are NFC + ECMAScript trim + empty→null
  (`normalizeText`). Wire-side truncation lives in the protocol layer
  (`companion/protocol/dtoMapper.ts`: `DISPLAY_NAME_LIMIT = 120`,
  `WINDOW_TITLE_LIMIT = 500`, `MEDIA_TEXT_LIMIT = 300` Unicode scalars,
  truncate-at-scalar-boundary then re-trim, empty→null) and belongs to the
  protocol spec, not this one.
- Excluded from sanitized output entirely (no field exists): `appId`,
  `exePath`, `sourceAppUserModelId`, process IDs, raw capture objects,
  artwork.
- The paused branch (`state: "paused"`, `rate: 0`) is implemented and
  test-pinned here, but `CaptureService` only feeds PLAYING media into
  `sanitizeMedia` (section 8.3) — through the production pipeline, paused
  media becomes `media: null`. Keep both behaviors (ambiguity A4).

## 6. Policy fingerprint (`privacy/fingerprint.ts`)

### 6.1 Purpose

Hashes the persisted privacy projection used by the consent gate. Any change
to the effective policy must change the string; cosmetic changes (rule/mapping
array order, empty rules, alias whitespace, appId case) must not.

### 6.2 Normative source (transcribed verbatim)

```ts
import { createHash } from "node:crypto";
import { isEmptyRule, normalizedRule } from "./model.js";

function canonicalize(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonicalize);
  if (value !== null && typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>)
      .filter(([, current]) => current !== undefined)
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
    return Object.fromEntries(entries.map(([key, current]) => [key, canonicalize(current)]));
  }
  return value;
}

export function policyFingerprint(config: PrivacyConfig): string {
  const rules = config.rules
    .map(normalizedRule)
    .filter((rule) => !isEmptyRule(rule))
    .sort((a, b) => (a.appId < b.appId ? -1 : a.appId > b.appId ? 1 : 0));
  const mappings = [...config.mappings].sort((a, b) => {
    const ka = `${a.type}\\0${a.from}`;
    const kb = `${b.type}\\0${b.from}`;
    return ka < kb ? -1 : ka > kb ? 1 : 0;
  });
  const projection = canonicalize({
    sources: config.sources,
    shareWindowTitles: config.shareWindowTitles,
    ignoreNullArtist: config.ignoreNullArtist,
    defaults: config.defaults,
    rules,
    mappings,
  });
  return createHash("sha256").update(JSON.stringify(projection)).digest("hex");
}
```

### 6.3 Step-by-step semantics

1. Rules: `map(normalizedRule)` (2.7: appId → normalizeAppId; alias →
   normalizeText, key omitted when null) → `filter(!isEmptyRule)` (2.6) →
   stable sort ascending by `appId` using JS `<`/`>` string comparison
   (UTF-16 code unit lexicographic). JS `Array.prototype.sort` is stable
   (ES2019+): rules whose normalized appIds are equal keep their config
   order. Rust: use a stable sort with a UTF-16 code-unit comparator.
2. Mappings: shallow copy, then stable sort by the concatenated key
   `type + SEP + from`, comparing with `<`/`>`. SEP verified by hex dump of
   the source: the template literal contains `\\0`, i.e. the runtime
   separator is the TWO characters U+005C REVERSE SOLIDUS followed by
   U+0030 DIGIT ZERO — NOT a NUL. This is almost certainly an escaping
   accident, but it is provably order-neutral: the three legal `type` values
   (`process_name`, `media_process_name`, `media_player_name`) are never
   prefixes of one another up to their first differing code unit, so the
   comparison never reaches the separator across different types, and for
   equal types the separator cancels out. Therefore the sort is exactly
   equivalent to lexicographic-by-(`type`, then `from`) in UTF-16 code-unit
   order. Rust may implement it either way; document the choice.
   Mappings are serialized RAW — `type`/`from`/`to` keep their original
   case and whitespace (no normalizeAppId, no trim). Only the SORT compares
   raw values too (`from` is not normalized for sorting).
3. `canonicalize` (recursive):
   - Arrays: element order PRESERVED, elements canonicalized.
   - Non-null objects: entries with `undefined` values dropped; remaining
     entries sorted by key with `<`/`>` (UTF-16 code-unit order); rebuilt in
     sorted insertion order; values canonicalized recursively.
   - Everything else (string, number, boolean, `null`): returned as-is.
4. `JSON.stringify` with no spacing: compact separators `","` and `":"`,
   object keys emitted in (sorted) insertion order.
5. Hash: SHA-256 over the UTF-8 encoding of the JSON string
   (`hash.update(string)` uses utf8 by default). Output:
   `digest("hex")` — 64 lowercase hex characters. (Rust: `sha2::Sha256`,
   `format!("{:x}", ...)`.)

### 6.4 Resulting key orders (all keys are ASCII, so UTF-16 order = byte order)

- Top level: `defaults`, `ignoreNullArtist`, `mappings`, `rules`,
  `shareWindowTitles`, `sources`.
- `defaults`: `application`, `media`, `windowTitle`.
- `sources`: `application`, `media`.
- Rule objects: `appId`, `application`, [`displayAlias` if present], `media`,
  `windowTitle`. (`"appId" < "application"` because `I` U+0049 < `l` U+006C.)
- Mapping objects: `from`, `to`, `type`.

The fingerprint preimage contains ONLY strings, booleans, arrays, and objects
— never numbers and never `null` (absent alias = omitted key), so JS number
formatting cannot affect it.

### 6.5 What feeds the fingerprint

`sources` (both flags), `shareWindowTitles`, `ignoreNullArtist`, `defaults`
(all three), normalized non-empty `rules`, raw `mappings`. Nothing else — in
particular NOT the current capture, preview, or connection state.

### 6.6 Golden vectors (spec-added; computed 2026-09-23 by executing the real
`policyFingerprint()` via tsx, Node v24.15.0 — the TS tests pin only
equality/inequality relations, not literals)

Base = `defaultPrivacyConfig()` (1.3); each row patches the base.

| config | sha256 hex |
| --- | --- |
| default (no patch) | `f59b6fb20c6c4d845f87f24a2700a3b8de6cfab90700728ad71f0a067098adc6` |
| `shareWindowTitles: true` | `0b9f60f43f35d3910dc720e8fafff62d9e6b00f02a7eec01259127f7a4c11bff` |
| `ignoreNullArtist: true` | `e056264d9861ab7c390e5ae0498c50c9dccb3df668562bb3a716de81215677d9` |
| `sources: { application: true, media: false }` | `d1743898a5d1dcb0386052a57e354af8b21cf71ce71af9460cd45febf991b296` |
| `defaults: { application: "share", windowTitle: "share", media: "share" }` | `2c1fd331109903db28e0ecd1d37da3e2bdc53047984c32694d22f21d0a1c3cf9` |
| rules: `[{ appId: "a.exe", application: "hide", windowTitle: "inherit", media: "inherit" }]` | `930dd61234871c1092ba3fb5d67c95f54685460cc3029ad1a6c48f096e7ef75b` |
| mappings: `[{ type: "process_name", from: "a", to: "b" }]` | `b82ed1287ec00b515dde30f19eba257499fb2bbacedf05f2f4baeb13a71aadf8` |
| rules: `[{ appId: "  Code.EXE ", application: "share", windowTitle: "share", media: "inherit", displayAlias: "  VS Code  " }]` | `717c3a7c86555b5b86849bf3668eb8282b8b40a2c88564831757e27b3a49a915` |
| rules `[a.exe app=hide; b.exe title=share]` + mappings `[process_name x→y; media_process_name x→y]` (any order) | `5d914a4d8f22fe0c67a5b16627a5d96c89b97553dc7a38a62a116f29b8937692` |

Verified preimages (re-hashing each string yields exactly the vector above):

Default config preimage:

```
{"defaults":{"application":"share","media":"share","windowTitle":"hide"},"ignoreNullArtist":false,"mappings":[],"rules":[],"shareWindowTitles":false,"sources":{"application":true,"media":true}}
```

`"  Code.EXE "` + alias `"  VS Code  "` rule preimage (shows appId/alias
normalization and the sorted rule keys with `displayAlias` present):

```
{"defaults":{"application":"share","media":"share","windowTitle":"hide"},"ignoreNullArtist":false,"mappings":[],"rules":[{"appId":"code.exe","application":"share","displayAlias":"VS Code","media":"inherit","windowTitle":"share"}],"shareWindowTitles":false,"sources":{"application":true,"media":true}}
```

Two-rules + two-mappings preimage (shows mapping sort: `media_process_name`
before `process_name`, and raw mapping serialization):

```
{"defaults":{"application":"share","media":"share","windowTitle":"hide"},"ignoreNullArtist":false,"mappings":[{"from":"x","to":"y","type":"media_process_name"},{"from":"x","to":"y","type":"process_name"}],"rules":[{"appId":"a.exe","application":"hide","media":"inherit","windowTitle":"inherit"},{"appId":"b.exe","application":"inherit","media":"inherit","windowTitle":"share"}],"shareWindowTitles":false,"sources":{"application":true,"media":true}}
```

## 7. MediaSessionTracker (`privacy/mediaSessionTracker.ts`)

### 7.1 Semantic identity

```ts
interface MediaSemanticIdentity {
  kind: MediaKind;
  title: string | null;
  artist: string | null;
  album: string | null;
  playerDisplayName: string | null;
  durationSeconds: number | null;
}
```

The caller (`CaptureService`) builds this from SANITIZED values (post-NFC,
post-alias/mapping, post-`normalizedSeconds`): `kind`, `title`, `artist`,
`album`, `playerDisplayName`, and `playback.durationSeconds` of the sanitized
media. Consequences: a policy change that alters the effective
`playerDisplayName` (alias or mapping edit) mints a new session, as does a
duration change.

### 7.2 Identity key and equality

```ts
function identityKey(identity) {
  return JSON.stringify([
    identity.kind, identity.title, identity.artist, identity.album,
    identity.playerDisplayName, identity.durationSeconds,
  ]);
}
```

Equality = string equality of this 6-element JSON array. The key never
leaves the process, so the Rust port may use structural equality of the
6-tuple instead of string keys, PROVIDED the same pairs compare equal. Two
JS-side edge cases to respect if using string keys: `JSON.stringify(NaN)` is
`"null"` (a NaN duration would equal a null duration — unreachable through
the pipeline because `normalizedSeconds` nulls NaN first) and
`JSON.stringify(-0)` is `"0"` (also unreachable: providers null out
non-positive durations). Structural equality with `f64::eq` on the duration
matches pipeline-reachable behavior exactly.

### 7.3 `sessionId(identity): string`

State: a single `current: { key, sessionId } | null`.

1. Compute `key = identityKey(identity)`.
2. If `current !== null && current.key === key` → return `current.sessionId`
   (same track keeps its id across position updates and repeated captures).
3. Else set `current = { key, sessionId: randomUUID() }` and return the new
   id. `randomUUID()` is UUID v4, lowercase, hyphenated
   (`^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`,
   pinned by test). The id is RANDOM — never derived from content; it must
   not be a fingerprintable hash (privacy requirement in the header comment).

Only ONE identity is remembered: alternating between two identities A, B, A
mints three distinct ids (no LRU, no map).

### 7.4 `reset()` and continuity breaks

`reset()` sets `current = null`; the next `sessionId()` call mints a fresh
UUID even for an identical identity. Callers of reset (exhaustive):

- `CaptureService.captureForDelivery`, whenever a capture that was supposed
  to include media ends with `media === null` — i.e. provider missing,
  `sources.media` off, provider timeout/error/null snapshot, media paused,
  hidden by policy, or dropped by the sanitizer (section 8.3 matrix).
- `CaptureService.resetMediaContinuity()`, invoked by the companion
  coordinator on generation teardown (stop/reconnect/disable) so a new Live
  Desk generation never continues an old session id.

### 7.5 Semantic change vs progress tick

- NOT part of identity (never mint a new id by themselves):
  `positionSeconds`, `sampledAt`, playback `state`/`rate` — natural progress
  and repeated captures keep the session id.
- Part of identity (any change mints a new id): `kind`, `title`, `artist`,
  `album`, `playerDisplayName`, `durationSeconds`.
- Pause/resume THROUGH THE PIPELINE: pausing makes `captureForDelivery` drop
  media (playing gate) → `reset()` → resuming the same track mints a NEW
  session id. This is deliberate continuity-break behavior.
- The tracker itself has NO debouncing and NO timers. Event debouncing lives
  in the capture-layer providers (each keeps a `lastSemanticKey` =
  `JSON.stringify([sourceAppId, title, artist, album, playing/playbackStatus])`
  and only fires `onSemanticChange` when it changes; timeline/position events
  are deliberately not subscribed). That is agent M's scope; noted here
  because the prompt for this spec asked — see A2.
- Position extrapolation is ALSO capture-layer, not tracker: both providers
  compute, at snapshot build time `now = Date.now()`:
  `elapsed = (now - lastNativeUpdateMs) / 1000`; if `position != null && playing && Number.isFinite(lastNativeUpdateMs) && elapsed > 0 && elapsed < 21600` (6 h)
  then `position += elapsed`; then clamp to duration when duration non-null;
  and always `sampledAt = now`. The privacy pipeline TRUSTS `sampledAt` as
  "epoch ms at which positionSeconds was computed" and passes it through
  unmodified.

## 8. CaptureService (`privacy/captureService.ts`)

Ported from `CompanionPresenceCapture.swift`. Header comment (normative):
"Every delivery captures anew — snapshots are never replayed. The privacy
configuration is re-read AFTER every await point (fail-closed: a rule
tightened during the media provider's suspension applies to both sources).
Media is captured before the application so the application decision uses
the newest configuration."

### 8.1 Construction

```
CaptureService(
  foreground: ForegroundSource,            // { current(): ForegroundInfo | null } — sync
  mediaProvider: () => MediaProvider | null, // getter; provider may appear/disappear at runtime
  currentPrivacy: () => PrivacyConfig,       // fresh config read on EVERY call
)
```

Owns one private `MediaSessionTracker`. Constant: `MEDIA_TIMEOUT_MS = 2000`.

### 8.2 Small API

- `fingerprint(): string` — `policyFingerprint(currentPrivacy())` (fresh read).
- `resetMediaContinuity(): void` — `tracker.reset()` (see 7.4).

### 8.3 `captureForDelivery({ includeMedia: boolean }): Promise<SanitizedPresenceSnapshot>`

Exact order of operations:

1. `observedAt = Date.now()` — taken at the START, before any await.
2. MEDIA BRANCH (first, async). Let `provider = mediaProvider()`.
   Enter only if `includeMedia && provider !== null && currentPrivacy().sources.media`
   (config read #1).
   a. `raw = await withTimeout(provider.getSnapshot(), 2000)` — note:
      `getSnapshot()` is invoked with NO arguments.
   b. `config = currentPrivacy()` — config read #2, AFTER the await
      (fail-closed re-read).
   c. Proceed only if `raw !== null && config.sources.media && raw.playing`
      — paused/absent media is dropped here; `sources.media` flipped off
      during the await also drops it.
   d. `playerName = raw.playerDisplayName ?? raw.sourceAppUserModelId ?? ""`.
   e. `decision = mediaDecision(config, raw.appId, playerName)` (4.2).
   f. Build `MediaSanitizeInput`: `kind/title/artist/album` from raw;
      `capturedPlayerName = raw.playerDisplayName`; `mappedPlayerName` per
      the exact expression in 3.2; `playing/durationSeconds/positionSeconds/sampledAt`
      from raw. Call `sanitizeMedia(input, decision, { requiresArtist: config.ignoreNullArtist })`.
   g. If sanitize returned non-null: `media = { ...sanitized, sessionId: tracker.sessionId({ kind, title, artist, album, playerDisplayName, durationSeconds: sanitized.playback.durationSeconds }) }`
      (identity from SANITIZED fields, 7.1).
   h. If, after all of the above, `media === null` (any drop reason):
      `tracker.reset()` — "Paused, hidden, or dropped media breaks session
      continuity."
3. ELSE-BRANCH: if `includeMedia` was `true` but the branch was not entered
   (no provider, or `sources.media` false at read #1): `tracker.reset()`.
   If `includeMedia === false`: the tracker is left UNTOUCHED (media-less
   deliveries requested by the caller do not break continuity).
4. APPLICATION BRANCH (second, sync). `config = currentPrivacy()` — config
   read #3 (the freshest; a rule tightened during the media await applies
   here). Enter only if `config.sources.application`.
   a. `info = foreground.current()`; skip if `null`.
   b. `decision = processDecision(config, info.appId)` (4.1).
   c. `application = sanitizeApplication({ capturedDisplayName: info.displayName, mappedDisplayName: applyMapping(config, "process_name", info.appId), windowTitle: info.windowTitle }, decision, config.shareWindowTitles)`.
5. Return `{ observedAt, application, media }` — exactly a
   `SanitizedPresenceSnapshot`; this is the shape handed to the companion
   layer.

Tracker reset matrix (from steps 2-3):

| includeMedia | provider | sources.media@1 | outcome media | tracker |
| --- | --- | --- | --- | --- |
| false | any | any | `null` | untouched |
| true | null | any | `null` | reset |
| true | present | false | `null` | reset |
| true | present | true | `null` (timeout / raw null / sources.media off @2 / paused / hidden / sanitize drop) | reset |
| true | present | true | non-null | sessionId assigned (7.3) |

### 8.4 `withTimeout(promise, ms)` semantics

`Promise.race([promise, timeoutPromise])` where the timeout promise RESOLVES
to `null` after `ms`; any rejection of the raced pair is caught and mapped to
`null`; the timer is cleared in `finally`. The losing `getSnapshot()` promise
is NOT cancelled (fire-and-forget; its late result is discarded). Rust:
`tokio::time::timeout(Duration::from_millis(2000), provider.get_snapshot(...))`
with both `Err(Elapsed)` and provider errors mapped to `None` reproduces
this (the ARCHITECTURE trait passes the timeout INTO `get_snapshot`; either
placement is fine as long as a hung provider yields `None` after ~2 s and
errors never propagate).

### 8.5 Callers (context, agent K's scope)

- Coordinator publish loop: `captureForDelivery({ includeMedia: this.includeMedia })`
  (negotiated flag), result passed as-is to `replacePresence(snapshot, leaseSeconds)`.
- `CompanionService.refreshPreview()`: `captureForDelivery({ includeMedia: config.privacy.sources.media })`,
  then `projectionOf(snapshot)` (section 9) is recorded as the consent basis.
- `confirmConsent`: performs TWO fresh captures (before and after persisting)
  and validates both projections against the recorded confirmation.
- Coordinator generation teardown calls `resetMediaContinuity()`.

## 9. PreviewProjection (`companion/consentGate.ts` + `shared/src/status.ts`)

`projectionOf(snapshot: SanitizedPresenceSnapshot): PreviewProjection` — the
exact sanitized values the user confirms before Live Desk may publish.

Field-by-field mapping (exhaustive):

| projection field | source | included |
| --- | --- | --- |
| `application` | `snapshot.application === null ? null : {...}` | yes |
| `application.displayName` | `snapshot.application.displayName` | yes |
| `application.windowTitle` | `snapshot.application.windowTitle` | yes (nullable) |
| `media` | `snapshot.media === null ? null : {...}` | yes |
| `media.kind` | `snapshot.media.kind` | yes |
| `media.title` / `media.artist` / `media.album` | same-named | yes (nullable) |
| `media.playerDisplayName` | same-named | yes (nullable) |
| `media.playback.state` | `snapshot.media.playback.state` | yes |
| `media.playback.durationSeconds` | same-named | yes (nullable) |
| `media.playback.rate` | same-named | yes |

Deliberately EXCLUDED (normative comments in both `status.ts` and
`consentGate.ts`):

- `snapshot.observedAt` — capture timestamp changes every capture.
- `media.sessionId` — random per-session UUID changes on continuity breaks.
- `media.playback.positionSeconds` — natural playback progress.
- `media.playback.sampledAt` — position sampling timestamp.

Rationale (consent stability): "natural playback progress is continuity, not
new disclosure" — the projection must compare equal across progress ticks so
consent survives them, "while any semantic change (track, app, pause state)
does" invalidate. Because `state`, `durationSeconds`, and `rate` ARE
included, track changes, pause/play flips, duration and rate changes all
invalidate consent. Consent comparison is structural deep-equality of
`{ policyFingerprint, projection }` (gate internals are agent K's scope; the
gate also re-validates against a fresh capture's projection).

The `Preview` wrapper sent to the UI is
`{ projection, policyFingerprint, observedAt }` — `observedAt` rides along
for display but is outside the compared projection.

## 10. Test vectors (verbatim from the five vitest files; all 44 pass)

Conventions below: `default` = `defaultPrivacyConfig()` (1.3);
`config(patch)` = default with top-level fields replaced by `patch`. Rules
are written `{appId, application, windowTitle, media, displayAlias?}`.
These become the Rust test suites `tests/{privacy_model,evaluator,sanitize,fingerprint,media_session_tracker}.rs`
with identical assertion values. (`consentGate.test.ts` has 8 more tests —
agent K's scope, not transcribed here.)

### 10.1 `model.test.ts` — "process-name application mappings" (4 tests)

Fixture config: `defaults {application: share, windowTitle: hide, media: share}`,
`rules: []`, `shareWindowTitles: false`, `ignoreNullArtist: false`,
`sources {application: true, media: true}`, and mappings:

```json
[
  { "type": "process_name",       "from": "code.exe",      "to": "Visual Studio Code" },
  { "type": "media_process_name", "from": "Spotify.EXE",   "to": "Spotify" },
  { "type": "media_player_name",  "from": "YouTube Music", "to": "YouTube Music Desktop" }
]
```

| test | call | expected |
| --- | --- | --- |
| replaces an app display name using the executable appId | `applyMapping(config, "process_name", "CODE.EXE")` | `"Visual Studio Code"` |
| normalizes media process names with the same matching rules | `applyMapping(config, "media_process_name", "spotify.exe")` | `"Spotify"` |
| does not cross mapping types | `applyMapping(config, "media_process_name", "code.exe")` | `null` |
| maps the captured media player name explicitly | `applyMapping(config, "media_player_name", " youtube music ")` | `"YouTube Music Desktop"` |

### 10.2 `evaluator.test.ts` (16 tests)

`processDecision` (10):

1. "shares by default with new-installation defaults":
   `processDecision(config(), "code.exe")` deep-equals
   `{ sharesApplication: true, sharesWindowTitle: false, displayAlias: null }`
   (false because `defaults.windowTitle = hide`).
2. "hide wins: rule application=hide hides app, title, and alias":
   rules `[{secret.exe, hide, share, inherit, alias "Alias"}]`;
   `processDecision(c, "secret.exe")` deep-equals
   `{ sharesApplication: false, sharesWindowTitle: false, displayAlias: null }`.
3. "global default application=hide hides apps without a rule":
   `defaults {hide, hide, share}`;
   `processDecision(c, "anything.exe").sharesApplication === false`.
4. "rule share overrides a hide default": `defaults {hide, hide, share}` +
   rules `[{code.exe, share, inherit, inherit}]`;
   `processDecision(c, "code.exe").sharesApplication === true`.
5.-8. Window-title truth table — rule `{x.exe, A, W, inherit}`, assert
   `processDecision(c, "x.exe").sharesWindowTitle`:
   (A=share, W=share) → `true`; (share, hide) → `false`;
   (hide, share) → `false`; (hide, hide) → `false`.
   (Test comment: the third switch, global `shareWindowTitles`, is applied
   at the sanitize layer.)
9. "matches appId case-insensitively and trims":
   rules `[{"Code.EXE", hide, inherit, inherit}]`;
   `processDecision(c, "  code.exe ").sharesApplication === false`.
10. "normalizes alias: blank alias becomes null":
   rules `[{a.exe, share, inherit, inherit, alias "   "}]`;
   `processDecision(c, "a.exe").displayAlias === null`.

`mediaDecision` (6):

11. "shares by default": `mediaDecision(config(), "spotify.exe", "Spotify")`
    deep-equals `{ sharesMedia: true, displayAlias: null }`.
12. "rule media=hide hides media and alias":
    rules `[{spotify.exe, inherit, inherit, hide, alias "MyPlayer"}]`;
    `mediaDecision(c, "spotify.exe", "Spotify")` deep-equals
    `{ sharesMedia: false, displayAlias: null }`.
13. "app hidden does NOT hide media (independent dimensions)":
    rules `[{spotify.exe, hide, inherit, inherit}]`;
    `mediaDecision(c, "spotify.exe", "Spotify").sharesMedia === true`.
14. "falls back to player-name matching when appId is null":
    rules `[{"Spotify", inherit, inherit, hide}]`;
    `mediaDecision(c, null, "spotify").sharesMedia === false` AND
    `mediaDecision(c, null, "Other Player").sharesMedia === true`.
15. "appId match takes precedence over player-name fallback":
    rules `[{"spotify", inherit, inherit, hide}]`;
    `mediaDecision(c, "spotify.exe", "Spotify").sharesMedia === true`
    (appId provided and different → no rule match → default share; NO
    player-name fallback).
16. "global media=hide default": `defaults {share, hide, hide}`;
    `mediaDecision(c, "x.exe", "X").sharesMedia === false`.

### 10.3 `sanitize.test.ts` (11 tests)

Fixtures:

- `share: ProcessDecision = { sharesApplication: true, sharesWindowTitle: true, displayAlias: null }`
- `appInput(patch)` base: `{ capturedDisplayName: "Visual Studio Code", mappedDisplayName: null, windowTitle: "secret.ts - project" }`
- `shareMedia: MediaDecision = { sharesMedia: true, displayAlias: null }`
- `mediaInput(patch)` base: `{ kind: "music", title: "Song", artist: "Artist", album: "Album", capturedPlayerName: "Spotify", mappedPlayerName: null, playing: true, durationSeconds: 200, positionSeconds: 60, sampledAt: 1753500000000 }`

`sanitizeApplication` (4):

1. "returns null when application is not shared":
   `sanitizeApplication(appInput(), {...share, sharesApplication: false}, true) === null`.
2. "display name precedence: alias > mapping > raw":
   `(appInput({mappedDisplayName: "Mapped"}), {...share, displayAlias: "Alias"}, true).displayName === "Alias"`;
   `(appInput({mappedDisplayName: "Mapped"}), share, true).displayName === "Mapped"`;
   `(appInput(), share, true).displayName === "Visual Studio Code"`.
3. "window title requires all three switches":
   `(appInput(), share, true).windowTitle === "secret.ts - project"`;
   `(appInput(), share, false).windowTitle === null` (global off);
   `(appInput(), {...share, sharesWindowTitle: false}, true).windowTitle === null`
   (rule hide beats global on).
4. "normalizes text: NFC + trim, empty title -> null":
   `appInput({capturedDisplayName: "  Café  ", windowTitle: "   "})` with
   `share, true` → `displayName === "Café"`, `windowTitle === null`.

`sanitizeMedia` (7; `requiresArtist: false` unless stated):

5. "returns null when media is not shared":
   decision `{sharesMedia: false, displayAlias: null}` → `null`.
6. "requiresArtist drops media without artist":
   `mediaInput({artist: null})` + `{requiresArtist: true}` → `null`;
   same input + `{requiresArtist: false}` → NOT `null`.
7. "requires title or artist after normalization":
   `mediaInput({title: "  ", artist: null})` → `null`.
8. "clamps position to duration and nulls invalid values":
   `mediaInput({durationSeconds: 100, positionSeconds: 150})` →
   `playback.positionSeconds === 100`;
   `mediaInput({durationSeconds: NaN, positionSeconds: -5})` →
   `playback.durationSeconds === null` AND `playback.positionSeconds === null`.
9. "preserves a real zero position (null means unavailable, 0 means start)":
   `mediaInput({positionSeconds: 0})` → `playback.positionSeconds === 0`.
10. "derives rate and state from playing flag":
    `mediaInput({playing: true})` → playback matches `{state: "playing", rate: 1}`;
    `mediaInput({playing: false})` → playback matches `{state: "paused", rate: 0}`.
11. "player display name precedence: alias > mapping > raw":
    `mediaInput({mappedPlayerName: "Mapped"})` + decision
    `{sharesMedia: true, displayAlias: "Alias"}` → `playerDisplayName === "Alias"`;
    same input + `shareMedia` → `playerDisplayName === "Mapped"`.

### 10.4 `fingerprint.test.ts` (9 tests; relations only — literals in 6.6)

1. "is stable for identical configs": `fp(config()) === fp(config())`.
2. "is order-insensitive for rules and mappings": with
   `ruleA = {a.exe, hide, inherit, inherit}`,
   `ruleB = {b.exe, inherit, share, inherit}`,
   `m1 = {process_name, x, y}`, `m2 = {media_process_name, x, y}`:
   `fp(config({rules: [ruleA, ruleB], mappings: [m1, m2]})) === fp(config({rules: [ruleB, ruleA], mappings: [m2, m1]}))`.
   (Golden value for either ordering: `5d914a4d...` per 6.6.)
3. "ignores empty rules and alias whitespace": rule
   `{noop.exe, inherit, inherit, inherit, alias "   "}` →
   `fp(config({rules: [empty]})) === fp(config())`
   (both `f59b6fb2...` per 6.6).
4.-9. "changes when X changes" — each patched fp `!==` default fp:
   `shareWindowTitles: true`; `ignoreNullArtist: true`;
   `sources: {application: true, media: false}`;
   `defaults: {application: share, windowTitle: share, media: share}`;
   `rules: [{a.exe, hide, inherit, inherit}]`;
   `mappings: [{process_name, a, b}]`.
   (Each patched golden value is in the 6.6 table; the Rust port should pin
   both the inequality AND the literals.)

### 10.5 `mediaSessionTracker.test.ts` (4 tests)

Fixture identity: `{ kind: "music", title: "Song", artist: "Artist", album: "Album", playerDisplayName: "Spotify", durationSeconds: 200 }`.

1. "keeps the same sessionId for the same semantic identity":
   `t.sessionId(identity) === t.sessionId({...identity})` (fresh equal object).
2. "mints a new sessionId when identity changes":
   `t.sessionId(identity)` then `t.sessionId({...identity, title: "Other"})`
   → different values.
3. "mints a new sessionId after reset (continuity break)":
   `a = t.sessionId(identity); t.reset(); t.sessionId(identity) !== a`.
4. "sessionIds are UUIDs, not content hashes": two independent trackers give
   DIFFERENT ids for the same identity, and the id matches
   `/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/`.

## 11. Traps for the Rust port

### 11.1 ECMAScript `trim()` is not `str::trim()`

`String.prototype.trim` strips the ECMAScript WhiteSpace and LineTerminator
sets, exactly: U+0009 TAB, U+000A LF, U+000B VT, U+000C FF, U+000D CR,
U+0020 SPACE, U+00A0 NBSP, U+1680, U+2000–U+200A, U+2028 LS, U+2029 PS,
U+202F, U+205F, U+3000, and U+FEFF (ZWNBSP/BOM). Differences from Rust
`char::is_whitespace` (Unicode `White_Space`): JS trims U+FEFF, Rust does
NOT; Rust trims U+0085 NEL, JS does NOT. Implement a dedicated
`js_trim`/`is_js_whitespace` helper and use it in `normalize_text` /
`normalize_app_id`; do not use `str::trim()`.

### 11.2 `toLowerCase()` semantics

ECMAScript `toLowerCase` is the full, locale-independent Unicode Default
Case Conversion (UnicodeData + SpecialCasing, including the Final_Sigma
condition; e.g. U+0130 lowercases to `i` + U+0307, two scalars). Rust
`str::to_lowercase` implements the same mapping and is believed equivalent;
keep the `unicode` crates' Unicode version reasonably current and add a
parity test for U+0130 and final sigma. There is NO Turkish-locale special
casing on either side.

### 11.3 NFC

`value.normalize("NFC")` — use the `unicode-normalization` crate
(`nfc()` iterator). Theoretical divergence only if Unicode table versions
differ drastically; not observable for the app's realistic inputs.

### 11.4 String ordering is UTF-16 code-unit order

Every `<`/`>` string comparison in the fingerprint (rule sort, mapping sort,
`canonicalize` key sort) is JS relational comparison = lexicographic by
UTF-16 CODE UNITS. Rust `&str` `Ord` is lexicographic by UTF-8 bytes =
code-point order, which DIFFERS for strings mixing BMP chars in
U+E000..U+FFFF with astral chars (e.g. JS sorts "\u{10000}" BEFORE
"\u{FF61}" because 0xD800 < 0xFF61; Rust sorts the opposite). For byte
parity implement `cmp_utf16(a, b)` via `a.encode_utf16().cmp(b.encode_utf16())`
and use it for rule/mapping sorting. For MAP KEYS specifically (11.7) plain
byte order is safe because all keys are fixed ASCII identifiers.

### 11.5 `JSON.stringify` details that the fingerprint hash depends on

- Compact output: separators `","` / `":"`, no whitespace.
- String escaping: `"` → `\"`, `\` → `\\`; control chars U+0000–U+001F use
  the short escapes `\b \t \n \f \r` where applicable, else `\u00xx` with
  LOWERCASE hex (verified on Node v24: ``). Non-ASCII, U+2028/U+2029,
  and U+007F are emitted RAW (unescaped). Lone surrogates are emitted as
  `\udXXX` escapes (well-formed `JSON.stringify`, ES2019; verified:
  `\ud800`). `serde_json::to_string` matches all of this for well-formed
  strings (it also emits lowercase ``-style escapes and raw
  non-ASCII); lone surrogates cannot exist in Rust `String` at all — see A7.
- Object keys in insertion order (canonicalize pre-sorts them).
- `undefined` object values are dropped by canonicalize's filter before
  stringify ever sees them (and stringify would drop them anyway).
- Hash input is the UTF-8 encoding of that string; SHA-256; lowercase hex
  digest (64 chars).

### 11.6 Numbers

The fingerprint preimage contains no numbers (6.4) — no float formatting
risk there. The tracker `identityKey` DOES stringify `durationSeconds`
using ECMAScript number-to-string (shortest round-trip; `JSON.stringify(NaN)`
= `"null"`, `JSON.stringify(-0)` = `"0"`). The key is process-internal, so
the Rust port should use structural 6-tuple equality (`f64` bitwise-free
`==` is fine; NaN/-0 are unreachable through the pipeline, 7.2) instead of
reproducing JS float formatting.

### 11.7 Object key order / maps

JS objects preserve insertion order; `canonicalize` rebuilds objects in
sorted-key order so serialization order is fully determined. In Rust, build
the projection as `serde_json::Value::Object` backed by the default
`BTreeMap` (do NOT enable serde_json's `preserve_order` feature for this
code path): byte-order key sorting equals the JS UTF-16 sort here because
every object key in the projection is a fixed ASCII identifier. The VALUES
being sorted (rule `appId`, mapping `type`/`from`) are user data — those
sorts must use the UTF-16 comparator (11.4) and stable sort semantics
(`slice::sort_by` is stable, matching ES2019+ `Array.prototype.sort`; ties
keep config order).

### 11.8 The mapping-sort separator

`fingerprint.ts` builds mapping sort keys with a template containing `\\0` —
at runtime that is BACKSLASH + DIGIT ZERO (U+005C U+0030), not NUL (hex-dump
verified). Provably order-neutral for the three legal mapping types (6.3);
implementing "sort by (type, from), stable, UTF-16 comparator" is exactly
equivalent. Do not "fix" it to NUL thinking it changes anything, and do not
let a NUL-based implementation be flagged as divergence — they are
indistinguishable for schema-valid configs.

### 11.9 Nullish coalescing and optional chaining

- `a ?? b` falls through ONLY on `null`/`undefined` — never on `""`. All
  precedence chains (`displayAlias ?? mapped ?? raw`) rely on
  `normalizeText` returning `null` (never `""`) for absent values, so Rust
  `Option` chains (`or_else`) are exactly equivalent.
- `rule?.displayAlias` with no rule yields `undefined`;
  `normalizeText(undefined)` → `null`. Rust: `rule.and_then(|r| normalize_text(r.display_alias.as_deref()))`.
- `applyMapping` returns the mapping's `to` VERBATIM; a whitespace-only `to`
  survives until `normalizeText` inside the sanitizer nulls it, causing
  fallthrough to the raw captured name — preserve this two-stage behavior.

### 11.10 Optional key vs null

`ApplicationPrivacyRule.displayAlias` is an OPTIONAL KEY. `normalizedRule`
OMITS the key when the alias normalizes away; it never writes `null`. This
affects fingerprint bytes: model the field as
`#[serde(skip_serializing_if = "Option::is_none")]` (or build the JSON value
map conditionally). Serializing `"displayAlias":null` would change every
fingerprint containing an alias-less normalized rule.

### 11.11 Misc

- `Array.prototype.find` = first match in array order for both rules and
  mappings; do not sort before lookup.
- `randomUUID()` = UUID v4, lowercase, hyphenated; `uuid::Uuid::new_v4()`
  with default `Display` (lowercase) matches, and must come from a CSPRNG
  (privacy requirement: ids must not be predictable or content-derived).
- `Date.now()` = epoch ms; keep as integer (`i64`); `observedAt` is taken
  before the media await, so `media.playback.sampledAt >= observedAt` is
  possible and legal.
- `sanitizeMedia` passes `sampledAt` through with NO validation; providers
  always supply `Date.now()`.
- `MediaSnapshot.appId` derivation (capture layer): `/\.exe$/i` test then
  `toLowerCase()` — the regex is case-insensitive ASCII on the ".exe"
  suffix.
- `withTimeout` maps BOTH timeout and provider rejection to `null` and never
  cancels the underlying operation; no error from the media path may escape
  `captureForDelivery`.

## 12. Ambiguities found (also reported to the coordinator)

- A1: `scalarLength` is exported from `privacy/model.ts` with a comment
  claiming "the unit all wire limits use", but it has ZERO callers in
  `packages/core/src` — the wire limits in `protocol/dtoMapper.ts` use their
  own private `truncateScalars`. Port it into `privacy/model.rs` (it is in
  the module's public surface) but nothing in this pipeline calls it.
- A2: the spec request placed "position extrapolation
  (positionSeconds + sampledAt math)" and "debouncing" under
  MediaSessionTracker; in this codebase both live in the capture-layer media
  providers (agent M), not the tracker. Documented in 7.5 with the exact
  formula so the WinRT provider can replicate it; the tracker itself is pure
  identity-caching.
- A3: the TS fingerprint tests pin only equality/inequality RELATIONS. The
  literal hex vectors in 6.6 are spec-added, computed from the real
  implementation (method described in the header). The Rust suite should pin
  them; the parity reviewer should sign off on adopting them as normative.
- A4: `sanitizeMedia`'s paused branch (`state:"paused"`, `rate:0`) is
  implemented and test-pinned, but unreachable through
  `captureForDelivery` (the `raw.playing` gate drops paused media before
  sanitization). Keep both: the gate AND the paused support (the
  `PreviewProjection` schema and consent comments still name the paused
  state).
- A5: asymmetric identifier use for attribution-less media: RULE fallback
  matching uses `playerDisplayName ?? sourceAppUserModelId ?? ""`, while
  MAPPING lookup uses only `playerDisplayName` (null → no lookup). Appears
  deliberate (mappings target the displayed name) but is not comment-
  documented; replicate exactly.
- A6: the legacy `media_process_name` compatibility lookup is keyed by the
  CURRENT player display name. A legacy mapping whose `from` is an exe-style
  process name (e.g. `"Spotify.EXE"`, as in the model test fixture) can
  never match a display name like `"Spotify"` (`"spotify.exe"` ≠
  `"spotify"` after normalization) — the shim only rescues legacy entries
  whose `from` happens to equal today's display name. Replicate as-is; flag
  to the product owner as a possible latent compat gap.
- A7: fingerprint byte-parity for ill-formed strings: V8 keeps lone
  surrogates (WTF-16) and stringifies them as `\udXXX`; Rust `String` cannot
  hold them, and `serde_json` parsing of such escapes yields U+FFFD. Config
  is produced by our UI and zod-validated, so this is unreachable in
  practice — but a parity fuzzer comparing fingerprints over arbitrary JSON
  configs would hit it.
- A8: the `\\0` mapping-sort separator (11.8) is almost certainly a typo for
  NUL (`\0`). It is provably behavior-neutral for schema-valid configs, so
  the Rust port must NOT "fix" the observable behavior; noted for an
  upstream TS cleanup.



