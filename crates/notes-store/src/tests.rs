//! Synthetic fixtures only; every string below is invented.

use notes_model::{ChangeItem, ChangesPage, CkRecord, CkTombstone, FolderKind, RecordName, RecordType, SystemFolder};
use serde_json::{Map, Value, json};

use super::*;

fn b64(s: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in s.as_bytes().chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn fields(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn rec(name: &str, ty: &str, tag: &str, f: Value, parent: Option<&str>) -> CkRecord {
    CkRecord {
        name: name.into(),
        record_type: RecordType::from_wire(ty),
        change_tag: tag.into(),
        fields: fields(f),
        parent: parent.map(RecordName::from),
    }
}

fn folder(name: &str, title: &str) -> ChangeItem {
    ChangeItem::Live(rec(name, "Folder", "f1", json!({"TitleEncrypted": {"value": b64(title)}}), None))
}

struct N<'a> {
    name: &'a str,
    folder: &'a str,
    title: &'a str,
    text: &'a str,
    modified: i64,
    deleted: i64,
    ty: &'a str,
    tag: &'a str,
}

fn n<'a>(name: &'a str, folder: &'a str, title: &'a str) -> N<'a> {
    N { name, folder, title, text: "body-v1", modified: 1000, deleted: 0, ty: "Note", tag: "t1" }
}

impl N<'_> {
    fn item(&self) -> ChangeItem {
        ChangeItem::Live(rec(
            self.name,
            self.ty,
            self.tag,
            json!({
                "TitleEncrypted": {"value": b64(self.title)},
                "SnippetEncrypted": {"value": b64("snippet words")},
                "TextDataEncrypted": {"value": b64(self.text)},
                "Folder": {"value": {"recordName": self.folder}},
                "ModificationDate": {"value": self.modified},
                "Deleted": {"value": self.deleted},
            }),
            Some(self.folder),
        ))
    }
}

fn user_specific(note: &str, system_paper: i64) -> ChangeItem {
    ChangeItem::Live(rec(
        &format!("Note_UserSpecific::me/Private::Notes::me::{note}"),
        "Note_UserSpecific",
        "u1",
        json!({"IsSystemPaper": {"value": system_paper}, "Note": {"value": {"recordName": note}}}),
        None,
    ))
}

fn attachment(name: &str, note: &str, media: Option<&str>) -> ChangeItem {
    let mut f = json!({"Note": {"value": {"recordName": note}}});
    if let Some(m) = media {
        f["Media"] = json!({"value": {"recordName": m}});
    }
    ChangeItem::Live(rec(name, "Attachment", "a1", f, Some(note)))
}

fn media(name: &str, note: &str) -> ChangeItem {
    ChangeItem::Live(rec(name, "Media", "m1", json!({}), Some(note)))
}

fn tomb(name: &str) -> ChangeItem {
    ChangeItem::Tombstone(CkTombstone { name: name.into() })
}

fn page(items: Vec<ChangeItem>, token: &str) -> ChangesPage {
    ChangesPage { items, sync_token: token.into(), more_coming: false }
}

fn store(items: Vec<ChangeItem>) -> SqliteStore {
    let mut s = SqliteStore::open_in_memory().unwrap();
    s.apply_page(&page(items, "tok")).unwrap();
    s
}

fn ids(v: &[NoteSummary]) -> Vec<&str> {
    v.iter().map(|s| s.id.as_str()).collect()
}

fn rn(s: &str) -> RecordName {
    s.into()
}

#[test]
fn token_is_saved_with_the_page() {
    let mut s = SqliteStore::open_in_memory().unwrap();
    assert_eq!(s.sync_token().unwrap(), None);
    s.apply_page(&page(vec![], "a")).unwrap();
    s.apply_page(&page(vec![], "b")).unwrap();
    assert_eq!(s.sync_token().unwrap().as_deref(), Some("b"));
}

#[test]
fn record_round_trips() {
    let s = store(vec![n("n1", DEFAULT_FOLDER, "Alpha").item()]);
    let r = s.record(&rn("n1")).unwrap().unwrap();
    assert_eq!(r.change_tag, "t1");
    assert_eq!(r.parent, Some(rn(DEFAULT_FOLDER)));
    assert_eq!(notes_model::str_b64(&r, "TitleEncrypted").as_deref(), Some("Alpha"));
    assert!(s.record(&rn("nope")).unwrap().is_none());
}

