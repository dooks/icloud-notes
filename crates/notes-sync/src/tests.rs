use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use notes_model::view::{FolderView, NoteSummary};
use notes_model::{
    ApiError, AssetRef, AuthError, ChangeItem, ChangesPage, CkError, CkRecord, CkTombstone, CloudKitApi, DecodeError,
    NotesStore, RecordName, RecordType, StoreError, SyncError,
};
use serde_json::json;

use super::*;

type Q<T> = Mutex<VecDeque<T>>;

#[derive(Default)]
struct FakeApi {
    zone_tokens: Q<Result<String, ApiError>>,
    pages: Q<Result<ChangesPage, ApiError>>,
    lookups: Q<Result<Vec<ChangeItem>, ApiError>>,
    downloads: Q<Result<Vec<u8>, ApiError>>,
    /// Token argument of every `changes` call.
    change_calls: Mutex<Vec<Option<String>>>,
    zone_calls: Mutex<u32>,
}

impl FakeApi {
    fn pages(self, pages: Vec<Result<ChangesPage, ApiError>>) -> Self {
        *self.pages.lock().unwrap() = pages.into();
        self
    }
    fn zone(self, t: Vec<Result<String, ApiError>>) -> Self {
        *self.zone_tokens.lock().unwrap() = t.into();
        self
    }
    fn calls(&self) -> Vec<Option<String>> {
        self.change_calls.lock().unwrap().clone()
    }
}

impl CloudKitApi for FakeApi {
    async fn zone_token(&self) -> Result<String, ApiError> {
        *self.zone_calls.lock().unwrap() += 1;
        self.zone_tokens.lock().unwrap().pop_front().expect("unscripted zone_token")
    }
    async fn changes(&self, token: Option<&str>, limit: u32) -> Result<ChangesPage, ApiError> {
        assert_eq!(limit, PAGE_LIMIT);
        self.change_calls.lock().unwrap().push(token.map(str::to_owned));
        self.pages.lock().unwrap().pop_front().expect("unscripted changes")
    }
    async fn lookup(&self, _names: &[RecordName], _keys: Option<&[&str]>) -> Result<Vec<ChangeItem>, ApiError> {
        self.lookups.lock().unwrap().pop_front().expect("unscripted lookup")
    }
    async fn download(&self, _asset: &AssetRef) -> Result<Vec<u8>, ApiError> {
        self.downloads.lock().unwrap().pop_front().expect("unscripted download")
    }
}

#[derive(Default)]
struct FakeStore {
    token: Option<String>,
    /// Names applied, in order (live and tombstone).
    applied: Vec<String>,
    events: Vec<&'static str>,
    records: HashMap<RecordName, CkRecord>,
    needing: Vec<RecordName>,
    bodies: HashMap<RecordName, (Vec<u8>, String)>,
    /// Fail `apply_page` on this call number (1-based).
    fail_apply_on: Option<u32>,
    apply_calls: u32,
}

impl NotesStore for FakeStore {
    fn sync_token(&self) -> Result<Option<String>, StoreError> {
        Ok(self.token.clone())
    }
    fn apply_page(&mut self, page: &ChangesPage) -> Result<(), StoreError> {
        self.apply_calls += 1;
        if self.fail_apply_on == Some(self.apply_calls) {
            return Err(StoreError::Db("disk full".into()));
        }
        self.applied.extend(page.items.iter().map(|i| i.name().to_string()));
        self.token = Some(page.sync_token.clone());
        Ok(())
    }
    fn begin_full_resync(&mut self) -> Result<(), StoreError> {
        self.events.push("begin");
        self.token = None;
        Ok(())
    }
    fn finish_full_resync(&mut self) -> Result<(), StoreError> {
        self.events.push("finish");
        Ok(())
    }
    fn record(&self, name: &RecordName) -> Result<Option<CkRecord>, StoreError> {
        Ok(self.records.get(name).cloned())
    }
    fn folders(&self) -> Result<Vec<FolderView>, StoreError> {
        Ok(vec![])
    }
    fn notes_in(&self, _folder: &RecordName) -> Result<Vec<NoteSummary>, StoreError> {
        Ok(vec![])
    }
    fn quick_notes(&self) -> Result<Vec<NoteSummary>, StoreError> {
        Ok(vec![])
    }
    fn search(&self, _q: &str) -> Result<Vec<NoteSummary>, StoreError> {
        Ok(vec![])
    }
    fn note_body(&self, note: &RecordName) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.bodies.get(note).map(|b| b.0.clone()))
    }
    fn put_note_body(&mut self, note: &RecordName, proto: &[u8], plain_text: &str) -> Result<(), StoreError> {
        self.bodies.insert(note.clone(), (proto.to_vec(), plain_text.to_owned()));
        self.needing.retain(|n| n != note);
        Ok(())
    }
    fn notes_needing_body(&self) -> Result<Vec<RecordName>, StoreError> {
        Ok(self.needing.clone())
    }
}

