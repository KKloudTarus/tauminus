// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The sync review list, version 1: each host another device changed,
//! with the fingerprint of what this device approved for it, or `null`
//! for a host that came from another device. As with hosts, an older
//! build refuses a newer file, and a damaged one is never read as empty.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tauminus_app::trust::{Approvals, Fingerprint};
use tauminus_domain::HostId;

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct File {
    version: u32,
    hosts: BTreeMap<String, Option<String>>,
}

pub(crate) fn to_json(approvals: &Approvals) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec_pretty(&File {
        version: VERSION,
        hosts: approvals
            .iter()
            .map(|(id, print)| (id.as_str().to_owned(), print.map(|p| hex(&p.0))))
            .collect(),
    })
}

pub(crate) fn from_json(bytes: &[u8]) -> Result<Approvals, String> {
    let file: File = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if file.version != VERSION {
        return Err(format!(
            "it is version {}, and this build reads version {VERSION}; update Tauminus",
            file.version
        ));
    }
    file.hosts
        .into_iter()
        .map(|(id, print)| {
            let id = HostId::parse(&id).map_err(|e| e.to_string())?;
            let print = print
                .map(|p| unhex(&p).ok_or_else(|| format!("{id}: not a fingerprint")))
                .transpose()?;
            Ok((id, print.map(Fingerprint)))
        })
        .collect()
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes
        .iter()
        .flat_map(|b| {
            [
                crate::HEX[usize::from(b >> 4)],
                crate::HEX[usize::from(b & 0x0f)],
            ]
        })
        .map(char::from)
        .collect()
}

fn unhex(text: &str) -> Option<[u8; 32]> {
    let digits = text.as_bytes();
    if digits.len() != 64 {
        return None;
    }
    let value = |d: u8| {
        char::from(d)
            .to_digit(16)
            .and_then(|v| u8::try_from(v).ok())
    };
    let mut out = [0; 32];
    let (pairs, _) = digits.as_chunks::<2>();
    for (byte, [high, low]) in out.iter_mut().zip(pairs) {
        *byte = value(*high)? << 4 | value(*low)?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_load_back_and_a_bad_file_is_refused() {
        let mut approvals = Approvals::new();
        approvals.insert(HostId::parse("new").unwrap(), None);
        let mut print = [0u8; 32];
        for (i, b) in print.iter_mut().enumerate() {
            *b = u8::try_from(i * 8).unwrap();
        }
        approvals.insert(HostId::parse("seen").unwrap(), Some(Fingerprint(print)));
        let bytes = to_json(&approvals).unwrap();
        assert_eq!(from_json(&bytes).unwrap(), approvals);
        for bad in [
            r#"{"version":2,"hosts":{}}"#,
            r#"{"version":1,"hosts":{"a b":null}}"#,
            r#"{"version":1,"hosts":{"h":"00"}}"#,
            r#"{"version":1,"hosts":{"h":"zz00000000000000000000000000000000000000000000000000000000000000"}}"#,
            r#"{"version":1}"#,
        ] {
            assert!(from_json(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}
