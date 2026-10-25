# Spike 6 findings: incremental sync (2026-10-25, phases A, B and C done)

Crate: `spikes\spike6-sync\` (throwaway Tauri 2 app, session code copied from spike 2, reuses spike 1's WebView2 profile).
`spike6-sync.exe offline` analyses spike 2's saved pages; with no argument it runs the network probes; `delta <label>`
syncs from `out\token-current.txt` and diffs against `out\cache.json`; `collapse` diffs everything since the R0 token.
Reports: `out\report-offline.txt`, `out\report-network.txt`. Raw pages are in `out\full-*.json` and `out\old-delta-*.json`, and the
final token is in `out\token-r0.txt`. `out\` is gitignored and holds raw note data.
All calls were read-only: `changes/zone`, `changes/database`, `zones/list`, `zones/lookup`, `records/lookup`. No note content was logged.

## Summary
- **The `changes/zone` syncToken is a self-contained, monotonic change-sequence cursor.** Saving the last page's
  token is enough to resume. Saving any page's token is enough to resume an interrupted full sync.
- **An incremental pull returns changed records whole**, in their latest state only. Tombstones are `{recordName, deleted:true}` only, with no recordType.
- **Deleting a note on the phone doesn't create a tombstone.** "Delete" moves the Note to `TrashFolder-CloudKit`.
  "Delete from Recently Deleted" sets `Deleted=1` on the same record, and the record stays.
- **Errors arrive as HTTP 200** with `zones[0].serverErrorCode`. The client must check the zone-level error, not just the status.
- **Cheap change check:** `zones/lookup` (463 B) returns the zone's current syncToken. If it is byte-equal to the saved
  token, nothing changed.
- **`changes/zone` records are complete.** `records/lookup` returned the same fields and values. Only asset-backed fields
  (`TextDataAsset`, previews and so on) need a separate download, and their `downloadURL` is re-signed on every fetch.

## 1. Paging (`moreComing`, `syncToken`, `resultsLimit`)

**Token layout.** The token is base64 (standard alphabet) of `0x1f` followed by a protobuf:
`1:{1=<change counter>} 3=0 4:{1=<big> 2=<big>} 5=0 [8:{1:{…} 2=<page#>}]`.
- Field 1.1 is a zone-wide change-sequence counter. Today's run reached 40215. It rises with every page.
- Field 4 is constant and probably identifies the zone or partition.
- Field 8 appears only while `moreComing=true`. It is a continuation cursor whose `2=` is the page number. A final
  (caught-up) token has no field 8: it is 34 bytes, versus 60–61 bytes mid-run.

**Pages are counter windows, not fixed-size batches.**
- With `resultsLimit` 200, pages held 45/123/48/160/143/151/1/69/18/169 records.
- Pages 7→8→9 advanced the counter by exactly 150 each and held only 1–69 records. This looks like a server scan budget
  over the change log.
- The largest page was 828 KB.

| `resultsLimit` | first 3 pages (records) |
|---|---|
| none | 45 / 123 / 48, identical to 200 and 1000, so about 200 looks like the server default/maximum |
| 1000 | 45 / 123 / 48 |
| 50 | 1 / 46 / 50 |
| 1 | 0 / 1 / 1 |

`resultsLimit` is an upper bound only. Pages can be empty with `moreComing=true` (limit 1, page 1), so loop on
`moreComing`, never on "got 0 records".

**The order is oldest change first.** `modified` timestamps rise strictly across pages, with 0 overlaps, from 2018 up to today on
the last page. A full pull from no token returns each record once (927 unique recordNames, no duplicates), at the
position of its latest change.

**Resume works and is deterministic.**
- Restarting from page 3's token gave pages 4..10 byte-for-byte equal in counts, 711/711 identical
  `(recordName, recordChangeTag)` pairs, and the same final token.
- Resuming from a mid-run token with a different `resultsLimit` also works.
- Spike 2's page-5 mid-run token, from a run 2.3 h earlier, still worked as well.

**A caught-up token is stable.** Sending the final token again returns 0 records, `moreComing=false` (234 B), and the
**identical** token. The token only changes when the zone changes.

**Full pulls change over time.** The full pull 2.3 h after spike 2 had 927 records versus 935. Live records stayed at 489, but there were 8
fewer tombstones. The server purges old tombstones from a from-scratch listing. The counters of the early pages also
shifted a little (24049 → 24051), because records whose latest change moved later leave their old slot.

## 2. What an incremental pull returns
**Phase C: phone rounds with one throwaway note** (`spike6-sync.exe delta <label>`, reports in `out\report-delta-r*.txt`).
Each round was one change, then a delta from the stored token. `zones/lookup` flagged every round, and each one moved
the counter by exactly 1.

| round (phone) | delta | what changed in the record |
|---|---|---|
| R1 create a note | 1 record, 5.3 KB: **new Note** | Folder=`DefaultFolder-CloudKit`, Deleted=0. Full body inline, no `SnippetEncrypted` yet. No Note_UserSpecific. |
| R2 edit (add a line) | 1 record, the same Note, **whole**, new changeTag | `ModificationDate`, `SnippetEncrypted`, `TextDataEncrypted` |
| R3 move to another folder | 1 record, the same Note, whole | `Folder`, `Folders`, `FoldersModificationDate` only. **No `ModificationDate` change**, and the Folder records were untouched |
| R4 delete (goes to Recently Deleted) | 1 record, the same Note, whole | `Folder`/`Folders` → **`TrashFolder-CloudKit`**, `FoldersModificationDate`. **`Deleted` stays 0, no tombstone** |
| R5 delete from Recently Deleted | 1 record, the same Note, whole | **`Deleted` 0 → 1** only. The record and its body are *still there*, so there is still no tombstone |
| collapse: one delta from the R0 token after R5 | **1 record**: the Note in its final state (in the trash folder, Deleted=1) | intermediate states are not replayed. A delta is "the latest version of each record changed since then" |

So:
- **Recently Deleted** = `Note.Folder == TrashFolder-CloudKit`, with `Deleted=0`.
- **Permanently deleted** = `Deleted=1`. The record stays as a soft-deleted row.
- A real tombstone (`deleted:true`) presumably comes later, when a device purges the record. The 446/438 tombstones in
  the full pull show that this does happen eventually. When it happens wasn't observed. A rerun of `delta later` in a few days
  would show it, together with the token-lifetime check.
- The original folder isn't kept on the Note once it is in the trash (`Folder`/`Folders` both point at the trash),
  so the app can't show "restore to …" from this record alone.

What phase B showed:
- **Delta from spike 2's final token (2.3 h old):** 2 records, both `Attachment`, returned **whole**, with all fields and
  a new `recordChangeTag`. The changed fields were preview images, sizes, summary and the replica version, probably
  re-rendered previews on a device. The final token equalled today's.
- **Tombstones carry only `recordName` and `deleted:true`.** There is no `recordType`, `modified` or `recordChangeTag`. The cache must
  keep a recordName → type index to know what was deleted. `records/lookup` on a tombstone gives
  `serverErrorCode=NOT_FOUND`, the same as for a name that never existed.
- **The soft-delete `Deleted` field** (INT64 0/1) is on Note, Attachment, Media, Hashtag, InlineAttachment and
  *_UserSpecific. All live Notes are currently `Deleted=0`; the 3 Hashtags and 2 of the 3 InlineAttachments are `Deleted=1`.
- **Folders (offline analysis):**
  - `TrashFolder-CloudKit` and `DefaultFolder-CloudKit` are Folder records with fixed names. The other 11 folders are UUIDs,
    and one of them is a smart folder (`SmartFolderQueryJSONEncrypted`).
  - Each Note has `Folder` plus `Folders` (a list of 1), and they always agree.
  - 22 `Note_UserSpecific` records point at `TrashFolder-CloudKit`. 19 belong to tombstoned notes and 3 to live notes in
    normal folders. Phase C confirms that **`Note_UserSpecific` is not the trash signal**: no round touched it.
  - 62 `Note_UserSpecific` records refer to notes outside this zone, probably shared notes (spike 7's area).

## 3. Token validity and errors
**Lifetime:** the oldest token tested was 2.3 h old (spike 2's), and it was fully valid, as was its mid-run cursor.
- The token holds no session or expiry data that we can see. It is a counter plus a zone id, and it survived a new
  web session.
- Rewriting the counter to an older value (−1000) is accepted and returns changes since that point. Rewriting it to 1
  acts like a full sync. So the server doesn't verify the token beyond its structure.
- Native CloudKit's `CHANGE_TOKEN_EXPIRED` was not seen. The tombstone purge in §1 means a *very* old token
  could, in principle, miss deletes. Without a long-lived token we can't tell whether the server flags that. A recheck with
  `token-r0.txt` after days or weeks is a cheap follow-up.

| token sent | HTTP | `zones[0]` result |
|---|---|---|
| one byte flipped | 200 | `serverErrorCode=BAD_REQUEST`, reason "Invalid continuation format" |
| truncated | 200 | `BAD_REQUEST`, "Invalid continuation format" |
| `"not-a-token"` | 200 | `BAD_REQUEST`, "Unknown sync continuation type" |
| `""` (empty string) | **500** | top-level `serverErrorCode=INTERNAL_ERROR` (omit the field instead) |
| counter +1,000,000 | 200 | `TRY_AGAIN_LATER`, "client knowledge is ahead of server knowledge" |
| counter −1000 | 200 | OK, changes since then |

## 4. Cheap "did anything change?" checks

| call | size | what it tells you |
|---|---|---|
| `POST zones/lookup {"zones":[{"zoneName":"Notes"}]}` | 463 B | the zone's current `syncToken`, byte-equal to the `changes/zone` final token. Compare with the stored token. **Best fit.** |
| `GET zones/list` | 1.3 KB | the same per-zone tokens for Notes, Metadata and _defaultZone (no token on the default zone) |
| `POST changes/zone` with the saved token | 234 B when nothing changed | 0 records plus the same token. Effectively free, and it is the real sync call anyway |
| `POST changes/database {}` | 401 B | lists changed zones (`Notes`, `Metadata`) plus a 9-byte DB token; with that token, 75 B and 0 zones. The DB token changes on every call, so compare zone lists, not tokens |

**Payload trimming works.** `desiredKeys:["ModificationDate"]` cut the 2-record delta from 15.9 KB to 1.8 KB. `desiredRecordTypes:["Note"]`
filters record types, but tombstones still come through, since they have no type. `records/lookup` honours `desiredKeys` too.

## 5. Inline vs separate fetch
- `records/lookup` on a Note with `TextDataAsset`, a Note with an inline body and an Attachment returned the **same field
  set, the same values and the same changeTag** as the `changes/zone` copy. Nothing is truncated in the changes feed.
- **Not inline:** 1 of 241 Notes keeps its body in `TextDataAsset` (an ASSETID with `downloadURL, fileChecksum,
  referenceChecksum, size, wrappingKey`). The largest inline body is 88,864 base64 characters, about 65 KB. The other asset
  fields are listed in spike 5. **Each fetch returns a new `downloadURL`**, so URLs must not be cached. Re-fetch them via
  `records/lookup` (optionally with `desiredKeys`) when needed.

## Recommended sync and cache strategy
1. **Cache:** a local DB of `recordName → {recordType, recordChangeTag, fields JSON}`, plus decoded note text. Store
   asset checksums, not URLs. Keep one stored value: the `Notes` zone syncToken. Save it only after a pull completes, or
   save each page's token as you apply that page, since mid-run tokens resume correctly.
2. **First sync:** loop `changes/zone` with `resultsLimit` 200 and no token, until `moreComing=false`. Apply records
   in page order (later wins). Skip tombstones, since there is nothing to delete yet.
3. **Refresh** (on app start or a timer): `zones/lookup`. If the token equals the stored one, stop. Otherwise loop
   `changes/zone` from the stored token. Live records **replace** the cached copy whole. Tombstones delete by recordName,
   using the cached type index to cascade: a Note removes its decoded text and its attachments' rows.
4. **Errors:** always check `zones[0].serverErrorCode`.
   - `BAD_REQUEST` (continuation): discard the token and do a full resync. Mark-and-sweep cache rows not seen
     in the new listing, because a full listing doesn't contain old tombstones.
   - `TRY_AGAIN_LATER`: back off and retry. If it persists, resync fully.
   - `CHANGE_TOKEN_EXPIRED`/`ZONE_NOT_FOUND`, if they ever appear: full resync.
   - Never send `syncToken: ""`.
5. **Assets:** download lazily via a fresh `records/lookup` URL. Re-download only when `fileChecksum` changes.
6. **Visibility rules**, applied on every upsert, since every state change arrives as a whole-record replace:
   - `Deleted == 1` (on Note, Attachment, Media, Hashtag, InlineAttachment): treat as gone. Drop the decoded text and
     its assets from the cache, but keep a stub row (recordName, type, changeTag) so a later tombstone or revival lines up.
   - `Note.Folder == TrashFolder-CloudKit`: show under "Recently Deleted" if wanted, otherwise hide it.
   - Otherwise the note lives in `Note.Folder`. A move only changes `Folder`/`Folders`/`FoldersModificationDate`, so sort by
     `ModificationDate` (a move doesn't change it).
   - `deleted:true` tombstone: remove the row and, by cascade, the Note's attachments and their media.
   - Ignore `*_UserSpecific` for folder placement and trash. They hold per-user state (pinned, last viewed).
7. **Re-decode bodies only when `TextDataEncrypted`/`TextDataAsset` changed**, by comparing the field value or changeTag. Moves and
   trash/delete flips don't touch the body.
8. **Open follow-up:** rerun `spike6-sync.exe delta later` after a few days. It shows whether the `Deleted=1` test note turns into a
   tombstone, and whether a token several days old (`token-current.txt`) still resumes.