#[test]
fn note_lives_in_its_folder_and_sorts_by_modified() {
    let mut a = n("n1", DEFAULT_FOLDER, "Alpha");
    a.modified = 10;
    let mut b = n("n2", DEFAULT_FOLDER, "Beta");
    b.modified = 20;
    let c = n("n3", "F-1", "Gamma");
    let s = store(vec![folder("F-1", "Work"), a.item(), b.item(), c.item()]);
    let list = s.notes_in(&rn(DEFAULT_FOLDER)).unwrap();
    assert_eq!(ids(&list), ["n2", "n1"]);
    assert_eq!(list[0].title, "Beta");
    assert_eq!(list[0].snippet, "snippet words");
    assert_eq!(list[0].modified, 20);
    assert!(!list[0].locked && !list[0].has_attachments && list[0].thumbnail.is_none());
    assert_eq!(ids(&s.notes_in(&rn("F-1")).unwrap()), ["n3"]);
}

#[test]
fn moving_a_note_keeps_its_modification_date() {
    let mut s = store(vec![folder("F-1", "Work"), n("n1", DEFAULT_FOLDER, "Alpha").item()]);
    let mut moved = n("n1", "F-1", "Alpha");
    moved.tag = "t2";
    s.apply_page(&page(vec![moved.item()], "tok2")).unwrap();
    assert!(s.notes_in(&rn(DEFAULT_FOLDER)).unwrap().is_empty());
    let list = s.notes_in(&rn("F-1")).unwrap();
    assert_eq!((ids(&list), list[0].modified), (vec!["n1"], 1000));
    // A move is not a body change.
    s.put_note_body(&rn("n1"), b"p", "text").unwrap();
    let mut again = n("n1", DEFAULT_FOLDER, "Alpha");
    again.tag = "t3";
    s.apply_page(&page(vec![again.item()], "tok3")).unwrap();
    assert!(s.notes_needing_body().unwrap().is_empty());
}

#[test]
fn deleted_flag_hides_note_keeps_stub_and_drops_body() {
    let mut s = store(vec![n("n1", DEFAULT_FOLDER, "Alpha").item()]);
    s.put_note_body(&rn("n1"), b"proto", "uniqueword").unwrap();
    assert_eq!(s.search("uniqueword").unwrap().len(), 1);

    let mut gone = n("n1", DEFAULT_FOLDER, "Alpha");
    gone.deleted = 1;
    gone.tag = "t2";
    s.apply_page(&page(vec![gone.item()], "t")).unwrap();
    assert!(s.notes_in(&rn(DEFAULT_FOLDER)).unwrap().is_empty());
    assert!(s.search("uniqueword").unwrap().is_empty());
    assert_eq!(s.note_body(&rn("n1")).unwrap(), None);
    assert!(s.notes_needing_body().unwrap().is_empty());
    let stub = s.record(&rn("n1")).unwrap().unwrap();
    assert_eq!((stub.change_tag.as_str(), stub.record_type), ("t2", RecordType::Note));
    assert!(stub.fields.is_empty());
    assert_eq!(s.folders().unwrap()[1].note_count, 0);

    // Revival: a later whole-record replace brings it back and wants a body again.
    let mut back = n("n1", DEFAULT_FOLDER, "Alpha");
    back.tag = "t3";
    s.apply_page(&page(vec![back.item()], "t")).unwrap();
    assert_eq!(ids(&s.notes_in(&rn(DEFAULT_FOLDER)).unwrap()), ["n1"]);
    assert_eq!(s.notes_needing_body().unwrap(), vec![rn("n1")]);
}

#[test]
fn trash_folder_notes_are_listed_there_only() {
    let s = store(vec![
        n("n1", TRASH_FOLDER, "Binned").item(),
        n("n2", DEFAULT_FOLDER, "Kept").item(),
        user_specific("n1", 1),
    ]);
    assert_eq!(ids(&s.notes_in(&rn(TRASH_FOLDER)).unwrap()), ["n1"]);
    assert_eq!(ids(&s.notes_in(&rn(DEFAULT_FOLDER)).unwrap()), ["n2"]);
    assert!(s.quick_notes().unwrap().is_empty());
    assert!(s.search("Binned").unwrap().is_empty());
    let f = s.folders().unwrap();
    assert_eq!(f.last().unwrap().note_count, 1);
}

