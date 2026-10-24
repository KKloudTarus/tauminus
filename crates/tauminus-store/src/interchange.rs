// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The saved lists in the files' own JSON, for sync to stamp and merge
//! field by field: one form, so a merged list is read back by the same
//! checks a saved one is.

use tauminus_domain::{Inventory, Library, Repair};

/// `inventory` as `hosts.json` holds it. O(H + G).
///
/// # Errors
/// Serialization failing, which it does not for a valid inventory.
pub fn hosts_to_json(inventory: &Inventory) -> Result<Vec<u8>, String> {
    crate::format::to_json(inventory).map_err(|e| e.to_string())
}

/// The inventory `hosts.json`'s JSON holds, repaired where a merge broke
/// it (see [`Inventory::repaired`]). O(G·d + H).
///
/// # Errors
/// A record that is not valid on its own, or more hosts or groups than
/// the limits.
pub fn hosts_from_json(bytes: &[u8]) -> Result<(Inventory, Vec<Repair>), String> {
    let (groups, hosts) = crate::format::parts(bytes)?;
    Inventory::repaired(groups, hosts).map_err(|e| e.to_string())
}

/// `library` as `snippets.json` holds it. O(total body size).
///
/// # Errors
/// Serialization failing, which it does not for a valid library.
pub fn snippets_to_json(library: &Library) -> Result<Vec<u8>, String> {
    crate::snippets::to_json(library).map_err(|e| e.to_string())
}

/// The library `snippets.json`'s JSON holds. O(total body size).
///
/// # Errors
/// A snippet that is not valid, or more than the limit.
pub fn snippets_from_json(bytes: &[u8]) -> Result<Library, String> {
    crate::snippets::from_json(bytes)
}

/// The newest `hosts.json` version this build reads and writes.
pub const HOSTS_VERSION: u32 = crate::format::VERSION;
/// The newest `snippets.json` version this build reads and writes.
pub const SNIPPETS_VERSION: u32 = crate::snippets::VERSION;

/// The file version a reader of [`hosts_to_json`] must know.
#[must_use]
pub fn hosts_version(bytes: &[u8]) -> Option<u64> {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()?
        .get("version")?
        .as_u64()
}

#[cfg(test)]
mod tests {
    use tauminus_domain::{Group, GroupId, Host, HostId, HostName, Label, Snippet, SnippetId};

    use super::*;

    fn group(id: &str, parent: Option<&str>) -> Group {
        Group {
            id: GroupId::parse(id).unwrap(),
            name: Label::parse(id).unwrap(),
            parent: parent.map(|p| GroupId::parse(p).unwrap()),
            settings: tauminus_domain::Settings::default(),
        }
    }

    #[test]
    fn the_lists_read_back_as_written() {
        let mut inventory = Inventory::default();
        inventory.put_group(group("g1", None)).unwrap();
        let mut host = Host::new(
            HostId::parse("h1").unwrap(),
            Label::parse("db").unwrap(),
            HostName::parse("db.internal").unwrap(),
        );
        host.group = Some(GroupId::parse("g1").unwrap());
        inventory.put_host(host).unwrap();
        let bytes = hosts_to_json(&inventory).unwrap();
        // The oldest version that holds what the list has.
        assert!(hosts_version(&bytes).is_some_and(|v| v <= u64::from(HOSTS_VERSION)));
        assert_eq!(hosts_from_json(&bytes).unwrap(), (inventory, vec![]));

        let mut library = Library::default();
        library
            .put(
                Snippet::new(
                    SnippetId::parse("s1").unwrap(),
                    Label::parse("deploy").unwrap(),
                    None,
                    "echo hi".into(),
                )
                .unwrap(),
            )
            .unwrap();
        let bytes = snippets_to_json(&library).unwrap();
        assert_eq!(snippets_from_json(&bytes).unwrap(), library);
    }

    #[test]
    fn a_cycle_two_merged_lists_made_is_repaired_and_said() {
        let mut inventory = Inventory::default();
        inventory.put_group(group("g1", None)).unwrap();
        inventory.put_group(group("g2", Some("g1"))).unwrap();
        let mut file: serde_json::Value =
            serde_json::from_slice(&hosts_to_json(&inventory).unwrap()).unwrap();
        // As if another device had moved g1 under g2 meanwhile.
        for g in file["groups"].as_array_mut().unwrap() {
            if g["id"] == "g1" {
                g["parent"] = serde_json::json!("g2");
            }
        }
        let (repaired, repairs) = hosts_from_json(&serde_json::to_vec(&file).unwrap()).unwrap();
        assert_eq!(
            repairs,
            [Repair::GroupToRoot(GroupId::parse("g1").unwrap())]
        );
        assert_eq!(repaired, inventory);
    }
}
