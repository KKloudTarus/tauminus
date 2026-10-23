// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The snippet file format, version 1. As with hosts, an older build
//! refuses a newer file instead of dropping what it does not know.

use serde::{Deserialize, Serialize};
use tauminus_domain::{Label, Library, Snippet, SnippetId};

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    #[serde(default)]
    snippets: Vec<SnippetRecord>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnippetRecord {
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder: Option<String>,
    body: String,
}

/// O(total body size).
pub(crate) fn to_json(library: &Library) -> Result<Vec<u8>, serde_json::Error> {
    let file = File {
        version: VERSION,
        snippets: library
            .snippets()
            .map(|s| SnippetRecord {
                id: s.id.to_string(),
                name: s.name.as_str().to_owned(),
                folder: s.folder.as_ref().map(|f| f.as_str().to_owned()),
                body: s.body().to_owned(),
            })
            .collect(),
    };
    let mut bytes = serde_json::to_vec_pretty(&file)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Checks every snippet as [`Snippet::new`] does.
pub(crate) fn from_json(bytes: &[u8]) -> Result<Library, String> {
    let file: File = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if file.version != VERSION {
        return Err(format!(
            "it is version {}, and this build reads version {VERSION}; update Tauminus",
            file.version
        ));
    }
    let err = |what: &str, e: &dyn std::fmt::Display| format!("{what}: {e}");
    let mut seen = std::collections::BTreeSet::new();
    let snippets = file
        .snippets
        .into_iter()
        .map(|s| {
            let id = SnippetId::parse(&s.id).map_err(|e| err("snippet id", &e))?;
            if !seen.insert(id.clone()) {
                return Err(format!("snippet {id} is listed twice"));
            }
            let name = Label::parse(&s.name).map_err(|e| err("snippet name", &e))?;
            let folder = s
                .folder
                .map(|f| Label::parse(&f))
                .transpose()
                .map_err(|e| err("snippet folder", &e))?;
            Snippet::new(id, name, folder, s.body).map_err(|e| err("snippet", &e))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Library::new(snippets).map_err(|e| e.to_string())
}
