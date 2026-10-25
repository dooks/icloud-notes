//! The facade `app/src-tauri` calls (WP-J, Wave 2). Every method maps 1:1 to a §3.4 command
//! or to the `notes-asset://` protocol. Errors are already [`CmdError`]s.

// Wave 0 stubs: remove once implemented.
#![allow(unused_variables)]

use std::path::PathBuf;
use std::sync::Arc;

use icloud_session::CookieJar;
use notes_model::{AssetVariant, CmdError, FolderView, NoteHtml, NoteSummary, RefreshResult, Status, SyncProgress};

/// Events the service raises; the host forwards them to the UI as Tauri events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceEvent {
    /// `sync-progress {pages, records}`.
    SyncProgress(SyncProgress),
    /// `sync-done`.
    SyncDone,
    /// `auth-required`.
    AuthRequired,
}

/// Where the service sends [`ServiceEvent`]s (the host implements this over `AppHandle::emit`).
pub trait EventSink: Send + Sync {
    fn emit(&self, event: ServiceEvent);
}

/// The app's backend. `Send + Sync`, held in Tauri state.
pub struct Service {
    _private: (),
}

impl Service {
    /// Open the cache and jar under `data_dir` (`%LOCALAPPDATA%\com.ziggy.icloud-notes`). No network.
    pub fn open(data_dir: PathBuf, events: Arc<dyn EventSink>) -> Result<Self, CmdError> {
        todo!("WP-J")
    }

    /// Validate the stored jar (emit `AuthRequired` on an auth error), then sync and hydrate in the background.
    pub async fn start(&self) -> Result<(), CmdError> {
        todo!("WP-J")
    }

    /// `status` command.
    pub fn status(&self) -> Status {
        todo!("WP-J")
    }

    /// Called by the host after the login window captured cookies: replace and persist the jar, then [`Service::start`].
    pub async fn signed_in(&self, jar: CookieJar) -> Result<(), CmdError> {
        todo!("WP-J")
    }

    /// `sign_out` command (service half): delete the jar. The host clears the WebView2 profile.
    pub async fn sign_out(&self) -> Result<(), CmdError> {
        todo!("WP-J")
    }

    /// `refresh` command: cheap zone check, then delta sync and hydration.
    pub async fn refresh(&self) -> Result<RefreshResult, CmdError> {
        todo!("WP-J")
    }

    /// `folders` command.
    pub fn folders(&self) -> Result<Vec<FolderView>, CmdError> {
        todo!("WP-J")
    }

    /// `notes` command. `folder_id` is a folder recordName or `notes_model::QUICK_NOTES_VIEW_ID`.
    pub fn notes(&self, folder_id: &str) -> Result<Vec<NoteSummary>, CmdError> {
        todo!("WP-J")
    }

    /// `search` command.
    pub fn search(&self, q: &str) -> Result<Vec<NoteSummary>, CmdError> {
        todo!("WP-J")
    }

    /// `note_html` command: decode the body, resolve attachments, render, wrap in `page`.
    pub fn note_html(&self, note_id: &str, dark: bool) -> Result<NoteHtml, CmdError> {
        todo!("WP-J")
    }

    /// `notes-asset://localhost/<attachment_id>/<variant>`: `(bytes, mime)`, from the disk cache keyed by
    /// `fileChecksum`, else a fresh `lookup` URL + `download`.
    pub async fn asset(&self, attachment_id: &str, variant: AssetVariant) -> Result<(Vec<u8>, String), CmdError> {
        todo!("WP-J")
    }
}
