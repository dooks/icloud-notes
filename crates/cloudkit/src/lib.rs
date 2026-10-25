//! Read-only CloudKit client (WP-E).
//!
//! Only `changes/zone`, `zones/lookup`, `records/lookup` and plain asset GETs. Never `records/modify`,
//! `zones/modify`, shares or subscriptions.
//!
//! Nothing here logs cookies, record fields or URLs: only counts, record names and error codes.

use std::sync::Arc;

use icloud_session::{Endpoints, Session};
use notes_model::{
    ApiError, AssetRef, AuthError, ChangeItem, ChangesPage, CkError, CkRecord, CkTombstone, CloudKitApi, RecordName,
    RecordType, NOTES_ZONE,
};
use serde_json::{json, Map, Value};

/// Path appended to `ckdatabasews` for the private Notes database.
pub const DB_PATH: &str = "/database/1/com.apple.notes/production/private";

/// `CloudKitApi` over an [`icloud_session::Session`].
pub struct CloudKitClient {
    session: Arc<Session>,
    /// `{ckdatabasews}{DB_PATH}`.
    base: String,
    /// Cookie-less client for asset downloads (the signed URL carries its own authorization).
    plain: reqwest::Client,
}

impl CloudKitClient {
    /// A client for `endpoints.ckdatabasews` (tests pass an httpmock URL there).
    pub fn new(session: Arc<Session>, endpoints: &Endpoints) -> Self {
        let base = format!("{}{DB_PATH}", endpoints.ckdatabasews.trim_end_matches('/'));
        CloudKitClient { session, base, plain: reqwest::Client::new() }
    }

    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }
}

/// What the client needs from the session. Implemented by [`Session`]; the seam lets the tests
/// exercise the protocol code over httpmock without a real session.
trait Transport {
    fn client(&self) -> &reqwest::Client;
    fn apply_headers(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder;
    fn absorb(&self, resp: &reqwest::Response);
    fn classify(&self, status: u16, body: &str) -> Option<AuthError>;
}

impl Transport for Session {
    fn client(&self) -> &reqwest::Client {
        Session::client(self)
    }
    fn apply_headers(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        Session::apply_headers(self, rb)
    }
    fn absorb(&self, resp: &reqwest::Response) {
        Session::absorb(self, resp)
    }
    fn classify(&self, status: u16, body: &str) -> Option<AuthError> {
        icloud_session::classify(status, body)
    }
}

fn zone_id() -> Value {
    json!({ "zoneName": NOTES_ZONE })
}

fn http_err(e: reqwest::Error) -> ApiError {
    // `without_url`: the URL carries the dsid.
    ApiError::Http(e.without_url().to_string())
}

fn ck_error(v: &Value) -> Option<CkError> {
    let code = v.get("serverErrorCode")?.as_str()?;
    let reason = v.get("reason").and_then(Value::as_str).unwrap_or_default();
    Some(CkError::new(code, reason))
}

/// POST `body` to `{base}/{path}`, absorb cookies, and return the parsed JSON of a successful response.
/// Non-200 responses become `Auth` (via classify), `Ck` (body carries `serverErrorCode`) or `Http`.
async fn post<T: Transport + ?Sized>(t: &T, base: &str, path: &str, body: &Value) -> Result<Value, ApiError> {
    let rb = t
        .client()
        .post(format!("{base}/{path}"))
        .header("Content-Type", "text/plain;charset=UTF-8")
        .header("Accept", "application/json")
        .body(body.to_string());
    let resp = t.apply_headers(rb).send().await.map_err(http_err)?;
    t.absorb(&resp);
    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(http_err)?;
    if status != 200 {
        if let Some(auth) = t.classify(status, &text) {
            return Err(auth.into());
        }
        if let Some(ck) = serde_json::from_str::<Value>(&text).ok().as_ref().and_then(ck_error) {
            return Err(ck.into());
        }
        return Err(ApiError::Http(format!("{path}: unexpected HTTP status {status}")));
    }
    let v: Value = serde_json::from_str(&text).map_err(|_| ApiError::Http(format!("{path}: unparseable response")))?;
    if let Some(ck) = ck_error(&v) {
        return Err(ck.into());
    }
    Ok(v)
}

/// `zones[0]`, with a zone-level `serverErrorCode` surfaced as `CkError`.
fn first_zone(v: &Value) -> Result<&Value, ApiError> {
    let zone = v.get("zones").and_then(|z| z.get(0)).ok_or_else(|| ApiError::Http("response has no zones[0]".into()))?;
    match ck_error(zone) {
        Some(e) => Err(e.into()),
        None => Ok(zone),
    }
}

/// A record entry from `changes/zone` or `records/lookup`. A tombstone is `{recordName, deleted:true}`.
fn parse_item(v: &Value) -> Result<ChangeItem, ApiError> {
    let name = v
        .get("recordName")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::Http("record without recordName".into()))?;
    if v.get("deleted").and_then(Value::as_bool) == Some(true) {
        return Ok(ChangeItem::Tombstone(CkTombstone { name: RecordName::new(name) }));
    }
    let record_type = v.get("recordType").and_then(Value::as_str).unwrap_or_default();
    Ok(ChangeItem::Live(CkRecord {
        name: RecordName::new(name),
        record_type: RecordType::from_wire(record_type),
        change_tag: v.get("recordChangeTag").and_then(Value::as_str).unwrap_or_default().to_owned(),
        fields: v.get("fields").and_then(Value::as_object).cloned().unwrap_or_else(Map::new),
        parent: v.get("parent").and_then(|p| p.get("recordName")).and_then(Value::as_str).map(RecordName::new),
    }))
}

