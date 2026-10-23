// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Rules matched against what a terminal shows, by regular expression, and
//! the sets that hold them. A set is attached to hosts and groups; what a
//! rule does when it matches is its action, today a highlight.
//!
//! Patterns are the `regex` crate's syntax, matched in time linear in the
//! text: what is matched comes from the remote host, so a pattern must not
//! be able to backtrack for seconds on text the host chose. Each pattern,
//! and each host's rules together, compile within a size limit.

use std::collections::BTreeMap;
use std::fmt;

use regex::{Regex, RegexBuilder, RegexSet, RegexSetBuilder};
use thiserror::Error;

use crate::hosts::{Label, TriggerSetId};

/// Longest pattern, in bytes.
pub const MAX_RULE_PATTERN: usize = 1024;
/// Most rules in one set.
pub const MAX_SET_RULES: usize = 100;
/// Most sets the library keeps.
pub const MAX_SETS: usize = 200;
/// Most sets one host or group lists, and most the library applies to
/// every session.
pub const MAX_LEVEL_SETS: usize = 8;
/// Most sets that apply to one session, nearest first: a host's, its
/// groups', then the defaults.
pub const MAX_APPLIED_SETS: usize = 16;
/// Most hits one scan returns: more than a pane draws.
pub const MAX_HITS: usize = 512;
/// Most lines one scan reads.
pub const MAX_SCAN_LINES: usize = 1024;
/// Longest line a scan reads, in bytes; longer lines are matched in their
/// first this many bytes.
pub const MAX_SCAN_LINE: usize = 16 * 1024;
/// Compiled size of one pattern, in bytes.
const PATTERN_SIZE: usize = 1024 * 1024;
/// Compiled size of every pattern for one session, together.
const SET_SIZE: usize = 32 * 1024 * 1024;
/// The lazy DFA's cache, per compiled pattern.
const DFA_SIZE: usize = 2 * 1024 * 1024;
/// The lazy DFA's cache for one session's rules together: at 2 MiB, 100
/// rules thrash it and a scan falls from 0.1 ms to 50 ms or more. It fills
/// only as text needs it.
const SET_DFA_SIZE: usize = 16 * 1024 * 1024;
/// How deep groups and repeats may nest in a pattern.
const NEST_LIMIT: u32 = 64;
/// Most rules one session matches with: the first this many, by
/// precedence. Bounds what one pane compiles.
pub const MAX_APPLIED_RULES: usize = 256;
/// Most text one scan reads, in bytes, over all its lines.
pub const MAX_SCAN_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TriggerError {
    #[error("a pattern is needed")]
    EmptyPattern,
    #[error("a pattern is at most {MAX_RULE_PATTERN} bytes")]
    LongPattern,
    #[error("the pattern is not valid: {0}")]
    BadPattern(String),
    #[error("a highlight needs a text or a background colour")]
    NoColour,
    #[error("a colour is a palette number from 0 to 15, or #rrggbb")]
    BadColour,
    #[error("a set holds at most {MAX_SET_RULES} rules")]
    TooManyRules,
    #[error("at most {MAX_SETS} sets")]
    TooManySets,
    #[error("at most {MAX_LEVEL_SETS} sets apply to every session, each once")]
    Defaults,
    #[error("no set {0}")]
    NoSuch(TriggerSetId),
    #[error("these rules together are too large to match: {0}")]
    TooLarge(String),
    #[error("text to send is at most {MAX_SEND} bytes")]
    LongSend,
    #[error("text to send holds no control characters; Enter is its own choice")]
    SendControl,
    #[error("a rule sends text, Enter, or both")]
    EmptySend,
}

/// A colour: one of the terminal's 16 palette colours, so it follows the
/// theme, or an exact one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Colour {
    Palette(u8),
    Rgb(u8, u8, u8),
}

impl Colour {
    pub fn palette(index: u8) -> Result<Self, TriggerError> {
        if index < 16 {
            Ok(Self::Palette(index))
        } else {
            Err(TriggerError::BadColour)
        }
    }

