# Yohaku Companion for Windows

Privacy-first Windows companion for Yohaku Live Desk. The app captures the
foreground application, an optional window title, and Windows SMTC media state,
sanitizes the data locally, and publishes the current projection through
Companion Protocol v2.

This is a Windows implementation inspired by
[Innei/YohakuCompanion](https://github.com/Innei/YohakuCompanion). It uses a
Node.js TypeScript core and a Tauri v2 desktop shell with a React settings UI.

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
- Node.js 24.15.0 and pnpm 11
- Rust stable and MSVC Build Tools for Tauri development/builds
- PowerShell 5.1 or later

## Development

```text
pnpm install
pnpm test
pnpm typecheck
pnpm --filter @yohaku/app build
```

Run the headless core:

```text
pnpm dev:core
pnpm --filter @yohaku/core mock-server
pnpm --filter @yohaku/core smoke:foreground
pnpm --filter @yohaku/core smoke:media
```

Run the Tauri app in development mode:

```text
pnpm dev:app
```

Build the unsigned per-user NSIS installer:

```text
pnpm dist
```

The release process downloads the pinned Node.js sidecar, verifies its
SHA-256 against the official `SHASUMS256.txt`, stages the core, and builds the
installer. The GitHub release workflow performs the same steps on a clean
Windows runner.

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

MIT. See [LICENSE](LICENSE).
