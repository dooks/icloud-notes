# iCloud Notes for Windows: build plan

A native Windows app (Rust + Tauri 2) that signs into **one** iCloud account and shows its Apple Notes **read-only**: no editing, no folder management, and no writes to Apple of any kind.

This plan is written to be run by **many Sonnet agents working in parallel**:
- **Wave 0** fixes every crate boundary and public API.
- **Wave 1** has 9 independent work packages (WPs), each owning its own files.
- **Wave 2** wires them together.

Each WP section is a self-contained brief: an agent should need only this file, the spike files it names, and its own crate.

Evidence for every decision is in `spikes\spikeN-*\FINDINGS.md` (spike 1 and 2 results are in their `out\report.txt`).

---

## 1. Fixed decisions (do not revisit inside a WP)

| Topic | Decision | Source |
|---|---|---|
| Auth | Apple's own sign-in page in a Tauri WebView2 window. Capture cookies with `cookies_for_url`. **We never see or handle the password.** | spike 1 |
| Session | Our own DPAPI-encrypted cookie jar (`%LOCALAPPDATA%\<app>\session.dpapi`) is the source of truth. Merge `Set-Cookie` after **every** response. The persistent WebView2 profile is the silent re-login fallback. | spike 4 |
| Session check | `POST https://setup.icloud.com/setup/ws/1/validate` with `Origin: https://www.icloud.com` and `Referer: https://www.icloud.com/`. A 200 response gives `webservices.ckdatabasews.url` and `dsInfo.dsid`. | spikes 1, 4 |
| Data API | `{ckdatabasews}/database/1/com.apple.notes/production/private/…`, zone `Notes`. No query params are needed. Origin and Referer are **required** (a 421 without them). | spike 2 |
| Endpoints used | `changes/zone`, `zones/lookup`, `records/lookup`, and plain GETs of asset URLs. **Nothing else.** In particular, no `records/modify` or shared DB (out of scope). | spikes 5–7 |
| Sync | syncToken cursor, a `zones/lookup` cheap check, and whole-record replace. Errors arrive as HTTP 200 with `zones[0].serverErrorCode`. | spike 6 |
| Body decode | base64 → gzip **or** zlib (sniff) → prost `NoteStoreProto`. Schema vendored from threeplanetssoftware/apple_cloud_notes_parser, compiled by `protox` (no protoc). Run lengths are UTF-16 units. | spike 3 |
| Render | Generate HTML ourselves, never filter. Show it in a sandboxed iframe with no IPC. Links open via the OS after a scheme check. | spike 8 |
| Attachments | Images, previews and fallbacks come via a fresh `records/lookup` URL (it expires after about 15 minutes, needs no cookies), cached on disk by `fileChecksum`. Tables are decoded from `MergeableDataEncrypted`. Drawings and Paper are fallback images only. | spike 5 |
| Folders | All folders are top level, matched by recordName for system folders. A note's folder is `Note.Folder`. Quick Notes is virtual (`Note_UserSpecific.IsSystemPaper == 1`). | spike 7 |
| Visibility | `Deleted == 1` means hidden. `Folder == TrashFolder-CloudKit` means it is shown under "Recently Deleted" (read-only, like any folder). A tombstone means removed. | spike 6 |
| Out of scope | Locked notes (`PasswordProtectedNote`): listed as "🔒 Locked", never decoded. Shared-in notes, nested folders, smart-folder queries other than Quick Notes, editing, multiple accounts. | user |
| UI tech | Vanilla HTML/CSS/JS frontend (no Node), and cargo only. | session 1 |

**Privacy rules for every WP:**
- Never log cookie values or note text, titles or snippets. Log names, lengths, counts and recordNames only.
- Never commit anything from `spikes\*\out\`.
- Test fixtures must be **synthetic**.
- Tests may read the real spike 2 pages in `spikes\spike2-cloudkit\out\` only when the tests are `#[ignore]`d and print nothing but stats.

---

## 2. Repository layout (created in Wave 0)

