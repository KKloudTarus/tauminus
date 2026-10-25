// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The synced state as records of stamped fields, and its merge: a
//! state-based CRDT. Per field, the value with the highest stamp wins. A
//! delete wins over any edit: a deleted record keeps only its tombstone,
//! no values, and record ids are random, so none comes back. Merging is
//! commutative, associative and idempotent, so devices that have seen the
//! same objects hold the same state.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::stamp::{Clock, Stamp};

/// One field's value and when it got it. `Null` is a field taken away.
#[derive(Clone, PartialEq, Eq)]
pub struct Cell {
    pub at: Stamp,
    pub value: Value,
}

/// Without the value: an identity's holds its password or key.
impl std::fmt::Debug for Cell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cell")
            .field("at", &self.at)
            .finish_non_exhaustive()
    }
}

/// A record's fields, or, deleted, when it was: then no fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    pub fields: BTreeMap<String, Cell>,
    pub gone: Option<Stamp>,
}

impl Record {
    /// Whether the record exists: never deleted.
    #[must_use]
    pub const fn alive(&self) -> bool {
        self.gone.is_none()
    }

    /// Its fields' values, without those taken away.
    #[must_use]
    pub fn values(&self) -> BTreeMap<String, Value> {
        self.fields
            .iter()
            .filter(|(_, c)| !c.value.is_null())
            .map(|(k, c)| (k.clone(), c.value.clone()))
            .collect()
    }

    /// Deletes it at `at`: the values go with it.
    fn delete(&mut self, at: Stamp) {
        self.fields.clear();
        if self.gone.as_ref().is_none_or(|g| at > *g) {
            self.gone = Some(at);
        }
    }
}

/// Every record, by `kind:id`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Doc {
    pub records: BTreeMap<String, Record>,
}

impl Doc {
    /// `self` joined with `other`. O(n log n) for n fields.
    pub fn merge(&mut self, other: &Self) {
        for (key, theirs) in &other.records {
            let ours = match self.records.get_mut(key) {
                Some(ours) => ours,
                None => self.records.entry(key.clone()).or_default(),
            };
            if let Some(gone) = theirs.gone.as_ref().or(ours.gone.as_ref()).cloned() {
                let gone = match (&ours.gone, &theirs.gone) {
                    (Some(a), Some(b)) => a.max(b).clone(),
                    _ => gone,
                };
                ours.delete(gone);
                continue;
            }
            for (field, cell) in &theirs.fields {
                match ours.fields.get(field) {
                    Some(mine) if mine.at >= cell.at => {}
                    _ => {
                        ours.fields.insert(field.clone(), cell.clone());
                    }
                }
            }
        }
    }

    /// Drops tombstones older than `keep` milliseconds before `now`. A
    /// device offline longer than that, still holding the record, may
    /// bring it back. O(n).
    pub fn expire(&mut self, now: u64, keep: u64) {
        self.records.retain(|_, r| {
            r.gone
                .as_ref()
                .is_none_or(|g| now.saturating_sub(g.ms) < keep)
        });
    }

    /// The highest stamp anywhere in it.
    #[must_use]
    pub fn latest(&self) -> Option<&Stamp> {
        self.records
            .values()
            .flat_map(|r| r.fields.values().map(|c| &c.at).chain(r.gone.iter()))
            .max()
    }
}

/// A stamp on the wire: `[ms, n, device]`, the device an index into the
/// doc's device table, which says each 32-digit id once.
type WireStamp = (u64, u32, u32);

#[derive(Serialize)]
struct WireOut<'a> {
    devices: Vec<&'a str>,
    records: BTreeMap<&'a str, WireRecordOut<'a>>,
}

#[derive(Serialize)]
struct WireRecordOut<'a>(
    Option<WireStamp>,
    BTreeMap<&'a str, (u64, u32, u32, &'a Value)>,
);

#[derive(Deserialize)]
struct WireIn {
    devices: Vec<String>,
    records: BTreeMap<String, WireRecordIn>,
}

#[derive(Deserialize)]
struct WireRecordIn(Option<WireStamp>, BTreeMap<String, (u64, u32, u32, Value)>);

