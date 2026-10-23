// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Saved commands with `{{variable}}` placeholders, kept in folders, and
//! the library that holds them.
//!
//! A placeholder is `{{name}}` or `{{name:default}}`. Names are letters,
//! digits, `_`, `-`, and `.`, at most 64; a default runs to the closing
//! braces. `{{{{` writes `{{` literally. Anything else that only looks like
//! a placeholder stays text, so a shell's own `${x}` or a Go template is
//! sent as typed.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::hosts::{Label, SnippetId};

/// Longest snippet body, in bytes.
pub const MAX_BODY: usize = 64 * 1024;
/// Most placeholders one snippet may use.
pub const MAX_VARIABLES: usize = 32;
const MAX_NAME: usize = 64;
const MAX_DEFAULT: usize = 1024;
/// The longest text between a placeholder's braces.
const LONGEST: usize = MAX_NAME + 1 + MAX_DEFAULT;
/// Longest value filled into one placeholder.
pub const MAX_VALUE: usize = 4096;
/// Longest text a filled snippet types: a body can use one placeholder
/// thousands of times, and a long value would multiply.
pub const MAX_FILLED: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SnippetError {
    #[error("a snippet is at most {MAX_BODY} bytes")]
    TooLong,
    #[error("a snippet uses at most {MAX_VARIABLES} different placeholders")]
    TooManyVariables,
    #[error("no value for {{{{{0}}}}}")]
    Missing(String),
    #[error("a snippet needs a command")]
    Empty,
    #[error("at most {MAX_SNIPPETS} snippets, of {MAX_LIBRARY} bytes in all")]
    TooMany,
    #[error("no snippet {0}")]
    NoSuch(SnippetId),
    #[error("a snippet holds no control characters but tab and line breaks")]
    Control,
    #[error("{{{{{0}}}}} gets at most {MAX_VALUE} bytes on one line")]
    Value(String),
    #[error("the snippet does not use {{{{{0}}}}}")]
    Unknown(String),
    #[error("filled in, the snippet would type more than {MAX_FILLED} bytes")]
    TooLongOutput,
}

/// Control characters act as editing keys in a shell (Ctrl+A, Ctrl+Y, ESC
/// sequences), so the text checked would not be the line that runs. Tab
/// and line breaks are the only ones a command needs.
fn has_control(text: &str, line_breaks: bool) -> bool {
    text.chars()
        .any(|c| c.is_control() && c != '\t' && !(line_breaks && (c == '\n' || c == '\r')))
}

/// Most snippets the library keeps.
pub const MAX_SNIPPETS: usize = 2000;
/// Most bytes of bodies the library keeps in all. JSON escapes quotes and
/// line breaks, so this keeps the file well under the store's 16 MiB.
pub const MAX_LIBRARY: usize = 8 * 1024 * 1024;

/// A saved command. `folder` groups snippets one level deep, as a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub id: SnippetId,
    pub name: Label,
    pub folder: Option<Label>,
    body: String,
}

impl Snippet {
    /// Checks the body: not blank, within [`MAX_BODY`], at most
    /// [`MAX_VARIABLES`] placeholders. O(n).
    pub fn new(
        id: SnippetId,
        name: Label,
        folder: Option<Label>,
        body: String,
    ) -> Result<Self, SnippetError> {
        if body.trim().is_empty() {
            return Err(SnippetError::Empty);
        }
        if has_control(&body, true) {
            return Err(SnippetError::Control);
        }
        variables(&body)?;
        Ok(Self {
            id,
            name,
            folder,
            body,
        })
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    /// Its placeholders, in first-use order.
    pub fn variables(&self) -> Vec<Variable> {
        // Checked in `new`.
        variables(&self.body).unwrap_or_default()
    }

    /// What to type into a shell to run it: filled in, each line ended
    /// by Enter. O(n + output).
    pub fn command(&self, values: &BTreeMap<String, String>) -> Result<String, SnippetError> {
        let filled = fill(&self.body, values)?;
        let mut out = String::with_capacity(filled.len() + 1);
        for line in filled.trim_end_matches(['\r', '\n']).lines() {
            out.push_str(line);
            out.push('\r');
        }
        Ok(out)
    }
}

/// Every saved snippet, by id. Each change keeps the count under
/// [`MAX_SNIPPETS`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Library {
    snippets: BTreeMap<SnippetId, Snippet>,
}

impl Library {
    /// O(s log s).
    pub fn new(snippets: Vec<Snippet>) -> Result<Self, SnippetError> {
        let bytes: usize = snippets.iter().map(|s| s.body.len()).sum();
        if snippets.len() > MAX_SNIPPETS || bytes > MAX_LIBRARY {
            return Err(SnippetError::TooMany);
        }
        Ok(Self {
            snippets: snippets.into_iter().map(|s| (s.id.clone(), s)).collect(),
        })
    }