fn rec(name: &str, fields: serde_json::Value) -> CkRecord {
    CkRecord {
        name: name.into(),
        record_type: RecordType::Note,
        change_tag: "t".into(),
        fields: fields.as_object().cloned().unwrap_or_default(),
        parent: None,
    }
}

fn live(name: &str) -> ChangeItem {
    ChangeItem::Live(rec(name, json!({})))
}

fn dead(name: &str) -> ChangeItem {
    ChangeItem::Tombstone(CkTombstone { name: name.into() })
}

fn page(items: Vec<ChangeItem>, token: &str, more: bool) -> Result<ChangesPage, ApiError> {
    Ok(ChangesPage { items, sync_token: token.into(), more_coming: more })
}

fn ck(code: &str) -> ApiError {
    ApiError::Ck(CkError::new(code, "scripted"))
}

fn auth() -> ApiError {
    ApiError::Auth(AuthError::SessionInvalid { status: 421, reason: "scripted".into() })
}

async fn run(api: &FakeApi, store: &mut FakeStore) -> (Result<SyncReport, SyncError>, Vec<SyncProgress>) {
    let mut seen = Vec::new();
    let r = sync(api, store, |p| seen.push(p)).await;
    (r, seen)
}

#[tokio::test]
async fn first_sync_over_three_pages() {
    let api = FakeApi::default().pages(vec![
        page(vec![live("a"), live("b")], "t1", true),
        // An empty page with moreComing must not end the loop.
        page(vec![], "t2", true),
        page(vec![live("c"), dead("a")], "t3", false),
    ]);
    let mut store = FakeStore::default();
    let (r, progress) = run(&api, &mut store).await;
    assert_eq!(r.unwrap(), SyncReport { pages: 3, records: 3, tombstones: 1, unchanged: false, full_resync: false });
    assert_eq!(api.calls(), vec![None, Some("t1".into()), Some("t2".into())]);
    assert_eq!(*api.zone_calls.lock().unwrap(), 0, "no stored token, so no cheap check");
    assert_eq!(store.token.as_deref(), Some("t3"));
    assert_eq!(store.applied, ["a", "b", "c", "a"]);
    assert_eq!(
        progress,
        vec![
            SyncProgress { pages: 1, records: 2 },
            SyncProgress { pages: 2, records: 2 },
            SyncProgress { pages: 3, records: 3 },
        ]
    );
    assert!(store.events.is_empty());
}

#[tokio::test]
async fn unchanged_token_skips_the_pull() {
    let api = FakeApi::default().zone(vec![Ok("t9".into())]);
    let mut store = FakeStore { token: Some("t9".into()), ..Default::default() };
    let (r, progress) = run(&api, &mut store).await;
    assert_eq!(r.unwrap(), SyncReport { unchanged: true, ..Default::default() });
    assert!(api.calls().is_empty());
    assert!(progress.is_empty());
}