#[test]
fn user_specific_never_decides_placement() {
    // A stale Note_UserSpecific pointing at the trash must not move a live note.
    let us = ChangeItem::Live(rec(
        "Note_UserSpecific::me/Private::Notes::me::n1",
        "Note_UserSpecific",
        "u",
        json!({"Folder": {"value": {"recordName": TRASH_FOLDER}}, "Note": {"value": {"recordName": "n1"}}}),
        None,
    ));
    let s = store(vec![n("n1", DEFAULT_FOLDER, "Alpha").item(), us]);
    assert_eq!(ids(&s.notes_in(&rn(DEFAULT_FOLDER)).unwrap()), ["n1"]);
    assert!(s.notes_in(&rn(TRASH_FOLDER)).unwrap().is_empty());
}

#[test]
fn quick_notes_follow_is_system_paper_in_either_arrival_order() {
    // User-specific first, note second (and a second note the other way round).
    let s = store(vec![
        user_specific("n1", 1),
        n("n1", DEFAULT_FOLDER, "Quick one").item(),
        n("n2", DEFAULT_FOLDER, "Plain").item(),
        user_specific("n2", 0),
    ]);
    assert_eq!(ids(&s.quick_notes().unwrap()), ["n1"]);
    assert_eq!(ids(&s.notes_in(&rn(QUICK_NOTES_VIEW_ID)).unwrap()), ["n1"]);
    // Quick notes keep their real folder too.
    assert_eq!(s.notes_in(&rn(DEFAULT_FOLDER)).unwrap().len(), 2);
    let f = s.folders().unwrap();
    assert_eq!((f[0].id.as_str(), f[0].note_count), (QUICK_NOTES_VIEW_ID, 1));
}

#[test]
fn quick_note_flag_clears_and_deleted_quick_notes_vanish() {
    let mut s = store(vec![n("n1", DEFAULT_FOLDER, "Q").item(), n("n2", DEFAULT_FOLDER, "R").item(), user_specific("n1", 1), user_specific("n2", 1)]);
    assert_eq!(s.quick_notes().unwrap().len(), 2);
    s.apply_page(&page(vec![user_specific("n1", 0)], "t")).unwrap();
    let mut d = n("n2", DEFAULT_FOLDER, "R");
    d.deleted = 1;
    s.apply_page(&page(vec![d.item()], "t")).unwrap();
    assert!(s.quick_notes().unwrap().is_empty());
    // Removing the user-specific record by tombstone also clears it.
    s.apply_page(&page(vec![user_specific("n1", 1)], "t")).unwrap();
    assert_eq!(s.quick_notes().unwrap().len(), 1);
    s.apply_page(&page(vec![tomb("Note_UserSpecific::me/Private::Notes::me::n1")], "t")).unwrap();
    assert!(s.quick_notes().unwrap().is_empty());
}

#[test]
fn password_protected_notes_are_locked_and_never_searchable() {
    let mut p = n("p1", DEFAULT_FOLDER, "Secret title");
    p.ty = "PasswordProtectedNote";
    let mut s = store(vec![p.item()]);
    let list = s.notes_in(&rn(DEFAULT_FOLDER)).unwrap();
    assert!(list[0].locked);
    assert_eq!(list[0].snippet, "");
    assert!(s.notes_needing_body().unwrap().is_empty());
    s.put_note_body(&rn("p1"), b"x", "secretword").unwrap();
    assert_eq!(s.note_body(&rn("p1")).unwrap(), None);
    assert!(s.search("secretword").unwrap().is_empty());
    assert!(s.search("Secret").unwrap().is_empty());
}