    /// `"0"` to `"15"` for a palette colour, or `"#rrggbb"`, as
    /// [`Colour`]'s `Display` writes it.
    pub fn parse(text: &str) -> Result<Self, TriggerError> {
        if let Some(hex) = text.strip_prefix('#') {
            if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(TriggerError::BadColour);
            }
            let byte = |at: usize| {
                u8::from_str_radix(&hex[at..at + 2], 16).map_err(|_| TriggerError::BadColour)
            };
            return Ok(Self::Rgb(byte(0)?, byte(2)?, byte(4)?));
        }
        // Digits only: `u8::from_str` takes a leading `+`.
        if text.is_empty() || text.len() > 2 || !text.bytes().all(|b| b.is_ascii_digit()) {
            return Err(TriggerError::BadColour);
        }
        Self::palette(text.parse().map_err(|_| TriggerError::BadColour)?)
    }
}

impl fmt::Display for Colour {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Palette(index) => write!(f, "{index}"),
            Self::Rgb(r, g, b) => write!(f, "#{r:02x}{g:02x}{b:02x}"),
        }
    }
}

/// How a highlight looks: a text colour, a background, or both, on the
/// match or on its whole line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Highlight {
    fg: Option<Colour>,
    bg: Option<Colour>,
    line: bool,
}

impl Highlight {
    pub fn new(fg: Option<Colour>, bg: Option<Colour>, line: bool) -> Result<Self, TriggerError> {
        if fg.is_none() && bg.is_none() {
            return Err(TriggerError::NoColour);
        }
        Ok(Self { fg, bg, line })
    }

    pub fn fg(&self) -> Option<Colour> {
        self.fg
    }

    pub fn bg(&self) -> Option<Colour> {
        self.bg
    }

    /// Whether the whole line is coloured, not only the match.
    pub fn line(&self) -> bool {
        self.line
    }
}

/// Longest text a rule sends, in bytes.
pub const MAX_SEND: usize = 1024;
/// Longest text of a line a trigger hands on (to a notice or a capture),
/// in bytes.
pub const MAX_FIRED_TEXT: usize = 1024;
/// Most triggers one scan fires.
pub const MAX_FIRED: usize = 256;

/// Text a rule types into the session, then Enter if `enter`. It holds no
/// control characters: they act as editing keys in a shell, and an escape
/// could end a bracketed paste. Nothing from the match goes into it: the
/// host would choose what runs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SendText {
    text: String,
    enter: bool,
}

impl SendText {
    pub fn new(text: String, enter: bool) -> Result<Self, TriggerError> {
        if text.len() > MAX_SEND {
            return Err(TriggerError::LongSend);
        }
        if text.chars().any(char::is_control) {
            return Err(TriggerError::SendControl);
        }
        if text.is_empty() && !enter {
            return Err(TriggerError::EmptySend);
        }
        Ok(Self { text, enter })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn enter(&self) -> bool {
        self.enter
    }
}

/// What a rule does when it matches.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Action {
    /// Colours the match where it shows.
    Highlight(Highlight),
    /// Tells the user, in the app and, while the window is in the back, the
    /// system's notifications.
    Notify,
    /// Keeps the line, for the user to read and copy later.
    Capture,
    /// Types text into the session, as the user would.
    Send(SendText),
}

impl Action {
    /// A highlight colours what shows; the rest are triggers, run once per
    /// line as output comes.
    pub fn is_highlight(&self) -> bool {
        matches!(self, Self::Highlight(_))
    }
}

/// A pattern whose syntax was checked. Compiling waits until a pane needs
/// it ([`Matcher::new`]) or the user saves it ([`RulePattern::checked`]):
/// reading a file of patterns costs only their length, however expensive
/// they are to compile.
#[derive(Clone, PartialEq, Eq)]
pub struct RulePattern {
    source: String,
}

impl RulePattern {
    /// Checks length and syntax, without compiling. O(n) in the pattern.
    pub fn parse(source: &str) -> Result<Self, TriggerError> {
        if source.is_empty() {
            return Err(TriggerError::EmptyPattern);
        }
        if source.len() > MAX_RULE_PATTERN {
            return Err(TriggerError::LongPattern);
        }
        regex_syntax::ParserBuilder::new()
            .nest_limit(NEST_LIMIT)
            .build()
            .parse(source)
            .map_err(|e| TriggerError::BadPattern(first_line(&e.to_string())))?;
        Ok(Self {
            source: source.to_owned(),
        })
    }