#[tokio::test]
async fn delta_pulls_from_the_stored_token() {
    let api = FakeApi::default().zone(vec![Ok("t6".into())]).pages(vec![page(vec![live("n")], "t6", false)]);
    let mut store = FakeStore { token: Some("t5".into()), ..Default::default() };
    let (r, _) = run(&api, &mut store).await;
    assert_eq!(r.unwrap(), SyncReport { pages: 1, records: 1, ..Default::default() });
    assert_eq!(api.calls(), vec![Some("t5".into())]);
    assert_eq!(store.token.as_deref(), Some("t6"));
    assert!(store.events.is_empty());
}

#[tokio::test]
async fn bad_token_triggers_full_resync() {
    for code in ["BAD_REQUEST", "CHANGE_TOKEN_EXPIRED", "ZONE_NOT_FOUND"] {
        let api = FakeApi::default().zone(vec![Ok("t6".into())]).pages(vec![
            Err(ck(code)),
            page(vec![live("a")], "f1", true),
            page(vec![live("b")], "f2", false),
        ]);
        let mut store = FakeStore { token: Some("old".into()), ..Default::default() };
        let (r, _) = run(&api, &mut store).await;
        let r = r.unwrap();
        assert!(r.full_resync, "{code}");
        assert_eq!((r.pages, r.records), (2, 2), "{code}");
        assert_eq!(api.calls(), vec![Some("old".into()), None, Some("f1".into())], "{code}");
        assert_eq!(store.events, ["begin", "finish"], "{code}");
        assert_eq!(store.token.as_deref(), Some("f2"));
    }
}

#[tokio::test]
async fn bad_token_during_continuation_page_resyncs() {
    let api = FakeApi::default().zone(vec![Ok("x".into())]).pages(vec![
        page(vec![live("a")], "t1", true),
        Err(ck("BAD_REQUEST")),
        page(vec![live("a"), live("b")], "f1", false),
    ]);
    let mut store = FakeStore { token: Some("old".into()), ..Default::default() };
    let (r, _) = run(&api, &mut store).await;
    let r = r.unwrap();
    // The report describes the resync only, not the abandoned partial pull.
    assert_eq!((r.pages, r.records, r.full_resync), (1, 2, true));
    assert_eq!(store.events, ["begin", "finish"]);
}

#[tokio::test]
async fn zone_not_found_on_cheap_check_resyncs() {
    let api = FakeApi::default().zone(vec![Err(ck("ZONE_NOT_FOUND"))]).pages(vec![page(vec![live("a")], "f1", false)]);
    let mut store = FakeStore { token: Some("old".into()), ..Default::default() };
    let (r, _) = run(&api, &mut store).await;
    assert!(r.unwrap().full_resync);
    assert_eq!(api.calls(), vec![None]);
}

#[tokio::test]
async fn resync_that_keeps_failing_returns_error_without_finishing() {
    let api = FakeApi::default().pages(vec![Err(ck("BAD_REQUEST")), Err(ck("BAD_REQUEST"))]);
    let mut store = FakeStore { token: None, ..Default::default() };
    let (r, _) = run(&api, &mut store).await;
    // First pull (no token) fails, resync pull fails again: surfaced, no infinite loop, no sweep.
    assert!(matches!(r, Err(SyncError::Api(ApiError::Ck(e))) if e.code == "BAD_REQUEST"));
    assert_eq!(store.events, ["begin"]);
}

#[tokio::test(start_paused = true)]
async fn try_again_later_backs_off_then_succeeds() {
    let api = FakeApi::default().pages(vec![
        Err(ck("TRY_AGAIN_LATER")),
        Err(ck("TRY_AGAIN_LATER")),
        page(vec![live("a")], "t1", false),
    ]);
    let mut store = FakeStore::default();
    let start = tokio::time::Instant::now();
    let (r, _) = run(&api, &mut store).await;
    assert_eq!(r.unwrap(), SyncReport { pages: 1, records: 1, ..Default::default() });
    assert_eq!(start.elapsed(), Duration::from_secs(3));
    assert_eq!(api.calls().len(), 3);
    assert!(store.events.is_empty());
}