async fn zone_token_via<T: Transport + ?Sized>(t: &T, base: &str) -> Result<String, ApiError> {
    let v = post(t, base, "zones/lookup", &json!({ "zones": [{ "zoneName": NOTES_ZONE }] })).await?;
    let zone = first_zone(&v)?;
    zone.get("syncToken")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ApiError::Http("zones/lookup: no syncToken".into()))
}

async fn changes_via<T: Transport + ?Sized>(
    t: &T,
    base: &str,
    token: Option<&str>,
    limit: u32,
) -> Result<ChangesPage, ApiError> {
    let mut zone = json!({ "zoneID": zone_id(), "resultsLimit": limit });
    // Never send `syncToken: ""` (the server answers 500); omit the field instead.
    if let Some(tok) = token.filter(|t| !t.is_empty()) {
        zone["syncToken"] = json!(tok);
    }
    let v = post(t, base, "changes/zone", &json!({ "zones": [zone] })).await?;
    let zone = first_zone(&v)?;
    let items = zone
        .get("records")
        .and_then(Value::as_array)
        .map(|rs| rs.iter().map(parse_item).collect::<Result<Vec<_>, _>>())
        .transpose()?
        .unwrap_or_default();
    let sync_token = zone
        .get("syncToken")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::Http("changes/zone: no syncToken".into()))?
        .to_owned();
    let more_coming = zone.get("moreComing").and_then(Value::as_bool).unwrap_or(false);
    tracing::debug!(records = items.len(), more_coming, "changes/zone page");
    Ok(ChangesPage { items, sync_token, more_coming })
}

async fn lookup_via<T: Transport + ?Sized>(
    t: &T,
    base: &str,
    names: &[RecordName],
    desired_keys: Option<&[&str]>,
) -> Result<Vec<ChangeItem>, ApiError> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let mut body = json!({
        "records": names.iter().map(|n| json!({ "recordName": n.as_str() })).collect::<Vec<_>>(),
        "zoneID": zone_id(),
    });
    if let Some(keys) = desired_keys {
        body["desiredKeys"] = json!(keys);
    }
    let v = post(t, base, "records/lookup", &body).await?;
    let records = v
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::Http("records/lookup: no records".into()))?;
    let mut out = Vec::with_capacity(records.len());
    for r in records {
        if let Some(e) = ck_error(r) {
            // A name that doesn't exist (or was purged) is reported the same way as a tombstone (spike 6).
            if e.code == "NOT_FOUND"
                && let Some(name) = r.get("recordName").and_then(Value::as_str)
            {
                out.push(ChangeItem::Tombstone(CkTombstone { name: RecordName::new(name) }));
                continue;
            }
            return Err(e.into());
        }
        out.push(parse_item(r)?);
    }
    Ok(out)
}

