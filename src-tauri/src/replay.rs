// Fortnite replay auto-upload.
//
// Folded in from the standalone "Lobby Replay" app (lobby-tauri) so a player
// installs ONE thing. Same server contract: POST the raw .replay to
// /client/ingest and let the API work out the tournament, lobby and game from
// the Epic ids inside the file — no auth, no tournament picker, no config.
//
// Two things the standalone app got wrong, fixed here:
//
//   • It uploaded on any filesystem event behind a 3-second debounce. Fortnite
//     writes the replay CONTINUOUSLY while you play, so a 12-minute match
//     would have fired upload after upload of a half-written file, each one
//     overwriting that lobby's results with mid-match standings. Here a file
//     must stop growing for QUIET before it is sent, and each (path, size) is
//     sent at most once.
//
//   • Its API host was user-editable and persisted. The copy on this machine
//     still held "https://api.lobby.gg" — a domain that does not resolve — so
//     every upload would have failed with nothing on screen to say why. The
//     host is a constant now, shared with the rest of the shell.
//
// Why polling and not a filesystem watcher: we need "has this file stopped
// growing" either way, and a 5-second directory listing of one folder is
// cheaper to reason about than debounced events plus a stability check on top.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tauri::menu::MenuItem;
use tauri::{AppHandle, Emitter, Wry};

/// How long a replay must stop changing before we treat the match as over.
const QUIET: Duration = Duration::from_secs(20);
/// How often the Demos folder is listed.
const POLL: Duration = Duration::from_secs(5);
/// Anything smaller than this is not a real match recording.
const MIN_BYTES: u64 = 64 * 1024;
/// A 100 MB replay over a thin upstream needs room; the API caps at 300 MB.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// The tray line that reports what the uploader last did. Registered by
/// `build_tray` so this module can write to it without owning the menu.
static STATUS_ITEM: OnceLock<MenuItem<Wry>> = OnceLock::new();

pub fn set_status_item(item: MenuItem<Wry>) {
    let _ = STATUS_ITEM.set(item);
}

fn set_status(app: &AppHandle, text: &str) {
    eprintln!("[replay] {text}");
    if let Some(item) = STATUS_ITEM.get() {
        let _ = item.set_text(text);
    }
    // The web app may one day render this; harmless when nothing listens.
    let _ = app.emit("replay-upload", text.to_string());
}

/// `%LOCALAPPDATA%\FortniteGame\Saved\Demos` — where Fortnite writes both saved
/// and "unsaved" replays. Same default the standalone app shipped with.
fn demos_dir() -> Option<PathBuf> {
    let local = std::env::var("LOCALAPPDATA").ok()?;
    let dir = Path::new(&local)
        .join("FortniteGame")
        .join("Saved")
        .join("Demos");
    dir.is_dir().then_some(dir)
}

fn replays_in(dir: &Path) -> Vec<(PathBuf, u64)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("replay") {
                return None;
            }
            let len = e.metadata().ok()?.len();
            Some((path, len))
        })
        .collect()
}

/// Start watching in the background. Never fails loudly — a machine without
/// Fortnite installed simply has nothing to watch.
pub fn start(app: AppHandle) {
    let Some(dir) = demos_dir() else {
        eprintln!("[replay] no Fortnite Demos folder — auto-upload idle");
        return;
    };

    std::thread::spawn(move || {
        // Everything already on disk at launch is recorded as-is and NOT
        // uploaded: opening the app must not re-submit the back catalogue.
        // Only a file that appears or changes from here on counts as "a match
        // you just played". The tray's "Upload latest replay" covers the case
        // where the app was closed while you played.
        let mut sizes: HashMap<PathBuf, (u64, Instant)> = HashMap::new();
        let mut sent: HashMap<PathBuf, u64> = HashMap::new();
        for (path, len) in replays_in(&dir) {
            sizes.insert(path.clone(), (len, Instant::now()));
            sent.insert(path, len);
        }
        eprintln!(
            "[replay] watching {} ({} existing file(s) skipped)",
            dir.display(),
            sent.len()
        );

        loop {
            std::thread::sleep(POLL);
            for (path, len) in replays_in(&dir) {
                match sizes.get(&path) {
                    // Grew (or shrank — a fresh recording reusing the name):
                    // restart its quiet clock.
                    Some((prev, _)) if *prev != len => {
                        sizes.insert(path.clone(), (len, Instant::now()));
                    }
                    // Steady. Send it once it has been steady long enough and
                    // this exact size has not already gone up.
                    Some((_, since)) => {
                        if since.elapsed() >= QUIET
                            && len >= MIN_BYTES
                            && sent.get(&path) != Some(&len)
                        {
                            sent.insert(path.clone(), len);
                            upload(&app, &path);
                        }
                    }
                    // First sighting after startup — a new match.
                    None => {
                        sizes.insert(path.clone(), (len, Instant::now()));
                    }
                }
            }
        }
    });
}

