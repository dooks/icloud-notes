//! Sync engine (WP-G), following spike 6's recommended strategy.

// Wave 0 stubs: remove once implemented.
#![allow(unused_variables)]

use notes_model::{CloudKitApi, DecodeError, NotesStore, SyncError, SyncProgress};

/// `resultsLimit` for `changes/zone`.
pub const PAGE_LIMIT: u32 = 200;

/// What a [`sync`] run did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub pages: u32,
    /// Live records applied.
    pub records: u32,
    pub tombstones: u32,
    /// The `zone_token` cheap check matched the stored token; nothing was pulled.
    pub unchanged: bool,
    /// A full mark-and-sweep resync was run.
    pub full_resync: bool,
}

/// A body ready for [`NotesStore::put_note_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedBody {
    /// Decompressed `NoteStoreProto` bytes.
    pub proto: Vec<u8>,
    /// Plain text for FTS.
    pub plain_text: String,
}

/// What a [`hydrate_bodies`] run did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HydrateReport {
    pub done: u32,
    pub failed: u32,
    /// Still needing a body after this run (beyond the `n` limit).
    pub remaining: u32,
}

/// Bring the store up to date.
///
/// 1. With a stored token, `zone_token`; equal → return `unchanged`.
/// 2. Loop `changes(PAGE_LIMIT)` until `more_coming == false`, `apply_page` per page, `progress` per page.
/// 3. `BAD_REQUEST` / `CHANGE_TOKEN_EXPIRED` / `ZONE_NOT_FOUND` → `begin_full_resync`, full pull, `finish_full_resync`.
/// 4. `TRY_AGAIN_LATER` → back off 3 times (tokio sleep), then resync.
/// 5. Auth errors bubble up unchanged.
pub async fn sync<A, S>(api: &A, store: &mut S, progress: impl FnMut(SyncProgress) + Send) -> Result<SyncReport, SyncError>
where
    A: CloudKitApi + Sync,
    S: NotesStore + Send,
{
    todo!("WP-G")
}

/// Fill in up to `n` bodies from [`NotesStore::notes_needing_body`].
///
/// For each note: take `TextDataEncrypted` (via `notes_model::bytes_b64`) or, if absent, `lookup` the note for a
/// fresh `TextDataAsset` URL and `download` it; pass the raw (still compressed) bytes to `decode`; store the result.
/// A decode failure counts as `failed` and doesn't stop the run. `decode` is injected so this crate doesn't depend
/// on `notes-proto` (the service passes `inflate_bytes` + `decode_note` + `plain_text`).
pub async fn hydrate_bodies<A, S>(
    api: &A,
    store: &mut S,
    n: usize,
    decode: impl Fn(&[u8]) -> Result<DecodedBody, DecodeError> + Send + Sync,
) -> Result<HydrateReport, SyncError>
where
    A: CloudKitApi + Sync,
    S: NotesStore + Send,
{
    todo!("WP-G")
}
