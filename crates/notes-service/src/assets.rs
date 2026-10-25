//! Which file of which record an `(attachment id, variant)` names, and the on-disk cache of downloads.
//!
//! Download URLs are never stored: they expire after about 15 minutes, so each cache miss does a fresh
//! `records/lookup` and a cookie-less GET. The cache is keyed by `fileChecksum` (spike 5).

use std::path::{Path, PathBuf};

use notes_model::{
    AssetRef, AssetVariant, CkRecord, CmdError, NotesStore, RecordName, RecordType, int, reference,
};
use serde_json::Value;

/// Where an asset lives: the record that holds it, the field, the list index (for `PreviewImages`), and the
/// stored metadata (only its checksum is trusted).
#[derive(Debug, Clone)]
pub struct Target {
    pub holder: RecordName,
    pub field: &'static str,
    pub index: Option<usize>,
    pub stored: AssetRef,
}

fn parse_asset(v: &Value) -> Option<AssetRef> {
    Some(AssetRef {
        download_url: v.get("downloadURL").and_then(Value::as_str).unwrap_or_default().to_owned(),
        size: v.get("size").and_then(Value::as_u64).unwrap_or(0),
        file_checksum: v.get("fileChecksum")?.as_str()?.to_owned(),
    })
}

/// The asset in `field`: a single `ASSETID`, or element `index` (default 0) of an `ASSETIDLIST`.
pub fn extract(rec: &CkRecord, field: &str, index: Option<usize>) -> Option<AssetRef> {
    let v = rec.fields.get(field)?.get("value")?;
    match v {
        Value::Array(a) => parse_asset(a.get(index.unwrap_or(0))?),
        Value::Object(_) => parse_asset(v),
        _ => None,
    }
}

fn int_list(rec: &CkRecord, field: &str) -> Vec<i64> {
    rec.fields
        .get(field)
        .and_then(|f| f.get("value"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

fn target(rec: &CkRecord, field: &'static str, index: Option<usize>) -> Option<Target> {
    Some(Target { holder: rec.name.clone(), field, index, stored: extract(rec, field, index)? })
}

/// The preview whose `PreviewAppearances` value is `wanted` (0 = light, 1 = dark); the order varies, so never
/// pick by position. Falls back to the first preview.
fn preview(rec: &CkRecord, wanted: i64) -> Option<Target> {
    let appearances = int_list(rec, "PreviewAppearances");
    let index = appearances.iter().position(|a| *a == wanted).unwrap_or(0);
    target(rec, "PreviewImages", Some(index))
}

fn stored_record(store: &dyn NotesStore, name: &RecordName) -> Result<Option<CkRecord>, CmdError> {
    Ok(store.record(name)?.filter(|r| int(r, "Deleted") != Some(1) && !r.fields.is_empty()))
}

/// Resolve `id`/`variant` against the store. `None` if the record has no such file.
pub fn locate(store: &dyn NotesStore, id: &RecordName, variant: AssetVariant) -> Result<Option<Target>, CmdError> {
    let Some(rec) = stored_record(store, id)? else {
        return Ok(None);
    };
    Ok(match rec.record_type {
        // The list thumbnail is an asset on the Note itself (the store uses the note's recordName as the id).
        RecordType::Note => target(&rec, "FirstAttachmentThumbnail", None),
        RecordType::Media => target(&rec, "Asset", None),
        RecordType::Attachment => {
            let original = || -> Result<Option<Target>, CmdError> {
                let Some(media) = reference(&rec, "Media") else { return Ok(None) };
                Ok(stored_record(store, &media)?.and_then(|m| target(&m, "Asset", None)))
            };
            match variant {
                AssetVariant::Original => match original()? {
                    Some(t) => Some(t),
                    None => target(&rec, "FallbackImage", None),
                },
                AssetVariant::Fallback => target(&rec, "FallbackImage", None).or_else(|| preview(&rec, 0)),
                AssetVariant::PreviewLight => preview(&rec, 0).or_else(|| target(&rec, "FallbackImage", None)),
                AssetVariant::PreviewDark => preview(&rec, 1).or_else(|| target(&rec, "FallbackImage", None)),
                AssetVariant::Thumb => preview(&rec, 0).or_else(|| target(&rec, "FallbackImage", None)),
            }
        }
        _ => None,
    })
}

/// Cache file for a checksum. Hex, so the name is always a safe single path component.
pub fn cache_path(dir: &Path, checksum: &str) -> PathBuf {
    let hex: String = checksum.bytes().map(|b| format!("{b:02x}")).collect();
    dir.join(hex)
}

pub fn read_cached(dir: &Path, checksum: &str) -> Option<Vec<u8>> {
    std::fs::read(cache_path(dir, checksum)).ok().filter(|b| !b.is_empty())
}

pub fn write_cached(dir: &Path, checksum: &str, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = cache_path(dir, checksum);
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Content type from magic bytes (CloudKit serves everything as `application/octet-stream`).
pub fn sniff_mime(b: &[u8]) -> &'static str {
    if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if b.starts_with(b"GIF8") {
        "image/gif"
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "image/webp"
    } else if b.starts_with(b"%PDF") {
        "application/pdf"
    } else if b.len() > 12 && &b[4..8] == b"ftyp" && matches!(&b[8..12], b"heic" | b"heix" | b"mif1") {
        "image/heic"
    } else {
        "application/octet-stream"
    }
}
