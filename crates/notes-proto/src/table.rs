//! Table decoding (WP-B): `Attachment.MergeableDataEncrypted` → `MergableDataProto` → [`Table`].

// Wave 0 stubs: remove once implemented.
#![allow(unused_variables)]

use notes_model::DecodeError;

use crate::proto;

/// A decoded table in display order: `rows[row][col]`. `None` is an empty cell.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Table {
    pub rows: Vec<Vec<Option<proto::Note>>>,
}

/// Decode a table attachment's `MergeableDataEncrypted`, **already base64-decoded**
/// (use `notes_model::bytes_b64`). The bytes are gzip, zlib or raw `MergableDataProto`.
/// Row and column order is resolved through `crRows`/`crColumns` and the UUID index.
pub fn decode_table(data: &[u8]) -> Result<Table, DecodeError> {
    todo!("WP-B")
}