```
icloud-notes\
  Cargo.toml                 workspace (resolver 2), shared [workspace.dependencies]
  .gitignore                 target/, out/, *.dpapi, spikes/*/out/
  PLAN.md
  crates\
    notes-model\             shared plain types + traits (no I/O)
    notes-proto\             vendored .proto, build.rs (protox), body + table decode
    notes-render\            Note/Table → HTML fragment
    icloud-session\          cookie jar, DPAPI persistence, /validate, auth error classification
    cloudkit\                HTTP client for the four endpoints + asset download
    notes-store\             SQLite cache (rusqlite, bundled, FTS5)
    notes-sync\              sync engine (first sync / refresh / resync / visibility)
    notes-service\           facade the app calls: startup flow, folder tree, note view, assets
  app\
    src-tauri\               Tauri 2 host: windows, login, commands, notes-asset:// protocol
    ui\                      index.html, app.js, app.css (three panes)
  spikes\                    read-only reference; never edited by WPs
```

The dependency graph is acyclic and enforced by Cargo:
```
notes-model ← notes-proto ← notes-render
notes-model ← icloud-session ← cloudkit
notes-model ← notes-store
notes-model ← notes-sync            (talks to cloudkit/store only through traits in notes-model)
everything  ← notes-service ← app/src-tauri
app/ui ↔ app/src-tauri only through the command contract (§3.4)
```

---

## 3. Wave 0: skeleton and contracts (one agent, sequential, must finish first)

**Goal:** a workspace that compiles, where every public function in §3.1–3.4 exists with its final signature, a doc comment, and a `todo!()` body. Wave 1 agents then fill in bodies **without changing signatures**. A WP that needs a contract change stops and reports it; it doesn't edit another crate.

Steps:
1. `git init`, `.gitignore`, workspace `Cargo.toml`.
   - Pin shared deps in `[workspace.dependencies]`: prost 0.14.4, prost-build 0.14.4, protox 0.10.0, flate2 1.1, base64 0.23, serde/serde_json, reqwest 0.12 (rustls, json, blocking off), tokio, rusqlite (bundled), thiserror, tracing, windows (Win32_Security_Cryptography), tauri 2.12, httpmock (dev).
   - Run `cargo generate-lockfile` once, so Wave 1 agents don't fight over `Cargo.lock`.
