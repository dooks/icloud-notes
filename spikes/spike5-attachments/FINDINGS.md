# Spike 5: attachments (2026-10-25)

**Verdict:** images and every kind of preview/fallback image can be downloaded through the web API, as plain files,
with no cookies needed once you have a fresh URL. Tables are inline CRDT protobufs and decode fully with the vendored
schema (30/30). Drawings and Paper are not renderable natively, but every live one has a PNG fallback.
Nothing in this account needed client-side decryption.

Input: spike 2's 10 saved `changes/zone` pages (935 records). Online part: 1 validate, 2 `records/lookup`,
range GETs, and 2 full downloads (a 17.7 KB PNG image and a 10.6 KB Paper preview PNG) into `out\`.
All calls were read-only. The spike 1 WebView2 profile was reused; no sign-in was needed.

## Attachment types: support summary

| Type (UTI) | Seen | Where the content is | Support | Suggested approach |
|---|---|---|---|---|
| Images: `public.png`, `org.webmproject.webp` (and presumably `public.jpeg`, `public.heic`) | 8 + 6 | `Attachment.Media` → `Media.Asset` (the original file); also `PreviewImages` ×2 | **Supported** | Download `Media.Asset`. Use `PreviewImages` for thumbnails |
| Tables: `com.apple.notes.table` | 30 | `Attachment.MergeableDataEncrypted` (inline, gzip → `MergableDataProto`) | **Supported (needs a decoder)** | Port apple_cloud_notes_parser's embedded-table logic (~150–250 lines). No image fallback exists |
| Legacy drawings: `com.apple.drawing.2` | 13 (11 live in bodies) | `MergeableDataEncrypted` (4 inline) / `MergeableDataAsset` (7): PencilKit `wrd\xf0` binary | **Partly**: image only | Show `FallbackImage` (full size) or `PreviewImages` (light/dark). Don't parse PencilKit |
| Paper / new drawings: `com.apple.paper` | 5 | `PaperDatabase` + `PaperAssets` (assets), `MetadataData` (JSON bounds) | **Partly**: image only | Show `FallbackImage` / `PreviewImages`. The paper database isn't usable on Windows |
| Inline "calculate" result: `com.apple.notes.inlinetextattachment.calculateresult` | 1 | `InlineAttachment` record: `TokenContentIdentifierEncrypted`, `AltTextEncrypted` (UTF-8) | **Supported (as text)** | Render `AltTextEncrypted` inline (not checked by eye) |
| Hashtags: `com.apple.notes.inlinetextattachment.hashtag` | 2 records, 0 in bodies | `InlineAttachment` + `Hashtag` records | **Likely supported (as text)** | Render the alt text / `Hashtag.DisplayTextEncrypted`. No live example in a body |
| Scans (`com.apple.notes.gallery`), PDFs, other files, audio, video, URL/link cards, maps, contacts | 0 | — | **Unknown (not in this account)** | Expected: `Media.Asset` for files/PDF/audio, `PreviewImages`/`FallbackImage` for the visual ones. Re-test when one exists |
| Anything in a `PasswordProtectedNote` | 1 drawing | `Crypto*` + `EncryptedValues` | **Out of scope** | Skip |

## Q1: records and links

- **Body → record.** A note body's attachment run (`U+FFFC` + `attachment_info{attachment_identifier, type_uti}`)
  uses the record's `recordName` as its identifier. **61/61 body attachments matched a record**, and the UTI and the record's
  `Note` back-reference matched every time:
  - 60 → `Attachment` records;
  - 1 (`…inlinetextattachment.calculateresult`) → an `InlineAttachment` record.
- **Record → note.** `Attachment.Note` is a `REFERENCE` to the note, and the record-level `parent` is the same note.
  `Media` records also have `parent` = the note.
- **Image → file.** `Attachment.Media` (`REFERENCE`, only on image attachments) → `Media` record → `Media.Asset` (`ASSETID`) +
  `Media.FilenameEncrypted` (plain UTF-8, real extension) and `FileSize` on the Attachment.
- **Unreferenced records.** 2 Attachment records are referenced by no body: 1 inside a locked note, and 1 drawing in a normal note with `Deleted=0`.
  This is probably a leftover. **Rendering should be driven by the body runs, not by "all Attachments whose Note = X".**
- **Other Attachment fields:**
  - `UTI` (plain string; equals `UTIEncrypted` in 62/62);
  - `TitleEncrypted` / `SummaryEncrypted` / `OcrSummaryEncrypted` / `HandwritingSummaryEncrypted` / `ImageClassificationSummaryEncrypted`
    are all plain UTF-8 (not printed);
  - `Width`/`Height`, `Orientation` (always 0), `ImageFilterType` (0), `CroppingQuad*` (all types),
    `MinimumSupportedNotesVersion` (0 images, 2 tables, 4 drawings, 7–10 paper).
- **Note-level.** `Note.FirstAttachmentThumbnail` (24 notes) is the list-view thumbnail, with `FirstAttachmentUTIEncrypted` alongside.
  `AttachmentViewType` is always 0.

## Q2: assets and downloading

- **Asset value shape.** Every `ASSETID` value is the same for all asset fields: `{downloadURL, size, fileChecksum, referenceChecksum, wrappingKey}`.
  - `fileChecksum`/`referenceChecksum`: 21 bytes, first byte `0x01`. This is **not** SHA-1 or SHA-256 of the file;
    it's MMCS's chunk-signature format. Use it as a stable cache key, but verify downloads by `size` only.
  - `wrappingKey`: 16 bytes. The **downloaded bytes were plain PNG**, so with standard data protection
    the server decrypts and the client never needs `wrappingKey`.
- **URL.** `https://cvws.icloud-content.com/B/<56-char id>/${f}?o&v&x&a&e&fl&r&k&ckc&ckz&p&s`.
  `${f}` is a filename placeholder: the request works with it replaced by anything, or left literal.
