// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! A shell lexer for the guard. It reads enough of POSIX and bash syntax to
//! find every simple command a line may run, and keeps going on input a
//! shell would reject or wait on: an open quote, a `$(` without its `)`, a
//! here-document whose end has not been typed. What it cannot resolve, such
//! as `$X` or `*`, it marks on the word instead of guessing.
//!
//! One pass over the input, O(n) time and space. The text inside each
//! substitution is returned for the caller to read as a script of its own,
//! so nesting costs O(n) per level.

/// Longest word that gets brace expansion.
const MAX_BRACE_WORD: usize = 4096;
/// Most words one brace expansion may produce; past it the word is opaque.
const MAX_BRACE_WORDS: usize = 64;
/// Most expansion steps per word. Each is O(L) for a word of L characters,
/// so a word costs at most O(256·L).
const MAX_BRACE_STEPS: usize = 256;
/// Longest text inside braces read as a sequence such as `{1..100..2}`.
const MAX_SEQUENCE: usize = 64;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a separate fact about the word that a different rule reads"
)]
pub(super) struct Word {
    /// The word after quote removal. Expansions keep their source text.
    pub(super) text: String,
    /// The word as typed.
    pub(super) raw: String,
    /// Holds a parameter, command, or arithmetic expansion.
    pub(super) expanded: bool,
    /// Holds an unquoted `*`, `?`, or `[`.
    pub(super) glob: bool,
    /// Holds a bash history expansion such as `!!` or `!$`.
    pub(super) history: bool,
    /// Holds a process substitution, `<(...)` or `>(...)`.
    pub(super) process: bool,
    /// Holds an unquoted expansion, which the shell splits into words: `$X`
    /// may become `-rf /`, while `"$X"` stays one word.
    pub(super) split: bool,
}