2. Create every crate in §2 with `lib.rs` stubs. Copy `spikes\spike3-decode\vendor\` and its `build.rs` into `notes-proto`, so the generated prost types exist from day 1.
3. Write the contracts below, then make sure `cargo build --workspace` and `cargo test --workspace` pass (the `todo!` paths are untested).
4. Scaffold `app\` with `cargo create-tauri-app -m cargo -t vanilla`, then point its `frontendDist` at `..\ui`. Register the commands in §3.4 as stubs.
5. Commit as "skeleton". Each Wave 1 agent works in its **own git worktree/branch** off this commit, because separate target dirs avoid cargo lock contention.

### 3.1 `notes-model` (complete in Wave 0, not stubbed)
- `RecordName(String)`, `RecordType` (enum: Note, Folder, Attachment, Media, InlineAttachment, Hashtag, NoteUserSpecific, FolderUserSpecific, PasswordProtectedNote, Other(String)).
- `CkRecord { name, record_type, change_tag, fields: serde_json::Map, parent: Option<RecordName> }` and `CkTombstone { name }`. `ChangeItem = Live(CkRecord) | Tombstone(CkTombstone)`.
- `ChangesPage { items: Vec<ChangeItem>, sync_token: String, more_coming: bool }`.
- `AssetRef { download_url: String, size: u64, file_checksum: String }`. The URL is short-lived and is never persisted.
- Typed field helpers: `fn str_b64(&CkRecord, key) -> Option<String>` (base64 → UTF-8 for `*Encrypted` strings), `fn bytes_b64`, `fn reference(&CkRecord, key) -> Option<RecordName>`, `fn int`, `fn asset`, `fn timestamp`.
- System folder constants: `DEFAULT_FOLDER = "DefaultFolder-CloudKit"`, `TRASH_FOLDER = "TrashFolder-CloudKit"`, `QUICK_NOTES = "SystemPaper-CloudKit"`.
- View types: `FolderView { id, title, kind: System(Default|Trash|QuickNotes)|User, note_count }` and `NoteSummary { id, folder, title, snippet, modified, has_attachments, locked, thumbnail: Option<AttachmentId> }`.
- Errors: `AuthError` (needs re-login), `CkError { code: String, reason: String }`, plus `SyncError`, `StoreError`, `DecodeError`.
  As built (`crates\notes-model\src\error.rs`):
  - `AuthError = NotSignedIn | SessionInvalid{status, reason} | PcsRejected{reason} | AccessDenied{reason}`
  - `CkError::needs_resync()` (BAD_REQUEST, CHANGE_TOKEN_EXPIRED, ZONE_NOT_FOUND) and `is_retry_later()` (TRY_AGAIN_LATER, THROTTLED)
  - `StoreError = Db(String) | Io(String) | Corrupt(String)` (strings, so the model doesn't depend on rusqlite)
  - `DecodeError = Base64 | Inflate | Protobuf | MissingDocument | Table` (all but `MissingDocument` carry a `String`)
  - `SyncError = Api(ApiError) | Store(StoreError) | GaveUp(CkError)`, with `From` for `ApiError`, `AuthError`, `CkError`, `StoreError`
- **Traits**, so `notes-sync` can be built and tested without the real HTTP and SQLite crates.
  `CloudKitApi` uses RPITIT with `+ Send` futures (so a sync can be `tokio::spawn`ed). Implementations may still write `async fn`:
  ```rust
  pub trait CloudKitApi {
      fn zone_token(&self) -> impl Future<Output = Result<String, ApiError>> + Send;                 // zones/lookup
      fn changes(&self, token: Option<&str>, limit: u32) -> impl Future<Output = Result<ChangesPage, ApiError>> + Send;
      fn lookup(&self, names: &[RecordName], desired_keys: Option<&[&str]>)
          -> impl Future<Output = Result<Vec<ChangeItem>, ApiError>> + Send;
      fn download(&self, asset: &AssetRef) -> impl Future<Output = Result<Vec<u8>, ApiError>> + Send; // plain GET, no cookies
  }
  pub trait NotesStore {
      fn sync_token(&self) -> Result<Option<String>, StoreError>;
      fn apply_page(&mut self, page: &ChangesPage) -> Result<(), StoreError>;      // one transaction, token included
      fn begin_full_resync(&mut self) -> Result<(), StoreError>;                    // mark all rows unseen
      fn finish_full_resync(&mut self) -> Result<(), StoreError>;                   // sweep unseen rows
      fn record(&self, name: &RecordName) -> Result<Option<CkRecord>, StoreError>;
      fn folders(&self) -> Result<Vec<FolderView>, StoreError>;
      fn notes_in(&self, folder: &RecordName) -> Result<Vec<NoteSummary>, StoreError>;
      fn quick_notes(&self) -> Result<Vec<NoteSummary>, StoreError>;
      fn search(&self, q: &str) -> Result<Vec<NoteSummary>, StoreError>;
      fn note_body(&self, note: &RecordName) -> Result<Option<Vec<u8>>, StoreError>; // decompressed proto bytes
      fn put_note_body(&mut self, note: &RecordName, proto: &[u8], plain_text: &str) -> Result<(), StoreError>;
      fn notes_needing_body(&self) -> Result<Vec<RecordName>, StoreError>;          // bodies changed or TextDataAsset pending
  }
  ```
  `ApiError = Auth(AuthError) | Ck(CkError) | Http(String)`. `Http` means "offline" to the UI.
- Also in `notes-model` (added in Wave 0, shared by the service, the host and its mock):
  - `fn string(&CkRecord, key)` for plain `STRING` fields; `ChangeItem::name()`.
  - Constants `QUICK_NOTES_VIEW_ID = "quick-notes"` (the Quick Notes `FolderView.id`), `APP_ID = "com.ziggy.icloud-notes"` (folder under `%LOCALAPPDATA%`), `NOTES_ZONE = "Notes"`.
  - Command types (`view.rs`, camelCase JSON): `Status`, `RefreshResult`, `NoteHtml`, `SyncProgress`, `CmdError { code: CmdErrorCode, message }` with `From` for every error type, and `AssetVariant` (`original|preview-light|preview-dark|fallback|thumb`, `parse`/`as_str`).
  - `FolderKind` JSON is `{"system":"default"|"trash"|"quickNotes"}` or `"user"`. `NoteSummary.modified` is ms since the epoch (`i64`). `AttachmentId` is an alias for `RecordName`.

### 3.2 `notes-proto` public API
- `fn inflate(b64: &str) -> Result<Vec<u8>, DecodeError>` (base64 → gzip/zlib/raw) and `fn inflate_bytes(&[u8])` (for the downloaded `TextDataAsset`).
- `fn decode_note(proto: &[u8]) -> Result<proto::Note, DecodeError>`. A missing `Document.note` is an error.
- `fn plain_text(&proto::Note) -> String` (for search).
- `fn decode_table(data: &[u8]) -> Result<Table, DecodeError>`, where `data` is the **already base64-decoded** `MergeableDataEncrypted` (use `notes_model::bytes_b64`), and `Table { rows: Vec<Vec<Option<proto::Note>>> }` is in display order.
- Re-export the generated `proto` module.
- Files: the body functions live in `src\body.rs` (WP-A) and the table in `src\table.rs` (WP-B). `lib.rs` only re-exports them, so neither WP needs to edit it.

### 3.3 `notes-render` public API
- `trait AttachmentResolver { fn resolve(&self, id: &str, uti: &str) -> AttachmentView; }`
- `enum AttachmentView { Image { src: String, w: Option<u32>, h: Option<u32>, alt: String }, Table(Table), Text(String), Placeholder { kind: String } }`.
  `src` is always `http://notes-asset.localhost/<attachment-id>/<variant>` (`notes_render::ASSET_BASE`). WebView2 cannot load `notes-asset://localhost/…`; Tauri serves the `notes-asset` protocol there as `http://notes-asset.localhost/…`, so the renderer emits that form directly and `page()`'s CSP is `img-src http://notes-asset.localhost`. The host does no URL rewriting.
