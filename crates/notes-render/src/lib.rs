//! Note → HTML (PLAN.md §3.3, WP-C). Port of `spikes/spike8-render/src/render.rs`.
//!
//! We generate HTML ourselves and escape everything; nothing from the note is passed through as markup.

// Wave 0 stubs: remove once implemented.
#![allow(unused_variables)]

use notes_proto::{Table, proto};

/// How an attachment found in the body should be shown. Provided by `notes-service`.
pub trait AttachmentResolver {
    /// `id` is the body's `attachment_identifier` (the Attachment recordName); `uti` its type identifier.
    fn resolve(&self, id: &str, uti: &str) -> AttachmentView;
}

/// What the renderer emits for one attachment.
#[derive(Debug, Clone, PartialEq)]
pub enum AttachmentView {
    /// `src` is always `notes-asset://localhost/<attachment-id>/<variant>`, never a URL from note content.
    Image { src: String, w: Option<u32>, h: Option<u32>, alt: String },
    /// Rendered as `<table>`; each cell uses the same inline renderer as the body.
    Table(Table),
    /// Inline text, e.g. a calculate result or a hashtag.
    Text(String),
    /// Unsupported or missing attachment, shown as a labelled box.
    Placeholder { kind: String },
}

/// Render a note body to an HTML **fragment** (no `<html>`/`<head>`).
pub fn render_note(note: &proto::Note, resolver: &dyn AttachmentResolver) -> String {
    todo!("WP-C")
}

/// Wrap a fragment in a full document for the iframe's `srcdoc`: strict CSP, `<base target=_blank>`,
/// Apple-Notes-like CSS, light or dark.
pub fn page(fragment: &str, dark: bool) -> String {
    todo!("WP-C")
}