impl Serialize for Doc {
    /// About 25 bytes of stamp per field, where a stamp spelled out takes
    /// about 90: the device id is most of it. O(n log n).
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut index: BTreeMap<&str, u32> = BTreeMap::new();
        let mut devices = Vec::new();
        for record in self.records.values() {
            for stamp in record
                .fields
                .values()
                .map(|c| &c.at)
                .chain(record.gone.iter())
            {
                if !index.contains_key(stamp.device.as_str()) {
                    let next = u32::try_from(devices.len()).map_err(serde::ser::Error::custom)?;
                    index.insert(&stamp.device, next);
                    devices.push(stamp.device.as_str());
                }
            }
        }
        let at = |s: &Stamp| (s.ms, s.n, index[s.device.as_str()]);
        let records = self
            .records
            .iter()
            .map(|(key, record)| {
                let fields = record
                    .fields
                    .iter()
                    .map(|(field, cell)| {
                        let (ms, n, device) = at(&cell.at);
                        (field.as_str(), (ms, n, device, &cell.value))
                    })
                    .collect();
                (
                    key.as_str(),
                    WireRecordOut(record.gone.as_ref().map(at), fields),
                )
            })
            .collect();
        WireOut { devices, records }.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Doc {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = WireIn::deserialize(deserializer)?;
        let stamp = |(ms, n, device): WireStamp| -> Result<Stamp, D::Error> {
            let device = usize::try_from(device)
                .ok()
                .and_then(|i| wire.devices.get(i))
                .ok_or_else(|| serde::de::Error::custom("a stamp names no device"))?;
            Ok(Stamp {
                ms,
                n,
                device: device.clone(),
            })
        };
        let mut records = BTreeMap::new();
        for (key, WireRecordIn(gone, fields)) in wire.records {
            let record = match gone {
                // A tombstone holds no values, whatever the object says.
                Some(gone) => Record {
                    fields: BTreeMap::new(),
                    gone: Some(stamp(gone)?),
                },
                None => Record {
                    fields: fields
                        .into_iter()
                        .map(|(field, (ms, n, device, value))| {
                            Ok((
                                field,
                                Cell {
                                    at: stamp((ms, n, device))?,
                                    value,
                                },
                            ))
                        })
                        .collect::<Result<_, D::Error>>()?,
                    gone: None,
                },
            };
            records.insert(key, record);
        }
        Ok(Self { records })
    }
}
/// A field this device and another both changed since the last sync: the
/// value kept, and the one that lost, with the device whose it was. A
/// record deleted on one and edited on the other is a clash of field
/// [`DELETED`], its values `"deleted"` and `"edited"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clash {
    pub record: String,
    pub field: String,
    pub kept: Value,
    pub lost: Value,
    pub loser: String,
}

/// The field a delete clashes on.
pub const DELETED: &str = "(deleted)";

/// The current records, stamped against `base`: a field whose value is
/// unchanged keeps its stamp, a changed or new one gets a new stamp, a
/// field gone gets a `Null` with one, and a record gone a tombstone.
/// Returns the stamped doc and the fields changed here. O(n log n).
pub fn stamp(
    base: &Doc,
    current: &BTreeMap<String, BTreeMap<String, Value>>,
    clock: &mut Clock,
    now_ms: u64,
    tombstones: bool,
) -> (Doc, BTreeSet<(String, String)>) {
    stamp_with(base.clone(), None, current, clock, now_ms, tombstones)
}