impl Word {
    /// The value is only known when the shell runs the line.
    pub(super) fn opaque(&self) -> bool {
        self.expanded || self.glob || self.history
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RedirectKind {
    /// `>`, `>>`, `>|`, `<>`, `&>`, `&>>`, and `>&` to a file.
    Write,
    /// `<`.
    Read,
    /// `<&` and `>&` to a descriptor.
    Dup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Redirect {
    pub(super) kind: RedirectKind,
    pub(super) target: Word,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Command {
    pub(super) words: Vec<Word>,
    pub(super) redirects: Vec<Redirect>,
    /// Position in its pipeline; 0 for the first command.
    pub(super) stage: usize,
    /// Text a here-document or here-string feeds to standard input.
    pub(super) input: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Script {
    pub(super) commands: Vec<Command>,
    /// The text inside each `$(...)`, backquote, and process substitution,
    /// in the order they appear: scripts the shell runs.
    pub(super) nested: Vec<String>,
    /// The text inside each `${...}` and `$((...))`. Its words never run,
    /// but substitutions inside them do: `${x:-$(id)}`.
    pub(super) scanned: Vec<String>,
    /// The input ended where the shell would ask for more: inside a quote
    /// or substitution, after `\`, `|`, `&&`, or `||`, or before a
    /// here-document's delimiter.
    pub(super) incomplete: bool,
}

/// Splits `src` into simple commands. Never fails.
pub(super) fn lex(src: &str) -> Script {
    let mut lexer = Lexer {
        s: src.chars().collect(),
        i: 0,
        script: Script::default(),
        command: Command::default(),
        word: None,
        redirect: None,
        heredocs: Vec::new(),
        after_operator: false,
    };
    lexer.run();
    lexer.script
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Redirect(RedirectKind),
    HereDoc { strip_tabs: bool },
    HereString,
}

struct HereDoc {
    /// Index the owning command has, or will have, in `script.commands`.
    command: usize,
    delimiter: String,
    strip_tabs: bool,
    /// An unquoted delimiter: the body's substitutions run.
    expands: bool,
}

#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the flags become the finished word's flags"
)]
struct WordBuf {
    /// Each character, and whether quoting made it literal.
    chars: Vec<(char, bool)>,
    start: usize,
    expanded: bool,
    glob: bool,
    history: bool,
    process: bool,
    split: bool,
}

struct Lexer {
    s: Vec<char>,
    i: usize,
    script: Script,
    command: Command,
    word: Option<WordBuf>,
    redirect: Option<Pending>,
    heredocs: Vec<HereDoc>,
    after_operator: bool,
}

impl Lexer {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn at(&self, offset: usize) -> Option<char> {
        self.s.get(self.i + offset).copied()
    }

    fn run(&mut self) {
        while let Some(c) = self.peek() {
            match c {
                ' ' | '\t' | '\r' => {
                    self.end_word();
                    self.i += 1;
                }
                '\n' => {
                    self.end_word();
                    self.i += 1;
                    self.end_command(false);
                    self.read_heredocs();
                }
                ';' => {
                    self.end_word();
                    self.i += 1;
                    // `;;`, `;&`, and `;;&` end a case branch.
                    while matches!(self.peek(), Some(';' | '&')) {
                        self.i += 1;
                    }
                    self.end_command(false);
                    self.after_operator = false;
                }
                '&' => self.ampersand(),
                '|' => {
                    self.end_word();
                    self.i += 1;
                    if self.peek() == Some('|') {
                        self.i += 1;
                        self.end_command(false);
                    } else {
                        if self.peek() == Some('&') {
                            self.i += 1;
                        }
                        self.end_command(true);
                    }
                    self.after_operator = true;
                }
                '(' | ')' => {
                    // A subshell, a function's `()`, or a case pattern's end.
                    self.end_word();
                    self.i += 1;
                    self.end_command(false);
                }
                '<' | '>' => self.angle(c),
                '#' if self.word.is_none() => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.i += 1;
                    }
                }
                '\\' => self.backslash(),
                '\'' => self.single_quote(),
                '"' => self.double_quote(),
                '`' => self.backquote(false),
                '$' => self.dollar(false),
                '*' | '?' | '[' => {
                    self.word().glob = true;
                    self.push(c, false);
                    self.i += 1;
                }
                '!' => {
                    self.bang();
                    self.push('!', false);
                    self.i += 1;
                }
                // `^old^new` reruns the last command with a substitution.
                '^' if self.word.is_none() => {
                    self.word().history = true;
                    self.push('^', false);
                    self.i += 1;
                }
                _ => {
                    self.push(c, false);
                    self.i += 1;
                }
            }
        }
        self.end_word();
        self.end_command(false);
        if !self.heredocs.is_empty() || self.after_operator {
            self.script.incomplete = true;
        }
    }

    /// The word being read, started here if there is none.
    fn word(&mut self) -> &mut WordBuf {
        let start = self.i;
        self.after_operator = false;
        self.word.get_or_insert_with(|| WordBuf {
            start,
            ..WordBuf::default()
        })
    }

    fn push(&mut self, c: char, quoted: bool) {
        self.word().chars.push((c, quoted));
    }

    fn push_str(&mut self, text: &str) {
        let word = self.word();
        word.chars.extend(text.chars().map(|c| (c, true)));
    }

    fn ampersand(&mut self) {
        self.end_word();
        self.i += 1;
        match self.peek() {
            Some('&') => {
                self.i += 1;
                self.end_command(false);
                self.after_operator = true;
            }
            Some('>') => {
                // `&>` and `&>>` send both outputs to a file.
                self.i += 1;
                if self.peek() == Some('>') {
                    self.i += 1;
                }
                self.redirect = Some(Pending::Redirect(RedirectKind::Write));
            }
            _ => {
                self.end_command(false);
                self.after_operator = false;
            }
        }
    }

    /// `<` or `>`: a redirection, or a process substitution.
    fn angle(&mut self, c: char) {
        if self.at(1) == Some('(') {
            let start = self.i;
            self.word().process = true;
            self.word().expanded = true;
            self.i += 2;
            let inner = self.group(')');
            self.script.nested.push(inner);
            let raw: String = self.s[start..self.i].iter().collect();
            self.push_str(&raw);
            return;
        }
        // A word of digits before the operator names a descriptor.
        let descriptor = self.word.as_ref().is_some_and(|w| {
            !w.chars.is_empty()
                && w.chars
                    .iter()
                    .all(|&(c, quoted)| !quoted && c.is_ascii_digit())
        });
        if descriptor {
            self.word = None;
        } else {
            self.end_word();
        }
        self.i += 1;
        let pending = match (c, self.peek()) {
            ('<', Some('<')) => {
                self.i += 1;
                match self.peek() {
                    Some('<') => {
                        self.i += 1;
                        Pending::HereString
                    }
                    Some('-') => {
                        self.i += 1;
                        Pending::HereDoc { strip_tabs: true }
                    }
                    _ => Pending::HereDoc { strip_tabs: false },
                }
            }
            ('<' | '>', Some('&')) => {
                self.i += 1;
                Pending::Redirect(RedirectKind::Dup)
            }
            ('<', Some('>')) | ('>', Some('>' | '|')) => {
                self.i += 1;
                Pending::Redirect(RedirectKind::Write)
            }
            ('<', _) => Pending::Redirect(RedirectKind::Read),
            _ => Pending::Redirect(RedirectKind::Write),
        };
        self.redirect = Some(pending);
    }

    fn backslash(&mut self) {
        match self.at(1) {
            // A line continuation joins the lines, even inside a word.
            Some('\n') => self.i += 2,
            Some(next) => {
                self.word();
                self.push(next, true);
                self.i += 2;
            }
            None => {
                self.script.incomplete = true;
                self.i += 1;
            }
        }
    }

    fn single_quote(&mut self) {
        self.word();
        self.i += 1;
        while let Some(c) = self.peek() {
            self.i += 1;
            if c == '\'' {
                return;
            }
            self.push(c, true);
        }
        self.script.incomplete = true;
    }

    fn double_quote(&mut self) {
        self.word();
        self.i += 1;
        while let Some(c) = self.peek() {
            match c {
                '"' => {
                    self.i += 1;
                    return;
                }
                '\\' => match self.at(1) {
                    Some('\n') => self.i += 2,
                    Some(next @ ('$' | '`' | '"' | '\\')) => {
                        self.push(next, true);
                        self.i += 2;
                    }
                    Some(next) => {
                        self.push('\\', true);
                        self.push(next, true);
                        self.i += 2;
                    }
                    None => self.i += 1,
                },
                '$' => self.dollar(true),
                '`' => self.backquote(true),
                '!' => {
                    self.bang();
                    self.push('!', true);
                    self.i += 1;
                }
                _ => {
                    self.push(c, true);
                    self.i += 1;
                }
            }
        }
        self.script.incomplete = true;
    }

    /// Bash expands `!` followed by these as history, outside single quotes.
    fn bang(&mut self) {
        if self
            .at(1)
            .is_some_and(|c| c.is_ascii_alphanumeric() || "!$^*?#-".contains(c))
        {
            self.word().history = true;
        }
    }

    fn backquote(&mut self, quoted: bool) {
        let start = self.i;
        self.word().expanded = true;
        self.word().split |= !quoted;
        self.i += 1;
        let mut inner = String::new();
        let mut closed = false;
        while let Some(c) = self.peek() {
            self.i += 1;
            match c {
                '`' => {
                    closed = true;
                    break;
                }
                '\\' => match self.peek() {
                    Some(next @ ('`' | '$' | '\\')) => {
                        inner.push(next);
                        self.i += 1;
                    }
                    _ => inner.push('\\'),
                },
                _ => inner.push(c),
            }
        }
        if !closed {
            self.script.incomplete = true;
        }
        self.script.nested.push(inner);
        let raw: String = self.s[start..self.i].iter().collect();
        self.push_str(&raw);
    }

    /// `$` inside a word: an expansion, ANSI-C quoting, or a plain `$`.
    fn dollar(&mut self, quoted: bool) {
        let start = self.i;
        match self.at(1) {
            Some('\'') if !quoted => {
                self.word();
                self.i += 2;
                self.ansi_c();
                return;
            }
            Some('"') if !quoted => {
                // Locale translation; reads as a double-quoted string.
                self.i += 1;
                self.double_quote();
                return;
            }
            Some('(') => {
                self.word().expanded = true;
                let arithmetic = self.at(2) == Some('(');
                self.i += 2;
                let inner = self.group(')');
                if arithmetic {
                    self.script.scanned.push(inner);
                } else {
                    self.script.nested.push(inner);
                }
            }
            Some('{') => {
                self.word().expanded = true;
                self.i += 2;
                let inner = self.group('}');
                self.script.scanned.push(inner);
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                self.word().expanded = true;
                self.i += 1;
                while self
                    .peek()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    self.i += 1;
                }
            }
            Some(c) if c.is_ascii_digit() || "@*#?-$!".contains(c) => {
                self.word().expanded = true;
                self.i += 2;
            }
            _ => {
                self.push('$', quoted);
                self.i += 1;
                return;
            }
        }
        self.word().split |= !quoted;
        let raw: String = self.s[start..self.i].iter().collect();
        self.push_str(&raw);
    }

