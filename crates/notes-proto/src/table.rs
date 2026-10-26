//! Table decoding (WP-B): `Attachment.MergeableDataEncrypted` → `MergableDataProto` → [`Table`].
//!
//! Port of `AppleNotesEmbeddedTable.rb` (apple_cloud_notes_parser). Rows and columns are
//! identified by an index into `mergeable_data_object_uuid_item`; `crRows`/`crColumns` give
//! the display order (`ordering.array.attachment`) plus aliases (`ordering.contents`) that map
//! other UUID indices onto an ordered one. `cellColumns[col][row]` points at a `Note`.

use std::collections::HashMap;
use std::io::Read;

use flate2::read::{MultiGzDecoder, ZlibDecoder};
use notes_model::DecodeError;
use prost::Message;

use crate::proto;

/// A decoded table in display order: `rows[row][col]`. `None` is an empty cell.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Table {
    pub rows: Vec<Vec<Option<proto::Note>>>,
}

type Entry = proto::MergeableDataObjectEntry;

fn err(msg: impl Into<String>) -> DecodeError {
    DecodeError::Table(msg.into())
}

/// gzip if `1f 8b`, zlib if a valid zlib header, otherwise raw.
fn inflate(raw: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::new();
    if raw.starts_with(&[0x1f, 0x8b]) {
        MultiGzDecoder::new(raw)
            .read_to_end(&mut out)
            .map_err(|e| DecodeError::Inflate(e.to_string()))?;
    } else if raw.len() >= 2
        && raw[0] & 0x0f == 8
        && (u16::from(raw[0]) << 8 | u16::from(raw[1])) % 31 == 0
    {
        ZlibDecoder::new(raw)
            .read_to_end(&mut out)
            .map_err(|e| DecodeError::Inflate(e.to_string()))?;
    } else {
        return Ok(raw.to_vec());
    }
    Ok(out)
}

/// The UUID index a row/column object stands for: `custom_map.map_entry[0].value.unsigned_integer_value`.
fn target_uuid(entries: &[Entry], object_index: i32) -> Result<u64, DecodeError> {
    let e = usize::try_from(object_index)
        .ok()
        .and_then(|i| entries.get(i))
        .ok_or_else(|| err("object_index out of range"))?;
    let first = e
        .custom_map
        .as_ref()
        .and_then(|m| m.map_entry.first())
        .ok_or_else(|| err("row/column object is not a custom_map"))?;
    Ok(first.value.unsigned_integer_value)
}

/// Resolve an ordered set to `uuid index → display position`, plus the number of positions.
fn order_of(
    entries: &[Entry],
    uuids: &[Vec<u8>],
    set: &proto::OrderedSet,
) -> Result<(HashMap<u64, usize>, usize), DecodeError> {
    let mut pos: HashMap<u64, usize> = HashMap::new();
    let att = &set.ordering.array.attachment;
    for (n, a) in att.iter().enumerate() {
        let idx = uuids
            .iter()
            .position(|u| *u == a.uuid)
            .ok_or_else(|| err("ordering uuid not in uuid_item"))?;
        pos.insert(idx as u64, n);
    }
    for el in &set.ordering.contents.element {
        let key = target_uuid(entries, el.key.object_index)?;
        let value = target_uuid(entries, el.value.object_index)?;
        if let Some(&p) = pos.get(&key) {
            pos.insert(value, p);
        }
    }
    Ok((pos, att.len()))
}

fn dict_at(entries: &[Entry], object_index: i32) -> Option<&proto::Dictionary> {
    usize::try_from(object_index)
        .ok()
        .and_then(|i| entries.get(i))?
        .dictionary
        .as_ref()
}

fn ordered_set_at(entries: &[Entry], object_index: i32) -> Result<&proto::OrderedSet, DecodeError> {
    usize::try_from(object_index)
        .ok()
        .and_then(|i| entries.get(i))
        .and_then(|e| e.ordered_set.as_ref())
        .ok_or_else(|| err("crRows/crColumns is not an ordered_set"))
}

