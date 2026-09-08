# Lobby Desktop

The Lobby platform (https://lobby-web.vercel.app) as a native Windows desktop
app — a lightweight Tauri shell that opens the web app in its own window.
(Note: lobby.gg is NOT ours — when a real custom domain lands, update
`src-tauri/tauri.conf.json` and cut a new release.)

Not to be confused with [lobby-tauri](https://github.com/isthiskev/lobby-tauri),
the Fortnite replay-upload companion tool.

## Download

Grab the installer from the
[latest release](https://github.com/isthiskev/lobby-desktop/releases/latest):

- `Lobby-setup.exe` — recommended installer (NSIS)
- `Lobby.msi` — MSI package

Windows 10/11, x64. The installer is unsigned, so SmartScreen may warn —
choose **More info → Run anyway**.

## Updates

The app updates itself (Tauri updater, since 0.4.0). On launch and then hourly
(it lives in the tray for weeks) it reads the `latest.json` manifest attached
to the newest GitHub release. If that's newer, a native dialog offers
**Update now**: the installer is downloaded, verified against the signing key
baked into the app, run with a passive progress UI, and Lobby restarts on the
new version. **Later** snoozes that version for a day; a newer version always
asks again. While hidden in the tray it never prompts — the offer waits for
the window. If the automatic install fails it falls back to offering the
download page.

### Releasing

1. Bump `version` in `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, and
   `package.json` (keep them identical).
2. `npm run build` — signs the artifacts with `~/.tauri/lobby-desktop.key` and
   stages `dist-release/` (`Lobby-setup.exe`, `Lobby.msi`, `latest.json`).
3. Create GitHub release `vX.Y.Z` and upload **all three** files.
   `latest.json` is what running apps poll (via
   `releases/latest/download/latest.json`) — a release without it is invisible
   to the updater.

The private key at `~/.tauri/lobby-desktop.key` (no password) is the identity
of the update channel: lose it and shipped apps can never auto-update again
(manual reinstall only), so keep a backup. Never commit it.

Builds ≤ 0.3.0 used a zero-setup check (read the tag `releases/latest`
redirects to, offer the download page); tagging releases `vX.Y.Z` with a
`Lobby-setup.exe` asset keeps those users prompted too.

## Development

```bash
npm install
npm run dev            # run against the live site in a dev window
npm run build          # signed release build + dist-release/ staging
npm run tauri build    # unsigned local build (won't be accepted by the updater)
```