    /// As [`RulePattern::parse`], and compiles within the size limit: for a
    /// pattern being saved or typed.
    pub fn checked(source: &str) -> Result<Self, TriggerError> {
        let pattern = Self::parse(source)?;
        pattern.compile()?;
        Ok(pattern)
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    fn compile(&self) -> Result<Regex, TriggerError> {
        RegexBuilder::new(&self.source)
            .size_limit(PATTERN_SIZE)
            .dfa_size_limit(DFA_SIZE)
            .nest_limit(NEST_LIMIT)
            .build()
            .map_err(|e| TriggerError::BadPattern(first_line(&e.to_string())))
    }
}

/// The error's own line, without the caret diagram under it.
fn first_line(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    // `regex` prints "regex parse error:", the pattern, a caret, then
    // "error: <what>"; the last line says what is wrong.
    lines
        .iter()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map_or_else(String::new, |l| {
            l.trim().trim_start_matches("error: ").to_owned()
        })
}

impl fmt::Debug for RulePattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RulePattern").field(&self.source).finish()
    }
}

/// One rule: when `pattern` matches, `action` runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerRule {
    pub name: Label,
    pub pattern: RulePattern,
    pub action: Action,
    pub enabled: bool,
}

/// Rules kept together under a name, attached to hosts and groups as one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerSet {
    pub id: TriggerSetId,
    pub name: Label,
    rules: Vec<TriggerRule>,
}

impl TriggerSet {
    /// Checks the count, and that every rule compiles and the rules
    /// compile together within the size limit, so a set that could never
    /// match is refused when saved. Compiling takes a moment.
    pub fn new(
        id: TriggerSetId,
        name: Label,
        rules: Vec<TriggerRule>,
    ) -> Result<Self, TriggerError> {
        Self::within(id, name, rules, SET_SIZE)
    }

    /// A set read back from a file: the count is checked, and the patterns'
    /// syntax was; nothing compiles. O(r).
    pub fn read(
        id: TriggerSetId,
        name: Label,
        rules: Vec<TriggerRule>,
    ) -> Result<Self, TriggerError> {
        if rules.len() > MAX_SET_RULES {
            return Err(TriggerError::TooManyRules);
        }
        Ok(Self { id, name, rules })
    }

    /// As [`TriggerSet::new`], with the rules' compiled size limited to `size`.
    fn within(
        id: TriggerSetId,
        name: Label,
        rules: Vec<TriggerRule>,
        size: usize,
    ) -> Result<Self, TriggerError> {
        if rules.len() > MAX_SET_RULES {
            return Err(TriggerError::TooManyRules);
        }
        for rule in &rules {
            rule.pattern.compile()?;
        }
        compile_set(rules.iter().map(|r| r.pattern.as_str()), size)?;
        Ok(Self { id, name, rules })
    }

    /// In the order they were written; an earlier rule wins where two
    /// colour the same text.
    pub fn rules(&self) -> &[TriggerRule] {
        &self.rules
    }
}

/// Every set, by id, and those applied to every session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TriggerLibrary {
    sets: BTreeMap<TriggerSetId, TriggerSet>,
    defaults: Vec<TriggerSetId>,
}

impl TriggerLibrary {
    /// O(s log s + d²). A default naming no set is dropped, as one deleted
    /// would be.
    pub fn new(sets: Vec<TriggerSet>, defaults: Vec<TriggerSetId>) -> Result<Self, TriggerError> {
        if sets.len() > MAX_SETS {
            return Err(TriggerError::TooManySets);
        }
        let mut library = Self {
            sets: sets.into_iter().map(|s| (s.id.clone(), s)).collect(),
            defaults: Vec::new(),
        };
        library.set_defaults(defaults)?;
        Ok(library)
    }

    pub fn get(&self, id: &TriggerSetId) -> Option<&TriggerSet> {
        self.sets.get(id)
    }

    /// By id.
    pub fn sets(&self) -> impl Iterator<Item = &TriggerSet> {
        self.sets.values()
    }

