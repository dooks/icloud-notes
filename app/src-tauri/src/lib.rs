//! Tauri host (WP-H): windows, login, the §3.4 commands and the `notes-asset://` protocol.
//!
//! Commands delegate to a `notes_service::Service` in Tauri state; until Wave 2 they use `mock::MockService`.
//! Extra Rust-side parameters (`State`, `AppHandle`, `Window`) may be added freely: the JS contract is
//! only the argument names and the JSON results.

// Wave 0 stubs: remove once implemented.
#![allow(unused_variables)]

mod mock;

use notes_model::{CmdError, FolderView, NoteHtml, NoteSummary, RefreshResult, Status};

/// `status` → `{signedIn, syncing, lastSync?, error?}`.
#[tauri::command]
async fn status() -> Result<Status, CmdError> {
    todo!("WP-H")
}

/// `sign_in`: open the login window; resolves once `X-APPLE-WEBAUTH-TOKEN` is captured into the jar.
#[tauri::command]
async fn sign_in(app: tauri::AppHandle) -> Result<(), CmdError> {
    todo!("WP-H")
}

/// `sign_out`: delete the jar and clear the login WebView2 profile's browsing data.
#[tauri::command]
async fn sign_out(app: tauri::AppHandle) -> Result<(), CmdError> {
    todo!("WP-H")
}

/// `refresh` → `{changed}`.
#[tauri::command]
async fn refresh() -> Result<RefreshResult, CmdError> {
    todo!("WP-H")
}

/// `folders` → `FolderView[]`.
#[tauri::command]
async fn folders() -> Result<Vec<FolderView>, CmdError> {
    todo!("WP-H")
}

/// `notes {folderId}` → `NoteSummary[]`, newest first. `folderId` may be `"quick-notes"`.
#[tauri::command]
async fn notes(folder_id: String) -> Result<Vec<NoteSummary>, CmdError> {
    todo!("WP-H")
}

/// `search {q}` → `NoteSummary[]`.
#[tauri::command]
async fn search(q: String) -> Result<Vec<NoteSummary>, CmdError> {
    todo!("WP-H")
}

/// `note_html {noteId, dark}` → `{title, html}` (a full page for `srcdoc`).
#[tauri::command]
async fn note_html(note_id: String, dark: bool) -> Result<NoteHtml, CmdError> {
    todo!("WP-H")
}

/// `open_link {url}`: re-check the scheme (http/https/mailto only), then open in the OS browser.
#[tauri::command]
async fn open_link(app: tauri::AppHandle, url: String) -> Result<(), CmdError> {
    todo!("WP-H")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            status, sign_in, sign_out, refresh, folders, notes, search, note_html, open_link
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
