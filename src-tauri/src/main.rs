// Lobby Desktop — a thin native shell around https://lobby.gg.
// The window URL lives in tauri.conf.json; there is no IPC surface.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running Lobby");
}
