//! Shared plain types and traits for StarNotes, the iCloud Notes reader.
//!
//! This crate does no I/O. It is the contract between the other crates (PLAN.md §3.1):
//! - CloudKit record types ([`CkRecord`], [`ChangeItem`], [`ChangesPage`], [`AssetRef`]) and typed field helpers;
//! - view types sent to the UI ([`FolderView`], [`NoteSummary`], and the command types in [`view`]);
//! - error types;
//! - the [`CloudKitApi`] and [`NotesStore`] traits, so `notes-sync` can be built against fakes.

pub mod error;
pub mod fields;
pub mod record;
pub mod traits;
pub mod view;

pub use error::{ApiError, AuthError, CkError, DecodeError, StoreError, SyncError};
pub use fields::{asset, bytes_b64, int, reference, str_b64, string, timestamp};
pub use record::{AssetRef, AttachmentId, ChangeItem, ChangesPage, CkRecord, CkTombstone, RecordName, RecordType};
pub use traits::{CloudKitApi, NotesStore};
pub use view::{
    AssetVariant, CmdError, CmdErrorCode, FolderKind, FolderView, NoteHtml, NoteSummary, RefreshResult, Status,
    SyncProgress, SystemFolder,
};

/// recordName of the default "Notes" folder.
pub const DEFAULT_FOLDER: &str = "DefaultFolder-CloudKit";
/// recordName of the "Recently Deleted" folder.
pub const TRASH_FOLDER: &str = "TrashFolder-CloudKit";
/// recordName of the "Quick Notes" smart folder. No note's `Folder` points here; membership is
/// `Note_UserSpecific.IsSystemPaper == 1` (spike 7).
pub const QUICK_NOTES: &str = "SystemPaper-CloudKit";
/// The id the UI uses for the virtual Quick Notes folder (`FolderView::id` and the `notes` command's `folderId`).
pub const QUICK_NOTES_VIEW_ID: &str = "quick-notes";
/// Tauri app identifier, also the folder name under `%LOCALAPPDATA%` (jar, cache, assets, WebView2 profile).
pub const APP_ID: &str = "com.ziggy.starnotes";
/// The only CloudKit zone this app reads.
pub const NOTES_ZONE: &str = "Notes";
