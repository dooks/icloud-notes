//! CloudKit record types, as parsed by `cloudkit` and persisted by `notes-store`.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A CloudKit `recordName`, e.g. `DefaultFolder-CloudKit` or a note's UUID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RecordName(pub String);

impl RecordName {
    pub fn new(s: impl Into<String>) -> Self {
        RecordName(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RecordName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for RecordName {
    fn from(s: &str) -> Self {
        RecordName(s.to_owned())
    }
}

impl From<String> for RecordName {
    fn from(s: String) -> Self {
        RecordName(s)
    }
}

/// An attachment is identified by its `Attachment` record's recordName (spike 5: the body's
/// `attachment_identifier` equals it).
pub type AttachmentId = RecordName;

/// CloudKit `recordType`. Unknown types are kept verbatim in [`RecordType::Other`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RecordType {
    Note,
    Folder,
    Attachment,
    Media,
    InlineAttachment,
    Hashtag,
    NoteUserSpecific,
    FolderUserSpecific,
    PasswordProtectedNote,
    Other(String),
}

impl RecordType {
    /// The wire name, e.g. `Note_UserSpecific`.
    pub fn as_str(&self) -> &str {
        match self {
            RecordType::Note => "Note",
            RecordType::Folder => "Folder",
            RecordType::Attachment => "Attachment",
            RecordType::Media => "Media",
            RecordType::InlineAttachment => "InlineAttachment",
            RecordType::Hashtag => "Hashtag",
            RecordType::NoteUserSpecific => "Note_UserSpecific",
            RecordType::FolderUserSpecific => "Folder_UserSpecific",
            RecordType::PasswordProtectedNote => "PasswordProtectedNote",
            RecordType::Other(s) => s,
        }
    }

    /// Parse a wire name. Never fails: unknown names become [`RecordType::Other`].
    pub fn from_wire(s: &str) -> Self {
        match s {
            "Note" => RecordType::Note,
            "Folder" => RecordType::Folder,
            "Attachment" => RecordType::Attachment,
            "Media" => RecordType::Media,
            "InlineAttachment" => RecordType::InlineAttachment,
            "Hashtag" => RecordType::Hashtag,
            "Note_UserSpecific" => RecordType::NoteUserSpecific,
            "Folder_UserSpecific" => RecordType::FolderUserSpecific,
            "PasswordProtectedNote" => RecordType::PasswordProtectedNote,
            other => RecordType::Other(other.to_owned()),
        }
    }
}

impl fmt::Display for RecordType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for RecordType {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RecordType {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(RecordType::from_wire(&s))
    }
}

/// A live CloudKit record.
///
/// `fields` is CloudKit's raw `fields` object: each value is `{"value": …, "type": "…"}`.
/// Read it with the helpers in [`crate::fields`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CkRecord {
    pub name: RecordName,
    pub record_type: RecordType,
    pub change_tag: String,
    pub fields: serde_json::Map<String, serde_json::Value>,
    /// Record-level `parent.recordName`, if any.
    pub parent: Option<RecordName>,
}

/// A deleted record. CloudKit tombstones carry only the recordName (spike 6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CkTombstone {
    pub name: RecordName,
}

/// One entry of a `changes/zone` or `records/lookup` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChangeItem {
    Live(CkRecord),
    Tombstone(CkTombstone),
}

impl ChangeItem {
    pub fn name(&self) -> &RecordName {
        match self {
            ChangeItem::Live(r) => &r.name,
            ChangeItem::Tombstone(t) => &t.name,
        }
    }
}

/// One page of `changes/zone`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangesPage {
    pub items: Vec<ChangeItem>,
    /// The cursor to store once this page is applied.
    pub sync_token: String,
    /// Keep paging while this is true (never stop on an empty page).
    pub more_coming: bool,
}

/// A CloudKit `ASSETID` value. `download_url` expires after about 15 minutes and is **never persisted**.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRef {
    pub download_url: String,
    pub size: u64,
    /// Stable until the content changes; use it as the disk cache key.
    pub file_checksum: String,
}
