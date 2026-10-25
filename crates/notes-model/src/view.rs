//! Types sent to the UI through the Tauri commands (PLAN.md §3.4). All serialize as camelCase JSON.

use serde::{Deserialize, Serialize};

use crate::error::{ApiError, DecodeError, StoreError, SyncError};
use crate::record::{AttachmentId, RecordName};

/// A built-in folder, matched by recordName (titles are localized).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SystemFolder {
    /// `DefaultFolder-CloudKit` ("Notes").
    Default,
    /// `TrashFolder-CloudKit` ("Recently Deleted").
    Trash,
    /// The virtual Quick Notes folder (id [`crate::QUICK_NOTES_VIEW_ID`]).
    QuickNotes,
}

/// JSON: `{"system": "default"|"trash"|"quickNotes"}` or `"user"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FolderKind {
    System(SystemFolder),
    User,
}

/// One row of the folder pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderView {
    /// The folder's recordName, or [`crate::QUICK_NOTES_VIEW_ID`] for Quick Notes.
    pub id: RecordName,
    pub title: String,
    pub kind: FolderKind,
    /// Visible notes in the folder (excludes `Deleted == 1`).
    pub note_count: u32,
}

/// One row of the note list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteSummary {
    pub id: RecordName,
    pub folder: RecordName,
    pub title: String,
    pub snippet: String,
    /// `ModificationDate`, ms since the Unix epoch (0 if unknown).
    pub modified: i64,
    pub has_attachments: bool,
    /// A `PasswordProtectedNote`: shown as "🔒 Locked", never decoded.
    pub locked: bool,
    pub thumbnail: Option<AttachmentId>,
}

/// Which file of an attachment `notes-asset://localhost/<attachment-id>/<variant>` refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssetVariant {
    Original,
    PreviewLight,
    PreviewDark,
    Fallback,
    Thumb,
}

impl AssetVariant {
    pub fn as_str(self) -> &'static str {
        match self {
            AssetVariant::Original => "original",
            AssetVariant::PreviewLight => "preview-light",
            AssetVariant::PreviewDark => "preview-dark",
            AssetVariant::Fallback => "fallback",
            AssetVariant::Thumb => "thumb",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "original" => AssetVariant::Original,
            "preview-light" => AssetVariant::PreviewLight,
            "preview-dark" => AssetVariant::PreviewDark,
            "fallback" => AssetVariant::Fallback,
            "thumb" => AssetVariant::Thumb,
            _ => return None,
        })
    }
}

/// `status` command result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub signed_in: bool,
    pub syncing: bool,
    /// RFC 3339 time of the last successful sync.
    pub last_sync: Option<String>,
    /// Short, content-free description of the last failure.
    pub error: Option<String>,
}

/// `refresh` command result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshResult {
    /// Records changed (live + tombstones) by this refresh.
    pub changed: u32,
}

/// `note_html` command result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteHtml {
    pub title: String,
    /// A full HTML document for the iframe's `srcdoc`.
    pub html: String,
}

/// Payload of the `sync-progress` event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgress {
    pub pages: u32,
    pub records: u32,
}

/// The error code every command fails with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CmdErrorCode {
    /// Sign-in needed.
    Auth,
    /// Apple unreachable; cached data still works.
    Offline,
    Internal,
}

/// Command error: `{code: "auth"|"offline"|"internal", message}`. `message` is content-free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct CmdError {
    pub code: CmdErrorCode,
    pub message: String,
}

impl CmdError {
    pub fn new(code: CmdErrorCode, message: impl Into<String>) -> Self {
        CmdError { code, message: message.into() }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        CmdError::new(CmdErrorCode::Internal, message)
    }
}

impl From<ApiError> for CmdError {
    fn from(e: ApiError) -> Self {
        let code = match e {
            ApiError::Auth(_) => CmdErrorCode::Auth,
            ApiError::Http(_) => CmdErrorCode::Offline,
            ApiError::Ck(_) => CmdErrorCode::Internal,
        };
        CmdError::new(code, e.to_string())
    }
}

impl From<StoreError> for CmdError {
    fn from(e: StoreError) -> Self {
        CmdError::internal(e.to_string())
    }
}

impl From<DecodeError> for CmdError {
    fn from(e: DecodeError) -> Self {
        CmdError::internal(e.to_string())
    }
}

impl From<SyncError> for CmdError {
    fn from(e: SyncError) -> Self {
        match e {
            SyncError::Api(a) => a.into(),
            SyncError::Store(s) => s.into(),
            SyncError::GaveUp(_) => CmdError::new(CmdErrorCode::Offline, e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AuthError;
    use serde_json::json;

    #[test]
    fn json_shapes_match_the_command_contract() {
        let f = FolderView {
            id: "quick-notes".into(),
            title: "Quick Notes".into(),
            kind: FolderKind::System(SystemFolder::QuickNotes),
            note_count: 2,
        };
        assert_eq!(
            serde_json::to_value(&f).unwrap(),
            json!({"id": "quick-notes", "title": "Quick Notes", "kind": {"system": "quickNotes"}, "noteCount": 2})
        );
        assert_eq!(serde_json::to_value(FolderKind::User).unwrap(), json!("user"));

        let s = Status { signed_in: true, ..Default::default() };
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            json!({"signedIn": true, "syncing": false, "lastSync": null, "error": null})
        );

        let e: CmdError = ApiError::Auth(AuthError::NotSignedIn).into();
        assert_eq!(serde_json::to_value(&e).unwrap()["code"], json!("auth"));
        let e: CmdError = ApiError::Http("timeout".into()).into();
        assert_eq!(e.code, CmdErrorCode::Offline);
    }

    #[test]
    fn asset_variant_round_trips() {
        for v in [
            AssetVariant::Original,
            AssetVariant::PreviewLight,
            AssetVariant::PreviewDark,
            AssetVariant::Fallback,
            AssetVariant::Thumb,
        ] {
            assert_eq!(AssetVariant::parse(v.as_str()), Some(v));
            assert_eq!(serde_json::to_value(v).unwrap(), json!(v.as_str()));
        }
        assert_eq!(AssetVariant::parse("../etc"), None);
    }
}
