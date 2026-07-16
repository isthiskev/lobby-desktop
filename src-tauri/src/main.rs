// Lobby Desktop — a thin native shell around the Lobby web app.
//
// The main window is created in `setup` (not declaratively in tauri.conf.json)
// so we can attach an `on_navigation` handler. Third-party sign-in (Google,
// Discord, Twitch, Steam, Epic) cannot run inside an embedded webview —
// providers detect it and refuse to render (blank window). So OAuth runs in
// the user's DEFAULT BROWSER: we cancel the in-app navigation, reopen the URL
// externally with `platform=mobile` (the API then finishes on the same
// `lobby://auth/callback` deep link the mobile app uses), and Windows routes
// that link back to us — single-instance forwards it from the second process,
// and the handler drops the token into the main webview's callback page.
//
// Two OS-integration preferences live in the web app's Settings screen and
// drive the shell over IPC:
//   • "Launch at startup"   → tauri-plugin-autostart registers this exe (with a
//     `--minimized` flag) in the Windows startup list.
//   • "Run in the background" → closing the window hides it to the system tray
//     instead of quitting. The choice is persisted so both the close handler
//     and a startup launch honour it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Listener, Manager, Url, WebviewUrl, WebviewWindowBuilder, Window, WindowEvent,
};
use tauri_plugin_autostart::{ManagerExt, MacosLauncher};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_store::StoreExt;
use sysinfo::{ProcessesToUpdate, System};

const WEB_URL: &str = "https://joinlobby.gg/?shell=desktop";
const WEB_HOST: &str = "joinlobby.gg";
const MAIN_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36 LobbyDesktop/0.2.1";

const STORE_FILE: &str = "settings.json";
const RIB_KEY: &str = "runInBackground";
const AUTOSTART_INIT_KEY: &str = "autostartInitialized";
// GitHub redirects this to the newest release's tag page (…/tag/vX.Y.Z); we read
// the version from the redirected URL and send users here to download.
const RELEASES_LATEST_URL: &str = "https://github.com/isthiskev/lobby-desktop/releases/latest";
// The Lobby API — used to pull the weekly-refreshed game-detection database.
const API_URL: &str = "https://admin-production-6de6.up.railway.app";

/// Live shell preferences, shared between the IPC commands and the window
/// close handler.
#[derive(Default)]
struct Settings {
    run_in_background: AtomicBool,
}

