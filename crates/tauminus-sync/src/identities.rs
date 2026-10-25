// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared vault identities as the engine carries them, a JSON list like the
//! saved files', version 1. A key and its passphrase are one field, `key`,
//! so a merge never pairs one device's key with another's passphrase. The
//! public key is not carried: each device derives it.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tauminus_app::ports::{Secret, SyncedIdentity};
use zeroize::Zeroizing;

pub(crate) const VERSION: u32 = 1;
pub(crate) const KINDS: [(&str, &str); 1] = [("identities", "identity")];
/// Fields that hold a secret: a clash shows neither value.
pub(crate) const SECRET_FIELDS: [&str; 2] = ["password", "key"];

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Key {
    private_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    passphrase: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key: Option<Key>,
}

/// The list as JSON, in a buffer wiped when dropped. O(n).
pub(crate) fn to_json(identities: &[SyncedIdentity]) -> Result<Zeroizing<Vec<u8>>, String> {
    let records: Vec<Record> = identities
        .iter()
        .map(|i| Record {
            id: i.id.clone(),
            name: i.name.clone(),
            user: i.user.clone(),
            password: i.password.as_deref().cloned(),
            key: i.private_key.as_ref().map(|k| Key {
                private_key: k.as_str().to_owned(),
                passphrase: i.passphrase.as_deref().cloned(),
            }),
        })
        .collect();
    let mut file = Map::new();
    file.insert("version".into(), Value::from(VERSION));
    file.insert(
        "identities".into(),
        serde_json::to_value(&records).map_err(|e| e.to_string())?,
    );
    let bytes = serde_json::to_vec(&Value::Object(file)).map_err(|e| e.to_string());
    // The records held the secrets as plain strings; they go now.
    for mut record in records {
        zero(&mut record);
    }
    bytes.map(Zeroizing::new)
}

fn zero(record: &mut Record) {
    use zeroize::Zeroize as _;
    record.password.zeroize();
    if let Some(key) = &mut record.key {
        key.private_key.zeroize();
        key.passphrase.zeroize();
    }
}

/// The identities a merged doc's records hold, in id order, and why any
/// was left out. O(n log n).
pub(crate) fn from_lists(lists: &Map<String, Value>) -> (Vec<SyncedIdentity>, Vec<String>) {
    let mut out = Vec::new();
    let mut notices = Vec::new();
    for value in lists
        .get("identities")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match serde_json::from_value::<Record>(value.clone()) {
            // Ids reach the review list and the vault: one no vault makes
            // is left out.
            Ok(mut record) if !tauminus_app::trust::identity_id_valid(&record.id) => {
                zero(&mut record);
                notices.push("an identity from another device has an id no vault makes".into());
            }
            Ok(mut record) => {
                out.push(SyncedIdentity {
                    id: record.id.clone(),
                    name: record.name.clone(),
                    user: record.user.clone(),
                    password: record
                        .password
                        .as_deref()
                        .map(|p| Secret::new(p.to_owned())),
                    private_key: record
                        .key
                        .as_ref()
                        .map(|k| Secret::new(k.private_key.clone())),
                    passphrase: record
                        .key
                        .as_ref()
                        .and_then(|k| k.passphrase.as_deref())
                        .map(|p| Secret::new(p.to_owned())),
                });
                zero(&mut record);
            }
            Err(_) => notices.push("an identity from another device could not be read".into()),
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    (out, notices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flat::{flatten, unflatten};

    fn identity(id: &str) -> SyncedIdentity {
        SyncedIdentity {
            id: id.into(),
            name: format!("{id} name"),
            user: Some("ops".into()),
            password: Some(Secret::new("pw".into())),
            private_key: Some(Secret::new("KEY".into())),
            passphrase: Some(Secret::new("pass".into())),
        }
    }

    #[test]
    fn identities_round_trip_with_the_key_one_field() {
        let list = vec![identity("a"), identity("b")];
        let json = to_json(&list).unwrap();
        let file: Value = serde_json::from_slice(&json).unwrap();
        let flat = flatten(&file, &KINDS);
        let record = &flat["identity:a"];
        assert_eq!(
            record["key"],
            serde_json::json!({"privateKey": "KEY", "passphrase": "pass"})
        );
        assert_eq!(record["password"], Value::from("pw"));
        let (back, notices) = from_lists(&unflatten(&flat, &KINDS));
        assert_eq!(notices, Vec::<String>::new());
        assert_eq!(back, list);
    }

    #[test]
    fn a_record_that_cannot_be_read_is_left_out_with_a_word() {
        let mut lists = Map::new();
        lists.insert(
            "identities".into(),
            serde_json::json!([
                {"id": "a", "name": "a"},
                {"id": "b", "name": 7},
                {"id": "c"},
                {"id": "a:b", "name": "x"},
                {"id": "x".repeat(65), "name": "x"}
            ]),
        );
        let (back, notices) = from_lists(&lists);
        assert_eq!(back.len(), 1);
        assert_eq!(notices.len(), 4);
    }
}