    /// The sets every session gets, below what its host and groups name.
    pub fn defaults(&self) -> &[TriggerSetId] {
        &self.defaults
    }

    /// O(d²), d ≤ [`MAX_LEVEL_SETS`]. Ids naming no set are dropped.
    pub fn set_defaults(&mut self, defaults: Vec<TriggerSetId>) -> Result<(), TriggerError> {
        let kept: Vec<TriggerSetId> = defaults
            .into_iter()
            .filter(|id| self.sets.contains_key(id))
            .collect();
        let repeated = kept
            .iter()
            .enumerate()
            .any(|(i, id)| kept[..i].contains(id));
        if kept.len() > MAX_LEVEL_SETS || repeated {
            return Err(TriggerError::Defaults);
        }
        self.defaults = kept;
        Ok(())
    }

    /// Adds or replaces. O(log s).
    pub fn put(&mut self, set: TriggerSet) -> Result<(), TriggerError> {
        if !self.sets.contains_key(&set.id) && self.sets.len() >= MAX_SETS {
            return Err(TriggerError::TooManySets);
        }
        self.sets.insert(set.id.clone(), set);
        Ok(())
    }

    /// Removes a set, and takes it out of the defaults. Hosts that name it
    /// skip it from then on. O(log s + d).
    pub fn remove(&mut self, id: &TriggerSetId) -> Result<TriggerSet, TriggerError> {
        let set = self
            .sets
            .remove(id)
            .ok_or_else(|| TriggerError::NoSuch(id.clone()))?;
        self.defaults.retain(|d| d != id);
        Ok(set)
    }

    /// The enabled rules for a session whose host and groups name `named`,
    /// nearest first, then the defaults: in that order of precedence, at
    /// most [`MAX_APPLIED_SETS`] sets, each once, and the first
    /// [`MAX_APPLIED_RULES`] rules; ids naming no set are skipped. O(s·r).
    pub fn applied<'a>(&'a self, named: &[TriggerSetId]) -> Vec<&'a TriggerRule> {
        let mut seen: Vec<&TriggerSetId> = Vec::new();
        for id in named.iter().chain(&self.defaults) {
            if seen.len() == MAX_APPLIED_SETS {
                break;
            }
            if self.sets.contains_key(id) && !seen.contains(&id) {
                seen.push(id);
            }
        }
        seen.into_iter()
            .filter_map(|id| self.sets.get(id))
            .flat_map(|set| set.rules.iter().filter(|r| r.enabled))
            .take(MAX_APPLIED_RULES)
            .collect()
    }
}

/// Where a rule matched: line `line` of a scan, from `start` to `end` in
/// Unicode scalar values (code points), as the view counts cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub line: u32,
    /// Index into [`Matcher::styles`].
    pub style: u16,
    pub start: u32,
    pub end: u32,
}

/// One session's rules, compiled together.
#[derive(Clone)]
pub struct Matcher {
    set: RegexSet,
    rules: Vec<(Regex, Highlight)>,
}

impl fmt::Debug for Matcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Matcher")
            .field("rules", &self.rules.len())
            .finish_non_exhaustive()
    }
}

impl Matcher {
    /// Compiles `rules`, in precedence order, within the size limit.
    pub fn new(rules: &[&TriggerRule]) -> Result<Self, TriggerError> {
        Self::within(rules, SET_SIZE)
    }

