//! Read-only CloudKit client (WP-E).
//!
//! Only `changes/zone`, `zones/lookup`, `records/lookup` and plain asset GETs. Never `records/modify`,
//! `zones/modify`, shares or subscriptions.

// Wave 0 stubs: remove once implemented.
#![allow(unused_variables, dead_code)]

use std::sync::Arc;

use icloud_session::{Endpoints, Session};
use notes_model::{ApiError, AssetRef, ChangeItem, ChangesPage, CloudKitApi, RecordName};

/// Path appended to `ckdatabasews` for the private Notes database.
pub const DB_PATH: &str = "/database/1/com.apple.notes/production/private";

/// `CloudKitApi` over an [`icloud_session::Session`].
pub struct CloudKitClient {
    session: Arc<Session>,
    /// `{ckdatabasews}{DB_PATH}`.
    base: String,
}

impl CloudKitClient {
    /// A client for `endpoints.ckdatabasews` (tests pass an httpmock URL there).
    pub fn new(session: Arc<Session>, endpoints: &Endpoints) -> Self {
        todo!("WP-E")
    }

    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }
}

impl CloudKitApi for CloudKitClient {
    /// `POST {base}/zones/lookup` → `zones[0].syncToken`.
    async fn zone_token(&self) -> Result<String, ApiError> {
        todo!("WP-E")
    }

    /// `POST {base}/changes/zone` with `{"zones":[{"zoneID":{"zoneName":"Notes"},"resultsLimit":N,"syncToken"?}]}`.
    /// `syncToken` is omitted (never `""`) when `token` is `None`. `zones[0].serverErrorCode` becomes `ApiError::Ck`
    /// even on HTTP 200.
    async fn changes(&self, token: Option<&str>, limit: u32) -> Result<ChangesPage, ApiError> {
        todo!("WP-E")
    }

    /// `POST {base}/records/lookup` with `{"records":[{"recordName":…}],"zoneID":{"zoneName":"Notes"},"desiredKeys"?}`.
    async fn lookup(&self, names: &[RecordName], desired_keys: Option<&[&str]>) -> Result<Vec<ChangeItem>, ApiError> {
        todo!("WP-E")
    }

    /// Plain GET of `asset.download_url` with **no cookies**; the length must equal `asset.size`.
    async fn download(&self, asset: &AssetRef) -> Result<Vec<u8>, ApiError> {
        todo!("WP-E")
    }
}
