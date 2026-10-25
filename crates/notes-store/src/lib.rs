//! SQLite cache (WP-F): `records`, `note_index`, `bodies`, `fts` (FTS5) and `kv`.
//!
//! `records` holds every CloudKit record whole. `note_index` is derived from it (and is re-derived
//! for a note whenever the note, its `Note_UserSpecific` or one of its attachments changes), so the
//! order in which records arrive within or across pages never matters.
//!
//! Visibility rules follow `spikes\spike6-sync\FINDINGS.md` ("Recommended", step 6) and
//! `spike7-shared`:
//! - `Deleted == 1` keeps a stub row (name, type, change tag, parent) and drops fields, body and FTS row.
//! - `Note.Folder` is the placement; a note in the trash folder is `trashed`.
//! - `*_UserSpecific` only contributes `IsSystemPaper` (Quick Notes).
//! - A tombstone removes the row and cascades: Note -> body, FTS, attachments, media, user-specific.

use std::path::{Path, PathBuf};

use notes_model::{
    CkRecord, ChangeItem, ChangesPage, FolderKind, FolderView, NoteSummary, NotesStore, RecordName, RecordType,
    StoreError, SystemFolder, APP_ID, DEFAULT_FOLDER, QUICK_NOTES, QUICK_NOTES_VIEW_ID, TRASH_FOLDER,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

const SCHEMA_VERSION: i64 = 1;
const KEY_SYNC_TOKEN: &str = "sync_token";

const SCHEMA: &str = "
CREATE TABLE records(
    name TEXT PRIMARY KEY,
    type TEXT NOT NULL,
    change_tag TEXT NOT NULL,
    fields_json TEXT NOT NULL,
    parent TEXT,
    owner TEXT,
    deleted INTEGER NOT NULL DEFAULT 0,
    seen INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX records_owner ON records(owner);
CREATE INDEX records_type ON records(type);
CREATE TABLE note_index(
    name TEXT PRIMARY KEY,
    folder TEXT NOT NULL,
    title TEXT NOT NULL,
    snippet TEXT NOT NULL,
    modified INTEGER NOT NULL,
    deleted INTEGER NOT NULL,
    trashed INTEGER NOT NULL,
    locked INTEGER NOT NULL,
    quick_note INTEGER NOT NULL,
    first_thumb TEXT,
    has_attachments INTEGER NOT NULL,
    text_key TEXT NOT NULL
);
CREATE INDEX note_index_folder ON note_index(folder, modified DESC);
CREATE TABLE bodies(
    name TEXT PRIMARY KEY,
    proto BLOB NOT NULL,
    text_hash TEXT NOT NULL
);
CREATE VIRTUAL TABLE fts USING fts5(name UNINDEXED, title, body, tokenize = 'unicode61 remove_diacritics 2');
CREATE TABLE kv(key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

/// [`NotesStore`] on rusqlite (bundled SQLite with FTS5).
pub struct SqliteStore {
    conn: rusqlite::Connection,
}

/// Internal error, converted to [`StoreError`] at the trait boundary.
#[derive(Debug)]
enum Err {
    Sql(rusqlite::Error),
    Json(serde_json::Error),
    Io(std::io::Error),
}

impl From<rusqlite::Error> for Err {
    fn from(e: rusqlite::Error) -> Self {
        Err::Sql(e)
    }
}

impl From<serde_json::Error> for Err {
    fn from(e: serde_json::Error) -> Self {
        Err::Json(e)
    }
}

impl From<std::io::Error> for Err {
    fn from(e: std::io::Error) -> Self {
        Err::Io(e)
    }
}

impl From<Err> for StoreError {
    fn from(e: Err) -> Self {
        match e {
            Err::Sql(e) => StoreError::Db(e.to_string()),
            Err::Json(e) => StoreError::Corrupt(e.to_string()),
            Err::Io(e) => StoreError::Io(e.to_string()),
        }
    }
}

type R<T> = Result<T, Err>;

impl SqliteStore {
    /// Open (creating and migrating if needed) the cache at `path`.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::open_inner(path).map_err(StoreError::from)
    }

    /// A fresh in-memory cache, for tests.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        (|| -> R<Self> {
            let conn = Connection::open_in_memory()?;
            init(&conn)?;
            Ok(SqliteStore { conn })
        })()
        .map_err(StoreError::from)
    }

    /// `%LOCALAPPDATA%\com.ziggy.icloud-notes\cache.sqlite`.
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        base.join(APP_ID).join("cache.sqlite")
    }

    fn open_inner(path: &Path) -> R<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        init(&conn)?;
        Ok(SqliteStore { conn })
    }
}