/// [`stamp`], where a value that `staged` holds keeps `staged`'s stamp:
/// a merge saved here before its base was, so the values it brought are
/// the other devices', not edits of this one's.
pub fn stamp_with(
    base: Doc,
    staged: Option<&Doc>,
    current: &BTreeMap<String, BTreeMap<String, Value>>,
    clock: &mut Clock,
    now_ms: u64,
    tombstones: bool,
) -> (Doc, BTreeSet<(String, String)>) {
    let staged_cell = |key: &str, field: &str, value: &Value| {
        staged
            .and_then(|d| d.records.get(key))
            .filter(|r| r.alive())
            .and_then(|r| r.fields.get(field))
            .filter(|c| c.value == *value)
            .cloned()
    };
    let mut doc = base;
    let mut changed = BTreeSet::new();
    for (key, values) in current {
        let record = doc.records.entry(key.clone()).or_default();
        if !record.alive() {
            // Deleted elsewhere: the delete wins, and an edit made here is
            // a clash.
            changed.extend(values.keys().map(|f| (key.clone(), f.clone())));
            continue;
        }
        for (field, value) in values {
            let same = record.fields.get(field).is_some_and(|c| c.value == *value);
            if !same && let Some(cell) = staged_cell(key, field, value) {
                record.fields.insert(field.clone(), cell);
            } else if !same {
                record.fields.insert(
                    field.clone(),
                    Cell {
                        at: clock.tick(now_ms),
                        value: value.clone(),
                    },
                );
                changed.insert((key.clone(), field.clone()));
            }
        }
        let gone: Vec<String> = record
            .fields
            .iter()
            .filter(|(f, c)| !values.contains_key(*f) && !c.value.is_null())
            .map(|(f, _)| f.clone())
            .collect();
        for field in gone {
            if let Some(cell) = staged_cell(key, &field, &Value::Null) {
                record.fields.insert(field, cell);
                continue;
            }
            record.fields.insert(
                field.clone(),
                Cell {
                    at: clock.tick(now_ms),
                    value: Value::Null,
                },
            );
            changed.insert((key.clone(), field));
        }
    }
    for (key, record) in &mut doc.records {
        if tombstones && !current.contains_key(key) && record.alive() {
            let staged_gone = staged
                .and_then(|d| d.records.get(key))
                .and_then(|r| r.gone.clone());
            if let Some(gone) = staged_gone {
                record.delete(gone);
                continue;
            }
            record.delete(clock.tick(now_ms));
            changed.insert((key.clone(), String::new()));
        }
    }
    (doc, changed)
}

/// The newest stamp of a record's fields.
fn newest(record: &Record) -> Option<&Stamp> {
    record.fields.values().map(|c| &c.at).max()
}

/// What this device (`me`) changed (`changed`) that another device also
/// changed since `base`: a field set to two values, or a record deleted on
/// one side and edited on the other, where the delete wins. Each says what
/// `merged` kept, what lost, and whose it was. `remote` is the other
/// devices' state joined. O(c log n) for c changed fields.
#[must_use]
pub fn clashes(
    me: &str,
    base: &Doc,
    local: &Doc,
    remote: &Doc,
    merged: &Doc,
    changed: &BTreeSet<(String, String)>,
) -> Vec<Clash> {
    let mut found = Vec::new();
    let mut deletes = BTreeSet::new();
    let delete_clash = |key: &String, loser: &str| Clash {
        record: key.clone(),
        field: DELETED.to_owned(),
        kept: Value::from("deleted"),
        lost: Value::from("edited"),
        loser: loser.to_owned(),
    };
    for (key, field) in changed {
        let since = base.records.get(key);
        let Some(kept_record) = merged.records.get(key) else {
            continue;
        };
        if !kept_record.alive() {
            let deleted_here = local.records.get(key).is_some_and(|r| !r.alive())
                && since.is_none_or(Record::alive);
            if deleted_here {
                // Deleted here; edited on another device since the last
                // sync. That device's edit lost.
                let editor = remote
                    .records
                    .get(key)
                    .filter(|r| r.alive())
                    .and_then(newest)
                    .filter(|at| Some(*at) > since.and_then(newest));
                if let Some(editor) = editor
                    && deletes.insert(key.clone())
                {
                    found.push(delete_clash(key, &editor.device));
                }
            } else if !field.is_empty() && deletes.insert(key.clone()) {
                // Edited here, deleted elsewhere.
                found.push(delete_clash(key, me));
            }
            continue;
        }
        let cell = |doc: &Doc| {
            doc.records
                .get(key)
                .and_then(|r| r.fields.get(field))
                .cloned()
        };
        let (Some(mine), Some(theirs), Some(kept)) = (cell(local), cell(remote), cell(merged))
        else {
            continue;
        };
        let since = cell(base).map(|c| c.at);
        if since.as_ref() >= Some(&theirs.at) || theirs.value == mine.value {
            continue;
        }
        let (lost, loser) = if kept.at == mine.at {
            (theirs.value, theirs.at.device)
        } else {
            (mine.value, mine.at.device)
        };
        found.push(Clash {
            record: key.clone(),
            field: field.clone(),
            kept: kept.value,
            lost,
            loser,
        });
    }
    found
}

#[cfg(test)]
mod tests;