/// Decode a table attachment's `MergeableDataEncrypted`, **already base64-decoded**
/// (use `notes_model::bytes_b64`). The bytes are gzip, zlib or raw `MergableDataProto`.
/// Row and column order is resolved through `crRows`/`crColumns` and the UUID index.
pub fn decode_table(data: &[u8]) -> Result<Table, DecodeError> {
    let inflated = inflate(data)?;
    let m = proto::MergableDataProto::decode(inflated.as_slice())
        .map_err(|e| DecodeError::Protobuf(e.to_string()))?;
    let d = &m.mergable_data_object.mergeable_data_object_data;
    let entries = &d.mergeable_data_object_entry;
    let ty = |i: i32| {
        usize::try_from(i)
            .ok()
            .and_then(|i| d.mergeable_data_object_type_item.get(i))
            .map(String::as_str)
    };
    let key = |i: i32| {
        usize::try_from(i)
            .ok()
            .and_then(|i| d.mergeable_data_object_key_item.get(i))
            .map(String::as_str)
    };

    let root = entries
        .iter()
        .find(|e| {
            e.custom_map
                .as_ref()
                .is_some_and(|c| ty(c.r#type) == Some("com.apple.notes.ICTable"))
        })
        .and_then(|e| e.custom_map.as_ref())
        .ok_or_else(|| err("no com.apple.notes.ICTable root"))?;

    let (mut rows, mut cols, mut cells) = (None, None, None);
    for me in &root.map_entry {
        let oi = me.value.object_index;
        match key(me.key) {
            Some("crRows") => {
                rows = Some(order_of(
                    entries,
                    &d.mergeable_data_object_uuid_item,
                    ordered_set_at(entries, oi)?,
                )?)
            }
            Some("crColumns") => {
                cols = Some(order_of(
                    entries,
                    &d.mergeable_data_object_uuid_item,
                    ordered_set_at(entries, oi)?,
                )?)
            }
            Some("cellColumns") => cells = dict_at(entries, oi),
            _ => {}
        }
    }
    let ((row_pos, n_rows), (col_pos, n_cols)) = rows
        .zip(cols)
        .ok_or_else(|| err("missing crRows or crColumns"))?;

    let mut out: Vec<Vec<Option<proto::Note>>> = vec![vec![None; n_cols]; n_rows];
    for col in cells.map(|c| c.element.as_slice()).unwrap_or_default() {
        let c = target_uuid(entries, col.key.object_index)?;
        let Some(&ci) = col_pos.get(&c) else { continue };
        let Some(col_rows) = dict_at(entries, col.value.object_index) else {
            continue;
        };
        for cell in &col_rows.element {
            let r = target_uuid(entries, cell.key.object_index)?;
            let Some(&ri) = row_pos.get(&r) else { continue };
            let note = usize::try_from(cell.value.object_index)
                .ok()
                .and_then(|i| entries.get(i))
                .and_then(|e| e.note.clone());
            out[ri][ci] = note;
        }
    }

    // The reference reverses each row for right-to-left tables. The direction is recorded as the
    // string_value of the first map entry of a custom_map keyed (crTableColumnDirection + 1).
    let dir_key = d
        .mergeable_data_object_key_item
        .iter()
        .position(|k| k == "crTableColumnDirection")
        .map(|i| i as i32 + 1);
    let rtl = dir_key.is_some_and(|dk| {
        entries.iter().any(|e| {
            e.custom_map
                .as_ref()
                .and_then(|c| c.map_entry.first())
                .is_some_and(|f| {
                    f.key == dk && f.value.string_value == "CRTableColumnDirectionRightToLeft"
                })
        })
    });
    if rtl {
        out.iter_mut().for_each(|r| r.reverse());
    }
    Ok(Table { rows: out })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::{Compression, write::GzEncoder};

    use super::*;

    fn oid(idx: i32) -> proto::ObjectId {
        proto::ObjectId {
            object_index: idx,
            ..Default::default()
        }
    }
    fn uid(u: u64) -> proto::ObjectId {
        proto::ObjectId {
            unsigned_integer_value: u,
            ..Default::default()
        }
    }
    fn entry() -> Entry {
        Entry {
            register_latest: proto::RegisterLatest { contents: oid(0) },
            ..Default::default()
        }
    }
    fn id_obj(uuid_idx: u64) -> Entry {
        Entry {
            custom_map: Some(proto::MergeableDataObjectMap {
                r#type: 1,
                map_entry: vec![proto::MapEntry {
                    key: 3,
                    value: uid(uuid_idx),
                }],
            }),
            ..entry()
        }
    }
    fn note(text: &str, bold: bool) -> proto::Note {
        proto::Note {
            note_text: text.into(),
            attribute_run: vec![proto::AttributeRun {
                length: text.encode_utf16().count() as i32,
                font_weight: bold.then_some(1),
                ..Default::default()
            }],
        }
    }
    fn ordered(uuids: &[&[u8]]) -> Entry {
        Entry {
            ordered_set: Some(proto::OrderedSet {
                ordering: proto::OrderedSetOrdering {
                    array: proto::OrderedSetOrderingArray {
                        contents: Default::default(),
                        attachment: uuids
                            .iter()
                            .enumerate()
                            .map(|(i, u)| proto::OrderedSetOrderingArrayAttachment {
                                index: i as i32,
                                uuid: u.to_vec(),
                            })
                            .collect(),
                    },
                    contents: Default::default(),
                },
                elements: Default::default(),
            }),
            ..entry()
        }
    }
    fn dict(pairs: &[(i32, i32)]) -> Entry {
        Entry {
            dictionary: Some(proto::Dictionary {
                element: pairs
                    .iter()
                    .map(|&(k, v)| proto::DictionaryElement {
                        key: oid(k),
                        value: oid(v),
                    })
                    .collect(),
            }),
            ..entry()
        }
    }

    /// 2 columns × 3 rows. UUID items (shuffled vs. display order): 0=r_b 1=c_y 2=r_c 3=c_x 4=r_a.
    /// Display rows: a, b, c. Display columns: x, y. Cells: (a,x) plain, (a,y) bold, (b,y), (c,x); (b,x),(c,y) empty.
    fn build(direction: Option<&str>) -> Vec<u8> {
        let uuids: Vec<Vec<u8>> = vec![
            b"r_b".to_vec(),
            b"c_y".to_vec(),
            b"r_c".to_vec(),
            b"c_x".to_vec(),
            b"r_a".to_vec(),
        ];
        let entries = vec![
            // 0: root
            Entry {
                custom_map: Some(proto::MergeableDataObjectMap {
                    r#type: 0,
                    map_entry: vec![
                        proto::MapEntry {
                            key: 0,
                            value: oid(1),
                        },
                        proto::MapEntry {
                            key: 1,
                            value: oid(2),
                        },
                        proto::MapEntry {
                            key: 2,
                            value: oid(3),
                        },
                    ],
                }),
                ..entry()
            },
            ordered(&[b"r_a", b"r_b", b"r_c"]), // 1: crRows
            ordered(&[b"c_x", b"c_y"]),         // 2: crColumns
            dict(&[(5, 7), (6, 8)]),            // 3: cellColumns: col x -> 7, col y -> 8
            id_obj(0),                          // 4: row b
            id_obj(3),                          // 5: col x
            id_obj(1),                          // 6: col y
            dict(&[(9, 11), (10, 12)]),         // 7: x column: row a -> 11, row c -> 12
            dict(&[(9, 13), (4, 14)]),          // 8: y column: row a -> 13, row b -> 14
            id_obj(4),                          // 9: row a
            id_obj(2),                          // 10: row c
            Entry {
                note: Some(note("a-x", false)),
                ..entry()
            },
            Entry {
                note: Some(note("c-x", false)),
                ..entry()
            },
            Entry {
                note: Some(note("a-y", true)),
                ..entry()
            },
            Entry {
                note: Some(note("b-y", false)),
                ..entry()
            },
            // 15: direction holder
            Entry {
                custom_map: Some(proto::MergeableDataObjectMap {
                    r#type: 1,
                    map_entry: vec![proto::MapEntry {
                        key: 5,
                        value: proto::ObjectId {
                            string_value: direction.unwrap_or("").into(),
                            ..Default::default()
                        },
                    }],
                }),
                ..entry()
            },
        ];
        let m = proto::MergableDataProto {
            mergable_data_object: proto::MergableDataObject {
                version: 0,
                mergeable_data_object_data: proto::MergeableDataObjectData {
                    mergeable_data_object_entry: entries,
                    mergeable_data_object_key_item: [
                        "crRows",
                        "crColumns",
                        "cellColumns",
                        "identity",
                        "crTableColumnDirection",
                        "self",
                    ]
                    .map(String::from)
                    .to_vec(),
                    mergeable_data_object_type_item: [
                        "com.apple.notes.ICTable",
                        "com.apple.CRDT.NSUUID",
                    ]
                    .map(String::from)
                    .to_vec(),
                    mergeable_data_object_uuid_item: uuids,
                },
            },
        };
        m.encode_to_vec()
    }

    fn texts(t: &Table) -> Vec<Vec<Option<&str>>> {
        t.rows
            .iter()
            .map(|r| {
                r.iter()
                    .map(|c| c.as_ref().map(|n| n.note_text.as_str()))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn synthetic_2x3_raw_gzip_zlib() {
        let raw = build(None);
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&raw).unwrap();
        let gz = gz.finish().unwrap();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(&raw).unwrap();
        let z = z.finish().unwrap();
        for bytes in [raw, gz, z] {
            let t = decode_table(&bytes).unwrap();
            assert_eq!(
                texts(&t),
                vec![
                    vec![Some("a-x"), Some("a-y")],
                    vec![None, Some("b-y")],
                    vec![Some("c-x"), None]
                ]
            );
            assert_eq!(
                t.rows[0][1].as_ref().unwrap().attribute_run[0].font_weight,
                Some(1)
            );
        }
    }

    #[test]
    fn right_to_left_reverses_rows() {
        let t = decode_table(&build(Some("CRTableColumnDirectionRightToLeft"))).unwrap();
        assert_eq!(texts(&t)[0], vec![Some("a-y"), Some("a-x")]);
    }

    #[test]
    fn alias_in_ordering_contents_resolves() {
        // Add alias: uuid index 9 (not in the array) maps onto row "r_a" (uuid index 4) via contents.
        let mut m = proto::MergableDataProto::decode(build(None).as_slice()).unwrap();
        let d = &mut m.mergable_data_object.mergeable_data_object_data;
        d.mergeable_data_object_entry.push(id_obj(4)); // 16 (key side)
        d.mergeable_data_object_entry.push(id_obj(9)); // 17 (alias side)
        d.mergeable_data_object_entry[1]
            .ordered_set
            .as_mut()
            .unwrap()
            .ordering
            .contents
            .element = vec![proto::DictionaryElement {
            key: oid(16),
            value: oid(17),
        }];
        // Point cell (a,x) at the alias row instead.
        d.mergeable_data_object_entry[7]
            .dictionary
            .as_mut()
            .unwrap()
            .element[0]
            .key = oid(17);
        let t = decode_table(&m.encode_to_vec()).unwrap();
        assert_eq!(texts(&t)[0][0], Some("a-x"));
    }

    #[test]
    fn errors() {
        assert!(matches!(
            decode_table(&[0xff, 0xff, 0xff]),
            Err(DecodeError::Protobuf(_))
        ));
        assert!(matches!(
            decode_table(&[0x1f, 0x8b, 0, 0]),
            Err(DecodeError::Inflate(_))
        ));
        let empty = proto::MergableDataProto {
            mergable_data_object: proto::MergableDataObject {
                version: 0,
                mergeable_data_object_data: Default::default(),
            },
        };
        assert!(matches!(
            decode_table(&empty.encode_to_vec()),
            Err(DecodeError::Table(_))
        ));
    }
}