/// Create the schema, or rebuild it if `user_version` is not ours (the cache is disposable; the
/// token goes with it, so the next sync is a full one).
fn init(conn: &Connection) -> R<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    if version != 0 {
        conn.execute_batch(
            "DROP TABLE IF EXISTS records; DROP TABLE IF EXISTS note_index; DROP TABLE IF EXISTS bodies;
             DROP TABLE IF EXISTS fts; DROP TABLE IF EXISTS kv;",
        )?;
    }
    conn.execute_batch(SCHEMA)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Record helpers
// ---------------------------------------------------------------------------------------------

fn is_note_type(t: &str) -> bool {
    t == "Note" || t == "PasswordProtectedNote"
}

fn is_user_specific(t: &str) -> bool {
    t.ends_with("_UserSpecific") && t != "Folder_UserSpecific"
}

/// The note a child record belongs to: attachments and media by `Note` / `parent`, user-specific
/// records by `Note`, else the note id embedded at the end of their recordName.
fn owner_of(rec: &CkRecord) -> Option<RecordName> {
    let t = rec.record_type.as_str();
    let child = matches!(t, "Attachment" | "InlineAttachment" | "Media") || is_user_specific(t);
    if !child {
        return None;
    }
    notes_model::reference(rec, "Note").or_else(|| rec.parent.clone()).or_else(|| {
        if is_user_specific(t) { rec.name.as_str().rsplit("::").next().map(RecordName::from) } else { None }
    })
}

fn is_deleted(rec: &CkRecord) -> bool {
    notes_model::int(rec, "Deleted") == Some(1)
}

fn load_record(conn: &Connection, name: &str) -> R<Option<CkRecord>> {
    let row = conn
        .query_row(
            "SELECT name, type, change_tag, fields_json, parent FROM records WHERE name = ?1",
            [name],
            row_to_parts,
        )
        .optional()?;
    row.map(parts_to_record).transpose()
}

type Parts = (String, String, String, String, Option<String>);