#[tokio::test(start_paused = true)]
async fn try_again_later_three_backoffs_then_resync() {
    let mut script = vec![Err(ck("TRY_AGAIN_LATER")); 4];
    script.push(page(vec![live("a")], "f1", false));
    let api = FakeApi::default().zone(vec![Ok("new".into())]).pages(script);
    let mut store = FakeStore { token: Some("old".into()), ..Default::default() };
    let start = tokio::time::Instant::now();
    let (r, _) = run(&api, &mut store).await;
    assert!(r.unwrap().full_resync);
    assert_eq!(start.elapsed(), Duration::from_secs(7), "1s + 2s + 4s");
    assert_eq!(api.calls(), vec![Some("old".into()); 4].into_iter().chain([None]).collect::<Vec<_>>());
    assert_eq!(store.events, ["begin", "finish"]);
}

#[tokio::test(start_paused = true)]
async fn persistent_try_again_later_during_resync_gives_up() {
    let mut store = FakeStore::default();
    // 4 calls in the first pull (1 + 3 retries), then a resync with 4 more.
    let api2 = FakeApi::default().pages(vec![Err(ck("TRY_AGAIN_LATER")); 8]);
    let (r, _) = run(&api2, &mut store).await;
    assert!(matches!(r, Err(SyncError::GaveUp(e)) if e.code == "TRY_AGAIN_LATER"));
    assert_eq!(api2.calls().len(), 8);
    assert_eq!(store.events, ["begin"]);
}

#[tokio::test]
async fn auth_error_bubbles_up_unchanged_and_keeps_progress() {
    let api = FakeApi::default().zone(vec![Ok("new".into())]).pages(vec![
        page(vec![live("a")], "t1", true),
        Err(auth()),
    ]);
    let mut store = FakeStore { token: Some("old".into()), ..Default::default() };
    let (r, _) = run(&api, &mut store).await;
    assert_eq!(r, Err(SyncError::Api(auth())));
    assert_eq!(store.token.as_deref(), Some("t1"), "page 1 stays applied");
    assert!(store.events.is_empty(), "no resync on auth errors");
}