async fn download_via(plain: &reqwest::Client, asset: &AssetRef) -> Result<Vec<u8>, ApiError> {
    let resp = plain.get(&asset.download_url).send().await.map_err(http_err)?;
    let status = resp.status();
    if !status.is_success() {
        return Err(ApiError::Http(format!("asset download: HTTP {}", status.as_u16())));
    }
    let bytes = resp.bytes().await.map_err(http_err)?;
    if bytes.len() as u64 != asset.size {
        return Err(ApiError::Http(format!(
            "asset download: got {} bytes, expected {}",
            bytes.len(),
            asset.size
        )));
    }
    Ok(bytes.to_vec())
}

impl CloudKitApi for CloudKitClient {
    /// `POST {base}/zones/lookup` → `zones[0].syncToken`.
    async fn zone_token(&self) -> Result<String, ApiError> {
        zone_token_via(&*self.session, &self.base).await
    }

    /// `POST {base}/changes/zone` with `{"zones":[{"zoneID":{"zoneName":"Notes"},"resultsLimit":N,"syncToken"?}]}`.
    /// `syncToken` is omitted (never `""`) when `token` is `None`. `zones[0].serverErrorCode` becomes `ApiError::Ck`
    /// even on HTTP 200.
    async fn changes(&self, token: Option<&str>, limit: u32) -> Result<ChangesPage, ApiError> {
        changes_via(&*self.session, &self.base, token, limit).await
    }

    /// `POST {base}/records/lookup` with `{"records":[{"recordName":…}],"zoneID":{"zoneName":"Notes"},"desiredKeys"?}`.
    /// A `NOT_FOUND` entry comes back as a tombstone.
    async fn lookup(&self, names: &[RecordName], desired_keys: Option<&[&str]>) -> Result<Vec<ChangeItem>, ApiError> {
        lookup_via(&*self.session, &self.base, names, desired_keys).await
    }

