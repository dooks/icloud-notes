# Spike 7 findings: folders, shared notes, shared DB (2026-10-25)

Code: `src-tauri\src\lib.rs` is a throwaway Tauri app. It reuses spike 1's WebView2 profile (`com.ziggy.icloud-notes-spike1`) and copies its request code from spike 2.
The redacted report is `out\report.txt` and the raw responses are in `out\*.json` (gitignored).
Offline input: spike 2's saved pages `spikes\spike2-cloudkit\out\changes-zone-page-*.json`, which the spike only reads.

**Calls made (read-only):** validate, `zones/list` and `changes/database` on the private and shared databases, `zones/list` on cksharews, and two `records/lookup` calls.
There were no writes, no share acceptance, and no `records/resolve`. No sign-in was needed because the saved profile was still valid.

**Account state (the user's answers, consistent with the data):**
- Nothing is shared with the user.
- Nothing is shared by the user now; some notes were shared in the past.
- There are no subfolders.

So the nesting and shared-in parts below describe request shapes and empty results, not real shared or nested records. Anything inferred is marked **(unverified)**.

## 1. Folder records

The private database, zone `Notes` (`REGULAR_CUSTOM_ZONE`), holds 13 `Folder` records. Their fields:

| Field | Seen | Notes |
|---|---|---|
| `TitleEncrypted` | 13/13 | base64 of UTF-8, as for notes (spike 3) |
| `MinimumSupportedNotesVersion` | 13/13 | 0, except 7 on the Quick Notes smart folder |
| `TitleModificationDate` | 9 | |
| `ParentModificationDate` | 9 | Present, but no parent field exists. See nesting below. |
| `SmartFolderQueryJSONEncrypted` | 1 | only on `SystemPaper-CloudKit` |

Record-level keys are `chainPrivateKey`, `chainProtectionInfo`, `created`, `modified`, `recordChangeTag`, and an always-empty `pluginFields`.
Folders have **no record-level `parent`** and no `shortGUID`/`stableUrl`. `records/lookup` returns the same fields as `changes/zone`, so nothing is hidden from the zone feed.
Asking for `desiredKeys` such as `ParentFolder`, `Parent`, `SortOrder`, `Share` or `Participants` returned only the keys that exist (`TitleEncrypted`, and `Folder` on the note).

**Special folders have fixed recordNames.** Match them by recordName, not by title, because titles are localized.

| recordName | Title | Behaviour |
|---|---|---|
| `DefaultFolder-CloudKit` | "Notes" | ordinary folder; holds 142 of 256 notes |
| `TrashFolder-CloudKit` | "Recently Deleted" | no live note has `Folder` pointing here in this account (see section 4) |
| `SystemPaper-CloudKit` | "Quick Notes" | **smart folder**. `SmartFolderQueryJSONEncrypted` decodes to plain JSON: `{"entity":"note","type":{"and":[{"systemPaper":true},{"deleted":false}]}}`. Its members are notes whose `Note_UserSpecific.IsSystemPaper == 1`. No note has `Folder` pointing here. |

Other user smart folders (tag or date filters) would carry the same `SmartFolderQueryJSONEncrypted`. There are none in this account, so the app should show smart folders but can leave evaluating their query DSL for later. Hashtag records exist (3), which is what tag filters would match on.

**Nesting:** no folder in this account has a parent, and there are no subfolders.
- The best guess **(unverified)** is a `ParentFolder` REFERENCE field plus a record-level `parent` pointing at the parent folder. That is how notes point at folders, and CloudKit hierarchical shares follow `parent` chains.
- `ParentModificationDate` on 9 top-level folders suggests the reference is set or cleared there. A cleared field is omitted from the JSON.
- The app should treat "no parent" as top level. If a `ParentFolder` or `parent` appears, it should nest under it. A one-off check: the user creates a subfolder in Apple Notes, and spike 7 is re-run with a `records/lookup` on it.

**Ordering:** no record carries a sort or order field. Apple Notes sorts folders itself. Suggested app order:
- Quick Notes first.
- Notes next.
- User folders sorted by title, case-insensitive and locale-aware.
- Shared-in folders after those.
- Recently Deleted last.

**Note → folder:** `Note.Folder` (REFERENCE), `Note.Folders[0]` and the note's record-level `parent.recordName` were identical in 256 of 256 notes (Note + PasswordProtectedNote). `Folders` always had exactly 1 element. Use `Folder`.

## 2. Notes and folders shared *with* the user: the shared database

| Request | Result |
|---|---|
| `GET {ckdatabasews}/database/1/com.apple.notes/production/shared/zones/list` | 200 `{"zones":[]}` |
| `POST …/shared/changes/database`, body `{}` | 200 `{"moreComing":false,"syncToken":"…","zones":[]}` |
| `GET {cksharews}/database/1/com.apple.notes/production/{shared,private}/zones/list` | 404 `No resource at this location` |

- The headers are the same as for the private database: cookies plus `Origin`/`Referer`, with no query parameters.
- **cksharews (`https://pNNN-ckshare.icloud.com`) is not a database endpoint.** In CloudKit JS it backs share URL resolution and acceptance, and this read-only app doesn't need it. Content that has already been accepted is read from `ckdatabasews …/shared/…`.

**How the app should fetch shared content.** The paging code is already in spike 7 (`zone_pull`); with no shares it never ran against real data. **(unverified)**
1. `POST …/shared/changes/database` with `{}`, or with `{"syncToken": <saved>}` for incremental updates. Each entry is `zoneID {zoneName, ownerRecordName, zoneType}`. The `ownerRecordName` is someone else's, not the user's.
2. For each zone, `POST …/shared/changes/zone` with `{"zones":[{"zoneID": <that whole zoneID, including ownerRecordName>, "resultsLimit":200, "syncToken"?}]}`. This is the same shape and paging as the private `Notes` zone.
3. Expect the same record types (`Folder`, `Note`, `Attachment`, `Media`, …) for whatever subtree was shared. A shared note's `Note.Folder` may point to a folder that isn't in that zone, so treat a missing folder as "Shared with me".
4. Store the sync tokens per database and per zone. Spike 6 covers sync-token semantics.
5. Expect a `share` key on the shared root record, and a `cloudkit.share` record in the zone. Participant details live there; don't display or log them.

**Per-user placement.** The `*_UserSpecific` recordNames encode database, zone and owner: `Note_UserSpecific::<me>/Private::Notes::<owner>::<noteId>`, and the same for `Folder_UserSpecific` and `PasswordProtectedNote_UserSpecific`.
- All 137 seen are `Private::Notes::<me>`.
- For a shared-in note, expect `…/Shared::Notes::<otherOwner>::<noteId>` in the user's own **private** `Notes` zone **(unverified)**. Its `Folder` field would be where the user filed it locally, and `IsPinned`/`IsSystemPaper` would be the user's own flags.

## 3. Notes the user shared with others

- Shared-out content stays in the private `Notes` zone; the shared database is only for other people's zones.
- **Nothing currently shared:**
  - No record has a `share` key.
  - No `cloudkit.share` records exist.
  - No recordName starts with `cloudkit.`.
  - Every zone's `hierarchyShareInfoModel.hierarchyShareParticipants` is `[]`.
- **Past sharing leaves link markers.** 43 Notes have record-level `shortGUID` (25 chars), `displayedHostname` (`www.icloud.com`), and `stableUrl` with the keys `routingKey`, `shortTokenHash`, `protectedFullToken`, `encryptedPublicSharingKey`, `displayedHostname`.
  - Their field sets match those of other notes.
  - They are the iCloud note links, which persist after sharing stops.
  - **They do not mean "currently shared"**, so the app should ignore them for the shared badge.
- **Zone-level share flags.** `zones/list` (private) shows the zones `Notes`, `Metadata` and `_defaultZone`, each with `atomic`, `hierarchyVersion` (0), `isEligibleForHierarchicalShare`, `isEligibleForZoneShare` and `hierarchyShareInfoModel`. The `Metadata` zone wasn't read. `changes/database` lists only `Notes` and `Metadata`.
- **What a currently shared note or folder should look like (unverified):**
  - a `share: {recordName, zoneID}` on the root `Note`/`Folder`
  - a `cloudkit.share` record in `Notes`, holding participants and permissions
  - for a shared folder, a CloudKit hierarchical share: children are linked by `parent` chains, which is why notes, attachments and media all carry `parent`/`chainParentKey`
- The app's "shared" badge is `share` present on the record or an ancestor, in either database.

## 4. Side findings that affect the folder tree

- **`Note_UserSpecific` is sparse and partly stale.** There are 129 with a `Folder` field:
  - 19 belong to live notes, and 16 of those agree with `Note.Folder`.
  - 3 point to `TrashFolder-CloudKit` while their note is live in another folder.
  - The other 110 belong to tombstoned notes, or to notes no longer in the feed.

  For notes the user owns, `Note.Folder` is the source of truth. Use `*_UserSpecific` only for per-user flags (`IsPinned`, `IsSystemPaper`) and, later, for shared-in placement.
- **Recently Deleted held no live note** (`Note.Folder == TrashFolder-CloudKit` for 0 of 256) and `Note.Deleted` was always 0. Deleted notes showed up as tombstones (`deleted:true`, recordName only; 446 of them).
  - Whether a note in Recently Deleted gets `Folder = TrashFolder-CloudKit` or something else is unverified, because none existed at fetch time.
  - Suggested rule: a note is in Recently Deleted if `Note.Folder` is the Trash folder or `Deleted == 1`.
- `Folder_UserSpecific` (7, `TitleEncrypted` only) exists for the 3 system folders and 4 custom ones. It looks like a per-user title. For owned folders, use `Folder.TitleEncrypted`.

## Recommended folder-tree algorithm

1. Read `Folder` records from private `Notes` and from every shared zone, keyed by `(ownerRecordName, recordName)`.
2. Put the system folders in fixed places by recordName. Quick Notes is a virtual folder: notes whose `Note_UserSpecific.IsSystemPaper == 1`, not deleted.
3. Folder parent: `ParentFolder` ref / `parent` when present, otherwise top level **(nesting unverified)**. Cycle guard: treat a parent that's missing or creates a loop as top level.
4. Note placement:
   - Owned notes: `Note.Folder`.
   - Shared-in notes: the user's `Note_UserSpecific.Folder` if one exists, else the note's own folder if it's in the shared zone, else a "Shared with me" group.
5. Sort as in section 1. Add a shared badge where a `share` key is present (section 3).
6. Tombstones (`deleted:true`) remove a folder or note. A folder that disappears re-parents its notes to nothing, and they disappear with it. Spike 6 covers the sync side.

## Out of scope (user decision, 2026-10-25)
The user has no subfolders, won't share data, and doesn't need Recently Deleted checked, so these stay unverified on purpose:
- real nesting fields
- real shared-in and shared-out shapes
- how a note in Recently Deleted is marked

The app treats all folders as top level and can skip the shared database. The `*_UserSpecific` Shared prefix and the `Metadata` zone contents were also left unchecked.