fn main() {
    tauri::Builder::default()
        // Single instance MUST be registered first. A lobby:// deep link (the
        // OAuth return) launches a second process on Windows; this forwards
        // its argv to the running app — the "deep-link" cargo feature then
        // delivers the URL to on_open_url — and we just refocus the window.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        // On Windows the MacosLauncher is ignored; `--minimized` is appended to
        // the registered startup command so an autostart launch can boot to the
        // tray (see `start_hidden` below).
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .plugin(tauri_plugin_dialog::init())
        .on_window_event(|window, event| {
            // Only the main window closes to the tray.
            if window.label() != "main" {
                return;
            }
            if let WindowEvent::CloseRequested { api, .. } = event {
                if background_enabled(window) {
                    // Keep the process alive in the tray instead of exiting.
                    api.prevent_close();
                    let _ = window.hide();
                }
                // Otherwise let the close proceed — with no other window left,
                // the app exits normally.
            }
        })
        .setup(|app| {
            // "Run in the background" defaults ON — the flag is only false if the
            // user explicitly turned it off (persisted).
            let run_in_background = app
                .store(STORE_FILE)
                .ok()
                .and_then(|s| s.get(RIB_KEY))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            app.manage(Settings {
                run_in_background: AtomicBool::new(run_in_background),
            });

            // "Launch at startup" also defaults ON: enable it once on first run
            // (tracked by a store flag) so we never re-enable it if the user
            // later turns it off. The startup entry passes `--minimized`, so a
            // boot launch with run-in-background on starts straight to the tray
            // with no window (see `start_hidden` below).
            if let Ok(store) = app.store(STORE_FILE) {
                let initialized = store
                    .get(AUTOSTART_INIT_KEY)
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !initialized {
                    let _ = app.autolaunch().enable();
                    store.set(AUTOSTART_INIT_KEY, true);
                    let _ = store.save();
                }
            }

            build_tray(app.handle())?;
            spawn_update_check(app.handle());

            // The web app runs from a REMOTE origin, which Tauri's ACL only lets
            // call plugin/core commands (window controls, autostart) — never our
            // own custom commands — AND backend→frontend event delivery to a
            // remote webview isn't reliable. So every web↔shell channel is a
            // frontend→backend EVENT (a plain command invocation, which works):
            //   • "set-run-in-background" (payload = bool) updates the live flag +
            //     persists it for the close handler / next boot.
            //   • "set-auth-token" (payload = JWT, "" when signed out) lets the
            //     shell report game activity to the API itself (scan thread below)
            //     rather than emitting the game list back to the web page.
            let rib_handle = app.handle().clone();
            app.listen("set-run-in-background", move |event| {
                let enabled = event.payload().trim() == "true";
                if let Some(settings) = rib_handle.try_state::<Settings>() {
                    settings.run_in_background.store(enabled, Ordering::Relaxed);
                }
                if let Ok(store) = rib_handle.store(STORE_FILE) {
                    store.set(RIB_KEY, enabled);
                    let _ = store.save();
                }
            });

            app.listen("set-auth-token", |event| {
                // Payload is JSON-encoded (a quoted string); "" ⇒ signed out.
                let token = serde_json::from_str::<String>(event.payload())
                    .ok()
                    .filter(|s| !s.is_empty());
                if let Ok(mut w) = auth_token().write() {
                    *w = token;
                }
            });

            // Detect running games and report them straight to the API, using the
            // token the web app relays above. The shell owns the whole loop —
            // scan → dedupe per UTC day → POST — because we can't reliably hand the
            // game list to the remote web page for it to post instead.
            let scan_handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(15));
                loop {
                    report_active_games(&scan_handle);
                    std::thread::sleep(std::time::Duration::from_secs(60));
                }
            });

            // Refresh the game-detection database from the API on launch (after a
            // short settle) and then weekly, so new games are detected without a
            // reinstall. Falls back to the bundled baseline until the first fetch.
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(25));
                loop {
                    refresh_games_db();
                    std::thread::sleep(std::time::Duration::from_secs(7 * 24 * 3600));
                }
            });

            // When Windows starts us via the autostart entry (which passes
            // `--minimized`) and background mode is on, boot straight to the
            // tray with no visible window.
            let start_hidden =
                run_in_background && std::env::args().any(|a| a == "--minimized");

            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(Url::parse(WEB_URL)?))
                .title("Lobby")
                .inner_size(1536.0, 864.0)
                .resizable(false)
                .maximizable(false)
                .decorations(false)
                .center()
                .visible(!start_hidden)
                .user_agent(MAIN_UA)
                .on_navigation(move |url| {
                    // OAuth start hops off our origin to the API/provider —
                    // providers refuse to render sign-in inside an embedded
                    // webview, so run the flow in the default browser instead.
                    // `platform=mobile` makes the API finish on the lobby://
                    // deep link (the mobile app's return path), which Windows
                    // routes back to this app (see handle_deep_link).
                    if url.path().contains("/auth/oauth/") {
                        let mut browser = url.clone();
                        let query = match browser.query() {
                            Some(q) if !q.is_empty() => format!("{q}&platform=mobile"),
                            _ => "platform=mobile".to_string(),
                        };
                        browser.set_query(Some(&query));
                        let _ = open::that(browser.as_str());
                        return false;
                    }
                    true
                })
                .build()?;

            // OAuth return path. The lobby:// scheme is registered with Windows
            // by the installer (deep-link plugin config); register again at
            // runtime so portable/dev runs work too. While the app runs, links
            // arrive via on_open_url (forwarded by single-instance); on a cold
            // start the launching URL sits in our own argv (get_current).
            let _ = app.deep_link().register_all();
            let dl_handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    handle_deep_link(&dl_handle, &url);
                }
            });
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                for url in urls {
                    handle_deep_link(app.handle(), &url);
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Lobby");
}

/// Read the current "run in the background" flag from managed state.
fn background_enabled(window: &Window) -> bool {
    window
        .try_state::<Settings>()
        .map(|s| s.run_in_background.load(Ordering::Relaxed))
        .unwrap_or(false)
}

