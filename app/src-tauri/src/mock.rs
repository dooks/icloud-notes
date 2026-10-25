//! `MockService` (WP-H): canned folders and notes with the same method set as `notes_service::Service`,
//! so the UI (WP-I) can be built before Wave 2. All data is synthetic.

use std::sync::Mutex;

use icloud_session::CookieJar;
use notes_model::{
    AssetVariant, CmdError, FolderKind, FolderView, NoteHtml, NoteSummary, QUICK_NOTES_VIEW_ID, RefreshResult, Status,
    SystemFolder, TRASH_FOLDER, DEFAULT_FOLDER,
};

/// A valid 1x1 PNG, served for every attachment.
const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49,
    0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d,
    0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

struct MockNote {
    id: &'static str,
    folder: &'static str,
    title: &'static str,
    snippet: &'static str,
    modified: i64,
    locked: bool,
    attachment: bool,
    quick: bool,
}

const NOTES: &[MockNote] = &[
    MockNote { id: "n1", folder: DEFAULT_FOLDER, title: "Shopping list", snippet: "Milk, eggs, bread", modified: 1_760_000_000_000, locked: false, attachment: false, quick: false },
    MockNote { id: "n2", folder: DEFAULT_FOLDER, title: "Trip photo", snippet: "A picture and a link", modified: 1_760_100_000_000, locked: false, attachment: true, quick: false },
    MockNote { id: "n3", folder: DEFAULT_FOLDER, title: "Secret", snippet: "", modified: 1_750_000_000_000, locked: true, attachment: false, quick: false },
    MockNote { id: "n4", folder: "work", title: "Meeting notes", snippet: "Agenda and action items", modified: 1_759_000_000_000, locked: false, attachment: false, quick: false },
    MockNote { id: "n5", folder: DEFAULT_FOLDER, title: "Quick idea", snippet: "Call back about the thing", modified: 1_761_000_000_000, locked: false, attachment: false, quick: true },
    MockNote { id: "n6", folder: TRASH_FOLDER, title: "Old draft", snippet: "Nothing to see", modified: 1_700_000_000_000, locked: false, attachment: false, quick: false },
];

pub struct MockService {
    state: Mutex<Status>,
}

impl MockService {
    pub fn new() -> Self {
        MockService {
            state: Mutex::new(Status {
                signed_in: true,
                syncing: false,
                last_sync: Some("2026-10-25T09:00:00Z".into()),
                error: None,
            }),
        }
    }

    fn st(&self) -> std::sync::MutexGuard<'_, Status> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self) -> Status {
        self.st().clone()
    }

    pub async fn signed_in(&self, _jar: CookieJar) -> Result<(), CmdError> {
        let mut st = self.st();
        st.signed_in = true;
        st.error = None;
        Ok(())
    }

    pub async fn sign_out(&self) -> Result<(), CmdError> {
        let mut st = self.st();
        st.signed_in = false;
        st.last_sync = None;
        Ok(())
    }

    pub async fn refresh(&self) -> Result<RefreshResult, CmdError> {
        Ok(RefreshResult { changed: 0 })
    }

    pub fn folders(&self) -> Result<Vec<FolderView>, CmdError> {
        let count = |f: &str| NOTES.iter().filter(|n| n.folder == f).count() as u32;
        Ok(vec![
            FolderView { id: DEFAULT_FOLDER.into(), title: "Notes".into(), kind: FolderKind::System(SystemFolder::Default), note_count: count(DEFAULT_FOLDER) },
            FolderView { id: "work".into(), title: "Work".into(), kind: FolderKind::User, note_count: count("work") },
            FolderView {
                id: QUICK_NOTES_VIEW_ID.into(),
                title: "Quick Notes".into(),
                kind: FolderKind::System(SystemFolder::QuickNotes),
                note_count: NOTES.iter().filter(|n| n.quick).count() as u32,
            },
            FolderView { id: TRASH_FOLDER.into(), title: "Recently Deleted".into(), kind: FolderKind::System(SystemFolder::Trash), note_count: count(TRASH_FOLDER) },
        ])
    }

    fn summary(n: &MockNote) -> NoteSummary {
        NoteSummary {
            id: n.id.into(),
            folder: n.folder.into(),
            title: n.title.into(),
            snippet: n.snippet.into(),
            modified: n.modified,
            has_attachments: n.attachment,
            locked: n.locked,
            thumbnail: n.attachment.then(|| "att1".into()),
        }
    }

    fn sorted(mut v: Vec<NoteSummary>) -> Vec<NoteSummary> {
        v.sort_by_key(|n| std::cmp::Reverse(n.modified));
        v
    }

    pub fn notes(&self, folder_id: &str) -> Result<Vec<NoteSummary>, CmdError> {
        let quick = folder_id == QUICK_NOTES_VIEW_ID;
        Ok(Self::sorted(
            NOTES.iter().filter(|n| if quick { n.quick } else { n.folder == folder_id }).map(Self::summary).collect(),
        ))
    }

    pub fn search(&self, q: &str) -> Result<Vec<NoteSummary>, CmdError> {
        let q = q.to_lowercase();
        Ok(Self::sorted(
            NOTES
                .iter()
                .filter(|n| !n.locked && n.folder != TRASH_FOLDER)
                .filter(|n| n.title.to_lowercase().contains(&q) || n.snippet.to_lowercase().contains(&q))
                .map(Self::summary)
                .collect(),
        ))
    }

    pub fn note_html(&self, note_id: &str, dark: bool) -> Result<NoteHtml, CmdError> {
        let n = NOTES.iter().find(|n| n.id == note_id).ok_or_else(|| CmdError::internal("no such note"))?;
        let (bg, fg) = if dark { ("#1e1e1e", "#eee") } else { ("#fff", "#111") };
        let img = if n.attachment {
            "<p><img src=\"notes-asset://localhost/att1/original\" width=\"64\" height=\"64\" alt=\"synthetic image\"></p>"
        } else {
            ""
        };
        let html = format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><base target=\"_blank\">\
             <style>body{{background:{bg};color:{fg};font-family:sans-serif}}</style></head>\
             <body><h1>{title}</h1><p>{snippet}</p>{img}\
             <p><a href=\"https://example.com/mock\">An https link</a> · <a href=\"mailto:a@example.com\">A mailto link</a></p>\
             </body></html>",
            title = n.title,
            snippet = n.snippet,
        );
        Ok(NoteHtml { title: n.title.into(), html })
    }

    pub async fn asset(&self, attachment_id: &str, variant: AssetVariant) -> Result<(Vec<u8>, String), CmdError> {
        let _ = variant;
        if attachment_id == "att1" {
            Ok((PNG_1X1.to_vec(), "image/png".into()))
        } else {
            Err(CmdError::internal("no such attachment"))
        }
    }
}