    /// Reads to the `close` that matches an opening already consumed,
    /// skipping quotes and nested pairs. Returns the text between them.
    fn group(&mut self, close: char) -> String {
        let open = if close == ')' { '(' } else { '{' };
        let start = self.i;
        let mut depth = 1usize;
        while let Some(c) = self.peek() {
            self.i += 1;
            match c {
                '\\' => self.i += 1,
                '\'' => {
                    while self.peek().is_some_and(|c| c != '\'') {
                        self.i += 1;
                    }
                    self.i += 1;
                }
                '"' => {
                    while let Some(c) = self.peek() {
                        self.i += 1;
                        match c {
                            '\\' => self.i += 1,
                            '"' => break,
                            _ => {}
                        }
                    }
                }
                '`' => {
                    while let Some(c) = self.peek() {
                        self.i += 1;
                        match c {
                            '\\' => self.i += 1,
                            '`' => break,
                            _ => {}
                        }
                    }
                }
                c if c == open => depth += 1,
                c if c == close => {
                    depth -= 1;
                    if depth == 0 {
                        return self.s[start..self.i - 1].iter().collect();
                    }
                }
                _ => {}
            }
        }
        self.i = self.i.min(self.s.len());
        self.script.incomplete = true;
        self.s[start..].iter().collect()
    }

