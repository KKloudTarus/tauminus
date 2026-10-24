// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The saved files' records as flat fields, so each merges on its own: a
//! group's or host's `settings` as `settings.<name>`, its `tags` one field
//! each (`tags/<tag>`), its environment variables by name
//! (`settings.env/<name>`), and a host's forwarding rules by id
//! (`forwards/<id>`, with the rule's place in the list). Any other list is
//! one field. Back again, the records are what the files hold.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

/// Records by `kind:id`, each as flat fields.
pub type Flat = BTreeMap<String, BTreeMap<String, Value>>;

const ENV: &str = "env";

/// The records of a `hosts.json` (`groups`, `hosts`) or `snippets.json`
/// (`snippets`) document, flat. O(n).
#[must_use]
pub fn flatten(file: &Value, kinds: &[(&str, &str)]) -> Flat {
    let mut flat = Flat::new();
    for (list, kind) in kinds {
        for record in file
            .get(*list)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(object) = record.as_object() else {
                continue;
            };
            let Some(id) = object.get("id").and_then(Value::as_str) else {
                continue;
            };
            let mut fields = BTreeMap::new();
            for (name, value) in object {
                match (name.as_str(), value) {
                    ("id", _) => {}
                    ("settings", Value::Object(settings)) => {
                        for (key, value) in settings {
                            if key == ENV {
                                for var in value.as_array().into_iter().flatten() {
                                    if let (Some(n), Some(v)) = (var.get("name"), var.get("value"))
                                        && let Some(n) = n.as_str()
                                    {
                                        fields.insert(format!("settings.{ENV}/{n}"), v.clone());
                                    }
                                }
                            } else {
                                fields.insert(format!("settings.{key}"), value.clone());
                            }
                        }
                    }
                    ("tags", Value::Array(tags)) => {
                        for tag in tags.iter().filter_map(Value::as_str) {
                            fields.insert(format!("tags/{tag}"), Value::Bool(true));
                        }
                    }
                    ("forwards", Value::Array(rules)) => {
                        for (at, rule) in rules.iter().enumerate() {
                            if let Some(rule_id) = rule.get("id").and_then(Value::as_str) {
                                fields.insert(
                                    format!("forwards/{rule_id}"),
                                    json!({ "at": at, "rule": rule }),
                                );
                            }
                        }
                    }
                    (name, value) => {
                        fields.insert(name.to_owned(), value.clone());
                    }
                }
            }
            flat.insert(format!("{kind}:{id}"), fields);
        }
    }
    flat
}

/// The lists of a document from flat records: `kinds` names each list
/// and the record kind it holds. Records come out in id order; tags and
/// variables sorted, rules in their place. O(n log n).
#[must_use]
pub fn unflatten(flat: &Flat, kinds: &[(&str, &str)]) -> Map<String, Value> {
    let mut lists: Map<String, Value> = Map::new();
    for (list, kind) in kinds {
        let prefix = format!("{kind}:");
        let mut records = Vec::new();
        for (key, fields) in flat.range(prefix.clone()..) {
            let Some(id) = key.strip_prefix(&prefix) else {
                break;
            };
            records.push(record(id, fields));
        }
        lists.insert((*list).to_owned(), Value::Array(records));
    }
    lists
}

fn record(id: &str, fields: &BTreeMap<String, Value>) -> Value {
    let mut object = Map::new();
    object.insert("id".into(), Value::String(id.to_owned()));
    let mut settings = Map::new();
    let mut env: Vec<(String, Value)> = Vec::new();
    let mut tags: Vec<String> = Vec::new();
    let mut rules: Vec<(u64, String, Value)> = Vec::new();
    for (name, value) in fields {
        if let Some(var) = name.strip_prefix(&format!("settings.{ENV}/")) {
            env.push((var.to_owned(), value.clone()));
        } else if let Some(key) = name.strip_prefix("settings.") {
            settings.insert(key.to_owned(), value.clone());
        } else if let Some(tag) = name.strip_prefix("tags/") {
            if value == &Value::Bool(true) {
                tags.push(tag.to_owned());
            }
        } else if let Some(rule_id) = name.strip_prefix("forwards/") {
            let at = value.get("at").and_then(Value::as_u64).unwrap_or(u64::MAX);
            if let Some(rule) = value.get("rule") {
                rules.push((at, rule_id.to_owned(), rule.clone()));
            }
        } else if name != "id" {
            // The record's key names it; a field cannot.
            object.insert(name.clone(), value.clone());
        }
    }
    if !env.is_empty() {
        settings.insert(
            ENV.into(),
            Value::Array(
                env.into_iter()
                    .map(|(name, value)| json!({ "name": name, "value": value }))
                    .collect(),
            ),
        );
    }
    if !settings.is_empty() {
        object.insert("settings".into(), Value::Object(settings));
    }
    if !tags.is_empty() {
        object.insert(
            "tags".into(),
            Value::Array(tags.into_iter().map(Value::String).collect()),
        );
    }
    if !rules.is_empty() {
        rules.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        object.insert(
            "forwards".into(),
            Value::Array(rules.into_iter().map(|(_, _, rule)| rule).collect()),
        );
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [(&str, &str); 2] = [("groups", "group"), ("hosts", "host")];

    #[test]
    fn a_file_round_trips_through_flat_fields() {
        let file = json!({
            "version": 11,
            "groups": [{"id": "g1", "name": "prod", "settings": {"user": "ops"}}],
            "hosts": [{
                "id": "h1",
                "label": "db",
                "address": "db.internal",
                "group": "g1",
                "tags": ["eu", "prod"],
                "settings": {
                    "port": 2222,
                    "env": [{"name": "A", "value": "1"}, {"name": "B", "value": "2"}],
                    "jumps": ["bastion"]
                },
                "forwards": [
                    {"id": "r2", "label": "web", "kind": "local", "bind": "loopback", "port": 8080},
                    {"id": "r1", "label": "db", "kind": "local", "bind": "loopback", "port": 5432}
                ]
            }]
        });
        let flat = flatten(&file, &KINDS);
        let host = &flat["host:h1"];
        assert_eq!(host["tags/eu"], Value::Bool(true));
        assert_eq!(host["settings.env/B"], json!("2"));
        assert_eq!(host["settings.jumps"], json!(["bastion"]));
        assert!(host.contains_key("forwards/r1"));
        let back = unflatten(&flat, &KINDS);
        assert_eq!(back["groups"], file["groups"]);
        assert_eq!(back["hosts"], file["hosts"], "rules keep their order");
    }
}
