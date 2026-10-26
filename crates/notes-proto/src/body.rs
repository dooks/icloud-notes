//! Note body decoding (WP-A). Port of `spikes/spike3-decode/src/lib.rs`.

use std::io::Read;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE};
use notes_model::DecodeError;
use prost::Message;

use crate::proto;

/// Decode a `TextDataEncrypted` value: base64 (with URL-safe / unpadded fallbacks), then
/// [`inflate_bytes`]. Returns the `NoteStoreProto` bytes.
pub fn inflate(b64: &str) -> Result<Vec<u8>, DecodeError> {
    let s = b64.trim();
    let raw = STANDARD
        .decode(s)
        .or_else(|_| URL_SAFE.decode(s))
        .or_else(|_| STANDARD_NO_PAD.decode(s.trim_end_matches('=')))
        .map_err(|e| DecodeError::Base64(e.to_string()))?;
    inflate_bytes(&raw)
}

/// Sniff and inflate raw bytes (a decoded `TextDataEncrypted` or a downloaded `TextDataAsset`):
/// gzip (`MultiGzDecoder`) if `1f 8b`, zlib if a valid zlib header, otherwise returned unchanged.
pub fn inflate_bytes(raw: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::new();
    match raw {
        [0x1f, 0x8b, ..] => {
            // MultiGzDecoder: tolerate concatenated members.
            flate2::read::MultiGzDecoder::new(raw)
                .read_to_end(&mut out)
                .map_err(|e| DecodeError::Inflate(format!("gunzip: {e}")))?;
            Ok(out)
        }
        [0x78, b, ..] if (0x7800u16 | u16::from(*b)).is_multiple_of(31) => {
            flate2::read::ZlibDecoder::new(raw)
                .read_to_end(&mut out)
                .map_err(|e| DecodeError::Inflate(format!("zlib: {e}")))?;
            Ok(out)
        }
        _ => Ok(raw.to_vec()),
    }
}

/// Payload of the last length-delimited field `tag` at the top level of `buf`.
/// `Ok(None)` if absent. prost decodes absent `required` messages to `Default` silently, so
/// presence has to be checked on the wire.
fn len_delimited_field(mut buf: &[u8], tag: u32) -> Result<Option<&[u8]>, DecodeError> {
    use prost::encoding::{WireType, decode_key, decode_varint};
    let err = |m: &str| DecodeError::Protobuf(m.to_string());
    let mut found = None;
    while !buf.is_empty() {
        let (t, wire) = decode_key(&mut buf).map_err(|e| DecodeError::Protobuf(e.to_string()))?;
        let skip = match wire {
            WireType::Varint => {
                decode_varint(&mut buf).map_err(|e| DecodeError::Protobuf(e.to_string()))?;
                0
            }
            WireType::SixtyFourBit => 8,
            WireType::ThirtyTwoBit => 4,
            WireType::LengthDelimited => {
                let len = decode_varint(&mut buf).map_err(|e| DecodeError::Protobuf(e.to_string()))?;
                let len = usize::try_from(len).map_err(|_| err("length overflow"))?;
                if len > buf.len() {
                    return Err(err("truncated field"));
                }
                if t == tag {
                    found = Some(&buf[..len]);
                }
                len
            }
            _ => return Err(err("unsupported wire type")),
        };
        if skip > buf.len() {
            return Err(err("truncated field"));
        }
        buf = &buf[skip..];
    }
    Ok(found)
}

/// Decode `NoteStoreProto` bytes and return `document.note`.
/// A missing `document` or `note` is [`DecodeError::MissingDocument`], not a default value.
pub fn decode_note(proto: &[u8]) -> Result<proto::Note, DecodeError> {
    let document = len_delimited_field(proto, 2)?.ok_or(DecodeError::MissingDocument)?;
    len_delimited_field(document, 3)?.ok_or(DecodeError::MissingDocument)?;
    let store = proto::NoteStoreProto::decode(proto).map_err(|e| DecodeError::Protobuf(e.to_string()))?;
    Ok(store.document.note)
}

/// The note's plain text for search: `note_text` with attachment placeholders (U+FFFC) removed.
pub fn plain_text(note: &proto::Note) -> String {
    note.note_text.replace('\u{FFFC}', "")
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn store(text: &str) -> Vec<u8> {
        proto::NoteStoreProto {
            document: proto::Document {
                version: 0,
                note: proto::Note {
                    note_text: text.to_string(),
                    attribute_run: vec![],
                },
            },
        }
        .encode_to_vec()
    }

    fn gzip(b: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b).unwrap();
        e.finish().unwrap()
    }

    fn zlib(b: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn gzip_zlib_raw() {
        let p = store("hello");
        for raw in [gzip(&p), zlib(&p), p.clone()] {
            let b64 = STANDARD.encode(&raw);
            let out = inflate(&b64).unwrap();
            assert_eq!(out, p);
            assert_eq!(decode_note(&out).unwrap().note_text, "hello");
        }
    }

    #[test]
    fn base64_fallbacks() {
        let p = gzip(&store("x?>>"));
        let url = URL_SAFE.encode(&p);
        let nopad = STANDARD_NO_PAD.encode(&p);
        assert_eq!(decode_note(&inflate(&url).unwrap()).unwrap().note_text, "x?>>");
        assert_eq!(decode_note(&inflate(&nopad).unwrap()).unwrap().note_text, "x?>>");
        assert!(matches!(inflate("!!!"), Err(DecodeError::Base64(_))));
    }

    #[test]
    fn concatenated_gzip_members() {
        let p = store("multi");
        let (a, b) = p.split_at(3);
        let mut raw = gzip(a);
        raw.extend(gzip(b));
        assert_eq!(inflate_bytes(&raw).unwrap(), p);
    }

    #[test]
    fn corrupt_gzip_is_inflate_error() {
        assert!(matches!(inflate_bytes(&[0x1f, 0x8b, 1, 2, 3]), Err(DecodeError::Inflate(_))));
    }

    #[test]
    fn non_bmp_emoji_and_placeholders() {
        let note = decode_note(&store("a😀b\u{FFFC}c")).unwrap();
        assert_eq!(note.note_text, "a😀b\u{FFFC}c");
        assert_eq!(plain_text(&note), "a😀bc");
    }

    #[test]
    fn missing_document_and_note() {
        // Empty message: no document. Document (tag 2) present but with no note (tag 3).
        assert!(matches!(decode_note(&[]), Err(DecodeError::MissingDocument)));
        assert!(matches!(decode_note(&[0x12, 0x02, 0x10, 0x00]), Err(DecodeError::MissingDocument)));
        // Empty-but-present note is valid (empty text), not an error.
        assert_eq!(decode_note(&[0x12, 0x02, 0x1a, 0x00]).unwrap().note_text, "");
        assert!(matches!(decode_note(&[0xff, 0xff]), Err(DecodeError::Protobuf(_))));
        assert!(matches!(decode_note(&[0x12, 0x09, 0x00]), Err(DecodeError::Protobuf(_))));
    }
}