    pub fn get(&self, id: &SnippetId) -> Option<&Snippet> {
        self.snippets.get(id)
    }

    /// By id.
    pub fn snippets(&self) -> impl Iterator<Item = &Snippet> {
        self.snippets.values()
    }

    /// Adds or replaces. O(log s).
    pub fn put(&mut self, snippet: Snippet) -> Result<(), SnippetError> {
        let replaced = self.snippets.get(&snippet.id).map(|s| s.body.len());
        let bytes = self.bytes() - replaced.unwrap_or(0) + snippet.body.len();
        if (replaced.is_none() && self.snippets.len() >= MAX_SNIPPETS) || bytes > MAX_LIBRARY {
            return Err(SnippetError::TooMany);
        }
        self.snippets.insert(snippet.id.clone(), snippet);
        Ok(())
    }

    /// Bytes of bodies kept. O(s).
    fn bytes(&self) -> usize {
        self.snippets.values().map(|s| s.body.len()).sum()
    }

    pub fn remove(&mut self, id: &SnippetId) -> Result<Snippet, SnippetError> {
        self.snippets
            .remove(id)
            .ok_or_else(|| SnippetError::NoSuch(id.clone()))
    }
}

/// One placeholder as the user fills it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    /// The first default given for it, if any.
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece<'a> {
    Text(&'a str),
    Braces,
    Variable {
        name: &'a str,
        default: Option<&'a str>,
    },
}

/// Splits a body into text and placeholders. O(n): the next `}}` is
/// remembered, and looked for again only once the scan passes it, so the
/// searches add up to one pass however many `{{` never close.
fn pieces(body: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut rest = body;
    // Where `rest` starts in `body`.
    let mut offset = 0;
    // The next `}}` at or after the last search, in `body`; `None` when
    // there is none left.
    let mut close: Option<usize> = None;
    let mut searched = false;
    while let Some(at) = rest.find("{{") {
        if rest[at..].starts_with("{{{{") {
            out.push(Piece::Text(&rest[..at]));
            out.push(Piece::Braces);
            offset += at + 4;
            rest = &rest[at + 4..];
            continue;
        }
        let from = offset + at + 2;
        if !searched || close.is_some_and(|c| c < from) {
            close = body[from..].find("}}").map(|e| from + e);
            searched = true;
        }
        let inside = &rest[at + 2..];
        let placeholder = close
            .map(|c| c - from)
            .filter(|&end| end <= LONGEST)
            .and_then(|end| {
                let (name, default) = match inside[..end].split_once(':') {
                    Some((name, default)) => (name, Some(default)),
                    None => (&inside[..end], None),
                };
                // A name starts with a letter or `_`, so a Go template's
                // `{{.Names}}` stays text.
                let valid = name.len() <= MAX_NAME
                    && name
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_alphabetic() || c == '_')
                    && name
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
                    && default.is_none_or(|d| d.len() <= MAX_DEFAULT && !has_control(d, false));
                valid.then_some((end, name, default))
            });
        if let Some((end, name, default)) = placeholder {
            out.push(Piece::Text(&rest[..at]));
            out.push(Piece::Variable { name, default });
            offset += at + 2 + end + 2;
            rest = &inside[end + 2..];
        } else {
            // Not ours: keep the first brace as text and look again after it.
            out.push(Piece::Text(&rest[..=at]));
            offset += at + 1;
            rest = &rest[at + 1..];
        }
    }
    out.push(Piece::Text(rest));
    out
}

/// The placeholders a body uses, in first-use order, each once.
/// O(n + v log v).
pub fn variables(body: &str) -> Result<Vec<Variable>, SnippetError> {
    if body.len() > MAX_BODY {
        return Err(SnippetError::TooLong);
    }
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    for piece in pieces(body) {
        if let Piece::Variable { name, default } = piece
            && !seen.contains(name)
        {
            if seen.len() == MAX_VARIABLES {
                return Err(SnippetError::TooManyVariables);
            }
            seen.insert(name);
            order.push(Variable {
                name: name.to_owned(),
                default: default.map(str::to_owned),
            });
        }
    }
    Ok(order)
}

