// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Command history: what one recorded command holds, how a search is
//! asked, and how much history is kept.

use thiserror::Error;

mod redact;

/// Longest command kept; a longer one is cut. 4 KiB covers a pasted
/// one-liner, not a pasted file.
pub const MAX_COMMAND: usize = 4 * 1024;
/// Most output kept per command; the start is kept, the rest dropped.
pub const MAX_OUTPUT: usize = 64 * 1024;
/// Most results one search returns.
pub const MAX_RESULTS: u32 = 500;
/// Longest search text.
const MAX_QUERY: usize = 512;

/// One command as it ran: where, when, what it printed, how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// The saved host's id; `None` for a local shell or an ad hoc connect.
    pub host: Option<String>,
    /// What the user saw on the tab, for showing results.
    pub label: String,
    /// Milliseconds since the Unix epoch.
    pub at_ms: i64,
    pub command: String,
    pub output: String,
    /// The exit status, when the shell reports one (OSC 133).
    pub exit: Option<i32>,
}

impl HistoryEntry {
    /// Cuts the command and output to their limits on a character boundary,
    /// drops control characters but tab and newline, and hides tokens of
    /// known shapes (API keys, passwords in arguments, bearer tokens, private
    /// keys). The command keeps
    /// no newline or tab either, as spaces: inserted into a shell without
    /// bracketed paste, a newline would run it. O(n).
    #[must_use]
    pub fn bounded(mut self) -> Self {
        // Hidden before the cut, so a token the cut ends short of a known
        // shape is hidden whole.
        let command = clean(
            &redact::redact(&clean(&self.command, MAX_COMMAND + REDACT_SLACK)),
            MAX_COMMAND,
        )
        .replace(['\n', '\t'], " ");
        command.trim().clone_into(&mut self.command);
        self.output = clean(
            &redact::redact(&clean(&self.output, MAX_OUTPUT + REDACT_SLACK)),
            MAX_OUTPUT,
        );
        self.label = clean(&self.label, 256);
        self
    }

    /// Bytes this entry counts against the size cap.
    pub fn size(&self) -> u64 {
        (self.command.len() + self.output.len() + self.label.len()) as u64
    }
}

/// Text past a limit read before hiding tokens, so a token the limit cuts
/// is seen whole.
const REDACT_SLACK: usize = 4096;

/// `text` as it may be shown or kept outside the terminal: secrets of a
/// known shape hidden, as the history hides them, control characters, line
/// breaks, and characters that change how text reads unseen (bidi controls,
/// zero-width ones, tags) out, at most `max` bytes. A line that is all
/// base64 is hidden whole: one line of a private key's body, which the
/// history finds between its BEGIN and END lines, comes alone here. O(n).
pub(crate) fn shown_safely(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    let base64 = trimmed.len() >= 40
        && trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='));
    if base64 {
        return "•••".to_owned();
    }
    // Read a little past what is kept, so a secret cut at the end is still
    // recognised and hidden whole.
    let visible: String = text
        .chars()
        .filter(|&c| !invisible(c))
        .take(max + SHOWN_SLACK)
        .collect();
    clean(&redact::redact(&clean(&visible, max + SHOWN_SLACK)), max).replace(['\n', '\t'], " ")
}

/// Bytes past the kept text that [`shown_safely`] reads to find secrets.
const SHOWN_SLACK: usize = 512;

/// Characters that change how text reads without being seen.
pub fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{ad}'
            | '\u{61c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{e0000}'..='\u{e007f}'
    )
}

fn clean(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max));
    for c in text.chars() {
        if c.is_control() && c != '\n' && c != '\t' {
            continue;
        }
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

/// One search result: the command, and the stretch of output that matched,
/// with matches marked by U+0001 and U+0002 for the view to highlight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryHit {
    pub id: i64,
    pub host: Option<String>,
    pub label: String,
    pub at_ms: i64,
    pub exit: Option<i32>,
    pub command: String,
    pub snippet: String,
}

/// Which exit statuses a search keeps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExitFilter {
    #[default]
    Any,
    /// Exit 0.
    Success,
    /// A known nonzero exit.
    Failure,
}

/// Most words a search reads.
pub(crate) const MAX_WORDS: usize = 8;
/// Longest prefix the index keeps, in characters.
pub(crate) const MAX_PREFIX: usize = 8;

