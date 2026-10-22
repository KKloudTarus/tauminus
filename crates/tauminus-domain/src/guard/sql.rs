// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Statements that drop or empty data, in SQL, Redis, and MongoDB. Read as
//! words, case-insensitively, over the whole line: they reach the database
//! through a client's prompt or through `-c`, `-e`, or a here-document, and
//! the words look the same in each. O(n).

use super::Rule;

/// What may follow `DROP` in a statement that removes something.
const DROPPED: &[&str] = &[
    "aggregate",
    "collection",
    "column",
    "constraint",
    "database",
    "databases",
    "dictionary",
    "domain",
    "event",
    "extension",
    "foreign",
    "function",
    "if",
    "index",
    "keyspace",
    "materialized",
    "owned",
    "partition",
    "policy",
    "procedure",
    "publication",
    "role",
    "schema",
    "sequence",
    "server",
    "stage",
    "stream",
    "subscription",
    "table",
    "tables",
    "tablespace",
    "temp",
    "temporary",
    "trigger",
    "type",
    "user",
    "view",
    "warehouse",
];

struct Token {
    word: String,
    /// The first character after the word that is not a space.
    next: Option<char>,
}

/// Each statement's words, lowercased. Statements end at `;`.
fn statements(text: &str) -> Vec<Vec<Token>> {
    let chars: Vec<char> = text.chars().collect();
    let mut statements = vec![Vec::new()];
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == ';' {
            statements.push(Vec::new());
            i += 1;
            continue;
        }
        if !c.is_ascii_alphanumeric() && c != '_' {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
            i += 1;
        }
        // MySQL's `/*!50000DROP` runs the words after the version digits.
        let word: String = chars[start..i]
            .iter()
            .skip_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .to_ascii_lowercase();
        if word.is_empty() {
            continue;
        }
        let next = chars[i..].iter().copied().find(|c| !c.is_whitespace());
        if let Some(statement) = statements.last_mut() {
            statement.push(Token { word, next });
        }
    }
    statements
}

pub(super) fn scan(text: &str) -> Vec<Rule> {
    let mut rules = Vec::new();
    for statement in statements(text) {
        // Whether `where` comes at or after each token, from one backward pass.
        let mut where_after = vec![false; statement.len() + 1];
        for k in (0..statement.len()).rev() {
            where_after[k] = where_after[k + 1] || statement[k].word == "where";
        }
        for (k, token) in statement.iter().enumerate() {
            let next = statement.get(k + 1).map(|t| t.word.as_str());
            match token.word.as_str() {
                "drop" if next.is_some_and(|n| DROPPED.contains(&n)) => {
                    rules.push(Rule::DropData);
                }
                // `truncate -s 0 file` is the coreutils command, not SQL.
                "truncate" if next.is_some() && token.next != Some('-') => {
                    rules.push(Rule::EmptyData);
                }
                "flushall" | "flushdb" => rules.push(Rule::EmptyData),
                "delete" if next == Some("from") && !where_after[k] => {
                    rules.push(Rule::DeleteAll);
                }
                _ => {}
            }
        }
    }
    let folded: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    if [".drop(", "dropdatabase(", "dropcollection("]
        .iter()
        .any(|call| folded.contains(call))
    {
        rules.push(Rule::DropData);
    }
    if ["deletemany({})", "remove({})", "deletemany()"]
        .iter()
        .any(|call| folded.contains(call))
    {
        rules.push(Rule::DeleteAll);
    }
    rules.sort_unstable();
    rules.dedup();
    rules
}