    /// Decodes `$'...'` after the opening quote, as bash does.
    fn ansi_c(&mut self) {
        while let Some(c) = self.peek() {
            self.i += 1;
            match c {
                '\'' => return,
                '\\' => {
                    if let Some(decoded) = self.ansi_escape() {
                        self.push(decoded, true);
                    }
                }
                _ => self.push(c, true),
            }
        }
        self.script.incomplete = true;
    }

    fn ansi_escape(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += 1;
        let simple = match c {
            'a' => Some('\u{7}'),
            'b' => Some('\u{8}'),
            'e' | 'E' => Some('\u{1b}'),
            'f' => Some('\u{c}'),
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            'v' => Some('\u{b}'),
            '\\' | '\'' | '"' | '?' => Some(c),
            _ => None,
        };
        if simple.is_some() {
            return simple;
        }
        match c {
            '0'..='7' => {
                let mut value = c.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    match self.peek().and_then(|d| d.to_digit(8)) {
                        Some(d) => {
                            value = value * 8 + d;
                            self.i += 1;
                        }
                        None => break,
                    }
                }
                char::from_u32(value & 0xff)
            }
            'x' => self.hex_escape(2),
            'u' => self.hex_escape(4),
            'U' => self.hex_escape(8),
            'c' => {
                let control = self.peek()?;
                self.i += 1;
                char::from_u32(u32::from(control) & 0x1f)
            }
            _ => {
                // Unknown escapes stay as written.
                self.push('\\', true);
                Some(c)
            }
        }
    }

    fn hex_escape(&mut self, max: usize) -> Option<char> {
        let mut value = 0u32;
        let mut digits = 0;
        while digits < max {
            match self.peek().and_then(|d| d.to_digit(16)) {
                Some(d) => {
                    value = value * 16 + d;
                    self.i += 1;
                    digits += 1;
                }
                None => break,
            }
        }
        if digits == 0 {
            self.push('\\', true);
            return Some(if max == 2 { 'x' } else { 'u' });
        }
        char::from_u32(value)
    }

    fn end_word(&mut self) {
        let Some(buf) = self.word.take() else {
            return;
        };
        let raw: String = self.s[buf.start..self.i.min(self.s.len())].iter().collect();
        let flags = Word {
            text: String::new(),
            raw,
            expanded: buf.expanded,
            glob: buf.glob,
            history: buf.history,
            process: buf.process,
            split: buf.split,
        };
        if let Some(pending) = self.redirect.take() {
            let target = Word {
                text: buf.chars.iter().map(|&(c, _)| c).collect(),
                ..flags
            };
            self.redirected(pending, target);
            return;
        }
        match brace_expand(&buf.chars) {
            Some(texts) => {
                for text in texts {
                    self.command.words.push(Word {
                        text,
                        ..flags.clone()
                    });
                }
            }
            None => self.command.words.push(Word {
                text: buf.chars.iter().map(|&(c, _)| c).collect(),
                expanded: true,
                ..flags
            }),
        }
    }

    fn redirected(&mut self, pending: Pending, target: Word) {
        match pending {
            Pending::Redirect(kind) => {
                // `>&file` writes to a file; `>&2` and `>&-` duplicate.
                let kind = if kind == RedirectKind::Dup
                    && !target.text.is_empty()
                    && !target.text.chars().all(|c| c.is_ascii_digit() || c == '-')
                {
                    RedirectKind::Write
                } else {
                    kind
                };
                self.command.redirects.push(Redirect { kind, target });
            }
            Pending::HereDoc { strip_tabs } => self.heredocs.push(HereDoc {
                command: self.script.commands.len(),
                expands: !target.raw.contains(['\'', '"', '\\']),
                delimiter: target.text,
                strip_tabs,
            }),
            Pending::HereString => {
                let input = self.command.input.get_or_insert_with(String::new);
                input.push_str(&target.text);
                input.push('\n');
            }
        }
    }

    fn end_command(&mut self, pipe: bool) {
        self.redirect = None;
        let stage = if pipe { self.command.stage + 1 } else { 0 };
        let command = std::mem::take(&mut self.command);
        self.command.stage = stage;
        if !command.words.is_empty() || !command.redirects.is_empty() || command.input.is_some() {
            self.script.commands.push(command);
        } else if pipe {
            // `| cmd` with nothing before it still feeds cmd a pipe.
            self.command.stage = stage.max(1);
        }
    }

    /// Reads the bodies of the here-documents started on the line just ended.
    fn read_heredocs(&mut self) {
        for doc in std::mem::take(&mut self.heredocs) {
            let mut body = String::new();
            let mut closed = false;
            while self.i < self.s.len() {
                let end = self.s[self.i..]
                    .iter()
                    .position(|&c| c == '\n')
                    .map_or(self.s.len(), |p| self.i + p);
                let line: String = self.s[self.i..end].iter().collect();
                self.i = (end + 1).min(self.s.len());
                let compared = if doc.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line.as_str()
                };
                if compared == doc.delimiter {
                    closed = true;
                    break;
                }
                body.push_str(compared);
                body.push('\n');
            }
            if !closed {
                self.script.incomplete = true;
            }
            if doc.expands {
                // Quotes are plain text in a body; `$(...)` inside them runs.
                let body: String = body.chars().filter(|c| !matches!(c, '\'' | '"')).collect();
                self.script.scanned.push(body);
            }
            let target = if doc.command < self.script.commands.len() {
                &mut self.script.commands[doc.command]
            } else {
                &mut self.command
            };
            target.input.get_or_insert_with(String::new).push_str(&body);
        }
    }
}