fn row_to_parts(r: &rusqlite::Row<'_>) -> rusqlite::Result<Parts> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

fn parts_to_record((name, ty, change_tag, fields_json, parent): Parts) -> R<CkRecord> {
    Ok(CkRecord {
        name: RecordName(name),
        record_type: RecordType::from_wire(&ty),
        change_tag,
        fields: serde_json::from_str(&fields_json)?,
        parent: parent.map(RecordName),
    })
}

/// 64-bit FNV-1a: stable across builds (unlike `DefaultHasher`), used to fingerprint body fields.
fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Fingerprint of the field(s) that carry a note's body, `""` if it has none. Changes exactly when
/// `TextDataEncrypted` / `TextDataAsset` changes, not on moves or trash flips.
fn text_key(rec: &CkRecord) -> String {
    if let Some(v) = rec.fields.get("TextDataEncrypted").and_then(|f| f.get("value")).and_then(|v| v.as_str()) {
        return format!("i:{:016x}:{}", fnv(v), v.len());
    }
    if let Some(a) = notes_model::asset(rec, "TextDataAsset") {
        return format!("a:{}", a.file_checksum);
    }
    String::new()
}

// ---------------------------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------------------------

fn upsert(tx: &Connection, rec: &CkRecord) -> R<()> {
    let deleted = is_deleted(rec);
    let owner = owner_of(rec);
    let fields_json = if deleted { "{}".to_owned() } else { serde_json::to_string(&rec.fields)? };
    let previous_owner: Option<String> = tx
        .query_row("SELECT owner FROM records WHERE name = ?1", [rec.name.as_str()], |r| r.get(0))
        .optional()?
        .flatten();
    tx.execute(
        "INSERT INTO records(name, type, change_tag, fields_json, parent, owner, deleted, seen)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)
         ON CONFLICT(name) DO UPDATE SET type = ?2, change_tag = ?3, fields_json = ?4, parent = ?5,
             owner = ?6, deleted = ?7, seen = 1",
        params![
            rec.name.as_str(),
            rec.record_type.as_str(),
            rec.change_tag,
            fields_json,
            rec.parent.as_ref().map(RecordName::as_str),
            owner.as_ref().map(RecordName::as_str),
            deleted,
        ],
    )?;
    reindex_note(tx, rec.name.as_str())?;
    if let Some(o) = &owner {
        reindex_note(tx, o.as_str())?;
    }
    if let Some(o) = previous_owner.filter(|p| Some(p.as_str()) != owner.as_ref().map(RecordName::as_str)) {
        reindex_note(tx, &o)?;
    }
    Ok(())
}

/// Delete a record (a tombstone, or a sweep of an unseen row) and cascade by type.
fn remove_record(tx: &Connection, name: &str) -> R<()> {
    let row: Option<(String, Option<String>, String)> = tx
        .query_row("SELECT type, owner, fields_json FROM records WHERE name = ?1", [name], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?;
    let Some((ty, owner, fields_json)) = row else {
        return Ok(());
    };
    if is_note_type(&ty) {
        tx.execute("DELETE FROM records WHERE owner = ?1", [name])?;
        tx.execute("DELETE FROM records WHERE name = ?1", [name])?;
        drop_note_derived(tx, name)?;
        tx.execute("DELETE FROM note_index WHERE name = ?1", [name])?;
        return Ok(());
    }
    if ty == "Attachment" {
        let fields: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&fields_json)?;
        let media = fields.get("Media").and_then(|f| f.get("value")).and_then(|v| v.get("recordName"));
        if let Some(m) = media.and_then(|m| m.as_str()) {
            tx.execute("DELETE FROM records WHERE name = ?1 AND type = 'Media'", [m])?;
        }
    }
    tx.execute("DELETE FROM records WHERE name = ?1", [name])?;
    if let Some(o) = owner {
        reindex_note(tx, &o)?;
    }
    Ok(())
}

fn drop_note_derived(tx: &Connection, name: &str) -> R<()> {
    tx.execute("DELETE FROM bodies WHERE name = ?1", [name])?;
    tx.execute("DELETE FROM fts WHERE name = ?1", [name])?;
    Ok(())
}

/// Rewrite the FTS row of `name`. `body = None` keeps the current body text.
fn set_fts(tx: &Connection, name: &str, title: &str, body: Option<&str>) -> R<()> {
    let body = match body {
        Some(b) => b.to_owned(),
        None => tx
            .query_row("SELECT body FROM fts WHERE name = ?1", [name], |r| r.get::<_, String>(0))
            .optional()?
            .unwrap_or_default(),
    };
    tx.execute("DELETE FROM fts WHERE name = ?1", [name])?;
    tx.execute("INSERT INTO fts(name, title, body) VALUES(?1, ?2, ?3)", params![name, title, body])?;
    Ok(())
}

/// Recompute the `note_index` row (and FTS title) for `name` from `records`. A name that is not a
/// note (or not present) just has its derived rows removed.
fn reindex_note(tx: &Connection, name: &str) -> R<()> {
    let rec = match load_record(tx, name)? {
        Some(r) if is_note_type(r.record_type.as_str()) => r,
        _ => {
            tx.execute("DELETE FROM note_index WHERE name = ?1", [name])?;
            return drop_note_derived(tx, name);
        }
    };
    let deleted: bool = tx.query_row("SELECT deleted FROM records WHERE name = ?1", [name], |r| r.get(0))?;
    let locked = rec.record_type == RecordType::PasswordProtectedNote;
    if deleted {
        tx.execute(
            "INSERT INTO note_index(name, folder, title, snippet, modified, deleted, trashed, locked, quick_note,
                 first_thumb, has_attachments, text_key)
             VALUES(?1, '', '', '', 0, 1, 0, ?2, 0, NULL, 0, '')
             ON CONFLICT(name) DO UPDATE SET folder = '', title = '', snippet = '', modified = 0, deleted = 1,
                 trashed = 0, locked = ?2, quick_note = 0, first_thumb = NULL, has_attachments = 0, text_key = ''",
            params![name, locked],
        )?;
        return drop_note_derived(tx, name);
    }

    let folder = notes_model::reference(&rec, "Folder")
        .or_else(|| rec.parent.clone())
        .map(|f| f.0)
        .unwrap_or_default();
    let title = notes_model::str_b64(&rec, "TitleEncrypted").unwrap_or_default();
    // A locked note's snippet is never shown.
    let snippet = if locked { String::new() } else { notes_model::str_b64(&rec, "SnippetEncrypted").unwrap_or_default() };
    let modified = notes_model::timestamp(&rec, "ModificationDate").unwrap_or(0);
    let trashed = folder == TRASH_FOLDER;

    let mut quick_note = false;
    {
        let mut stmt = tx.prepare("SELECT fields_json FROM records WHERE owner = ?1 AND deleted = 0 AND type LIKE '%\\_UserSpecific' ESCAPE '\\'")?;
        let rows = stmt.query_map([name], |r| r.get::<_, String>(0))?;
        for row in rows {
            let fields: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&row?)?;
            let v = fields.get("IsSystemPaper").and_then(|f| f.get("value"));
            if v.and_then(|v| v.as_i64()) == Some(1) {
                quick_note = true;
            }
        }
    }
    let has_attachments: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM records WHERE owner = ?1 AND type = 'Attachment' AND deleted = 0)",
        [name],
        |r| r.get(0),
    )?;
    // The list thumbnail is an asset on the Note itself; the Note's recordName stands in for the
    // attachment id (see the hand-over note).
    let first_thumb = notes_model::asset(&rec, "FirstAttachmentThumbnail").map(|_| name.to_owned());

    tx.execute(
        "INSERT INTO note_index(name, folder, title, snippet, modified, deleted, trashed, locked, quick_note,
             first_thumb, has_attachments, text_key)
         VALUES(?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(name) DO UPDATE SET folder = ?2, title = ?3, snippet = ?4, modified = ?5, deleted = 0,
             trashed = ?6, locked = ?7, quick_note = ?8, first_thumb = ?9, has_attachments = ?10, text_key = ?11",
        params![name, folder, title, snippet, modified, trashed, locked, quick_note, first_thumb, has_attachments, text_key(&rec)],
    )?;
    if locked {
        drop_note_derived(tx, name)
    } else {
        set_fts(tx, name, &title, None)
    }
}

