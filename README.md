# Yohaku Companion for Windows

Privacy-first Windows companion for Yohaku Live Desk. The app captures the
foreground application, an optional window title, and Windows SMTC media state,
sanitizes the data locally, and publishes the current projection through
Companion Protocol v2.

This is a Windows implementation inspired by
[Innei/YohakuCompanion](https://github.com/Innei/YohakuCompanion). It uses a
native Rust core (running inside the Tauri v2 process) and a React settings
UI.

## Privacy model

- Pairing installs a credential but never enables Live Desk.
- Enabling Live Desk requires reviewing and confirming the current sanitized
  preview. A privacy or source change invalidates that confirmation.
- Outbound data has no executable path, process ID, raw application identifier,
  credentials, screenshots, or keystrokes.
- Window titles are disabled by default and require both the global switch and
  an application rule that allows sharing.
- Device credentials use Windows Credential Manager, with an encrypted DPAPI
  file fallback when Credential Manager is unavailable.
- There is no telemetry. Logs exclude window titles and media text.

## Requirements

- Windows 10 1809 or later; Windows 11 recommended
- Node.js 24.15.0 and pnpm 11 (web UI build)
- Rust stable and MSVC Build Tools

## Development

```text
pnpm install
pnpm test
pnpm typecheck
pnpm --filter @yohaku/app build
```

Run the Rust core tests (workspace covers the core crate and the Tauri shell):

```text
cd packages/app/src-tauri
cargo test --workspace
```

Run the Tauri app in development mode:

```text
pnpm dev:app
```

Build the unsigned per-user NSIS installer:

```text
pnpm dist
```

The GitHub release workflow runs the TypeScript and Rust test suites and
builds the installer on a clean Windows runner. There is no bundled Node.js
sidecar anymore; the core is compiled into the app binary.

## Data locations

- `%APPDATA%\yohaku-companion-win\config.json`: non-sensitive configuration
- `%APPDATA%\yohaku-companion-win\sequence.json`: protocol sequence state
- Windows Credential Manager or the DPAPI fallback: device credentials

## Scope

The first Windows release implements Companion Protocol v2 only. It does not
include legacy MixSpace, Slack, Discord, Moments, S3 artwork hosting, or media
playback-link publishing. Artwork and playback-link fields are omitted unless
those capabilities are explicitly negotiated in a future version.

## License

GPLv3. See [LICENSE](LICENSE).