/// The body with every placeholder replaced: by `values`, else its
/// default, else an error naming it. A value is one line of at most
/// [`MAX_VALUE`] bytes for a placeholder the body uses, and the result at
/// most [`MAX_FILLED`]. O(n + output + k·p) for k values and p pieces.
pub fn fill(body: &str, values: &BTreeMap<String, String>) -> Result<String, SnippetError> {
    if body.len() > MAX_BODY {
        return Err(SnippetError::TooLong);
    }
    let pieces = pieces(body);
    for (name, value) in values {
        let used = pieces
            .iter()
            .any(|p| matches!(p, Piece::Variable { name: n, .. } if n == name));
        if !used {
            return Err(SnippetError::Unknown(name.clone()));
        }
        if value.len() > MAX_VALUE || has_control(value, false) {
            return Err(SnippetError::Value(name.clone()));
        }
    }
    let mut out = String::with_capacity(body.len());
    for piece in pieces {
        let text = match piece {
            Piece::Text(text) => text,
            Piece::Braces => "{{",
            // A blank value counts as none: `rm -rf /srv/{{site}}`
            // must not run as `rm -rf /srv/`.
            Piece::Variable { name, default } => values
                .get(name)
                .map(String::as_str)
                .filter(|v| !v.trim().is_empty())
                .or(default)
                .ok_or_else(|| SnippetError::Missing(name.to_owned()))?,
        };
        if out.len() + text.len() > MAX_FILLED {
            return Err(SnippetError::TooLongOutput);
        }
        out.push_str(text);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn placeholders_are_found_once_in_order_with_their_first_default() {
        let found = variables("tail -n {{lines:100}} {{file}} | grep {{word}} {{file:x}}").unwrap();
        let names: Vec<_> = found
            .iter()
            .map(|v| (v.name.as_str(), v.default.as_deref()))
            .collect();
        assert_eq!(
            names,
            [("lines", Some("100")), ("file", None), ("word", None)]
        );
    }

    #[test]
    fn values_and_defaults_fill_in() {
        let body = "kubectl -n {{ns:default}} logs {{pod}}";
        assert_eq!(
            fill(body, &values(&[("pod", "web-1")])).unwrap(),
            "kubectl -n default logs web-1"
        );
        assert_eq!(
            fill(body, &values(&[("pod", "a"), ("ns", "prod")])).unwrap(),
            "kubectl -n prod logs a"
        );
        assert_eq!(
            fill(body, &values(&[])),
            Err(SnippetError::Missing("pod".into()))
        );
        assert_eq!(
            fill(body, &values(&[("pod", "  ")])),
            Err(SnippetError::Missing("pod".into()))
        );
        // A blank value takes the default where there is one.
        assert_eq!(
            fill(body, &values(&[("pod", "a"), ("ns", "")])).unwrap(),
            "kubectl -n default logs a"
        );
    }

    #[test]
    fn things_that_only_look_like_placeholders_stay_as_typed() {
        for body in [
            "echo ${HOME} {x}",
            "{{ .Values.image }}",
            "docker ps --format '{{.Names}}'",
            "kubectl get -o go-template='{{-range .items}}x{{- end}}'",
            "{{}} and {{a b}} and {{unclosed",
            "printf '{{{{literal}}'",
        ] {
            let filled = fill(body, &values(&[])).unwrap();
            let expected = body.replace("{{{{", "{{");
            assert_eq!(filled, expected, "{body}");
        }
        assert_eq!(variables("{{ .Values.image }}").unwrap(), []);
    }

    #[test]
    fn bounds_hold() {
        assert_eq!(
            variables(&"x".repeat(MAX_BODY + 1)),
            Err(SnippetError::TooLong)
        );
        let many = (0..=MAX_VARIABLES).fold(String::new(), |mut out, i| {
            use std::fmt::Write;
            let _ = write!(out, "{{{{v{i}}}}}");
            out
        });
        assert_eq!(variables(&many), Err(SnippetError::TooManyVariables));
        let long_default = format!("{{{{a:{}}}}}", "d".repeat(MAX_DEFAULT + 1));
        assert_eq!(variables(&long_default).unwrap(), []);
    }

    fn snippet(body: &str) -> Result<Snippet, SnippetError> {
        Snippet::new(
            SnippetId::parse("s1").unwrap(),
            Label::parse("Logs").unwrap(),
            None,
            body.to_owned(),
        )
    }

    #[test]
    fn a_command_ends_each_line_with_enter() {
        let multi = snippet("cd /srv\nls -la {{dir:.}}\n\n").unwrap();
        assert_eq!(multi.command(&values(&[])).unwrap(), "cd /srv\rls -la .\r");
        let crlf = snippet("uptime\r\n").unwrap();
        assert_eq!(crlf.command(&values(&[])).unwrap(), "uptime\r");
        assert_eq!(snippet("  \n"), Err(SnippetError::Empty));
    }

    #[test]
    fn the_library_stays_within_its_limit() {
        let many: Vec<Snippet> = (0..MAX_SNIPPETS)
            .map(|i| {
                Snippet::new(
                    SnippetId::parse(&format!("s{i}")).unwrap(),
                    Label::parse("x").unwrap(),
                    None,
                    "true".into(),
                )
                .unwrap()
            })
            .collect();
        let mut library = Library::new(many).unwrap();
        let extra = Snippet::new(
            SnippetId::parse("extra").unwrap(),
            Label::parse("x").unwrap(),
            None,
            "true".into(),
        )
        .unwrap();
        assert_eq!(library.put(extra), Err(SnippetError::TooMany));
        let mut big = Library::default();
        let body = "x".repeat(MAX_BODY);
        for i in 0..MAX_LIBRARY / MAX_BODY {
            let id = SnippetId::parse(&format!("b{i}")).unwrap();
            big.put(Snippet::new(id, Label::parse("b").unwrap(), None, body.clone()).unwrap())
                .unwrap();
        }
        let one_more = Snippet::new(
            SnippetId::parse("over").unwrap(),
            Label::parse("o").unwrap(),
            None,
            "x".into(),
        )
        .unwrap();
        assert_eq!(big.put(one_more), Err(SnippetError::TooMany));
        // Replacing a body with a shorter one still fits.
        let shorter = Snippet::new(
            SnippetId::parse("b0").unwrap(),
            Label::parse("b").unwrap(),
            None,
            "y".into(),
        )
        .unwrap();
        assert!(big.put(shorter).is_ok());
        // Replacing one is not adding.
        let again = snippet("date").unwrap();
        library.put(again.clone()).unwrap_or(());
        let first = SnippetId::parse("s0").unwrap();
        let replaced =
            Snippet::new(first.clone(), Label::parse("y").unwrap(), None, "id".into()).unwrap();
        library.put(replaced).unwrap();
        assert_eq!(library.get(&first).unwrap().body(), "id");
        assert!(library.remove(&first).is_ok());
        assert!(matches!(
            library.remove(&first),
            Err(SnippetError::NoSuch(_))
        ));
    }

    #[test]
    fn many_unclosed_braces_stay_linear() {
        // 64 KiB of `{{ a` and one `}}` at the end: every `{{` is text.
        let mut body = "{{ a".repeat(MAX_BODY / 4 - 1);
        body.push_str("}}");
        let started = std::time::Instant::now();
        assert_eq!(fill(&body, &values(&[])).unwrap(), body);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        // A placeholder right at the window's edge still counts.
        let edge = format!(
            "{{{{{}:{}}}}}",
            "n".repeat(MAX_NAME),
            "d".repeat(MAX_DEFAULT)
        );
        assert_eq!(fill(&edge, &values(&[])).unwrap(), "d".repeat(MAX_DEFAULT));
    }

    #[test]
    fn controls_never_reach_a_shell_as_keys() {
        assert_eq!(snippet("cd /srv\u{1}rm \r"), Err(SnippetError::Control));
        assert_eq!(snippet("echo \u{1b}[201~"), Err(SnippetError::Control));
        assert!(snippet("printf 'a\tb'\r\nls\n").is_ok());
        // A default with a control is not a placeholder: it stays text,
        // and the body itself is then refused for the control.
        assert_eq!(variables("{{a:x\ry}}").unwrap(), []);
        let body = "echo {{v}}";
        for bad in ["a\nb", "a\u{3}", "\u{1b}[A"] {
            assert_eq!(
                fill(body, &values(&[("v", bad)])),
                Err(SnippetError::Value("v".into()))
            );
        }
        assert_eq!(
            fill(body, &values(&[("v", &"x".repeat(MAX_VALUE + 1))])),
            Err(SnippetError::Value("v".into()))
        );
        assert_eq!(
            fill(body, &values(&[("v", "1"), ("w", "2")])),
            Err(SnippetError::Unknown("w".into()))
        );
    }

    #[test]
    fn a_filled_snippet_stays_within_its_output_limit() {
        let body = "{{a}}".repeat(MAX_BODY / 5);
        let long = "x".repeat(MAX_VALUE);
        assert_eq!(
            fill(&body, &values(&[("a", &long)])),
            Err(SnippetError::TooLongOutput)
        );
        assert!(fill(&body, &values(&[("a", "x")])).is_ok());
    }

    #[test]
    fn an_unclosed_brace_leaves_later_escapes_and_placeholders_working() {
        let body = format!("{{{{ open {} {{{{{{{{ {{{{a}}}}", "x".repeat(2000));
        assert_eq!(
            fill(&body, &values(&[("a", "1")])).unwrap(),
            format!("{{{{ open {} {{{{ 1", "x".repeat(2000))
        );
        assert_eq!(fill("{{ {{{{", &values(&[])).unwrap(), "{{ {{");
    }

    #[test]
    fn a_value_is_not_parsed_again() {
        let filled = fill("echo {{a}}", &values(&[("a", "{{b}}")])).unwrap();
        assert_eq!(filled, "echo {{b}}");
    }
}