- **URLs expire quickly.** `e` is a Unix timestamp about **15 minutes after the page was fetched**. Spike 2's saved URLs (2.3 h old)
  returned **410 Gone**, and still did after a fresh lookup.
- **Getting a fresh URL.** `records/lookup` (`{"records":[{"recordName":…}],"zoneID":{"zoneName":"Notes"}}`, same Origin/cookies as
  `changes/zone`) returns a new URL. `o, a, e, r, s` (and sometimes `p`) change, while the path, `fileChecksum`, `referenceChecksum`,
  `wrappingKey` and `size` stay the same.
- **No auth on the download.** The fresh URL is a bearer URL: **no cookies, no Origin, no extra headers** needed (206/200 either way).
  Range requests work. Responses come back as `content-type: application/octet-stream`, with `cache-control: private, max-age=2592000`.
  So sniff the magic bytes, or use the UTI, to get the type.
- **App approach:**
  - store the asset metadata from `changes/zone`;
  - when an attachment is shown, batch-`records/lookup` its record(s) for fresh URLs (`desiredKeys` to trim the response is untested);
  - GET without cookies;
  - cache on disk keyed by `fileChecksum`, which stays stable until the content changes.
- **Large note bodies.** The one note with `Note.TextDataAsset` should be fetched the same way (not downloaded here; it's note content).

## Q3: previews and fallbacks

| Type | `PreviewImages` | `PreviewAppearances` | `FallbackImage` |
|---|---|---|---|
| png / webp | 2 per record (6–120 KB) | `[0,0]` | — |
| drawing.2 | 2 per live record (16–86 KB) | `[0,1]` = light + dark | 11/13 (all live ones), up to 3.4 MB |
| paper | 2 (11–767 KB) | `[0,1]` or `[1,0]`: the order varies, so pick by value | 5/5 |
| table | none | — | none |

- `PreviewWidths/Heights/Scales/ScaleWhenDrawings` are parallel lists to `PreviewImages`.
- The downloaded Paper preview was a real PNG.
- PDFs, scans and link cards don't exist in this account, so their previews are unconfirmed (see the table above).
- **Text fallbacks for tables.** Tables carry `FallbackTitle` / `FallbackSubtitleIOS` / `FallbackSubtitleMac`, which are constant strings (lengths 5/31/33,
  1 distinct value each). This is presumably the "Table" + "update to view" text for old clients. They're not useful as content, but fine as an
  "unsupported table" placeholder.

## Q4: tables

- **Encoding.** `MergeableDataEncrypted` = base64 → gzip → `MergableDataProto` (version 0). The vendored `notestore.proto` decodes all 30
  with no errors.
- **CRDT names.** Key items are `identity, crRows, crColumns, cellColumns, crTableColumnDirection, self, UUIDIndex`. Type items include
  `com.apple.notes.ICTable`, `com.apple.notes.CRTable`, `com.apple.CRDT.*` (`NSString`, `NSUUID`, `CRTree`, `CRRegisterMultiValue`, …).
  Some tables also list `ICTTAudioRecording*` types; these look like a shared type registry, not content.
- **Structure.** The root is a `custom_map` entry typed `ICTable`, with map keys:
  - `crRows` / `crColumns`: `ordered_set` entries. `ordering.array.attachment` gives the UUIDs in order, and its count equals
    `elements` in 30/30;
  - `cellColumns`: dictionary of column → dictionary of row → entry with a `Note`. That's the same `Note`
    (text + `AttributeRun`s) as a note body, so cells can carry formatting;
  - `crTableColumnDirection`: presumably LTR/RTL.
- **Size.** 1–7 columns, 2–57 rows. The cell count is ≤ rows × columns (empty cells are simply missing). There are 0 attachments inside cells,
  and some cells have styled runs (bold/links/…).
- **Difficulty: moderate.**
  - The cell text is already a `Note`, so spike 3's run renderer can be reused.
  - The work is resolving `object_index` and `UUIDIndex` to put rows and columns in the right order. The count-only walk here didn't verify ordering.
  - Estimate: ~200 lines plus tests, using `embedded_table.rb` in apple_cloud_notes_parser as the reference.

## Q5: not available, or not practical

- **Vector drawings.** PencilKit data (`wrd\xf0` blobs, the Paper database) can't be rendered without Apple frameworks, so they're image-only.
- **Locked notes' attachments.** Encrypted. Out of scope.
- **Never tested.** OCR, handwriting and image-classification summaries are present (plain UTF-8), but whether they're worth anything wasn't checked.
- **Not in this account.** Scans, PDFs, audio, video, link cards, files, and checklists inside tables don't exist here, so whatever they need can't be confirmed.

## Files

- **Code:**
  - `src\lib.rs`: decode helpers copied from spike 3, a record loader, and `table_stats`;
  - `src\bin\analyze.rs`: offline;
  - `src\bin\fetch.rs`: Tauri, reuses the spike 1 profile (identifier `com.ziggy.icloud-notes-spike1`), stops instead of asking for sign-in.
- **Output** (`out\`, gitignored): `analysis.txt`, `fetch-report.txt`, `targets.json`, `media.png`, `preview.png`.
  The two PNGs are the user's own content and were checked only by magic bytes and size, not viewed.
- **Commands:**
  ```
  cargo run --bin analyze      # offline, reads ..\spike2-cloudkit\out
  cargo run --bin fetch        # online, max 2 targets
  ```