/// Bash brace expansion over unquoted `{a,b}` and `{x..y}`. Returns `None`
/// when the word is too long or would expand to too many words.
fn brace_expand(chars: &[(char, bool)]) -> Option<Vec<String>> {
    if !chars.iter().any(|&(c, quoted)| c == '{' && !quoted) {
        return Some(vec![chars.iter().map(|&(c, _)| c).collect()]);
    }
    if chars.len() > MAX_BRACE_WORD {
        return None;
    }
    let mut out = Vec::new();
    let mut steps = 0;
    expand_into(chars, &mut out, &mut steps).then_some(out)
}

fn expand_into(chars: &[(char, bool)], out: &mut Vec<String>, steps: &mut usize) -> bool {
    *steps += 1;
    if *steps > MAX_BRACE_STEPS {
        return false;
    }
    let Some((open, close, alternatives)) = first_brace(chars) else {
        out.push(chars.iter().map(|&(c, _)| c).collect());
        return out.len() <= MAX_BRACE_WORDS;
    };
    let Braces::Expand(alternatives) = alternatives else {
        return false;
    };
    for alternative in alternatives {
        let mut next = chars[..open].to_vec();
        next.extend(alternative);
        next.extend_from_slice(&chars[close + 1..]);
        if !expand_into(&next, out, steps) {
            return false;
        }
    }
    true
}