/// Build the system-tray icon. Left-click restores the window; the menu offers
/// an explicit Show / Quit. Quit is the only way to fully exit while background
/// mode keeps the app alive after the window is closed.
fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Lobby", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Lobby", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    let mut builder = TrayIconBuilder::new()
        .tooltip("Lobby")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}

/// Reveal and focus the main window (from the tray).
fn show_main(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

/// On launch, check GitHub for a newer release and, if there is one, offer to
/// open the download page. Runs off the UI thread; prompts at most once per new
/// version (the choice is remembered so we don't nag every launch).
fn spawn_update_check(app: &tauri::AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        // Let the window finish opening before we might interrupt with a dialog.
        std::thread::sleep(std::time::Duration::from_secs(3));

        let Some(latest_str) = latest_release_version() else {
            return;
        };
        let Ok(latest) = semver::Version::parse(&latest_str) else {
            return;
        };
        let current = app.package_info().version.clone();
        if latest <= current {
            return; // already up to date
        }
        if dismissed_update(&app).as_deref() == Some(latest_str.as_str()) {
            return; // already offered this version
        }

        let app_cb = app.clone();
        let version_cb = latest_str.clone();
        app.dialog()
            .message(format!(
                "Lobby {latest} is available — you're on {current}. Open the download page?"
            ))
            .title("Update available")
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Download".to_string(),
                "Later".to_string(),
            ))
            .show(move |download| {
                if download {
                    let _ = open::that(RELEASES_LATEST_URL);
                }
                // Remember we've shown this version either way, so we don't ask
                // again for it on every launch (a newer release will re-prompt).
                set_dismissed_update(&app_cb, &version_cb);
            });
    });
}

/// The version string of the newest GitHub release, read from the URL that
/// `…/releases/latest` redirects to (`…/tag/vX.Y.Z`). No API token or rate-limit
/// concerns, and no JSON parsing. `None` on any network/parse failure.
fn latest_release_version() -> Option<String> {
    let resp = ureq::get(RELEASES_LATEST_URL)
        .set("User-Agent", "lobby-desktop")
        .call()
        .ok()?;
    let tag = resp.get_url().rsplit('/').next()?.to_string();
    let version = tag.strip_prefix('v').unwrap_or(&tag);
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

fn dismissed_update(app: &tauri::AppHandle) -> Option<String> {
    app.store(STORE_FILE)
        .ok()?
        .get("dismissedUpdate")?
        .as_str()
        .map(|s| s.to_string())
}

fn set_dismissed_update(app: &tauri::AppHandle, version: &str) {
    if let Ok(store) = app.store(STORE_FILE) {
        store.set("dismissedUpdate", version);
        let _ = store.save();
    }
}

/// Game-detection database: executable basename (lowercased) → display name.
/// Bundled from the public Discord "detectable games" dataset (~9,260 games),
/// kept in lockstep with `packages/api/src/lib/games.json`. Parsed once. We only
/// ever emit a NAME from this fixed database — never an arbitrary process name.
// Starts from the compiled-in baseline, then gets hot-swapped weekly from the
// API's `/telemetry/games-db` (which the server refreshes from Discord). RwLock
// so the 60s scan can read while a refresh occasionally replaces it.
static GAME_DB: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();
fn game_db() -> &'static RwLock<HashMap<String, String>> {
    GAME_DB.get_or_init(|| {
        RwLock::new(serde_json::from_str(include_str!("../games.json")).unwrap_or_default())
    })
}

/// The signed-in user's auth token, relayed from the web app via "set-auth-token"
/// (`None` when signed out). Used ONLY to POST game activity — the API resolves
/// country from it, then discards identity.
static AUTH_TOKEN: OnceLock<RwLock<Option<String>>> = OnceLock::new();
fn auth_token() -> &'static RwLock<Option<String>> {
    AUTH_TOKEN.get_or_init(|| RwLock::new(None))
}

/// "game|day-number" keys already reported this run, so a game left open is only
/// counted once per UTC day per device.
static REPORTED: OnceLock<RwLock<HashSet<String>>> = OnceLock::new();
fn reported() -> &'static RwLock<HashSet<String>> {
    REPORTED.get_or_init(|| RwLock::new(HashSet::new()))
}

