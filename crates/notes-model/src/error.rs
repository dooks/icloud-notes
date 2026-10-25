//! Error types shared across crates. Messages must never contain cookie values or note content.

use thiserror::Error;

/// The session is not usable and the user must sign in again (spike 4's failure table).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AuthError {
    /// No stored cookie jar (first run, or after sign-out).
    #[error("not signed in")]
    NotSignedIn,
    /// `X-APPLE-WEBAUTH-TOKEN` / `-USER` missing or invalid: `/validate` 421, CloudKit 401/421.
    #[error("session invalid (HTTP {status}): {reason}")]
    SessionInvalid { status: u16, reason: String },
    /// CloudKit rejected the PCS cookie (`INTERNAL_ERROR` "could not decrypt pcs cookie", 401).
    #[error("PCS cookie rejected: {reason}")]
    PcsRejected { reason: String },
    /// CloudKit `ACCESS_DENIED` (403), e.g. "private db access disabled for this account".
    #[error("access denied: {reason}")]
    AccessDenied { reason: String },
}

/// A CloudKit `serverErrorCode` + `reason`, including errors returned inside an HTTP 200
/// (`zones[0].serverErrorCode`, spike 6).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("CloudKit {code}: {reason}")]
pub struct CkError {
    pub code: String,
    pub reason: String,
}

impl CkError {
    pub fn new(code: impl Into<String>, reason: impl Into<String>) -> Self {
        CkError { code: code.into(), reason: reason.into() }
    }

    /// The token or zone is unusable: start a full resync.
    pub fn needs_resync(&self) -> bool {
        matches!(self.code.as_str(), "BAD_REQUEST" | "CHANGE_TOKEN_EXPIRED" | "ZONE_NOT_FOUND")
    }

    /// Back off and retry.
    pub fn is_retry_later(&self) -> bool {
        matches!(self.code.as_str(), "TRY_AGAIN_LATER" | "THROTTLED")
    }
}

/// Any failure talking to Apple.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ApiError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Ck(#[from] CkError),
    /// Transport failure, unexpected status or unparseable body. Treated as "offline" by the UI.
    #[error("http: {0}")]
    Http(String),
}

/// Local cache failure. Kept as strings so this crate doesn't depend on rusqlite.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Db(String),
    #[error("io: {0}")]
    Io(String),
    /// A stored row couldn't be interpreted (bad JSON, missing column, …).
    #[error("corrupt data: {0}")]
    Corrupt(String),
}

/// Note body or table decode failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DecodeError {
    #[error("base64: {0}")]
    Base64(String),
    #[error("inflate: {0}")]
    Inflate(String),
    #[error("protobuf: {0}")]
    Protobuf(String),
    /// `NoteStoreProto.document` or `Document.note` absent (prost would silently default it).
    #[error("missing document/note in NoteStoreProto")]
    MissingDocument,
    #[error("table: {0}")]
    Table(String),
}

/// A sync or hydrate run failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SyncError {
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Server kept returning `TRY_AGAIN_LATER` (or another retryable code) after backoff and resync.
    #[error("gave up after retries: {0}")]
    GaveUp(CkError),
}

impl From<AuthError> for SyncError {
    fn from(e: AuthError) -> Self {
        SyncError::Api(ApiError::Auth(e))
    }
}

impl From<CkError> for SyncError {
    fn from(e: CkError) -> Self {
        SyncError::Api(ApiError::Ck(e))
    }
}
