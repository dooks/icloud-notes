//! SQLite cache (WP-F): `records`, `note_index`, `bodies`, `fts` (FTS5) and `kv`.

// Wave 0 stubs: remove once implemented.
#![allow(unused_variables, dead_code)]

use std::path::{Path, PathBuf};

use notes_model::{ChangesPage, CkRecord, FolderView, NoteSummary, NotesStore, RecordName, StoreError};

/// [`NotesStore`] on rusqlite (bundled SQLite with FTS5).
pub struct SqliteStore {
    conn: rusqlite::Connection,
}

impl SqliteStore {
    /// Open (creating and migrating if needed) the cache at `path`.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        todo!("WP-F")
    }

    /// A fresh in-memory cache, for tests.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        todo!("WP-F")
    }

    /// `%LOCALAPPDATA%\com.ziggy.icloud-notes\cache.sqlite`.
    pub fn default_path() -> PathBuf {
        todo!("WP-F")
    }
}

impl NotesStore for SqliteStore {
    fn sync_token(&self) -> Result<Option<String>, StoreError> {
        todo!("WP-F")
    }

    fn apply_page(&mut self, page: &ChangesPage) -> Result<(), StoreError> {
        todo!("WP-F")
    }

    fn begin_full_resync(&mut self) -> Result<(), StoreError> {
        todo!("WP-F")
    }

    fn finish_full_resync(&mut self) -> Result<(), StoreError> {
        todo!("WP-F")
    }

    fn record(&self, name: &RecordName) -> Result<Option<CkRecord>, StoreError> {
        todo!("WP-F")
    }

    fn folders(&self) -> Result<Vec<FolderView>, StoreError> {
        todo!("WP-F")
    }

    fn notes_in(&self, folder: &RecordName) -> Result<Vec<NoteSummary>, StoreError> {
        todo!("WP-F")
    }

    fn quick_notes(&self) -> Result<Vec<NoteSummary>, StoreError> {
        todo!("WP-F")
    }

    fn search(&self, q: &str) -> Result<Vec<NoteSummary>, StoreError> {
        todo!("WP-F")
    }

    fn note_body(&self, note: &RecordName) -> Result<Option<Vec<u8>>, StoreError> {
        todo!("WP-F")
    }

    fn put_note_body(&mut self, note: &RecordName, proto: &[u8], plain_text: &str) -> Result<(), StoreError> {
        todo!("WP-F")
    }

    fn notes_needing_body(&self) -> Result<Vec<RecordName>, StoreError> {
        todo!("WP-F")
    }
}