/// Pull the current game-detection database from the API and swap it in, so
/// newly-released games are detected without a reinstall. Keeps the current DB
/// on any network/parse failure or an implausibly small result.
fn refresh_games_db() {
    let url = format!("{API_URL}/telemetry/games-db");
    let Ok(resp) = ureq::get(&url).set("User-Agent", "lobby-desktop").call() else {
        return;
    };
    let Ok(text) = resp.into_string() else { return };
    let next: HashMap<String, String> = match serde_json::from_str(&text) {
        Ok(m) => m,
        Err(_) => return,
    };
    if next.len() > 1000 {
        if let Ok(mut w) = game_db().write() {
            *w = next;
        }
    }
}

/// Scan running processes and return the names of any known games currently
/// running. Called every 60s by `report_active_games`, which POSTs each to the
/// API for anonymous, country-only aggregation. We read ONLY process names and
/// match them against the detection database — nothing else.
fn scan_active_games() -> Vec<String> {
    let db = match game_db().read() {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let mut found: Vec<String> = Vec::new();
    for process in sys.processes().values() {
        let name = process.name().to_string_lossy().to_ascii_lowercase();
        if let Some(game) = db.get(name.as_str()) {
            if !found.iter().any(|g| g == game) {
                found.push(game.clone());
            }
        }
    }
    found
}

/// One scan+report cycle: detect running games and POST each (once per UTC day)
/// to the API, which tags it only with the country it derives from the token.
/// While signed out there's no token to attribute to, so it just logs and skips.
fn report_active_games(app: &tauri::AppHandle) {
    let games = scan_active_games();
    let token = auth_token().read().ok().and_then(|g| g.clone());
    log_debug(app, &games, token.is_some());
    let Some(token) = token else { return };
    let day = now_day();
    for game in games {
        let key = format!("{game}|{day}");
        if reported().read().map(|r| r.contains(&key)).unwrap_or(false) {
            continue;
        }
        if post_game_activity(&token, &game) {
            if let Ok(mut w) = reported().write() {
                w.insert(key);
            }
        }
    }
}

/// Whole days since the Unix epoch (UTC) — a stable per-day key for dedupe.
fn now_day() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0)
}

/// POST one detected game to the telemetry endpoint. Returns whether it was
/// accepted (2xx). The API stores only country+game+day; the token resolves the
/// country and is then discarded.
fn post_game_activity(token: &str, game: &str) -> bool {
    let url = format!("{API_URL}/telemetry/game-activity");
    let body = serde_json::json!({ "game": game }).to_string();
    ureq::post(&url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .send_string(&body)
        .is_ok()
}

/// Append one line per scan to a small log beside our settings, so a "no stats
/// showing up" report can be diagnosed (was a game detected? was a token
/// present?) without a debug build. Best-effort; never fails the scan.
fn log_debug(app: &tauri::AppHandle, games: &[String], has_token: bool) {
    let Ok(dir) = app.path().app_config_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("t={epoch} token={has_token} games={games:?}\n");
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("gametrack.log"))
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Route a `lobby://` deep link (the OAuth return from the default browser)
/// back into the web app inside the main window. Sign-in returns
/// `lobby://auth/callback#token=…` — the fragment carries the session, and the
/// web app's /auth/callback page completes the login exactly as on the web.
/// Provider-LINK flows (from Profile) return `…?linked=…` / `…?error=…`
/// queries instead, which belong on the profile page.
fn handle_deep_link(app: &tauri::AppHandle, url: &Url) {
    if url.scheme() != "lobby" {
        return;
    }
    // lobby://auth/callback → host "auth", path "/callback".
    let is_auth = url.host_str() == Some("auth") && url.path().starts_with("/callback");
    let target = if !is_auth {
        None
    } else if let Some(fragment) = url.fragment() {
        Some(format!("https://{WEB_HOST}/auth/callback#{fragment}"))
    } else if let Some(query) = url.query() {
        Some(format!("https://{WEB_HOST}/profile?{query}"))
    } else {
        None
    };
    if let Some(target) = target {
        if let Some(main) = app.get_webview_window("main") {
            let escaped = target.replace('\\', "\\\\").replace('\'', "\\'");
            let _ = main.eval(&format!("window.location.replace('{escaped}')"));
        }
    }
    // Whatever the link carried, surface the app (it may be hidden in the tray).
    show_main(app);
}
