//! Note body decoding (WP-A). Port of `spikes/spike3-decode/src/lib.rs`.

// Wave 0 stubs: remove once implemented.
#![allow(unused_variables)]

use notes_model::DecodeError;

use crate::proto;

/// Decode a `TextDataEncrypted` value: base64 (with URL-safe / unpadded fallbacks), then
/// [`inflate_bytes`]. Returns the `NoteStoreProto` bytes.
pub fn inflate(b64: &str) -> Result<Vec<u8>, DecodeError> {
    todo!("WP-A")
}

/// Sniff and inflate raw bytes (a decoded `TextDataEncrypted` or a downloaded `TextDataAsset`):
/// gzip (`MultiGzDecoder`) if `1f 8b`, zlib if a valid zlib header, otherwise returned unchanged.
pub fn inflate_bytes(raw: &[u8]) -> Result<Vec<u8>, DecodeError> {
    todo!("WP-A")
}

/// Decode `NoteStoreProto` bytes and return `document.note`.
/// A missing `document` or `note` is [`DecodeError::MissingDocument`], not a default value.
pub fn decode_note(proto: &[u8]) -> Result<proto::Note, DecodeError> {
    todo!("WP-A")
}

/// The note's plain text for search: `note_text` with attachment placeholders (U+FFFC) removed.
pub fn plain_text(note: &proto::Note) -> String {
    todo!("WP-A")
}
