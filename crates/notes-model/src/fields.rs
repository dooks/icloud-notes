//! Typed readers for [`CkRecord::fields`]. Each CloudKit field is `{"value": …, "type": "…"}`;
//! every helper returns `None` when the field is missing or has the wrong shape.

use base64::Engine;
use serde_json::Value;

use crate::record::{AssetRef, CkRecord, RecordName};

fn value<'a>(rec: &'a CkRecord, key: &str) -> Option<&'a Value> {
    rec.fields.get(key)?.get("value")
}

/// Decode base64 the way CloudKit sends it (standard, with fallbacks for URL-safe and unpadded).
fn b64(s: &str) -> Option<Vec<u8>> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    let s = s.trim();
    STANDARD
        .decode(s)
        .or_else(|_| URL_SAFE.decode(s))
        .or_else(|_| STANDARD_NO_PAD.decode(s.trim_end_matches('=')))
        .or_else(|_| URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')))
        .ok()
}

/// A plain `STRING` field.
pub fn string(rec: &CkRecord, key: &str) -> Option<String> {
    value(rec, key)?.as_str().map(str::to_owned)
}

/// An `ENCRYPTED_BYTES` field holding UTF-8 text (`TitleEncrypted`, `SnippetEncrypted`, …): base64 → UTF-8.
pub fn str_b64(rec: &CkRecord, key: &str) -> Option<String> {
    String::from_utf8(bytes_b64(rec, key)?).ok()
}

/// An `ENCRYPTED_BYTES` / `BYTES` field: base64 → bytes (`TextDataEncrypted`, `MergeableDataEncrypted`, …).
pub fn bytes_b64(rec: &CkRecord, key: &str) -> Option<Vec<u8>> {
    b64(value(rec, key)?.as_str()?)
}

/// A `REFERENCE` field: `value.recordName`.
pub fn reference(rec: &CkRecord, key: &str) -> Option<RecordName> {
    value(rec, key)?.get("recordName")?.as_str().map(RecordName::from)
}

/// An `INT64` field (e.g. `Deleted`, `IsSystemPaper`).
pub fn int(rec: &CkRecord, key: &str) -> Option<i64> {
    let v = value(rec, key)?;
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))
}

/// An `ASSETID` field: `{downloadURL, size, fileChecksum, …}`.
pub fn asset(rec: &CkRecord, key: &str) -> Option<AssetRef> {
    let v = value(rec, key)?;
    Some(AssetRef {
        download_url: v.get("downloadURL")?.as_str()?.to_owned(),
        size: v.get("size").and_then(Value::as_u64).unwrap_or(0),
        file_checksum: v.get("fileChecksum")?.as_str()?.to_owned(),
    })
}

/// A `TIMESTAMP` field, in milliseconds since the Unix epoch (e.g. `ModificationDate`).
pub fn timestamp(rec: &CkRecord, key: &str) -> Option<i64> {
    int(rec, key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::RecordType;
    use serde_json::json;

    fn rec(fields: Value) -> CkRecord {
        CkRecord {
            name: "n1".into(),
            record_type: RecordType::Note,
            change_tag: "t".into(),
            fields: fields.as_object().unwrap().clone(),
            parent: None,
        }
    }

    #[test]
    fn reads_every_field_kind() {
        let r = rec(json!({
            "TitleEncrypted": {"value": "SGVsbG8g8J+YgA==", "type": "ENCRYPTED_BYTES"},
            "Unpadded": {"value": "SGk", "type": "ENCRYPTED_BYTES"},
            "Folder": {"value": {"recordName": "DefaultFolder-CloudKit", "action": "VALIDATE"}, "type": "REFERENCE"},
            "Deleted": {"value": 1, "type": "INT64"},
            "ModificationDate": {"value": 1_700_000_000_123_i64, "type": "TIMESTAMP"},
            "Plain": {"value": "abc", "type": "STRING"},
            "Asset": {"value": {"downloadURL": "https://example.invalid/a", "size": 42, "fileChecksum": "AQ"}, "type": "ASSETID"},
        }));
        assert_eq!(str_b64(&r, "TitleEncrypted").as_deref(), Some("Hello 😀"));
        assert_eq!(bytes_b64(&r, "Unpadded"), Some(b"Hi".to_vec()));
        assert_eq!(reference(&r, "Folder"), Some(RecordName::from("DefaultFolder-CloudKit")));
        assert_eq!(int(&r, "Deleted"), Some(1));
        assert_eq!(timestamp(&r, "ModificationDate"), Some(1_700_000_000_123));
        assert_eq!(string(&r, "Plain").as_deref(), Some("abc"));
        assert_eq!(
            asset(&r, "Asset"),
            Some(AssetRef { download_url: "https://example.invalid/a".into(), size: 42, file_checksum: "AQ".into() })
        );
    }

    #[test]
    fn missing_or_wrong_shape_is_none() {
        let r = rec(json!({"Deleted": {"value": "x", "type": "STRING"}, "Bad": {"value": "!!!", "type": "BYTES"}}));
        assert_eq!(int(&r, "Deleted"), None);
        assert_eq!(bytes_b64(&r, "Bad"), None);
        assert_eq!(reference(&r, "Folder"), None);
        assert_eq!(asset(&r, "Deleted"), None);
    }

    #[test]
    fn record_type_round_trips() {
        for t in ["Note", "Note_UserSpecific", "PasswordProtectedNote", "AccountData"] {
            assert_eq!(RecordType::from_wire(t).as_str(), t);
        }
        assert_eq!(RecordType::from_wire("AccountData"), RecordType::Other("AccountData".into()));
    }
}