// ---------------------------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------------------------

const SUMMARY_COLS: &str = "name, folder, title, snippet, modified, has_attachments, locked, first_thumb";

fn summaries(conn: &Connection, sql_tail: &str, args: impl rusqlite::Params) -> R<Vec<NoteSummary>> {
    let mut stmt = conn.prepare(&format!("SELECT {SUMMARY_COLS} FROM note_index {sql_tail}"))?;
    let rows = stmt.query_map(args, |r| {
        Ok(NoteSummary {
            id: RecordName(r.get(0)?),
            folder: RecordName(r.get(1)?),
            title: r.get(2)?,
            snippet: r.get(3)?,
            modified: r.get(4)?,
            has_attachments: r.get(5)?,
            locked: r.get(6)?,
            thumbnail: r.get::<_, Option<String>>(7)?.map(RecordName),
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Quote each whitespace-separated term for FTS5 and make it a prefix match; terms are ANDed.
fn fts_query(q: &str) -> Option<String> {
    let terms: Vec<String> =
        q.split_whitespace().map(|t| format!("\"{}\"*", t.replace('"', "\"\""))).collect();
    if terms.is_empty() { None } else { Some(terms.join(" ")) }
}

impl SqliteStore {
    fn folders_inner(&self) -> R<Vec<FolderView>> {
        let conn = &self.conn;
        let mut counts = std::collections::HashMap::<String, u32>::new();
        {
            let mut stmt = conn.prepare("SELECT folder, COUNT(*) FROM note_index WHERE deleted = 0 GROUP BY folder")?;
            for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))? {
                let (f, n) = row?;
                counts.insert(f, n);
            }
        }
        let quick_count: u32 = conn.query_row(
            "SELECT COUNT(*) FROM note_index WHERE quick_note = 1 AND deleted = 0 AND trashed = 0",
            [],
            |r| r.get(0),
        )?;

        let mut titles = std::collections::HashMap::<String, String>::new();
        let mut users: Vec<(String, String)> = Vec::new();
        {
            let mut stmt =
                conn.prepare("SELECT name, type, change_tag, fields_json, parent FROM records WHERE type = 'Folder' AND deleted = 0")?;
            for row in stmt.query_map([], row_to_parts)? {
                let rec = parts_to_record(row?)?;
                let title = notes_model::str_b64(&rec, "TitleEncrypted").unwrap_or_default();
                let n = rec.name.as_str();
                let is_system = n == DEFAULT_FOLDER || n == TRASH_FOLDER || n == QUICK_NOTES;
                // Smart folders (query DSL) are not evaluated: omitted rather than shown empty.
                let smart = rec.fields.contains_key("SmartFolderQueryJSONEncrypted");
                if is_system {
                    titles.insert(n.to_owned(), title);
                } else if !smart {
                    users.push((n.to_owned(), title));
                }
            }
        }
        users.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()).then_with(|| a.0.cmp(&b.0)));

        let title_or = |id: &str, default: &str| match titles.get(id) {
            Some(t) if !t.is_empty() => t.clone(),
            _ => default.to_owned(),
        };
        let count = |id: &str| counts.get(id).copied().unwrap_or(0);
        let mut out = vec![
            FolderView {
                id: RecordName::from(QUICK_NOTES_VIEW_ID),
                title: title_or(QUICK_NOTES, "Quick Notes"),
                kind: FolderKind::System(SystemFolder::QuickNotes),
                note_count: quick_count,
            },
            FolderView {
                id: RecordName::from(DEFAULT_FOLDER),
                title: title_or(DEFAULT_FOLDER, "Notes"),
                kind: FolderKind::System(SystemFolder::Default),
                note_count: count(DEFAULT_FOLDER),
            },
        ];
        for (id, title) in users {
            let note_count = count(&id);
            out.push(FolderView {
                title: if title.is_empty() { "Untitled".to_owned() } else { title },
                id: RecordName(id),
                kind: FolderKind::User,
                note_count,
            });
        }
        out.push(FolderView {
            id: RecordName::from(TRASH_FOLDER),
            title: title_or(TRASH_FOLDER, "Recently Deleted"),
            kind: FolderKind::System(SystemFolder::Trash),
            note_count: count(TRASH_FOLDER),
        });
        Ok(out)
    }
}

impl NotesStore for SqliteStore {
    fn sync_token(&self) -> Result<Option<String>, StoreError> {
        self.conn
            .query_row("SELECT value FROM kv WHERE key = ?1", [KEY_SYNC_TOKEN], |r| r.get(0))
            .optional()
            .map_err(|e| StoreError::from(Err::from(e)))
    }

    fn apply_page(&mut self, page: &ChangesPage) -> Result<(), StoreError> {
        let run = |tx: &Transaction<'_>| -> R<()> {
            for item in &page.items {
                match item {
                    ChangeItem::Live(rec) => upsert(tx, rec)?,
                    ChangeItem::Tombstone(t) => remove_record(tx, t.name.as_str())?,
                }
            }
            tx.execute(
                "INSERT INTO kv(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
                params![KEY_SYNC_TOKEN, page.sync_token],
            )?;
            Ok(())
        };
        (|| -> R<()> {
            let tx = self.conn.transaction()?;
            run(&tx)?;
            tx.commit()?;
            Ok(())
        })()
        .map_err(StoreError::from)
    }

    fn begin_full_resync(&mut self) -> Result<(), StoreError> {
        self.conn
            .execute("UPDATE records SET seen = 0", [])
            .map(|_| ())
            .map_err(|e| StoreError::from(Err::from(e)))
    }

    fn finish_full_resync(&mut self) -> Result<(), StoreError> {
        (|| -> R<()> {
            let tx = self.conn.transaction()?;
            let unseen: Vec<String> = {
                let mut stmt = tx.prepare("SELECT name FROM records WHERE seen = 0")?;
                stmt.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?
            };
            for name in unseen {
                remove_record(&tx, &name)?;
            }
            tx.commit()?;
            Ok(())
        })()
        .map_err(StoreError::from)
    }

    fn record(&self, name: &RecordName) -> Result<Option<CkRecord>, StoreError> {
        load_record(&self.conn, name.as_str()).map_err(StoreError::from)
    }

    fn folders(&self) -> Result<Vec<FolderView>, StoreError> {
        self.folders_inner().map_err(StoreError::from)
    }

    fn notes_in(&self, folder: &RecordName) -> Result<Vec<NoteSummary>, StoreError> {
        if folder.as_str() == QUICK_NOTES_VIEW_ID {
            return self.quick_notes();
        }
        summaries(&self.conn, "WHERE folder = ?1 AND deleted = 0 ORDER BY modified DESC, name", [folder.as_str()])
            .map_err(StoreError::from)
    }

    fn quick_notes(&self) -> Result<Vec<NoteSummary>, StoreError> {
        summaries(
            &self.conn,
            "WHERE quick_note = 1 AND deleted = 0 AND trashed = 0 ORDER BY modified DESC, name",
            [],
        )
        .map_err(StoreError::from)
    }

    fn search(&self, q: &str) -> Result<Vec<NoteSummary>, StoreError> {
        let Some(query) = fts_query(q) else {
            return Ok(Vec::new());
        };
        summaries(
            &self.conn,
            "WHERE deleted = 0 AND trashed = 0 AND locked = 0
               AND name IN (SELECT name FROM fts WHERE fts MATCH ?1)
             ORDER BY modified DESC, name",
            [query],
        )
        .map_err(StoreError::from)
    }

    fn note_body(&self, note: &RecordName) -> Result<Option<Vec<u8>>, StoreError> {
        self.conn
            .query_row("SELECT proto FROM bodies WHERE name = ?1", [note.as_str()], |r| r.get(0))
            .optional()
            .map_err(|e| StoreError::from(Err::from(e)))
    }

    fn put_note_body(&mut self, note: &RecordName, proto: &[u8], plain_text: &str) -> Result<(), StoreError> {
        (|| -> R<()> {
            let tx = self.conn.transaction()?;
            let row: Option<(String, String)> = tx
                .query_row(
                    "SELECT text_key, title FROM note_index WHERE name = ?1 AND deleted = 0 AND locked = 0",
                    [note.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            // The note vanished (or is locked) since the body was fetched: nothing to store.
            let Some((key, title)) = row else {
                return Ok(());
            };
            tx.execute(
                "INSERT INTO bodies(name, proto, text_hash) VALUES(?1, ?2, ?3)
                 ON CONFLICT(name) DO UPDATE SET proto = ?2, text_hash = ?3",
                params![note.as_str(), proto, key],
            )?;
            set_fts(&tx, note.as_str(), &title, Some(plain_text))?;
            tx.commit()?;
            Ok(())
        })()
        .map_err(StoreError::from)
    }

    fn notes_needing_body(&self) -> Result<Vec<RecordName>, StoreError> {
        (|| -> R<Vec<RecordName>> {
            let mut stmt = self.conn.prepare(
                "SELECT n.name FROM note_index n LEFT JOIN bodies b ON b.name = n.name
                 WHERE n.deleted = 0 AND n.locked = 0 AND n.text_key != ''
                   AND (b.name IS NULL OR b.text_hash != n.text_key)
                 ORDER BY n.modified DESC, n.name",
            )?;
            let rows = stmt.query_map([], |r| Ok(RecordName(r.get(0)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })()
        .map_err(StoreError::from)
    }
}

#[cfg(test)]
mod tests;