    /// As [`Matcher::new`], with the rules' compiled size limited to `size`.
    /// Rules that are not highlights are left out.
    fn within(rules: &[&TriggerRule], size: usize) -> Result<Self, TriggerError> {
        let rules: Vec<(&TriggerRule, Highlight)> = rules
            .iter()
            .filter_map(|r| match r.action {
                Action::Highlight(style) => Some((*r, style)),
                _ => None,
            })
            .collect();
        let set = compile_set(rules.iter().map(|(r, _)| r.pattern.as_str()), size)?;
        let rules = rules
            .into_iter()
            .map(|(r, style)| Ok((r.pattern.compile()?, style)))
            .collect::<Result<_, TriggerError>>()?;
        Ok(Self { set, rules })
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// How each rule colours, by the index hits carry.
    pub fn styles(&self) -> Vec<Highlight> {
        self.rules.iter().map(|(_, style)| *style).collect()
    }

    /// Where the rules match in `lines`. The set finds which rules match a
    /// line at all in one pass, and only those look for their places. A
    /// whole-line rule gives one hit over its line. A match that overlaps
    /// text an earlier rule coloured is left out whole; whole-line rules are
    /// a layer of their own. Empty matches are skipped. At most
    /// [`MAX_SCAN_LINES`] lines of at most [`MAX_SCAN_LINE`] bytes,
    /// [`MAX_SCAN_BYTES`] in all, and [`MAX_HITS`] hits: a slow path in the
    /// engine (a Unicode `\b` on text that is not ASCII) is bounded by the
    /// text it reads.
    /// O(Σ n · m) for m rules that match a line, with n its length.
    pub fn scan(&self, lines: &[&str]) -> Vec<Hit> {
        let mut hits = Vec::new();
        let mut budget = MAX_SCAN_BYTES;
        for (index, line) in lines.iter().take(MAX_SCAN_LINES).enumerate() {
            let line = cut(line, MAX_SCAN_LINE.min(budget));
            if line.is_empty() && budget == 0 {
                break;
            }
            budget -= line.len();
            // Which bytes an earlier rule coloured. One rule's matches never
            // overlap each other, so checking and marking cost each rule at
            // most the line's length.
            let mut taken: Vec<bool> = Vec::new();
            let mut line_taken = false;
            let mut found: Vec<(usize, usize, usize)> = Vec::new();
            'rules: for rule in &self.set.matches(line) {
                let (regex, style) = &self.rules[rule];
                if style.line {
                    if !line_taken {
                        line_taken = true;
                        found.push((rule, 0, line.len()));
                    }
                    continue;
                }
                if taken.is_empty() {
                    taken = vec![false; line.len()];
                }
                for m in regex.find_iter(line) {
                    if hits.len() + found.len() >= MAX_HITS {
                        break 'rules;
                    }
                    let span = m.start()..m.end();
                    if !span.is_empty() && !taken[span.clone()].contains(&true) {
                        taken[span.clone()].fill(true);
                        found.push((rule, span.start, span.end));
                    }
                }
            }
            // By start, a longer span first, then by precedence: one order
            // for the same text every time.
            found.sort_unstable_by_key(|&(rule, start, end)| (start, std::cmp::Reverse(end), rule));
            let chars = code_points(line, found.iter().flat_map(|&(_, s, e)| [s, e]));
            let at = |byte: usize| {
                chars
                    .binary_search_by_key(&byte, |&(b, _)| b)
                    .map_or(0, |i| chars[i].1)
            };
            for (rule, start, end) in found {
                if hits.len() == MAX_HITS {
                    return hits;
                }
                hits.push(Hit {
                    line: u32::try_from(index).unwrap_or(u32::MAX),
                    style: u16::try_from(rule).unwrap_or(u16::MAX),
                    start: at(start),
                    end: at(end),
                });
            }
        }
        hits
    }
}

/// The rules' patterns as one set, within `size`.
fn compile_set<'a>(
    patterns: impl Iterator<Item = &'a str>,
    size: usize,
) -> Result<RegexSet, TriggerError> {
    RegexSetBuilder::new(patterns)
        .size_limit(size)
        .dfa_size_limit(SET_DFA_SIZE)
        .nest_limit(NEST_LIMIT)
        .build()
        .map_err(|e| TriggerError::TooLarge(first_line(&e.to_string())))
}

/// A trigger that fired: rule `rule` (an index into
/// [`TriggerMatcher::actions`]) matched line `line` first from `start` to
/// `end`, in code points. `text` is the line as it may be shown or kept:
/// secrets of a known shape hidden, control characters out, at most
/// [`MAX_FIRED_TEXT`] bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fired {
    pub line: u32,
    pub rule: u16,
    pub start: u32,
    pub end: u32,
    pub text: String,
}

/// One session's trigger rules (all but highlights), compiled together.
#[derive(Clone)]
pub struct TriggerMatcher {
    set: RegexSet,
    rules: Vec<(Regex, Action, Label)>,
}