- `fn render_note(&proto::Note, &dyn AttachmentResolver) -> String` returns an HTML **fragment**.
- `fn page(fragment: &str, dark: bool) -> String` returns a full document with CSP and CSS, for the iframe's `srcdoc`.

### 3.4 Tauri command contract (`app/src-tauri` ↔ `app/ui`)
All commands are async, return JSON, and fail with error codes `{code: "auth"|"offline"|"internal", message}`.
| command | args | returns |
|---|---|---|
| `status` | – | `{signedIn: bool, syncing: bool, lastSync: string?, error: string?}` |
| `sign_in` | – | opens the login window and resolves when the jar is captured |
| `sign_out` | – | deletes the jar and the WebView2 profile data |
| `refresh` | – | `{changed: number}` |
| `folders` | – | `FolderView[]` |
| `notes` | `{folderId}` (or `"quick-notes"`) | `NoteSummary[]` sorted by `modified` desc |
| `search` | `{q}` | `NoteSummary[]` |
| `note_html` | `{noteId, dark}` | `{title, html}`: a full page for `srcdoc` |
| `open_link` | `{url}` | opens http/https/mailto in the OS browser after a re-check |

Events: `sync-progress {pages, records}`, `sync-done`, `auth-required`.

Rust side (`app\src-tauri\src\lib.rs`): each command is `async fn …(args) -> Result<T, notes_model::CmdError>`, with snake_case arguments (`folder_id`, `note_id`), which Tauri exposes to JS as camelCase. WP-H may add `State`, `AppHandle` or `Window` parameters freely, because the JS contract is only the argument names and the JSON results. The capability grants `core:default` only (the scaffold's `opener:default` was removed, so JS can't open URLs directly).

### 3.5 I/O crate APIs (as stubbed in Wave 0)
- **`icloud-session`:**
  - `StoredCookie { name, value, domain, path, expires: Option<i64>, secure, http_only }`
  - `CookieJar` (`new`, `insert`, `merge_set_cookies(&[String]) -> usize`, `header`, `get`, `names`, `has_session`, `to_json`, `from_json`) and `SharedJar = Arc<Mutex<CookieJar>>`
  - `protect` / `unprotect` (DPAPI)
  - `DpapiStore` (`new`, `default_path`, `path`, `load -> Option<CookieJar>`, `save`, `delete`)
  - `Endpoints { ckdatabasews, dsid }`
  - `Session::new(jar, Option<DpapiStore>)`, `with_setup_base` (for httpmock), `jar`, `client`, `replace_jar`, `async validate() -> Result<Endpoints, ApiError>`, `apply_headers(RequestBuilder)`, `absorb(&Response)`
  - `classify(status, body) -> Option<AuthError>`
  - `SessionError = Dpapi | Io | Json`
  - Changes from §4 WP-D: `validate` returns `ApiError`, not `AuthError`, so an offline failure isn't mistaken for a re-login. `apply_headers` and `absorb` are `Session` methods, because they need the jar.