    /// Plain GET of `asset.download_url` with **no cookies**; the length must equal `asset.size`.
    async fn download(&self, asset: &AssetRef) -> Result<Vec<u8>, ApiError> {
        download_via(&self.plain, asset).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    /// A session-free transport: sends Origin/Referer and a dummy Cookie, classifies like the spike 4 table.
    struct Fake {
        client: reqwest::Client,
    }

    impl Transport for Fake {
        fn client(&self) -> &reqwest::Client {
            &self.client
        }
        fn apply_headers(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
            rb.header("Origin", "https://www.icloud.com")
                .header("Referer", "https://www.icloud.com/")
                .header("Cookie", "test=1")
        }
        fn absorb(&self, _resp: &reqwest::Response) {}
        fn classify(&self, status: u16, body: &str) -> Option<AuthError> {
            match status {
                401 if body.contains("pcs") => Some(AuthError::PcsRejected { reason: "pcs".into() }),
                401 => Some(AuthError::SessionInvalid { status, reason: "401".into() }),
                403 => Some(AuthError::AccessDenied { reason: "403".into() }),
                _ => None,
            }
        }
    }

    fn fake() -> Fake {
        Fake { client: reqwest::Client::new() }
    }

    fn base(server: &MockServer) -> String {
        format!("{}{DB_PATH}", server.base_url())
    }

    fn note(name: &str, tag: &str) -> Value {
        json!({
            "recordName": name,
            "recordType": "Note",
            "recordChangeTag": tag,
            "parent": { "recordName": "Folder-1" },
            "fields": {
                "Folder": { "value": { "recordName": "DefaultFolder-CloudKit", "action": "NONE" }, "type": "REFERENCE" },
                "Deleted": { "value": 0, "type": "INT64" }
            }
        })
    }

    #[tokio::test]
    async fn changes_first_page_omits_sync_token_and_parses_records() {
        let server = MockServer::start();
        let m = server.mock(|when, then| {
            when.method(POST)
                .path(format!("{DB_PATH}/changes/zone"))
                .header("Origin", "https://www.icloud.com")
                .header("Referer", "https://www.icloud.com/")
                .json_body(json!({ "zones": [{ "zoneID": { "zoneName": "Notes" }, "resultsLimit": 200 }] }));
            then.status(200).json_body(json!({ "zones": [{
                "zoneID": { "zoneName": "Notes" },
                "records": [ note("n1", "t1"), { "recordName": "gone", "deleted": true } ],
                "moreComing": true,
                "syncToken": "tok-1"
            }]}));
        });
        let page = changes_via(&fake(), &base(&server), None, 200).await.unwrap();
        m.assert();
        assert_eq!(page.sync_token, "tok-1");
        assert!(page.more_coming);
        assert_eq!(page.items.len(), 2);
        let ChangeItem::Live(r) = &page.items[0] else { panic!("expected live") };
        assert_eq!(r.name, RecordName::new("n1"));
        assert_eq!(r.record_type, RecordType::Note);
        assert_eq!(r.change_tag, "t1");
        assert_eq!(r.parent, Some(RecordName::new("Folder-1")));
        assert_eq!(notes_model::reference(r, "Folder"), Some(RecordName::new("DefaultFolder-CloudKit")));
        assert_eq!(page.items[1], ChangeItem::Tombstone(CkTombstone { name: RecordName::new("gone") }));
    }

    #[tokio::test]
    async fn empty_token_is_never_sent() {
        let server = MockServer::start();
        let m = server.mock(|when, then| {
            when.method(POST)
                .path(format!("{DB_PATH}/changes/zone"))
                .json_body(json!({ "zones": [{ "zoneID": { "zoneName": "Notes" }, "resultsLimit": 5 }] }));
            then.status(200).json_body(json!({ "zones": [{ "records": [], "moreComing": false, "syncToken": "t" }] }));
        });
        changes_via(&fake(), &base(&server), Some(""), 5).await.unwrap();
        m.assert();
    }

    #[tokio::test]
    async fn paging_chain_follows_tokens() {
        let server = MockServer::start();
        let b = base(&server);
        let p1 = server.mock(|when, then| {
            when.method(POST).path(format!("{DB_PATH}/changes/zone")).json_body(json!({
                "zones": [{ "zoneID": { "zoneName": "Notes" }, "resultsLimit": 2 }]
            }));
            then.status(200).json_body(json!({ "zones": [{ "records": [note("a", "1"), note("b", "1")], "moreComing": true, "syncToken": "T1" }] }));
        });
        let p2 = server.mock(|when, then| {
            when.method(POST).path(format!("{DB_PATH}/changes/zone")).json_body(json!({
                "zones": [{ "zoneID": { "zoneName": "Notes" }, "resultsLimit": 2, "syncToken": "T1" }]
            }));
            then.status(200).json_body(json!({ "zones": [{ "records": [], "moreComing": true, "syncToken": "T2" }] }));
        });
        let p3 = server.mock(|when, then| {
            when.method(POST).path(format!("{DB_PATH}/changes/zone")).json_body(json!({
                "zones": [{ "zoneID": { "zoneName": "Notes" }, "resultsLimit": 2, "syncToken": "T2" }]
            }));
            then.status(200).json_body(json!({ "zones": [{ "records": [note("c", "1")], "moreComing": false, "syncToken": "T3" }] }));
        });
        let t = fake();
        let mut token: Option<String> = None;
        let mut names = Vec::new();
        loop {
            let page = changes_via(&t, &b, token.as_deref(), 2).await.unwrap();
            names.extend(page.items.iter().map(|i| i.name().to_string()));
            token = Some(page.sync_token);
            if !page.more_coming {
                break;
            }
        }
        assert_eq!(names, ["a", "b", "c"]);
        assert_eq!(token.as_deref(), Some("T3"));
        p1.assert();
        p2.assert();
        p3.assert();
    }

    #[tokio::test]
    async fn error_in_200_becomes_ck_error() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path(format!("{DB_PATH}/changes/zone"));
            then.status(200).json_body(json!({ "zones": [{
                "zoneID": { "zoneName": "Notes" },
                "serverErrorCode": "BAD_REQUEST",
                "reason": "Invalid continuation format"
            }]}));
        });
        let err = changes_via(&fake(), &base(&server), Some("x"), 10).await.unwrap_err();
        let ApiError::Ck(ck) = err else { panic!("expected Ck, got {err:?}") };
        assert_eq!(ck.code, "BAD_REQUEST");
        assert!(ck.needs_resync());
    }

    #[tokio::test]
    async fn try_again_later_is_retryable() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path(format!("{DB_PATH}/changes/zone"));
            then.status(200).json_body(json!({ "zones": [{
                "serverErrorCode": "TRY_AGAIN_LATER",
                "reason": "client knowledge is ahead of server knowledge"
            }]}));
        });
        let ApiError::Ck(ck) = changes_via(&fake(), &base(&server), Some("x"), 10).await.unwrap_err() else {
            panic!("expected Ck")
        };
        assert!(ck.is_retry_later());
    }

    #[tokio::test]
    async fn http_status_mapping() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path(format!("{DB_PATH}/zones/lookup"));
            then.status(401).json_body(json!({ "serverErrorCode": "INTERNAL_ERROR", "reason": "could not decrypt pcs cookie" }));
        });
        server.mock(|when, then| {
            when.method(POST).path(format!("{DB_PATH}/changes/zone"));
            then.status(500).json_body(json!({ "serverErrorCode": "INTERNAL_ERROR", "reason": "boom" }));
        });
        server.mock(|when, then| {
            when.method(POST).path(format!("{DB_PATH}/records/lookup"));
            then.status(503).body("<html>down</html>");
        });
        let b = base(&server);
        let t = fake();
        assert!(matches!(
            zone_token_via(&t, &b).await.unwrap_err(),
            ApiError::Auth(AuthError::PcsRejected { .. })
        ));
        let ApiError::Ck(ck) = changes_via(&t, &b, None, 1).await.unwrap_err() else { panic!("expected Ck") };
        assert_eq!(ck.code, "INTERNAL_ERROR");
        assert!(matches!(
            lookup_via(&t, &b, &[RecordName::new("x")], None).await.unwrap_err(),
            ApiError::Http(_)
        ));
    }

    #[tokio::test]
    async fn transport_failure_is_http_error_without_url() {
        let err = zone_token_via(&fake(), "http://127.0.0.1:1/secret-dsid-path").await.unwrap_err();
        let ApiError::Http(msg) = err else { panic!("expected Http") };
        assert!(!msg.contains("secret-dsid-path"));
    }

    #[tokio::test]
    async fn zone_token_reads_sync_token() {
        let server = MockServer::start();
        let m = server.mock(|when, then| {
            when.method(POST)
                .path(format!("{DB_PATH}/zones/lookup"))
                .json_body(json!({ "zones": [{ "zoneName": "Notes" }] }));
            then.status(200).json_body(json!({ "zones": [{ "zoneID": { "zoneName": "Notes" }, "syncToken": "ZT" }] }));
        });
        assert_eq!(zone_token_via(&fake(), &base(&server)).await.unwrap(), "ZT");
        m.assert();
    }

    #[tokio::test]
    async fn lookup_sends_desired_keys_and_maps_not_found() {
        let server = MockServer::start();
        let m = server.mock(|when, then| {
            when.method(POST).path(format!("{DB_PATH}/records/lookup")).json_body(json!({
                "records": [{ "recordName": "n1" }, { "recordName": "missing" }],
                "zoneID": { "zoneName": "Notes" },
                "desiredKeys": ["ModificationDate"]
            }));
            then.status(200).json_body(json!({ "records": [
                note("n1", "t"),
                { "recordName": "missing", "serverErrorCode": "NOT_FOUND", "reason": "Record not found" }
            ]}));
        });
        let items = lookup_via(&fake(), &base(&server), &["n1".into(), "missing".into()], Some(&["ModificationDate"]))
            .await
            .unwrap();
        m.assert();
        assert!(matches!(&items[0], ChangeItem::Live(r) if r.name.as_str() == "n1"));
        assert_eq!(items[1], ChangeItem::Tombstone(CkTombstone { name: "missing".into() }));
    }

    #[tokio::test]
    async fn lookup_without_desired_keys_omits_them_and_other_record_errors_fail() {
        let server = MockServer::start();
        let m = server.mock(|when, then| {
            when.method(POST).path(format!("{DB_PATH}/records/lookup")).json_body(json!({
                "records": [{ "recordName": "n1" }],
                "zoneID": { "zoneName": "Notes" }
            }));
            then.status(200).json_body(json!({ "records": [
                { "recordName": "n1", "serverErrorCode": "ACCESS_DENIED", "reason": "nope" }
            ]}));
        });
        let err = lookup_via(&fake(), &base(&server), &["n1".into()], None).await.unwrap_err();
        m.assert();
        assert!(matches!(err, ApiError::Ck(c) if c.code == "ACCESS_DENIED"));
        // No names: no request at all.
        assert!(lookup_via(&fake(), "http://127.0.0.1:1", &[], None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn download_is_cookie_free_and_checks_size() {
        let server = MockServer::start();
        let ok = server.mock(|when, then| {
            when.method(GET).path("/asset/ok").is_true(|req| {
                !req.headers().iter().any(|(k, _)| {
                    let k = k.as_str().to_ascii_lowercase();
                    k == "cookie" || k == "origin" || k == "referer"
                })
            });
            then.status(200).body("hello");
        });
        server.mock(|when, then| {
            when.method(GET).path("/asset/short");
            then.status(200).body("hel");
        });
        server.mock(|when, then| {
            when.method(GET).path("/asset/gone");
            then.status(404);
        });
        let plain = reqwest::Client::new();
        let asset = |p: &str, size| AssetRef {
            download_url: server.url(p),
            size,
            file_checksum: "c".into(),
        };
        assert_eq!(download_via(&plain, &asset("/asset/ok", 5)).await.unwrap(), b"hello");
        ok.assert();
        assert!(matches!(download_via(&plain, &asset("/asset/short", 5)).await, Err(ApiError::Http(_))));
        assert!(matches!(download_via(&plain, &asset("/asset/gone", 5)).await, Err(ApiError::Http(_))));
    }

    /// Needs WP-D's real `Session` (the Wave 0 stubs `todo!()`). Run after the merge with `--ignored`.
    #[tokio::test]
    #[ignore = "requires icloud-session bodies (WP-D)"]
    async fn end_to_end_with_real_session() {
        use icloud_session::{CookieJar, StoredCookie};
        let server = MockServer::start();
        let m = server.mock(|when, then| {
            when.method(POST)
                .path(format!("{DB_PATH}/zones/lookup"))
                .header("Origin", "https://www.icloud.com")
                .header_exists("Cookie");
            then.status(200).json_body(json!({ "zones": [{ "syncToken": "ZT" }] }));
        });
        let mut jar = CookieJar::new();
        jar.insert(StoredCookie {
            name: "X-APPLE-WEBAUTH-TOKEN".into(),
            value: "synthetic".into(),
            domain: ".icloud.com".into(),
            path: "/".into(),
            expires: None,
            secure: true,
            http_only: true,
        });
        let session = Arc::new(Session::new(jar, None));
        let ep = Endpoints { ckdatabasews: server.base_url(), dsid: "1".into() };
        let client = CloudKitClient::new(session, &ep);
        assert_eq!(client.zone_token().await.unwrap(), "ZT");
        m.assert();
    }
}