type Alternatives = Vec<Vec<(char, bool)>>;

enum Braces {
    Expand(Alternatives),
    /// A sequence with more than `MAX_BRACE_WORDS` terms.
    TooMany,
}

/// The leftmost unquoted `{...}` that expands, with what it expands to.
/// Pairs braces with a stack in one pass: O(L).
fn first_brace(chars: &[(char, bool)]) -> Option<(usize, usize, Braces)> {
    // Each open brace, with the commas at its own level.
    let mut stack: Vec<(usize, Vec<usize>)> = Vec::new();
    let mut best: Option<(usize, usize, Vec<usize>)> = None;
    for (at, &(c, quoted)) in chars.iter().enumerate() {
        if quoted {
            continue;
        }
        match c {
            '{' => stack.push((at, Vec::new())),
            ',' => {
                if let Some((_, commas)) = stack.last_mut() {
                    commas.push(at);
                }
            }
            '}' => {
                let Some((open, commas)) = stack.pop() else {
                    continue;
                };
                if best.as_ref().is_some_and(|(b, _, _)| *b < open) {
                    continue;
                }
                let inner = &chars[open + 1..at];
                let expands = !commas.is_empty()
                    || (inner.len() <= MAX_SEQUENCE && sequence(inner).is_some());
                if expands {
                    best = Some((open, at, commas));
                }
            }
            _ => {}
        }
    }
    let (open, close, commas) = best?;
    if commas.is_empty() {
        return Some((open, close, sequence(&chars[open + 1..close])?));
    }
    let mut alternatives = Vec::with_capacity(commas.len() + 1);
    let mut from = open + 1;
    for &comma in &commas {
        alternatives.push(chars[from..comma].to_vec());
        from = comma + 1;
    }
    alternatives.push(chars[from..close].to_vec());
    Some((open, close, Braces::Expand(alternatives)))
}

/// `{a..e}`, `{1..9}`, or `{1..9..2}`; `None` when `inner` is not one.
fn sequence(inner: &[(char, bool)]) -> Option<Braces> {
    let text: String = inner.iter().map(|&(c, _)| c).collect();
    let parts: Vec<&str> = text.split("..").collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let step = match parts.get(2) {
        Some(step) => step.parse::<i64>().ok()?.unsigned_abs().max(1),
        None => 1,
    };
    let literal = |s: String| s.chars().map(|c| (c, true)).collect::<Vec<_>>();
    // `None` for too many terms, or for arithmetic that would overflow.
    let terms = |from: i64, to: i64| -> Option<Vec<i64>> {
        let count = (from.abs_diff(to) / step).checked_add(1)?;
        if count > MAX_BRACE_WORDS as u64 {
            return None;
        }
        let delta = i64::try_from(step).ok()?;
        let mut terms = Vec::new();
        let mut value = Some(from);
        for _ in 0..count {
            let current = value?;
            terms.push(current);
            value = if from <= to {
                current.checked_add(delta)
            } else {
                current.checked_sub(delta)
            };
        }
        Some(terms)
    };
    if let (Ok(from), Ok(to)) = (parts[0].parse::<i64>(), parts[1].parse::<i64>()) {
        return Some(terms(from, to).map_or(Braces::TooMany, |terms| {
            Braces::Expand(terms.into_iter().map(|n| literal(n.to_string())).collect())
        }));
    }
    let single = |s: &str| {
        let mut chars = s.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_alphabetic() => Some(c),
            _ => None,
        }
    };
    let (from, to) = (single(parts[0])?, single(parts[1])?);
    Some(
        terms(i64::from(u32::from(from)), i64::from(u32::from(to))).map_or(
            Braces::TooMany,
            |terms| {
                Braces::Expand(
                    terms
                        .into_iter()
                        .filter_map(|n| char::from_u32(u32::try_from(n).ok()?))
                        .map(|c| literal(c.to_string()))
                        .collect(),
                )
            },
        ),
    )
}