#[test]
fn attachments_and_thumbnail_show_in_summary() {
    let mut note = n("n1", DEFAULT_FOLDER, "Pics");
    note.tag = "t1";
    let mut item = note.item();
    if let ChangeItem::Live(r) = &mut item {
        r.fields.insert(
            "FirstAttachmentThumbnail".into(),
            json!({"value": {"downloadURL": "https://example.invalid/x", "size": 1, "fileChecksum": "AQ"}}),
        );
    }
    let mut s = store(vec![item, attachment("a1", "n1", Some("m1")), media("m1", "n1")]);
    let list = s.notes_in(&rn(DEFAULT_FOLDER)).unwrap();
    assert!(list[0].has_attachments);
    assert_eq!(list[0].thumbnail, Some(rn("n1")));
    // A Deleted=1 attachment no longer counts.
    let mut dead = attachment("a1", "n1", Some("m1"));
    if let ChangeItem::Live(r) = &mut dead {
        r.fields.insert("Deleted".into(), json!({"value": 1}));
    }
    s.apply_page(&page(vec![dead], "t")).unwrap();
    assert!(!s.notes_in(&rn(DEFAULT_FOLDER)).unwrap()[0].has_attachments);
}

#[test]
fn note_tombstone_cascades() {
    let mut s = store(vec![
        n("n1", DEFAULT_FOLDER, "Alpha").item(),
        n("n2", DEFAULT_FOLDER, "Other").item(),
        attachment("a1", "n1", Some("m1")),
        media("m1", "n1"),
        attachment("a2", "n2", None),
        user_specific("n1", 1),
    ]);
    s.put_note_body(&rn("n1"), b"p", "cascadeword").unwrap();
    s.put_note_body(&rn("n2"), b"q", "other text").unwrap();
    s.apply_page(&page(vec![tomb("n1")], "t")).unwrap();
    for gone in ["n1", "a1", "m1", "Note_UserSpecific::me/Private::Notes::me::n1"] {
        assert!(s.record(&rn(gone)).unwrap().is_none(), "{gone}");
    }
    assert!(s.record(&rn("a2")).unwrap().is_some());
    assert_eq!(s.note_body(&rn("n1")).unwrap(), None);
    assert_eq!(s.note_body(&rn("n2")).unwrap().as_deref(), Some(&b"q"[..]));
    assert!(s.search("cascadeword").unwrap().is_empty());
    assert_eq!(ids(&s.notes_in(&rn(DEFAULT_FOLDER)).unwrap()), ["n2"]);
    let in_index: i64 = s.conn.query_row("SELECT COUNT(*) FROM note_index WHERE name = 'n1'", [], |r| r.get(0)).unwrap();
    assert_eq!(in_index, 0);
}

#[test]
fn attachment_tombstone_removes_media_and_updates_note() {
    let mut s = store(vec![n("n1", DEFAULT_FOLDER, "A").item(), attachment("a1", "n1", Some("m1")), media("m1", "n1")]);
    s.apply_page(&page(vec![tomb("a1")], "t")).unwrap();
    assert!(s.record(&rn("m1")).unwrap().is_none());
    assert!(!s.notes_in(&rn(DEFAULT_FOLDER)).unwrap()[0].has_attachments);
    // Unknown tombstones are harmless.
    s.apply_page(&page(vec![tomb("never-seen")], "t")).unwrap();
}

#[test]
fn full_resync_sweeps_unseen_rows() {
    let mut s = store(vec![
        folder("F-1", "Work"),
        n("n1", DEFAULT_FOLDER, "Keep").item(),
        n("n2", DEFAULT_FOLDER, "Vanish").item(),
        attachment("a2", "n2", None),
    ]);
    s.put_note_body(&rn("n2"), b"p", "vanishword").unwrap();
    s.begin_full_resync().unwrap();
    // The new listing no longer has n2 / a2 / F-1 and has a changed n1.
    let mut n1 = n("n1", DEFAULT_FOLDER, "Keep");
    n1.tag = "t9";
    s.apply_page(&page(vec![n1.item(), n("n3", DEFAULT_FOLDER, "New").item()], "fresh")).unwrap();
    s.finish_full_resync().unwrap();
    assert_eq!(s.sync_token().unwrap().as_deref(), Some("fresh"));
    assert_eq!(ids(&s.notes_in(&rn(DEFAULT_FOLDER)).unwrap()).len(), 2);
    for gone in ["n2", "a2", "F-1"] {
        assert!(s.record(&rn(gone)).unwrap().is_none(), "{gone}");
    }
    assert!(s.search("vanishword").unwrap().is_empty());
    assert_eq!(s.note_body(&rn("n2")).unwrap(), None);
    assert_eq!(s.record(&rn("n1")).unwrap().unwrap().change_tag, "t9");
    // A second sweep without begin removes nothing.
    s.finish_full_resync().unwrap();
    assert_eq!(s.notes_in(&rn(DEFAULT_FOLDER)).unwrap().len(), 2);
}

