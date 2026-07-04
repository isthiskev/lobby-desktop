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

## Development

```bash
npm install
npm run dev     # run against lobby.gg in a dev window
npm run build   # produce installers in src-tauri/target/release/bundle/
```