#[tokio::test]
async fn auth_error_on_cheap_check_bubbles_up() {
    let api = FakeApi::default().zone(vec![Err(auth())]);
    let mut store = FakeStore { token: Some("old".into()), ..Default::default() };
    let (r, _) = run(&api, &mut store).await;
    assert_eq!(r, Err(SyncError::Api(auth())));
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn transport_error_bubbles_up() {
    let api = FakeApi::default().pages(vec![Err(ApiError::Http("offline".into()))]);
    let mut store = FakeStore::default();
    let (r, _) = run(&api, &mut store).await;
    assert_eq!(r, Err(SyncError::Api(ApiError::Http("offline".into()))));
}

#[tokio::test]
async fn store_error_aborts() {
    let api = FakeApi::default().pages(vec![page(vec![live("a")], "t1", false)]);
    let mut store = FakeStore { fail_apply_on: Some(1), ..Default::default() };
    let (r, _) = run(&api, &mut store).await;
    assert_eq!(r, Err(SyncError::Store(StoreError::Db("disk full".into()))));
}

#[tokio::test]
async fn resumes_after_an_interruption() {
    // Run 1 dies on page 3 (transport error); pages 1 and 2 were applied and their tokens stored.
    let api = FakeApi::default().pages(vec![
        page(vec![live("a")], "t1", true),
        page(vec![live("b")], "t2", true),
        Err(ApiError::Http("connection reset".into())),
    ]);
    let mut store = FakeStore::default();
    let (r, _) = run(&api, &mut store).await;
    assert!(r.is_err());
    assert_eq!(store.token.as_deref(), Some("t2"));

    // Run 2 sees a different zone token and continues from t2 rather than starting over.
    let api = FakeApi::default().zone(vec![Ok("t3".into())]).pages(vec![page(vec![live("c")], "t3", false)]);
    let (r, _) = run(&api, &mut store).await;
    assert_eq!(r.unwrap(), SyncReport { pages: 1, records: 1, ..Default::default() });
    assert_eq!(api.calls(), vec![Some("t2".into())]);
    assert_eq!(store.applied, ["a", "b", "c"]);
}

// ---- hydrate_bodies ----

fn decode_text(raw: &[u8]) -> Result<DecodedBody, DecodeError> {
    if raw == b"bad" {
        return Err(DecodeError::Protobuf("scripted".into()));
    }
    Ok(DecodedBody { proto: raw.to_vec(), plain_text: String::from_utf8_lossy(raw).into_owned() })
}

fn store_with(notes: Vec<CkRecord>) -> FakeStore {
    let mut s = FakeStore::default();
    for r in notes {
        s.needing.push(r.name.clone());
        s.records.insert(r.name.clone(), r);
    }
    s
}

#[tokio::test]
async fn hydrate_inline_body() {
    let api = FakeApi::default();
    // "SGk=" is base64 for "Hi".
    let mut store = store_with(vec![rec("n1", json!({"TextDataEncrypted": {"value": "SGk=", "type": "ENCRYPTED_BYTES"}}))]);
    let r = hydrate_bodies(&api, &mut store, 10, decode_text).await.unwrap();
    assert_eq!(r, HydrateReport { done: 1, failed: 0, remaining: 0 });
    assert_eq!(store.bodies[&RecordName::from("n1")], (b"Hi".to_vec(), "Hi".to_owned()));
}

#[tokio::test]
async fn hydrate_asset_body_via_lookup_and_download() {
    let asset_rec = rec(
        "n2",
        json!({"TextDataAsset": {"value": {"downloadURL": "https://example.invalid/x", "size": 5, "fileChecksum": "c"}, "type": "ASSETID"}}),
    );
    let api = FakeApi::default();
    api.lookups.lock().unwrap().push_back(Ok(vec![ChangeItem::Live(asset_rec)]));
    api.downloads.lock().unwrap().push_back(Ok(b"Hello".to_vec()));
    let mut store = store_with(vec![rec("n2", json!({}))]);
    let r = hydrate_bodies(&api, &mut store, 10, decode_text).await.unwrap();
    assert_eq!(r, HydrateReport { done: 1, failed: 0, remaining: 0 });
    assert_eq!(store.bodies[&RecordName::from("n2")].1, "Hello");
}

#[tokio::test]
async fn hydrate_respects_limit_and_counts_failures() {
    let inline = |s: &str| json!({"TextDataEncrypted": {"value": s, "type": "ENCRYPTED_BYTES"}});
    // "YmFk" = "bad", "SGk=" = "Hi".
    let api = FakeApi::default();
    let mut store = store_with(vec![
        rec("bad", inline("YmFk")),
        rec("ok", inline("SGk=")),
        rec("later", inline("SGk=")),
    ]);
    let r = hydrate_bodies(&api, &mut store, 2, decode_text).await.unwrap();
    assert_eq!(r, HydrateReport { done: 1, failed: 1, remaining: 1 });
    assert!(store.bodies.contains_key(&RecordName::from("ok")));
    assert!(!store.bodies.contains_key(&RecordName::from("bad")));
}

#[tokio::test]
async fn hydrate_note_without_body_fields_fails_softly() {
    let api = FakeApi::default();
    api.lookups.lock().unwrap().push_back(Ok(vec![live("n3")]));
    api.lookups.lock().unwrap().push_back(Err(ck("NOT_FOUND")));
    let mut store = store_with(vec![rec("n3", json!({})), rec("n4", json!({}))]);
    let r = hydrate_bodies(&api, &mut store, 10, decode_text).await.unwrap();
    assert_eq!(r, HydrateReport { done: 0, failed: 2, remaining: 0 });
}

#[tokio::test]
async fn hydrate_auth_error_aborts() {
    let api = FakeApi::default();
    api.lookups.lock().unwrap().push_back(Err(auth()));
    let mut store = store_with(vec![rec("n5", json!({}))]);
    let r = hydrate_bodies(&api, &mut store, 10, decode_text).await;
    assert_eq!(r, Err(SyncError::Api(auth())));
}