- **`cloudkit`:** `CloudKitClient::new(Arc<Session>, &Endpoints)` (tests put an httpmock URL in `ckdatabasews`), `session()`, and `impl CloudKitApi` including `download`. `DB_PATH` constant.
- **`notes-store`:** `SqliteStore::open(&Path)`, `open_in_memory()`, `default_path()`, and `impl NotesStore`.
- **`notes-sync`:**
  - `PAGE_LIMIT = 200`
  - `async fn sync<A: CloudKitApi + Sync, S: NotesStore + Send>(api: &A, store: &mut S, progress: impl FnMut(SyncProgress) + Send) -> Result<SyncReport, SyncError>`
  - `SyncReport { pages, records, tombstones, unchanged, full_resync }`
  - `async fn hydrate_bodies(api, store, n: usize, decode: impl Fn(&[u8]) -> Result<DecodedBody, DecodeError> + Send + Sync) -> Result<HydrateReport, SyncError>`
    - `decode` receives the raw, still-compressed body bytes (inline field after base64, or the downloaded asset).
    - `DecodedBody { proto, plain_text }`
    - `HydrateReport { done, failed, remaining }`
- **`notes-service`** (stub for WP-J; WP-H's `MockService` mirrors these method names):
  - `Service` is `Core<CloudKitClient>`; `Core<A: CloudKitApi>` (with `with_parts(data_dir, events, jar_store, connect)`) lets tests use fakes. Reads use their own SQLite connection, so they never wait for a sync. Extras: `wait_idle()`, `spawn_periodic_refresh(every, focused)`.
  - `Service::open(data_dir, Arc<dyn EventSink>)`, `start`, `status`, `signed_in(CookieJar)`, `sign_out`, `refresh`, `folders`, `notes(folder_id)`, `search(q)`, `note_html(note_id, dark)`, `asset(attachment_id, AssetVariant) -> (bytes, mime)` (served as `http://notes-asset.localhost/<id>/<variant>`; for `Thumb`, `<id>` may be a note recordName)
  - All return `CmdError`.
  - `trait EventSink: Send + Sync { fn emit(&self, ServiceEvent) }` with `ServiceEvent = SyncProgress(SyncProgress) | SyncDone | AuthRequired`.

**Stub lint allowances:** stub files start with `#![allow(unused_variables)]` (plus `dead_code` where a struct's fields are unused) under a "Wave 0 stubs" comment. Remove it when you implement the file.

---

## 4. Wave 1: parallel work packages

**Every WP:**
- Owns only the listed paths.
- Doesn't change signatures from §3.
- Adds tests.
- Must pass `cargo test -p <crate>` and `cargo clippy -p <crate> -- -D warnings`.
- Follows the privacy rules in §1.
- Makes no network calls to Apple, except WP-H's manual check, which runs with the user present.
- Reports back: what was done, any contract problems, and anything left unverified.

### WP-A: note body decode (`crates\notes-proto\src\body.rs`)
- Port `spikes\spike3-decode\src\lib.rs`: base64 with fallbacks, gzip (`MultiGzDecoder`) / zlib / raw sniffing, prost decode, UTF-16 `spans()`.
- Treat a missing `Document.note` as an error (prost decodes it to Default silently; see spike 3 finding 1).
- Tests:
  - synthetic gzip, zlib and raw notes
  - a non-BMP emoji
  - a missing document
  - an `#[ignore]` stats test over the spike 2 pages that asserts 240/240 decoded

### WP-B: table decode (`crates\notes-proto\src\table.rs`)
- Decode `Attachment.MergeableDataEncrypted` (base64 → gzip → `MergableDataProto`) into an ordered `Table`.
- Resolve `crRows`/`crColumns` ordering through `object_index` / `UUIDIndex`. Cells are `cellColumns[col][row] → Note`.
- Reference: `embedded_table.rb` in threeplanetssoftware/apple_cloud_notes_parser. Read it online; don't vendor Ruby. Also see spike 5 §Q4.
- Tests:
  - a synthetic 2×3 table built with prost, including an empty cell and a styled cell
  - an `#[ignore]` test over the spike 2 pages asserting 30/30 tables decode and row and column counts equal `elements`
- Doesn't touch `body.rs`.

### WP-C: HTML renderer (`crates\notes-render\**`)
- Port `spikes\spike8-render\src\render.rs`, including all its sanitization rules and its 12 tests. Wire in `AttachmentResolver`.
- Render `AttachmentView::Table` as `<table>`, with each cell rendered by the same inline renderer, and `Image` as `<img loading=lazy>`.
- Dark mode: drop explicit colours close to the default text colour (spike 8 open issue 4).
- `page()` CSS should look like Apple Notes: system font, comfortable line height.
- Build the `proto::Note` values in tests by hand. Doesn't depend on WP-A or WP-B being finished.

### WP-D: session (`crates\icloud-session\**`)
- Port `spikes\spike4-session\common`.
- `CookieJar`: merges `Set-Cookie` by name, serializes to JSON, and is `Arc<Mutex<>>`-shareable.
- `DpapiStore`: `CryptProtectData`/`CryptUnprotectData`, CurrentUser scope, `CRYPTPROTECT_UI_FORBIDDEN`, app entropy, atomic tmp-then-rename writes.
- `Session::validate() -> Result<Endpoints{ckdatabasews, dsid}, AuthError>`.
- `classify(status, body) -> Option<AuthError>`, using spike 4's failure table. A 421 caused by a missing Origin header must be reported as a bug, not a re-login.
- Provide `fn apply_headers(RequestBuilder) -> RequestBuilder` (Origin, Referer, UA, Cookie) and `fn absorb(&Response)` for the cloudkit crate to use.
- Tests: Set-Cookie merging, a DPAPI round trip, and classification of every row in spike 4's table, using httpmock.

### WP-E: CloudKit client (`crates\cloudkit\**`)
- Implement `CloudKitApi` on top of `icloud-session`.
  - `changes` builds `{"zones":[{"zoneID":{"zoneName":"Notes"},"resultsLimit":N,"syncToken"?}]}`. **Never** send `syncToken: ""`; omit the field instead.
  - Parse records, tombstones, `moreComing` and `syncToken`.
  - Surface `zones[0].serverErrorCode` as `CkError` even on HTTP 200.
- `lookup` uses `records/lookup` with optional `desiredKeys`. `zone_token` uses `zones/lookup`.
- `download(&AssetRef) -> bytes` is a plain GET with **no cookies**. Verify the result by `size`.
- After every response, call `session.absorb`.
- Map 401/421 auth bodies to `ApiError::Auth`.
- Tests: use httpmock with synthetic JSON shaped like spike 2's pages (records, tombstones, an error-in-200 response, a paging chain).

### WP-F: SQLite store (`crates\notes-store\**`)
- Implement `NotesStore` with rusqlite (bundled).
- Tables:
  - `records(name PK, type, change_tag, fields_json, parent, deleted, seen)`
  - `note_index(name PK, folder, title, snippet, modified, deleted, trashed, locked, quick_note, first_thumb)`
  - `bodies(name PK, proto BLOB, text_hash)`
  - `fts` (FTS5 over title + plain text)
  - `kv(key PK, value)` for the sync token
- `apply_page` runs in one transaction. Upsert a live record whole. A tombstone deletes the row, cascading by type: a Note removes its body, FTS row, attachments and media.
- Derive `note_index` from fields:
  - title and snippet from `TitleEncrypted` / `SnippetEncrypted`
  - `folder` from the `Folder` reference
  - `trashed` = folder is the trash folder
  - `deleted` = `Deleted == 1`
  - `locked` = type is PasswordProtectedNote
  - `quick_note` from the matching `Note_UserSpecific.IsSystemPaper`
- Mark a body stale when the `TextDataEncrypted`/`TextDataAsset` value changes.
- `folders()`:
  - order: Quick Notes, Notes, user folders (title, case-insensitive), Recently Deleted
  - counts exclude deleted notes
- DB path: `%LOCALAPPDATA%\<app>\cache.sqlite`, with an in-memory constructor for tests.
- Tests: every visibility rule in spike 6 §"Recommended" step 6, tombstone cascade, mark-and-sweep resync, FTS search.

### WP-G: sync engine (`crates\notes-sync\**`)
- `async fn sync(api: &impl CloudKitApi, store: &mut impl NotesStore, progress) -> Result<SyncReport, SyncError>`, following spike 6's strategy.
  1. With a stored token: `zone_token`. If it's equal to the stored token, return.
  2. Otherwise, loop `changes` (limit 200) until `more_coming == false`, calling `apply_page` per page. Loop on `moreComing`, never on an empty page.
  3. On `BAD_REQUEST`, `CHANGE_TOKEN_EXPIRED` or `ZONE_NOT_FOUND`: run `begin_full_resync`, a full pull, then `finish_full_resync`.
  4. On `TRY_AGAIN_LATER`: back off 3 times, then resync.
  5. Auth errors bubble up unchanged.
- `async fn hydrate_bodies(api, store, n)`: for `notes_needing_body`, inflate the inline field or download `TextDataAsset` via `lookup` → `download`, then `put_note_body` with the plain text. The decode function is injected as a closure, so this crate doesn't depend on `notes-proto`.
- Tests: an in-crate `FakeApi` and `FakeStore` (both in-memory). Scripted scenarios: first sync over 3 pages, no-change shortcut, delta, bad token → resync, TRY_AGAIN_LATER, auth error mid-pull, resume after an interruption.

### WP-H: Tauri host (`app\src-tauri\**`)
- **Login window:** a separate WebviewWindow on `https://www.icloud.com` with the default persistent profile (never incognito). Poll `cookies_for_url("https://setup.icloud.com/setup/ws/1/validate")` until `X-APPLE-WEBAUTH-TOKEN` appears. Port the logic from `spikes\spike1-login` and `spikes\spike4-session\login`. Hand the cookies over to a `CookieJar`, close the window, and emit `sign-in-done`. Give this window **no capabilities**.
- **Main window:** loads `app\ui`.
  - Capabilities: only the commands in §3.4.
  - CSP: `default-src 'self'; img-src 'self' notes-asset: http://notes-asset.localhost; frame-src 'self'; style-src 'self' 'unsafe-inline'`.
  - `on_navigation` denies everything except the app's own origin.
  - `on_new_window` always denies, and passes http/https/mailto URLs to the OS opener after the scheme check. This is how links in note iframes open (see WP-I).
- **`notes-asset://` custom protocol:** parse `<attachment-id>/<variant>` (variant ∈ `original|preview-light|preview-dark|fallback|thumb`). Resolve it **only** through `notes-service`, never through a URL taken from note content. Serve the right content type, sniffed from magic bytes.
- **Commands:** implement §3.4 by delegating to a `notes_service::Service` held in Tauri state. Until Wave 2, use a `MockService` in `src-tauri/src/mock.rs` that returns canned folders and notes, so WP-I can work against it.
- `sign_out` clears the jar and calls `clear_all_browsing_data` on the login webview's profile.
- Manual check, with the user present: sign in, see the mock UI, restart, and stay signed in (the jar is used).

### WP-I: UI (`app\ui\**`)
- Three panes: a folder list (with counts), a note list (title, snippet, date, thumbnail, 🔒 for locked notes), and a note view (`<iframe sandbox srcdoc=…>`, **with no `allow-scripts`**).
- Toolbar: search box (debounced, calls `search`), Refresh button with a progress indicator, and a status line ("Updated 2 min ago" / "Sign-in needed" → button).
- Keyboard: ↑/↓ moves through notes, and Ctrl+F focuses search.
- Light and dark mode via `prefers-color-scheme`, passing `dark` to `note_html`.
- Link clicks: the iframe can't run scripts. `page()` adds `<base target=_blank>`, and the iframe gets `sandbox="allow-popups"`, so a click becomes a new-window request. WP-H's `on_new_window` handler denies it and routes the URL to the `open_link` logic. Verify this on WebView2 early. If it doesn't fire, fall back to rendering links as plain text with a "copy link" affordance.
- **No edit affordances anywhere:** no new-note button, no drag to folders, no context menu actions.
- Plain JS modules, no build step. `textContent` only, never `innerHTML` for data, except the iframe `srcdoc`, which receives the Rust-generated page.
- Develop against WP-H's mock commands. If those aren't merged yet, use a `window.__MOCK__` shim in `ui\mock.js`, included only when `?mock=1` is set.

---

## 5. Wave 2: integration (sequential, 2–3 agents)

Merge order: skeleton → A, B, C, D, E, F, G (all crates) → H, I.
- After each merge, run `cargo test --workspace` and `cargo clippy --workspace -- -D warnings`.
- `Cargo.lock` conflicts: take either side, then run `cargo update -w`.

### WP-J: `notes-service` facade (`crates\notes-service\**`), one agent
- `Service::start()`:
  1. Load the jar and `validate`. On an auth error, emit `auth-required`.
  2. Run `sync` in the background, then `hydrate_bodies`.
  3. Emit progress events.
- `folders`, `notes`, `search` delegate to the store.
- `note_html(id, dark)`:
  1. Load the body proto and decode it.
  2. Build an `AttachmentResolver` from the store's Attachment, Media and InlineAttachment records:
     - image → `Image{src: http://notes-asset.localhost/<id>/original}`
     - table → `decode_table`
     - drawing.2 and paper → `fallback`, else `preview-{light|dark}` (pick the preview by its `PreviewAppearances` value, not its position)
     - calculateresult and hashtag → `Text(AltTextEncrypted / DisplayTextEncrypted)`
     - anything else → `Placeholder`
  3. Return `render_note` wrapped in `page`.
  - Drive rendering by the body's runs, not by "all attachments whose Note = X" (spike 5).
- `asset(attachment_id, variant) -> (bytes, mime)`:
  1. Check the disk cache `%LOCALAPPDATA%\<app>\assets\<fileChecksum>`.
  2. Otherwise, batch a `lookup` for a fresh URL, then `download`, then store it in the cache.
  - Re-download only when `fileChecksum` changes.
- A periodic refresh every 10 min while the window is focused, using the cheap `zone_token` check.
- Re-auth flow: an `ApiError::Auth` from anywhere → emit `auth-required` → the UI shows "Sign in". `sign_in` reopens the login window, which often signs in silently via the profile. Then replace the jar and resume.

### WP-K: wire host and end-to-end check, one agent with the user present
- Replace `MockService` with `notes_service::Service`.
- First real run with the user:
  - sign in
  - full sync (about 930 records)
  - compare 5 notes with the phone, including a table, an image and a drawing
  - restart, with no login prompt
  - edit a note on the phone, then Refresh, and only that note updates
  - search finds a known word
  - Sign out, then the app asks to sign in again
- **Read-only guarantee check:** grep the workspace for `records/modify`, `zones/modify`, `/share` and `subscriptions`. There must be no matches outside comments.

---

## 6. Wave 3: finishing (parallel, after WP-K)

- **WP-L, packaging:**
  - `cargo tauri build` produces an NSIS installer (per-user, no admin).
  - App identifier `com.ziggy.icloud-notes`, plus an icon.
  - Confirm the WebView2 bootstrapper setting.
- **WP-M, open data questions** (needs the user's phone; each check is ≤30 min):
  1. An interrupted then continued numbered list, to settle the `ps7` rule. Adjust WP-C if needed.
  2. A checklist and a block quote, to confirm the synthetic-only paths.
  3. A scan, a PDF and an audio attachment, to fill in spike 5's "unknown" rows.
  4. Rerun `spikes\spike6-sync delta later` for token lifetime and tombstone timing.
- **WP-N, hardening:**
  - fuzz `inflate` / `decode_note` / `decode_table` with `cargo fuzz` or proptest
  - a 2 MB render cap
  - a redacted log file at `%LOCALAPPDATA%\<app>\logs`, with dates of sign-in and re-auth (to learn the real session lifetime)

---

## 7. Parallelism summary

| Wave | Agents | Packages | Blocked by |
|---|---|---|---|
| 0 | 1 | skeleton + contracts | – |
| 1 | **9 in parallel** | A, B, C, D, E, F, G, H, I | Wave 0 |
| 2 | 1, then 1 (+user) | J, then K | all of Wave 1 |
| 3 | 3 in parallel | L, M (+user), N | K |

Each Wave 1 agent should be started with: "Read `PLAN.md` §1, §3 and your WP section. Work only in your paths, in your own worktree branch `wp-<letter>`. Do not change §3 signatures. Report contract problems instead of working around them."

## 8. Definition of done (whole app)
- After the first sign-in, the app starts without asking for credentials and shows cached notes immediately, offline too.
- All 240 non-locked notes render. Formatting matches the phone for the checked sample. Tables, images and drawing previews show.
- Refresh picks up phone edits, moves and deletes. Deleted notes disappear, and trashed ones show under Recently Deleted.
- No request in the codebase can modify iCloud data, and no note content or cookie value appears in any log.
