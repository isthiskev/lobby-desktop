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

On launch the app checks the GitHub releases for a newer version (by reading the
tag that `releases/latest` redirects to) and, if one exists, shows a native
"Update available" dialog offering to open the download page. It asks at most
once per new version. So **publishing a new GitHub release is all it takes** to
prompt existing users — no manifest or signing key required.

> Want fully automatic one-click updates (download + install + relaunch, no
> manual step)? That's the Tauri updater — it needs a signing key and each
> release to ship signed artifacts + a `latest.json`. Worth adding once releases
> are built in CI; the current prompt is the zero-setup version.

## Development

```bash
npm install
npm run dev     # run against lobby.gg in a dev window
npm run build   # produce installers in src-tauri/target/release/bundle/
```
