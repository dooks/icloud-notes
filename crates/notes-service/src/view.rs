//! `note_html`: body runs + stored records → [`AttachmentView`]s → HTML.

use notes_model::{
    AssetVariant, CkRecord, NotesStore, RecordName, RecordType, bytes_b64, int, str_b64,
};
use notes_proto::decode_table;
use notes_render::{ASSET_BASE, AttachmentResolver, AttachmentView};

/// Image UTIs a WebView can show as the original file. Everything else (HEIC, TIFF, …) goes through previews.
const WEB_IMAGES: [&str; 5] = ["public.png", "public.jpeg", "public.jpg", "public.gif", "org.webmproject.webp"];

pub fn asset_src(id: &str, variant: AssetVariant) -> String {
    format!("{ASSET_BASE}{id}/{}", variant.as_str())
}

/// Resolves the attachments a body's runs name against the store. Only records the body references are
/// looked at (spike 5: never "all attachments whose Note = X").
pub struct StoreResolver<'a> {
    pub store: &'a dyn NotesStore,
    pub dark: bool,
}

impl StoreResolver<'_> {
    fn record(&self, id: &str) -> Option<CkRecord> {
        let rec = self.store.record(&RecordName::from(id)).ok()??;
        // A `Deleted == 1` stub keeps no fields.
        (int(&rec, "Deleted") != Some(1) && !rec.fields.is_empty()).then_some(rec)
    }

    fn preview(&self, id: &str, rec: &CkRecord) -> Option<AttachmentView> {
        has_list(rec, "PreviewImages").then(|| {
            let v = if self.dark { AssetVariant::PreviewDark } else { AssetVariant::PreviewLight };
            self.image(id, rec, v)
        })
    }

    fn image(&self, id: &str, rec: &CkRecord, variant: AssetVariant) -> AttachmentView {
        let dim = |k: &str| int(rec, k).and_then(|v| u32::try_from(v).ok()).filter(|v| *v > 0);
        AttachmentView::Image { src: asset_src(id, variant), w: dim("Width"), h: dim("Height"), alt: "Image".into() }
    }
}

fn has_list(rec: &CkRecord, field: &str) -> bool {
    rec.fields.get(field).and_then(|f| f.get("value")).and_then(|v| v.as_array()).is_some_and(|a| !a.is_empty())
}

fn has_value(rec: &CkRecord, field: &str) -> bool {
    rec.fields.get(field).and_then(|f| f.get("value")).is_some_and(|v| !v.is_null())
}

impl AttachmentResolver for StoreResolver<'_> {
    fn resolve(&self, id: &str, uti: &str) -> AttachmentView {
        let Some(rec) = self.record(id) else {
            return AttachmentView::Placeholder { kind: "attachment".into() };
        };
        let uti = if uti.is_empty() { notes_model::string(&rec, "UTI").unwrap_or_default() } else { uti.to_owned() };
        match uti.as_str() {
            u if u.ends_with(".calculateresult") => text_of(&rec, &["AltTextEncrypted"]),
            u if u.ends_with(".hashtag") => text_of(&rec, &["DisplayTextEncrypted", "AltTextEncrypted"]),
            "com.apple.notes.table" => match bytes_b64(&rec, "MergeableDataEncrypted").map(|d| decode_table(&d)) {
                Some(Ok(t)) => AttachmentView::Table(t),
                Some(Err(e)) => {
                    tracing::warn!(error = %e, "table decode failed");
                    AttachmentView::Placeholder { kind: "table".into() }
                }
                None => AttachmentView::Placeholder { kind: "table".into() },
            },
            "com.apple.drawing.2" | "com.apple.drawing" | "com.apple.paper" => {
                let kind = if uti == "com.apple.paper" { "paper" } else { "drawing" };
                if has_value(&rec, "FallbackImage") {
                    self.image(id, &rec, AssetVariant::Fallback)
                } else {
                    self.preview(id, &rec).unwrap_or(AttachmentView::Placeholder { kind: kind.into() })
                }
            }
            u if has_value(&rec, "Media") && WEB_IMAGES.contains(&u) => self.image(id, &rec, AssetVariant::Original),
            _ if has_list(&rec, "PreviewImages") => self.preview(id, &rec).expect("checked"),
            _ if has_value(&rec, "Media") => AttachmentView::Placeholder { kind: "image".into() },
            "com.apple.notes.gallery" => AttachmentView::Placeholder { kind: "scan".into() },
            _ => AttachmentView::Placeholder { kind: "attachment".into() },
        }
    }
}

fn text_of(rec: &CkRecord, keys: &[&str]) -> AttachmentView {
    keys.iter()
        .find_map(|k| str_b64(rec, k))
        .filter(|t| !t.is_empty())
        .map_or(AttachmentView::Placeholder { kind: "attachment".into() }, AttachmentView::Text)
}

/// A note's title as stored (`TitleEncrypted`), whatever its type.
pub fn title_of(rec: &CkRecord) -> String {
    str_b64(rec, "TitleEncrypted").unwrap_or_default()
}

pub fn is_note(rec: &CkRecord) -> bool {
    matches!(rec.record_type, RecordType::Note | RecordType::PasswordProtectedNote)
}