#[test]
fn search_matches_title_and_body_with_prefixes_and_ignores_operators() {
    let mut s = store(vec![
        n("n1", DEFAULT_FOLDER, "Grocery list").item(),
        n("n2", DEFAULT_FOLDER, "Meeting").item(),
        n("n3", DEFAULT_FOLDER, "Other").item(),
    ]);
    s.put_note_body(&rn("n2"), b"p", "buy bananas and oranges").unwrap();
    s.put_note_body(&rn("n3"), b"p", "unrelated").unwrap();
    assert_eq!(ids(&s.search("groc").unwrap()), ["n1"]); // title, prefix
    assert_eq!(ids(&s.search("BANANAS oranges").unwrap()), ["n2"]); // body, AND, case-insensitive
    assert!(s.search("bananas unrelated").unwrap().is_empty());
    assert!(s.search("").unwrap().is_empty());
    assert!(s.search("   ").unwrap().is_empty());
    // FTS syntax characters are literals, not errors.
    for q in ["\"", "a OR", "NEAR(", "title:x", "*", "'; DROP TABLE records;--"] {
        s.search(q).unwrap();
    }
    // Title changes are searchable without re-hydrating, and the body survives.
    let mut renamed = n("n2", DEFAULT_FOLDER, "Standup");
    renamed.tag = "t2";
    s.apply_page(&page(vec![renamed.item()], "t")).unwrap();
    assert_eq!(ids(&s.search("standup").unwrap()), ["n2"]);
    assert!(s.search("meeting").unwrap().is_empty());
    assert_eq!(ids(&s.search("bananas").unwrap()), ["n2"]);
}

#[test]
fn body_goes_stale_only_when_text_data_changes() {
    let mut s = store(vec![n("n1", DEFAULT_FOLDER, "A").item(), n("n2", DEFAULT_FOLDER, "B").item()]);
    assert_eq!(s.notes_needing_body().unwrap().len(), 2);
    s.put_note_body(&rn("n1"), b"p1", "one").unwrap();
    s.put_note_body(&rn("n2"), b"p2", "two").unwrap();
    assert!(s.notes_needing_body().unwrap().is_empty());
    assert_eq!(s.note_body(&rn("n1")).unwrap().as_deref(), Some(&b"p1"[..]));

    // New change tag and new title, same text: still fresh.
    let mut t = n("n1", DEFAULT_FOLDER, "A renamed");
    t.tag = "t2";
    // Different text: stale.
    let mut u = n("n2", DEFAULT_FOLDER, "B");
    u.text = "body-v2";
    u.tag = "t2";
    s.apply_page(&page(vec![t.item(), u.item()], "t")).unwrap();
    assert_eq!(s.notes_needing_body().unwrap(), vec![rn("n2")]);
    s.put_note_body(&rn("n2"), b"p2b", "two again").unwrap();
    assert!(s.notes_needing_body().unwrap().is_empty());
    assert_eq!(s.note_body(&rn("n2")).unwrap().as_deref(), Some(&b"p2b"[..]));
}

#[test]
fn asset_bodies_are_tracked_by_checksum() {
    let asset_note = |sum: &str, url: &str| {
        ChangeItem::Live(rec(
            "big",
            "Note",
            "t",
            json!({
                "TextDataAsset": {"value": {"downloadURL": url, "size": 9, "fileChecksum": sum}},
                "Folder": {"value": {"recordName": DEFAULT_FOLDER}},
            }),
            None,
        ))
    };
    let mut s = store(vec![asset_note("c1", "https://example.invalid/1")]);
    assert_eq!(s.notes_needing_body().unwrap(), vec![rn("big")]);
    s.put_note_body(&rn("big"), b"p", "txt").unwrap();
    // Only the signed URL changed (every lookup re-signs it): still fresh.
    s.apply_page(&page(vec![asset_note("c1", "https://example.invalid/2")], "t")).unwrap();
    assert!(s.notes_needing_body().unwrap().is_empty());
    s.apply_page(&page(vec![asset_note("c2", "https://example.invalid/3")], "t")).unwrap();
    assert_eq!(s.notes_needing_body().unwrap(), vec![rn("big")]);
}