/// Send the newest replay on disk, whatever its age. Manual escape hatch for
/// "I played before opening the app" — and the way to retry a failed upload.
pub fn upload_latest(app: AppHandle) {
    std::thread::spawn(move || {
        let Some(dir) = demos_dir() else {
            set_status(&app, "Replay upload: no Fortnite folder");
            return;
        };
        let newest = replays_in(&dir)
            .into_iter()
            .filter(|(_, len)| *len >= MIN_BYTES)
            .filter_map(|(path, _)| {
                let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
                Some((path, modified))
            })
            .max_by_key(|(_, modified)| *modified)
            .map(|(path, _)| path);

        match newest {
            Some(path) => upload(&app, &path),
            None => set_status(&app, "Replay upload: nothing to send"),
        }
    });
}

fn upload(app: &AppHandle, path: &Path) {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("replay.replay")
        .to_string();

    set_status(app, &format!("Uploading {name}…"));

    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            set_status(app, &format!("Replay upload failed: {e}"));
            return;
        }
    };

    // Hand-rolled multipart: the shell already speaks ureq, and one form field
    // is not worth a second HTTP stack.
    let boundary = format!("----lobby{}", std::process::id());
    let mut body = Vec::with_capacity(bytes.len() + 512);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let res = ureq::post(&format!("{}/client/ingest", crate::API_URL))
        .timeout(UPLOAD_TIMEOUT)
        .set(
            "Content-Type",
            &format!("multipart/form-data; boundary={boundary}"),
        )
        .send_bytes(&body);

    // The endpoint answers with either {matched:true, tournamentId, gameNumber,
    // lobbyNumber, inserted, unmatched} or {matched:false, reason} — a casual
    // match that belongs to no tournament is a normal outcome, not an error.
    // ureq is pulled in without its "json" feature, so decode by hand — the
    // same way the games-DB refresh does.
    let as_json = |r: ureq::Response| -> Option<serde_json::Value> {
        r.into_string().ok().and_then(|t| serde_json::from_str(&t).ok())
    };

    match res {
        Ok(r) => match as_json(r).ok_or("unreadable response") {
            Ok(j) if j.get("matched").and_then(|m| m.as_bool()) == Some(true) => {
                let inserted = j.get("inserted").and_then(|v| v.as_u64()).unwrap_or(0);
                let unmatched = j.get("unmatched").and_then(|v| v.as_u64()).unwrap_or(0);
                let game = j.get("gameNumber").and_then(|v| v.as_u64()).unwrap_or(1);
                set_status(
                    app,
                    &format!("Game {game} scored — {inserted} player(s), {unmatched} unmatched"),
                );
            }
            Ok(j) => {
                let reason = j
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("not part of a tournament");
                set_status(app, &format!("Replay ignored — {reason}"));
            }
            Err(e) => set_status(app, &format!("Replay upload: bad response ({e})")),
        },
        Err(ureq::Error::Status(code, r)) => {
            let detail = as_json(r)
                .and_then(|j| j.get("error").and_then(|v| v.as_str()).map(str::to_owned))
                .unwrap_or_else(|| format!("HTTP {code}"));
            set_status(app, &format!("Replay upload failed: {detail}"));
        }
        Err(e) => set_status(app, &format!("Replay upload failed: {e}")),
    }
}
