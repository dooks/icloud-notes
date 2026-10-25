//! Synthetic records, a fake `CloudKitApi` and a real (temp-file) store. No network, no real Apple data.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use flate2::Compression;
use flate2::write::GzEncoder;
use icloud_session::StoredCookie;
use notes_model::{
    AuthError, ChangesPage, CkError, CkRecord, DEFAULT_FOLDER, FolderKind, SystemFolder, TRASH_FOLDER,
};
use notes_proto::proto::{AttachmentInfo, AttributeRun, Document, Note, NoteStoreProto};
use prost::Message;
use serde_json::{Value, json};

use super::*;

// ---- fixtures -------------------------------------------------------------------------------

fn b64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

fn gzip(b: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = GzEncoder::new(Vec::new(), Compression::default());
    e.write_all(b).unwrap();
    e.finish().unwrap()
}

enum Part {
    T(&'static str),
    A(&'static str, &'static str),
}

/// A gzipped `NoteStoreProto` for `TextDataEncrypted`.
fn body(parts: &[Part]) -> String {
    let mut text = String::new();
    let mut runs = Vec::new();
    for p in parts {
        match p {
            Part::T(t) => {
                text.push_str(t);
                runs.push(AttributeRun { length: t.encode_utf16().count() as i32, ..Default::default() });
            }
            Part::A(id, uti) => {
                text.push('\u{FFFC}');
                runs.push(AttributeRun {
                    length: 1,
                    attachment_info: Some(AttachmentInfo {
                        attachment_identifier: Some((*id).into()),
                        type_uti: Some((*uti).into()),
                    }),
                    ..Default::default()
                });
            }
        }
    }
    let proto = NoteStoreProto {
        document: Document { version: 0, note: Note { note_text: text, attribute_run: runs } },
    };
    b64(&gzip(&proto.encode_to_vec()))
}

fn rec(name: &str, ty: &str, f: Value, parent: Option<&str>) -> ChangeItem {
    ChangeItem::Live(CkRecord {
        name: name.into(),
        record_type: RecordType::from_wire(ty),
        change_tag: "t1".into(),
        fields: f.as_object().unwrap().clone(),
        parent: parent.map(RecordName::from),
    })
}

fn r(name: &str) -> Value {
    json!({"value": {"recordName": name}, "type": "REFERENCE"})
}

fn s(text: &str) -> Value {
    json!({"value": b64(text.as_bytes()), "type": "ENCRYPTED_BYTES"})
}

fn asset_json(checksum: &str) -> Value {
    json!({"downloadURL": format!("https://stale.invalid/{checksum}"), "size": 8, "fileChecksum": checksum})
}

fn asset(checksum: &str) -> Value {
    json!({"value": asset_json(checksum), "type": "ASSETID"})
}

fn asset_list(checksums: &[&str]) -> Value {
    json!({"value": checksums.iter().map(|c| asset_json(c)).collect::<Vec<_>>(), "type": "ASSETIDLIST"})
}

fn folder(name: &str, title: &str) -> ChangeItem {
    rec(name, "Folder", json!({"TitleEncrypted": s(title)}), None)
}

fn note(name: &str, ty: &str, folder: &str, title: &str, modified: i64, text: Option<String>) -> ChangeItem {
    let mut f = json!({
        "TitleEncrypted": s(title),
        "SnippetEncrypted": s("snippet"),
        "Folder": r(folder),
        "ModificationDate": {"value": modified, "type": "TIMESTAMP"},
    });
    if let Some(t) = text {
        f["TextDataEncrypted"] = json!({"value": t, "type": "ENCRYPTED_BYTES"});
    }
    rec(name, ty, f, Some(folder))
}

fn attachment(name: &str, note: &str, uti: &str, extra: Value) -> ChangeItem {
    let mut f = json!({"Note": r(note), "UTI": {"value": uti, "type": "STRING"}});
    for (k, v) in extra.as_object().unwrap() {
        f[k] = v.clone();
    }
    rec(name, "Attachment", f, Some(note))
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nxxxx";

/// A tiny table (1 row, 1 column) holding "cell-text".
fn table_bytes() -> Vec<u8> {
    use notes_proto::proto::*;
    let oid = |i: i32| ObjectId { object_index: i, ..Default::default() };
    let entry = || MergeableDataObjectEntry { register_latest: RegisterLatest { contents: oid(0) }, ..Default::default() };
    let ordered = |u: &[u8]| MergeableDataObjectEntry {
        ordered_set: Some(OrderedSet {
            ordering: OrderedSetOrdering {
                array: OrderedSetOrderingArray {
                    contents: Default::default(),
                    attachment: vec![OrderedSetOrderingArrayAttachment { index: 0, uuid: u.to_vec() }],
                },
                contents: Default::default(),
            },
            elements: Default::default(),
        }),
        ..entry()
    };
    let dict = |k: i32, v: i32| MergeableDataObjectEntry {
        dictionary: Some(Dictionary { element: vec![DictionaryElement { key: oid(k), value: oid(v) }] }),
        ..entry()
    };
    let id_obj = |uuid_idx: u64| MergeableDataObjectEntry {
        custom_map: Some(MergeableDataObjectMap {
            r#type: 1,
            map_entry: vec![MapEntry {
                key: 3,
                value: ObjectId { unsigned_integer_value: uuid_idx, ..Default::default() },
            }],
        }),
        ..entry()
    };
    let map = |k: i32, v: i32| MapEntry { key: k, value: oid(v) };
    let entries = vec![
        MergeableDataObjectEntry {
            custom_map: Some(MergeableDataObjectMap { r#type: 0, map_entry: vec![map(0, 1), map(1, 2), map(2, 3)] }),
            ..entry()
        },
        ordered(b"r1"),
        ordered(b"c1"),
        dict(4, 5),
        id_obj(1),
        dict(6, 7),
        id_obj(0),
        MergeableDataObjectEntry {
            note: Some(Note {
                note_text: "cell-text".into(),
                attribute_run: vec![AttributeRun { length: 9, ..Default::default() }],
            }),
            ..entry()
        },
    ];
    MergableDataProto {
        mergable_data_object: MergableDataObject {
            version: 0,
            mergeable_data_object_data: MergeableDataObjectData {
                mergeable_data_object_entry: entries,
                mergeable_data_object_key_item: [
                    "crRows",
                    "crColumns",
                    "cellColumns",
                    "identity",
                    "crTableColumnDirection",
                    "self",
                ]
                .map(String::from)
                .to_vec(),
                mergeable_data_object_type_item: ["com.apple.notes.ICTable", "com.apple.CRDT.NSUUID"]
                    .map(String::from)
                    .to_vec(),
                mergeable_data_object_uuid_item: vec![b"r1".to_vec(), b"c1".to_vec()],
            },
        },
    }
    .encode_to_vec()
}

/// Every record the fake account holds.
fn account() -> Vec<ChangeItem> {
    use Part::*;
    let mut v = vec![
        folder(DEFAULT_FOLDER, "Notes"),
        folder("F-work", "Work"),
        folder(TRASH_FOLDER, "Recently Deleted"),
    ];
    // An image (original + previews), with a list thumbnail.
    let mut img = note("n-img", "Note", DEFAULT_FOLDER, "Picture", 5000, Some(body(&[T("see "), A("att-img", "public.png"), T("\n")])));
    if let ChangeItem::Live(n) = &mut img {
        n.fields.insert("FirstAttachmentThumbnail".into(), asset("CS-THUMB"));
    }
    v.push(img);
    v.push(attachment(
        "att-img",
        "n-img",
        "public.png",
        json!({"Media": r("media-1"), "Width": {"value": 100}, "Height": {"value": 50},
               "PreviewImages": asset_list(&["CS-IMG-P0", "CS-IMG-P1"]), "PreviewAppearances": {"value": [0, 0]}}),
    ));
    v.push(rec("media-1", "Media", json!({"Asset": asset("CS-IMG")}), Some("n-img")));
    // A table.
    v.push(note("n-table", "Note", DEFAULT_FOLDER, "Grid", 4000, Some(body(&[A("att-table", "com.apple.notes.table"), T("\n")]))));
    v.push(attachment(
        "att-table",
        "n-table",
        "com.apple.notes.table",
        json!({"MergeableDataEncrypted": {"value": b64(&gzip(&table_bytes())), "type": "ENCRYPTED_BYTES"}}),
    ));
    // Drawings: one with a fallback; one with previews only, appearance order [dark, light].
    v.push(note(
        "n-draw",
        "Note",
        "F-work",
        "Sketch",
        3000,
        Some(body(&[A("att-fb", "com.apple.drawing.2"), A("att-prev", "com.apple.paper"), A("att-none", "com.apple.paper"), T("\n")])),
    ));
    v.push(attachment(
        "att-fb",
        "n-draw",
        "com.apple.drawing.2",
        json!({"FallbackImage": asset("CS-FB"), "PreviewImages": asset_list(&["CS-FB-P0", "CS-FB-P1"]),
               "PreviewAppearances": {"value": [0, 1]}}),
    ));
    v.push(attachment(
        "att-prev",
        "n-draw",
        "com.apple.paper",
        json!({"PreviewImages": asset_list(&["CS-PV-DARK", "CS-PV-LIGHT"]), "PreviewAppearances": {"value": [1, 0]}}),
    ));
    v.push(attachment("att-none", "n-draw", "com.apple.paper", json!({})));
    // Inline attachments and an unknown kind.
    v.push(note(
        "n-inline",
        "Note",
        DEFAULT_FOLDER,
        "Inline",
        2000,
        Some(body(&[
            T("sum "),
            A("inl-calc", "com.apple.notes.inlinetextattachment.calculateresult"),
            T(" "),
            A("inl-tag", "com.apple.notes.inlinetextattachment.hashtag"),
            T(" "),
            A("att-pdf", "com.apple.notes.gallery"),
            T("\n"),
        ])),
    ));
    v.push(rec("inl-calc", "InlineAttachment", json!({"AltTextEncrypted": s("=42")}), Some("n-inline")));
    v.push(rec("inl-tag", "InlineAttachment", json!({"DisplayTextEncrypted": s("#synthetic")}), Some("n-inline")));
    v.push(attachment("att-pdf", "n-inline", "com.apple.notes.gallery", json!({})));
    // Locked, trashed, Deleted=1, Quick Note.
    v.push(note("n-lock", "PasswordProtectedNote", DEFAULT_FOLDER, "Secret", 1500, None));
    v.push(note("n-trash", "Note", TRASH_FOLDER, "Old draft", 1000, Some(body(&[T("discarded words\n")]))));
    let mut gone = note("n-gone", "Note", DEFAULT_FOLDER, "Hidden", 900, Some(body(&[T("hidden words\n")])));
    if let ChangeItem::Live(n) = &mut gone {
        n.fields.insert("Deleted".into(), json!({"value": 1}));
    }
    v.push(gone);
    v.push(note("n-quick", "Note", DEFAULT_FOLDER, "Quick idea", 6000, Some(body(&[T("quickword here\n")]))));
    v.push(rec(
        "Note_UserSpecific::me/Private::Notes::me::n-quick",
        "Note_UserSpecific",
        json!({"IsSystemPaper": {"value": 1}, "Note": r("n-quick")}),
        None,
    ));
    v
}

/// Replace every asset URL with a fresh one, the way `records/lookup` does.
fn freshen(v: &mut Value) {
    match v {
        Value::Object(o) => {
            if let Some(Value::String(u)) = o.get_mut("downloadURL") {
                *u = u.replace("stale.invalid", "fresh.invalid");
            }
            o.values_mut().for_each(freshen);
        }
        Value::Array(a) => a.iter_mut().for_each(freshen),
        _ => {}
    }
}

// ---- the fake API ---------------------------------------------------------------------------

struct Shared {
    items: Mutex<Vec<ChangeItem>>,
    token: Mutex<String>,
    files: Mutex<HashMap<String, Vec<u8>>>,
    zone_checks: AtomicUsize,
    lookups: AtomicUsize,
    downloads: AtomicUsize,
    /// `lookup` fails with an auth error.
    auth_fail: AtomicBool,
    /// `connect` fails with an auth error.
    connect_auth_fail: AtomicBool,
    connect_offline: AtomicBool,
}

#[derive(Clone)]
struct FakeApi(Arc<Shared>);

impl CloudKitApi for FakeApi {
    async fn zone_token(&self) -> Result<String, ApiError> {
        self.0.zone_checks.fetch_add(1, Ordering::SeqCst);
        Ok(lock(&self.0.token).clone())
    }

    async fn changes(&self, token: Option<&str>, _limit: u32) -> Result<ChangesPage, ApiError> {
        let current = lock(&self.0.token).clone();
        let items = if token == Some(current.as_str()) { Vec::new() } else { lock(&self.0.items).clone() };
        Ok(ChangesPage { items, sync_token: current, more_coming: false })
    }

    async fn lookup(&self, names: &[RecordName], _keys: Option<&[&str]>) -> Result<Vec<ChangeItem>, ApiError> {
        self.0.lookups.fetch_add(1, Ordering::SeqCst);
        if self.0.auth_fail.load(Ordering::SeqCst) {
            return Err(AuthError::SessionInvalid { status: 421, reason: "synthetic".into() }.into());
        }
        let items = lock(&self.0.items);
        Ok(items
            .iter()
            .filter(|i| names.contains(i.name()))
            .cloned()
            .map(|mut i| {
                if let ChangeItem::Live(r) = &mut i {
                    let mut f = Value::Object(std::mem::take(&mut r.fields));
                    freshen(&mut f);
                    r.fields = f.as_object().unwrap().clone();
                }
                i
            })
            .collect())
    }

    async fn download(&self, asset: &AssetRef) -> Result<Vec<u8>, ApiError> {
        self.0.downloads.fetch_add(1, Ordering::SeqCst);
        if !asset.download_url.starts_with("https://fresh.invalid/") {
            return Err(ApiError::Http("410 Gone (stale URL)".into()));
        }
        lock(&self.0.files)
            .get(&asset.file_checksum)
            .cloned()
            .ok_or_else(|| ApiError::Ck(CkError::new("NOT_FOUND", "no such file")))
    }
}

fn files() -> HashMap<String, Vec<u8>> {
    ["CS-IMG", "CS-IMG-P0", "CS-IMG-P1", "CS-THUMB", "CS-FB", "CS-FB-P0", "CS-FB-P1", "CS-PV-DARK", "CS-PV-LIGHT"]
        .into_iter()
        .map(|c| (c.to_owned(), [PNG, c.as_bytes()].concat()))
        .collect()
}

#[derive(Default)]
struct Events(Mutex<Vec<ServiceEvent>>);

impl EventSink for Events {
    fn emit(&self, e: ServiceEvent) {
        lock(&self.0).push(e);
    }
}

impl Events {
    fn all(&self) -> Vec<ServiceEvent> {
        lock(&self.0).clone()
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!("notes-service-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn session_jar() -> CookieJar {
    let mut jar = CookieJar::new();
    jar.insert(StoredCookie {
        name: icloud_session::SESSION_COOKIE.into(),
        value: "synthetic-token".into(),
        domain: ".icloud.com".into(),
        path: "/".into(),
        expires: None,
        secure: true,
        http_only: true,
    });
    jar
}

struct Env {
    svc: Core<FakeApi>,
    api: FakeApi,
    events: Arc<Events>,
    dir: TempDir,
}

fn env_with(items: Vec<ChangeItem>, signed_in: bool) -> Env {
    let dir = TempDir::new();
    let api = FakeApi(Arc::new(Shared {
        items: Mutex::new(items),
        token: Mutex::new("tok-1".into()),
        files: Mutex::new(files()),
        zone_checks: AtomicUsize::new(0),
        lookups: AtomicUsize::new(0),
        downloads: AtomicUsize::new(0),
        auth_fail: AtomicBool::new(false),
        connect_auth_fail: AtomicBool::new(false),
        connect_offline: AtomicBool::new(false),
    }));
    let events = Arc::new(Events::default());
    let svc = open_at(&dir.0, &api, events.clone(), signed_in);
    Env { svc, api, events, dir }
}

fn open_at(dir: &std::path::Path, api: &FakeApi, events: Arc<Events>, signed_in: bool) -> Core<FakeApi> {
    let jars = DpapiStore::new(dir.join("session.dpapi"));
    if signed_in {
        jars.save(&session_jar()).unwrap();
    }
    let handle = api.clone();
    let connect: Connect<FakeApi> = Box::new(move |_jar| {
        let api = handle.clone();
        Box::pin(async move {
            if api.0.connect_auth_fail.load(Ordering::SeqCst) {
                return Err(AuthError::SessionInvalid { status: 421, reason: "synthetic".into() }.into());
            }
            if api.0.connect_offline.load(Ordering::SeqCst) {
                return Err(ApiError::Http("synthetic offline".into()));
            }
            Ok(api)
        })
    });
    Core::with_parts(dir.to_path_buf(), events, jars, connect).unwrap()
}

async fn synced() -> Env {
    let env = env_with(account(), true);
    env.svc.start().await.unwrap();
    env.svc.wait_idle().await;
    env
}

fn ids(v: &[NoteSummary]) -> Vec<&str> {
    v.iter().map(|n| n.id.as_str()).collect()
}

// ---- tests ----------------------------------------------------------------------------------

#[tokio::test]
async fn start_syncs_hydrates_and_reports() {
    let env = env_with(account(), true);
    assert!(env.svc.status().signed_in);
    env.svc.start().await.unwrap();
    env.svc.wait_idle().await;

    let st = env.svc.status();
    assert!(st.signed_in && !st.syncing && st.error.is_none());
    let last = st.last_sync.unwrap();
    assert!(last.len() == 20 && last.ends_with('Z'), "{last}");

    let ev = env.events.all();
    assert!(ev.iter().any(|e| matches!(e, ServiceEvent::SyncProgress(p) if p.pages == 1 && p.records > 0)));
    assert_eq!(ev.last(), Some(&ServiceEvent::SyncDone));
    assert!(!ev.contains(&ServiceEvent::AuthRequired));
}

#[tokio::test]
async fn folders_notes_and_visibility() {
    let env = synced().await;
    let folders = env.svc.folders().unwrap();
    let titles: Vec<_> = folders.iter().map(|f| (f.id.as_str(), f.title.as_str(), f.note_count)).collect();
    assert_eq!(
        titles,
        vec![
            ("quick-notes", "Quick Notes", 1),
            (DEFAULT_FOLDER, "Notes", 5), // img, table, inline, lock, quick (Deleted=1 excluded)
            ("F-work", "Work", 1),
            (TRASH_FOLDER, "Recently Deleted", 1),
        ]
    );
    assert_eq!(folders[0].kind, FolderKind::System(SystemFolder::QuickNotes));

    let default = env.svc.notes(DEFAULT_FOLDER).unwrap();
    assert_eq!(ids(&default), vec!["n-quick", "n-img", "n-table", "n-inline", "n-lock"]);
    let lock_row = default.iter().find(|n| n.id.as_str() == "n-lock").unwrap();
    assert!(lock_row.locked && lock_row.snippet.is_empty());
    let img_row = default.iter().find(|n| n.id.as_str() == "n-img").unwrap();
    assert!(img_row.has_attachments);
    assert_eq!(img_row.thumbnail.as_ref().map(|t| t.as_str()), Some("n-img"));

    assert_eq!(ids(&env.svc.notes("quick-notes").unwrap()), vec!["n-quick"]);
    assert_eq!(ids(&env.svc.notes(TRASH_FOLDER).unwrap()), vec!["n-trash"]);
    assert_eq!(ids(&env.svc.notes("F-work").unwrap()), vec!["n-draw"]);
}

#[tokio::test]
async fn search_covers_hydrated_bodies_but_not_hidden_notes() {
    let env = synced().await;
    assert_eq!(ids(&env.svc.search("quickword").unwrap()), vec!["n-quick"]);
    assert_eq!(ids(&env.svc.search("picture").unwrap()), vec!["n-img"]); // title
    assert!(env.svc.search("hidden").unwrap().is_empty()); // Deleted=1
    assert!(env.svc.search("discarded").unwrap().is_empty()); // trashed
    assert!(env.svc.search("secret").unwrap().is_empty()); // locked
    assert!(env.svc.search("   ").unwrap().is_empty());
}

#[tokio::test]
async fn note_html_image_uses_the_webview_asset_host() {
    let env = synced().await;
    let page = env.svc.note_html("n-img", false).unwrap();
    assert_eq!(page.title, "Picture");
    let h = &page.html;
    assert!(h.contains(r#"src="http://notes-asset.localhost/att-img/original""#), "{h}");
    assert!(h.contains(r#"width="100""#) && h.contains(r#"height="50""#));
    assert!(h.contains("img-src http://notes-asset.localhost"), "CSP must allow the asset host");
    assert!(!h.contains("notes-asset://"));
    assert!(h.contains("see "));
}

#[tokio::test]
async fn note_html_table_and_inline_attachments() {
    let env = synced().await;
    let h = env.svc.note_html("n-table", false).unwrap().html;
    assert!(h.contains("<table") && h.contains("cell-text"), "{h}");

    let h = env.svc.note_html("n-inline", false).unwrap().html;
    assert!(h.contains("=42") && h.contains("#synthetic"), "{h}");
    assert!(!h.contains("att-pdf"), "unknown attachment becomes a placeholder: {h}");
}

#[tokio::test]
async fn note_html_drawings_use_fallback_then_the_preview_for_the_theme() {
    let env = synced().await;
    let light = env.svc.note_html("n-draw", false).unwrap().html;
    assert!(light.contains("/att-fb/fallback"), "{light}");
    assert!(light.contains("/att-prev/preview-light"), "{light}");
    let dark = env.svc.note_html("n-draw", true).unwrap().html;
    assert!(dark.contains("/att-prev/preview-dark"), "{dark}");
    // No image at all → labelled placeholder, no <img> for it.
    assert!(!light.contains("att-none"));
}

#[tokio::test]
async fn note_html_locked_trashed_deleted_unknown() {
    let env = synced().await;
    let locked = env.svc.note_html("n-lock", false).unwrap();
    assert_eq!(locked.title, "Secret");
    assert!(locked.html.contains("locked"));

    let trashed = env.svc.note_html("n-trash", true).unwrap();
    assert!(trashed.html.contains("discarded words") && trashed.html.contains("class=\"dark\""));

    assert!(env.svc.note_html("n-gone", false).is_err());
    assert!(env.svc.note_html("no-such-note", false).is_err());
    assert!(env.svc.note_html(DEFAULT_FOLDER, false).is_err(), "a folder is not a note");
}

#[tokio::test]
async fn assets_are_fetched_once_then_served_from_the_checksum_cache() {
    let env = synced().await;
    let (bytes, mime) = env.svc.asset("att-img", AssetVariant::Original).await.unwrap();
    assert_eq!(bytes, [PNG, b"CS-IMG"].concat());
    assert_eq!(mime, "image/png");
    assert_eq!(env.api.0.lookups.load(Ordering::SeqCst), 1);
    assert_eq!(env.api.0.downloads.load(Ordering::SeqCst), 1);

    env.svc.asset("att-img", AssetVariant::Original).await.unwrap();
    assert_eq!(env.api.0.downloads.load(Ordering::SeqCst), 1, "second read is a cache hit");

    // A new process (new Core on the same data dir) still hits the cache, even with the API gone.
    let api2 = env.api.clone();
    api2.0.connect_offline.store(true, Ordering::SeqCst);
    let again = open_at(&env.dir.0, &api2, Arc::new(Events::default()), true);
    let (b2, _) = again.asset("att-img", AssetVariant::Original).await.unwrap();
    assert_eq!(b2, bytes);
    assert_eq!(api2.0.downloads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn asset_variants_pick_by_value_and_the_note_thumbnail_works() {
    let env = synced().await;
    let get = |id: &'static str, v| {
        let svc = &env.svc;
        async move { svc.asset(id, v).await.unwrap().0 }
    };
    // PreviewAppearances [1, 0]: dark is the *first* image, light the second.
    assert_eq!(get("att-prev", AssetVariant::PreviewDark).await, [PNG, b"CS-PV-DARK"].concat());
    assert_eq!(get("att-prev", AssetVariant::PreviewLight).await, [PNG, b"CS-PV-LIGHT"].concat());
    // [0, 1]: the other way round.
    assert_eq!(get("att-fb", AssetVariant::PreviewDark).await, [PNG, b"CS-FB-P1"].concat());
    assert_eq!(get("att-fb", AssetVariant::Fallback).await, [PNG, b"CS-FB"].concat());
    // The note's list thumbnail.
    assert_eq!(get("n-img", AssetVariant::Thumb).await, [PNG, b"CS-THUMB"].concat());
    // A previews-only attachment has no fallback, so `fallback` degrades to the light preview.
    assert_eq!(get("att-prev", AssetVariant::Fallback).await, [PNG, b"CS-PV-LIGHT"].concat());
}

#[tokio::test]
async fn asset_errors_are_cmd_errors() {
    let env = synced().await;
    for (id, v) in [
        ("no-such-id", AssetVariant::Original),
        ("n-trash", AssetVariant::Thumb),        // a note without a thumbnail
        ("att-table", AssetVariant::Original),   // no file
        ("n-gone", AssetVariant::Thumb),         // Deleted=1
    ] {
        let e = env.svc.asset(id, v).await.unwrap_err();
        assert_eq!(e.code, CmdErrorCode::Internal, "{id}");
    }
}

#[tokio::test]
async fn changed_checksum_means_redownload() {
    let env = synced().await;
    env.svc.asset("att-img", AssetVariant::Original).await.unwrap();
    assert_eq!(env.api.0.downloads.load(Ordering::SeqCst), 1);

    // The image is replaced on the server: new content, new checksum, same record name.
    lock(&env.api.0.files).insert("CS-IMG-V2".into(), [PNG, b"V2"].concat());
    for item in lock(&env.api.0.items).iter_mut() {
        if let ChangeItem::Live(rec) = item
            && rec.name.as_str() == "media-1"
        {
            rec.fields.insert("Asset".into(), asset("CS-IMG-V2"));
            rec.change_tag = "t2".into();
        }
    }
    *lock(&env.api.0.token) = "tok-2".into();
    let changed = env.svc.refresh().await.unwrap().changed;
    assert!(changed > 0);

    let (bytes, _) = env.svc.asset("att-img", AssetVariant::Original).await.unwrap();
    assert_eq!(bytes, [PNG, b"V2"].concat());
    assert_eq!(env.api.0.downloads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn refresh_with_nothing_new_is_the_cheap_check() {
    let env = synced().await;
    let checks = env.api.0.zone_checks.load(Ordering::SeqCst);
    let r = env.svc.refresh().await.unwrap();
    assert_eq!(r.changed, 0);
    assert_eq!(env.api.0.zone_checks.load(Ordering::SeqCst), checks + 1);
}

#[tokio::test]
async fn start_without_a_jar_asks_for_sign_in() {
    let env = env_with(account(), false);
    assert!(!env.svc.status().signed_in);
    let e = env.svc.start().await.unwrap_err();
    assert_eq!(e.code, CmdErrorCode::Auth);
    assert_eq!(env.events.all(), vec![ServiceEvent::AuthRequired]);
    assert!(!env.svc.status().signed_in && env.svc.status().error.is_some());
    // Cached data (none yet) is still readable.
    assert!(env.svc.folders().unwrap().iter().all(|f| f.note_count == 0));
}

#[tokio::test]
async fn rejected_session_raises_auth_required_and_offline_does_not() {
    let env = env_with(account(), true);
    env.api.0.connect_offline.store(true, Ordering::SeqCst);
    let e = env.svc.start().await.unwrap_err();
    assert_eq!(e.code, CmdErrorCode::Offline);
    assert!(env.events.all().is_empty());
    assert!(env.svc.status().signed_in, "offline is not a sign-out");

    env.api.0.connect_offline.store(false, Ordering::SeqCst);
    env.api.0.connect_auth_fail.store(true, Ordering::SeqCst);
    let e = env.svc.refresh().await.unwrap_err();
    assert_eq!(e.code, CmdErrorCode::Auth);
    assert_eq!(env.events.all(), vec![ServiceEvent::AuthRequired]);
    assert!(!env.svc.status().signed_in);
}

#[tokio::test]
async fn an_auth_error_from_an_asset_fetch_raises_auth_required() {
    let env = synced().await;
    env.api.0.auth_fail.store(true, Ordering::SeqCst);
    let e = env.svc.asset("att-img", AssetVariant::Original).await.unwrap_err();
    assert_eq!(e.code, CmdErrorCode::Auth);
    assert!(env.events.all().contains(&ServiceEvent::AuthRequired));
    assert!(!env.svc.status().signed_in);

    // Re-login: the host hands over a fresh jar and the service resumes.
    env.api.0.auth_fail.store(false, Ordering::SeqCst);
    env.svc.signed_in(session_jar()).await.unwrap();
    env.svc.wait_idle().await;
    assert!(env.svc.status().signed_in && env.svc.status().error.is_none());
    env.svc.asset("att-img", AssetVariant::Original).await.unwrap();
}

#[tokio::test]
async fn sign_in_then_sign_out() {
    let env = env_with(account(), false);
    env.svc.signed_in(session_jar()).await.unwrap();
    env.svc.wait_idle().await;
    assert!(env.svc.status().signed_in && env.svc.status().last_sync.is_some());
    assert_eq!(env.svc.folders().unwrap().len(), 4);

    env.svc.sign_out().await.unwrap();
    assert!(!env.svc.status().signed_in);
    assert!(!env.dir.0.join("session.dpapi").exists());
    assert_eq!(env.svc.refresh().await.unwrap_err().code, CmdErrorCode::Auth);
}

#[tokio::test]
async fn an_undecodable_body_does_not_stall_the_run() {
    let mut items = account();
    items.push(note("n-bad", "Note", DEFAULT_FOLDER, "Broken", 7000, Some(b64(b"not a protobuf at all"))));
    let env = env_with(items, true);
    env.svc.start().await.unwrap();
    env.svc.wait_idle().await;
    assert!(env.svc.status().error.is_none());
    // The good bodies are searchable; the bad one shows a placeholder instead of failing.
    assert_eq!(ids(&env.svc.search("quickword").unwrap()), vec!["n-quick"]);
    let h = env.svc.note_html("n-bad", false).unwrap().html;
    assert!(h.contains("not been downloaded"), "{h}");
}

#[tokio::test]
async fn periodic_refresh_runs_only_while_focused() {
    let env = synced().await;
    let focused = Arc::new(AtomicBool::new(false));
    let f = focused.clone();
    let task = env.svc.spawn_periodic_refresh(Duration::from_millis(20), move || f.load(Ordering::SeqCst));
    let base = env.api.0.zone_checks.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(env.api.0.zone_checks.load(Ordering::SeqCst), base, "unfocused: no refresh");
    focused.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(150)).await;
    task.abort();
    assert!(env.api.0.zone_checks.load(Ordering::SeqCst) > base, "focused: refreshes");
}

#[test]
fn timestamps_are_rfc3339() {
    assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
    assert_eq!(rfc3339(1_760_000_000), "2025-10-09T08:53:20Z");
    assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
}