/// A search over recorded commands and their output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryQuery {
    /// Words to find; empty lists the most recent commands.
    pub text: String,
    /// Only this saved host; `Some("")` means local and ad hoc sessions.
    pub host: Option<String>,
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub exit: ExitFilter,
    /// At most [`MAX_RESULTS`].
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct HistoryQueryError(&'static str);

impl HistoryQuery {
    pub fn check(&self) -> Result<(), HistoryQueryError> {
        if self.text.len() > MAX_QUERY {
            return Err(HistoryQueryError("the search text is over 512 bytes"));
        }
        if self.limit == 0 || self.limit > MAX_RESULTS {
            return Err(HistoryQueryError("ask for 1 to 500 results"));
        }
        if self.to_ms.is_some() && self.from_ms.is_none() {
            // Every newer row would be read to skip it.
            return Err(HistoryQueryError("a time range needs its start"));
        }
        if let (Some(from), Some(to)) = (self.from_ms, self.to_ms)
            && from > to
        {
            return Err(HistoryQueryError("the time range ends before it starts"));
        }
        Ok(())
    }

    /// The text as an FTS5 query: each word quoted, so no word is read as
    /// an operator or a column, all words required, the last one matched as
    /// a prefix (from two letters) so results follow typing. The index
    /// keeps prefixes of up to [`MAX_PREFIX`] letters, so a longer last word
    /// matches on its first ones; past [`MAX_WORDS`] words, the rest are
    /// left out, as each word read costs its whole posting list. `None`
    /// when there are no words. O(n).
    pub fn fts_query(&self) -> Option<String> {
        let words: Vec<&str> = self.text.split_whitespace().take(MAX_WORDS).collect();
        let (last, rest) = words.split_last()?;
        // Cut where the index's prefixes end, unless the cut would leave
        // no letter or digit to match.
        let last = match last.char_indices().nth(MAX_PREFIX) {
            Some((at, _)) if last[..at].chars().any(char::is_alphanumeric) => &last[..at],
            _ => last,
        };
        let quote = |word: &str| format!("\"{}\"", word.replace('"', "\"\""));
        let mut query: Vec<String> = rest.iter().map(|w| quote(w)).collect();
        // One letter as a prefix matches nearly every row; it is a word.
        if last.chars().count() > 1 {
            query.push(format!("{}*", quote(last)));
        } else {
            query.push(quote(last));
        }
        Some(query.join(" "))
    }
}

/// Where an excerpt of the output may come from: its start.
pub const EXCERPT_FROM: usize = 16 * 1024;
/// Characters an excerpt keeps before its first match, and after it.
const EXCERPT_BEFORE: usize = 40;
const EXCERPT_AFTER: usize = 120;

impl HistoryQuery {
    /// A line of `output` around the first word of the search it holds,
    /// with each word it holds between `\u{1}` and `\u{2}`, as the dialog
    /// marks them. ASCII letters match in any case; others match as typed,
    /// so a word found only by folding diacritics gives no excerpt. Empty
    /// when no word is found. O(n · w) for n characters and w words.
    #[must_use]
    pub fn excerpt(&self, output: &str) -> String {
        let words: Vec<String> = self
            .text
            .split_whitespace()
            .take(MAX_WORDS)
            .map(str::to_ascii_lowercase)
            .collect();
        // ASCII lowercasing keeps every byte offset.
        let folded = output.to_ascii_lowercase();
        let find_from = |from: usize| {
            words
                .iter()
                .filter_map(|w| {
                    folded[from..]
                        .find(w.as_str())
                        .map(|at| (from + at, w.len()))
                })
                .min()
        };
        let Some((first, _)) = find_from(0) else {
            return String::new();
        };
        let line_start = output[..first].rfind('\n').map_or(0, |n| n + 1);
        let line_end = output[first..]
            .find('\n')
            .map_or(output.len(), |n| first + n);
        let start = floor_char(output, first.saturating_sub(EXCERPT_BEFORE).max(line_start));
        let end = floor_char(output, (first + EXCERPT_AFTER).min(line_end));
        let mut out = String::new();
        if start > line_start {
            out.push('…');
        }
        let mut at = start;
        while let Some((hit, len)) = find_from(at).filter(|&(hit, len)| hit + len <= end) {
            out.push_str(&output[at..hit]);
            out.push('\u{1}');
            out.push_str(&output[hit..hit + len]);
            out.push('\u{2}');
            at = hit + len;
        }
        out.push_str(&output[at..end]);
        if end < line_end {
            out.push('…');
        }
        out
    }
}

/// The nearest character boundary at or before `at`.
fn floor_char(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// How much history is kept. Pruning drops the oldest commands first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    /// Most bytes of command and output text kept.
    pub max_bytes: u64,
    /// Commands older than this many days are dropped; 0 keeps them.
    pub max_days: u32,
}

impl Default for Retention {
    /// 256 MiB and a year: about 2 million commands at typical sizes.
    fn default() -> Self {
        Self {
            max_bytes: 256 * 1024 * 1024,
            max_days: 365,
        }
    }
}

#[cfg(test)]
mod tests;
