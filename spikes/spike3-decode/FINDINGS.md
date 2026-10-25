# Spike 3: decode `TextDataEncrypted` → text + attribute runs (2026-10-24, real data 2026-10-25)

**Verdict: it works on real data.** `TextDataEncrypted` from CloudKit `changes/zone` (standard data protection)
is plaintext base64 → gzip/zlib → protobuf. **All 240 inline note bodies decoded, with 0 failures.** The run lengths
covered the text exactly in UTF-16 units for all 240. The published schema is enough to render text and formatting.
The extra fields Apple sends are CRDT/replica bookkeeping and paragraph IDs.

The main run was stats only (`records ..\spike2-cloudkit\out`). The style mapping was then checked with `--show-text 3` (user OK)
and with `--pick 73,83,87,99 --mask` (letters → x/X, digits → 9, structure kept). See "Style mapping check" below.

## Style mapping check (2026-10-25)
- **`--show-text 3` didn't help much:** the first 3 notes are one decorative emoji/whitespace note (1 plain run) and two sketch
  notes (drawing attachments + single letters). None of them has lists, `ps7` or style 3. They did confirm that `note_text` keeps
  odd whitespace (U+3000, U+2002, braille blank, ZWJ) and that `TitleEncrypted` is the first line, truncated with "…".
  For mapping, the target notes were picked via the stats and shown **masked**.
- **The high bullet-list count is counting runs, not paragraphs.** One paragraph is split into many runs (CRDT edit fragments
  and inline formatting), and every run repeats the paragraph's `ParagraphStyle`. Note #99 really is a long bulleted note:
  1060 bullet runs, each a fragment of a bullet paragraph. **Style 100 = bullet list is consistent with the data.**
  The renderer has to split `note_text` at `\n` and take each paragraph's style from its runs, not emit one list item per run.
- **List markers aren't in the text:** no `•` or `1.` characters. The renderer draws them.
- **`ParagraphStyle.7` = the list item number.** In note #83 (numbered list, style 102) the six consecutive items carry
  `ps7` = 1, 2, 3, 4, 5, 6, constant across all runs of each paragraph. It's absent on about half of all numbered runs, and it also
  appears (as 1, 2 or 4) on some bullet/body paragraphs in #99, probably left over after list type changes.
  **Rule:** for style 102, use `ps7` when present, else count. Ignore it for other styles. Medium confidence.