impl fmt::Debug for TriggerMatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TriggerMatcher")
            .field("rules", &self.rules.len())
            .finish_non_exhaustive()
    }
}

impl TriggerMatcher {
    /// Compiles the rules of `rules` that are not highlights, in precedence
    /// order, within the size limit.
    pub fn new(rules: &[&TriggerRule]) -> Result<Self, TriggerError> {
        let rules: Vec<&TriggerRule> = rules
            .iter()
            .copied()
            .filter(|r| !r.action.is_highlight())
            .collect();
        let set = compile_set(rules.iter().map(|r| r.pattern.as_str()), SET_SIZE)?;
        let rules = rules
            .into_iter()
            .map(|r| Ok((r.pattern.compile()?, r.action.clone(), r.name.clone())))
            .collect::<Result<_, TriggerError>>()?;
        Ok(Self { set, rules })
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Rule `index`'s name and action. O(1).
    pub fn action(&self, index: usize) -> Option<(&Label, &Action)> {
        self.rules
            .get(index)
            .map(|(_, action, name)| (name, action))
    }

    /// Each rule's name and what it does, by the index fired triggers
    /// carry.
    pub fn actions(&self) -> Vec<(Label, Action)> {
        self.rules
            .iter()
            .map(|(_, action, name)| (name.clone(), action.clone()))
            .collect()
    }

    /// Each rule that matches each line, once per line, at its first
    /// match, in rule order. Rules do not hide each other: each does its
    /// own thing. Bounded as [`Matcher::scan`] is, and at most
    /// [`MAX_FIRED`]. O(Σ n · m) for m rules that match a line.
    pub fn scan(&self, lines: &[&str]) -> Vec<Fired> {
        let mut fired = Vec::new();
        let mut budget = MAX_SCAN_BYTES;
        for (index, line) in lines.iter().take(MAX_SCAN_LINES).enumerate() {
            let line = cut(line, MAX_SCAN_LINE.min(budget));
            if line.is_empty() && budget == 0 {
                break;
            }
            budget -= line.len();
            let matched = self.set.matches(line);
            if !matched.matched_any() {
                continue;
            }
            // Made only for a rule that hands the line on: send rules do not.
            let mut text: Option<String> = None;
            for rule in &matched {
                if fired.len() == MAX_FIRED {
                    return fired;
                }
                let Some(m) = self.rules[rule].0.find(line) else {
                    continue;
                };
                let chars = code_points(line, [m.start(), m.end()].into_iter());
                let at = |byte: usize| {
                    chars
                        .binary_search_by_key(&byte, |&(b, _)| b)
                        .map_or(0, |i| chars[i].1)
                };
                let shown = if matches!(self.rules[rule].1, Action::Send(_)) {
                    String::new()
                } else {
                    text.get_or_insert_with(|| crate::history::shown_safely(line, MAX_FIRED_TEXT))
                        .clone()
                };
                fired.push(Fired {
                    line: u32::try_from(index).unwrap_or(u32::MAX),
                    rule: u16::try_from(rule).unwrap_or(u16::MAX),
                    start: at(m.start()),
                    end: at(m.end()),
                    text: shown,
                });
            }
        }
        fired
    }
}

/// `text` up to `max` bytes, ending on a character boundary.
fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The code-point offset of each byte offset in `bytes`, all on character
/// boundaries of `text`, as (byte, code points) sorted by byte: one pass
/// over the text. O(n + h log h) for h offsets.
fn code_points(text: &str, bytes: impl Iterator<Item = usize>) -> Vec<(usize, u32)> {
    let mut wanted: Vec<usize> = bytes.collect();
    wanted.sort_unstable();
    wanted.dedup();
    let mut out = Vec::with_capacity(wanted.len());
    let mut next = wanted.iter().peekable();
    let ends = text
        .char_indices()
        .map(|(byte, _)| byte)
        .chain([text.len()]);
    for (count, byte) in (0u32..).zip(ends) {
        while let Some(&w) = next.next_if(|&&w| w <= byte) {
            out.push((w, count));
        }
    }
    out
}

#[cfg(test)]
mod tests;