#[test]
fn folders_are_ordered_and_counted() {
    let mut gone = n("n3", "F-b", "Gone");
    gone.deleted = 1;
    let s = store(vec![
        folder(TRASH_FOLDER, "Recently Deleted"),
        folder("F-b", "banana"),
        folder("F-a", "Apple"),
        folder("F-c", "cherry"),
        folder(DEFAULT_FOLDER, "Notes"),
        folder(QUICK_NOTES, "Quick Notes"),
        ChangeItem::Live(rec("F-smart", "Folder", "t", json!({"TitleEncrypted": {"value": b64("Smart")}, "SmartFolderQueryJSONEncrypted": {"value": "e30="}}), None)),
        n("n1", "F-b", "x").item(),
        n("n2", "F-b", "y").item(),
        gone.item(),
        n("n4", DEFAULT_FOLDER, "z").item(),
        n("n5", TRASH_FOLDER, "t").item(),
    ]);
    let f = s.folders().unwrap();
    let got: Vec<(&str, u32)> = f.iter().map(|f| (f.title.as_str(), f.note_count)).collect();
    assert_eq!(
        got,
        [("Quick Notes", 0), ("Notes", 1), ("Apple", 0), ("banana", 2), ("cherry", 0), ("Recently Deleted", 1)]
    );
    assert_eq!(f[0].id.as_str(), QUICK_NOTES_VIEW_ID);
    assert_eq!(f[0].kind, FolderKind::System(SystemFolder::QuickNotes));
    assert_eq!(f[1].kind, FolderKind::System(SystemFolder::Default));
    assert_eq!(f[3].kind, FolderKind::User);
    assert_eq!(f[5].kind, FolderKind::System(SystemFolder::Trash));
}

#[test]
fn system_folders_exist_even_in_an_empty_cache() {
    let s = SqliteStore::open_in_memory().unwrap();
    let f = s.folders().unwrap();
    assert_eq!(f.len(), 3);
    assert_eq!((f[0].title.as_str(), f[1].title.as_str(), f[2].title.as_str()), ("Quick Notes", "Notes", "Recently Deleted"));
}

#[test]
fn failed_page_rolls_back_including_the_token() {
    let mut s = store(vec![n("n1", DEFAULT_FOLDER, "A").item()]);
    // A record whose JSON can't be stored never happens via serde_json, so provoke an SQL error
    // instead: drop a table the page needs.
    s.conn.execute_batch("DROP TABLE kv").unwrap();
    let bad = page(vec![n("n9", DEFAULT_FOLDER, "B").item()], "next");
    assert!(s.apply_page(&bad).is_err());
    let count: i64 = s.conn.query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1);
}

#[test]
fn reopens_from_disk_and_rebuilds_on_version_mismatch() {
    let dir = std::env::temp_dir().join(format!("notes-store-test-{}", std::process::id()));
    let path = dir.join("nested").join("cache.sqlite");
    {
        let mut s = SqliteStore::open(&path).unwrap();
        s.apply_page(&page(vec![n("n1", DEFAULT_FOLDER, "A").item()], "keep")).unwrap();
    }
    {
        let s = SqliteStore::open(&path).unwrap();
        assert_eq!(s.sync_token().unwrap().as_deref(), Some("keep"));
        assert_eq!(s.notes_in(&rn(DEFAULT_FOLDER)).unwrap().len(), 1);
        s.conn.pragma_update(None, "user_version", 99).unwrap();
    }
    {
        let s = SqliteStore::open(&path).unwrap();
        assert_eq!(s.sync_token().unwrap(), None);
        assert!(s.notes_in(&rn(DEFAULT_FOLDER)).unwrap().is_empty());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn default_path_is_under_the_app_folder() {
    let p = SqliteStore::default_path();
    assert!(p.ends_with(std::path::Path::new(APP_ID).join("cache.sqlite")));
}