- **`style_type = 3` = Body (set explicitly).** All 3 runs (notes #73, #87) are ordinary sentences with `align=4`. That fits
  Apple's Format menu order Title 0 / Heading 1 / Subheading 2 / **Body 3** / Monostyled 4. Render as body.
- **`alignment = 4`** is probably NSTextAlignment.natural, so render it as the default (left).
- **`ParagraphStyle.3` = 1** appears on whole-note runs everywhere and still has no visible meaning. Ignore it.

## Real data results (spike 2 pages 001–010, 2026-10-25)
**Records:** Note 241, Note_UserSpecific 106, Attachment 62, PasswordProtectedNote 15 (+24 _UserSpecific; out of scope),
Media 14, Folder 13, Folder_UserSpecific 7, Hashtag 3, InlineAttachment 3, AccountData 1.

**Note fields** (name:type → count of 241): CreationDate, ModificationDate, Folder:REFERENCE, Folders:REFERENCE_LIST,
FoldersModificationDate, TitleEncrypted (241); TextDataEncrypted:ENCRYPTED_BYTES (240); Deleted, AttachmentViewType,
MinimumSupportedNotesVersion, PaperStyleType, ReplicaIDToUserIDEncrypted (239); ReplicaIDToNotesVersionDataEncrypted (238);
SnippetEncrypted (212); FirstAttachmentThumbnail:ASSETID, FirstAttachmentUTIEncrypted (24), …Orientation (26);
**TextDataAsset:ASSETID (1)**.
- `TitleEncrypted` (241/241) and `SnippetEncrypted` (212/212) are base64 of **plain UTF-8**. "Encrypted" in the field name doesn't
  mean the web client has to decrypt anything for this account.
- **One note has no inline `TextDataEncrypted` and carries `TextDataAsset` instead.** This is almost certainly a large body
  stored as a CloudKit asset (download URL). The app must fetch that asset and run the same decoder on it. Not tested yet.

**Decode:**
- Compression: **gzip 236, zlib 4**, raw 0. Both have to be supported.
- Compressed size min/avg/max is 117 / 2701 / 66647 B. Text runs 1 / 1315 / 72836 chars, with 0 empty notes.
  Runs per note: 1 / 61 / 1108.
- `Document.version` = 0 in all 240 (the parser's comments suggest higher values; here it's 0).
- Run coverage == UTF-16 length: 240/240. **Run lengths are UTF-16 code units, confirmed.**

**Run kinds seen** (counts of runs carrying each attribute):
bullet-list (style 100) 7246, plain 5164, indent=1 2255, numbered-list (102) 809, bold 623, indent=2 547, title (0) 487,
font(size only) 230, italic 214, subheading (2) 91, dash-list (101) 88, underline 82, heading (1) 80, align=4 54,
indent=3 30, link 28, bold+italic 18, monospaced (4) 6, color 5, **style 3: 3 (unknown value)**, strike 2, indent=4 2.
Attachments (U+FFFC + attachment_info): com.apple.notes.table 30, com.apple.drawing.2 11, public.png 8,
org.webmproject.webp 6, com.apple.paper 5, com.apple.notes.inlinetextattachment.calculateresult 1 (61 total, matching
62 Attachment records). **Checklists (style 103 / ParagraphStyle.5): 0 seen.** This account has none, so that path is
tested only synthetically. `emphasis_style` (14), superscript (8) and AttributeRun.13 values were not looked at.

## Schema differences (tag census, tag numbers + small-int header/style values only)
| Field | Seen | What it looks like | Needed for rendering? |
|---|---|---|---|
| `NoteStoreProto.1` varint | 240/240 | always 0 | no |
| `Document.1` varint | 240/240 | always 0 | no |
| `Note.3` (repeated msg) | 55,627 | subfields 1 (msg), 2 (varint), 3 (msg), 4 (varint, 82%), 5 (varint) | no: the topotext CRDT substring graph. `note_text` is already the materialized text |
| `Note.4` (msg) | 240/240 | repeated subfield 1 (msg), 1–6 entries | no: replica/version clock |
| `ParagraphStyle.3` varint | 13,246 | always 1 | unknown, ignore |
| `ParagraphStyle.7` varint | 406 | values 1–6 (1: 302, 2: 45, 4: 47, …) | yes for numbered lists: the item number (see the style mapping check) |
| `ParagraphStyle.9` bytes | 6,545 | always 16 bytes | no: a paragraph UUID |
| `AttributeRun.13` varint | 14,295 of 14,648 runs | in the schema as `unknown_identifier` | unknown |
| `ParagraphStyle.style_type = 3` | 3 runs | not in the parser's style list | = Body (set explicitly), render as body |

Schema fields never seen in this data: `ParagraphStyle.checklist` (5), `ParagraphStyle.block_quote` (8), `AttributeRun.superscript` (8),
`AttributeRun.emphasis_style` (14). Seen but rare: `color` (10) 5, `strikethrough` (7) 2, `link` (9) 28, `Font.font_name` 13.

## What was built
- Crate: `spikes\spike3-decode\` (prost 0.14.4, prost-build 0.14.4, protox 0.10.0, flate2 1.1, base64 0.23, serde_json).
- Schema: `vendor\apple_cloud_notes_parser\notestore.proto` comes from
  `github.com/threeplanetssoftware/apple_cloud_notes_parser` at commit `4754a2b62686570cca46690d101079e80cf6ae66`
  (MIT, LICENSE included, see `SOURCE.txt`). The plan's `jareware/...` link points to the wrong repo.
- **No protoc needed:** `build.rs` uses `protox::compile` → `prost_build::Config::compile_fds`. The build is cargo-only.
- Pipeline (`src\lib.rs`): base64 (standard, with URL-safe and no-pad fallbacks) → sniff the first bytes
  (`1f 8b` = gzip via `MultiGzDecoder`, `78 xx` with a valid zlib header = zlib, otherwise raw protobuf) → `NoteStoreProto::decode`.
- Run summary: style (title/heading/subheading/monospaced/lists/checklist), checklist done state, indent/alignment/quote,
  bold/italic, underline, strikethrough, super/sub, link, color, font, emphasis, and attachment(type_uti).
- Tag census: a raw protobuf wire walk over NoteStoreProto → Document → Note → AttributeRun → ParagraphStyle/Checklist/
  Font/Color/AttachmentInfo. It counts every `message.tag (wiretype)` and flags tags that aren't in the schema.
  prost silently drops unknown fields, so this is how schema differences show up. It records tag numbers only, never values.

## Usage
```
cargo test                                   # 3 tests: gzip round trip, zlib + raw, unknown-tag detection
cargo run -- synth                           # decode a built-in fake note, print text + runs
cargo run -- records <file.json | dir>       # stats only: no note text, titles or link targets
cargo run -- records <dir> --show-text N     # also print text/title/runs for the first N notes
cargo run -- records <dir> --pick 73,83      # print only these notes (1-based, in stats order)
cargo run -- records <dir> --pick 83 --mask  # same, with letters -> x/X and digits -> 9 (structure only)
```
The run listing shows `[st=<style_type> ps3=.. ps7=..]`. ps3/ps7 are read from the raw bytes because prost drops them.
`records` searches the JSON for any object with `recordType` + `fields`, so it accepts a raw `changes/zone` response,
a directory of saved pages, or an array of either. For Note records it reads `fields.TextDataEncrypted.value`.

## Findings so far (synthetic data + tooling)
1. **prost 0.14 + proto2 `required`:** a required *message* field (`NoteStoreProto.document`, `Document.note`) becomes a
   plain struct, not `Option`. A missing required field decodes **without error** as `Default`, so an empty text means
   "absent or empty". Use the tag census to tell the two apart. The real app should treat `Document.note` being
   absent as a failure.
2. **Run lengths are UTF-16 code units** (NSString), not bytes or chars. This assumption is built into `spans()`. The synthetic
   note includes a non-BMP emoji (🥚 = 2 units), and the `run coverage == utf16 len` check passes. This still needs
   confirming on real notes. The `records` output prints `matches utf16` / `MISMATCH` for every note.
3. **Attachments** are U+FFFC in `note_text`, with a 1-unit run that carries `attachment_info {attachment_identifier, type_uti}`.
   The identifier should match an `Attachment` record in the zone. That link isn't checked yet.
4. **The published schema is partial.** `Note` defines only fields 2 (text) and 5 (runs), and its own comment says other fields exist.
   Expect the census to show more `Note.*`, `Document.*` and `AttributeRun.*` tags in real data.
5. Failure modes are reported per note, not fatally: bad base64, inflate errors, protobuf errors (with the first 4 bytes
   for diagnosis), and Note records with no `TextDataEncrypted` (counted as `missing`).

## Still open
- Compare one numbered list (`ps7` present vs absent) against icloud.com's rendering to confirm the item-number rule.
- The `TextDataAsset` path for large notes: download the asset and decode it.
- Checklists: no real examples in this account.
- Tables (`com.apple.notes.table`, 30) are separate MergableDataProto blobs on the Attachment records, and drawings/paper
  have fallback images. Those are the next spike, not this one.
- Deleted notes (`Deleted` field) and the `Note_UserSpecific` records weren't analysed here.

## Privacy
Default output contains no note content: only record names (UUIDs), sizes, counts, tag numbers, and small-int values of
header/paragraph-style fields. Titles and snippets are checked for UTF-8 validity, never printed. `.gitignore` excludes
`*.json` and `out/` so saved records can't be committed from this folder.
