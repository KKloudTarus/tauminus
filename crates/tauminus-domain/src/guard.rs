// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The prod guard: decides whether a command line about to run on a guarded
//! host needs the user's confirmation.
//!
//! The guard over-approximates. It reads every simple command the line may
//! run, including those inside `$(...)`, backquotes, `bash -c`, `eval`,
//! `ssh host '...'`, here-documents, and the code strings of `python -c` and
//! friends, and checks each against the rules. A command whose name it
//! cannot know before the shell runs, such as `$X -rf /`, is flagged rather
//! than guessed. A false alarm costs one click; a miss can cost a server.
//!
//! What it cannot see: aliases and functions defined on the server, scripts
//! run by file name, and programs that delete data without a recognizable
//! command line.

mod lex;
mod sql;

use std::fmt;

use thiserror::Error;

use lex::{Command, RedirectKind, Word};

/// Longest line the guard reads. Longer lines are flagged unread.
pub const MAX_LINE: usize = 64 * 1024;
/// Most allow or deny patterns on one host or group.
pub const MAX_PATTERNS: usize = 100;
const MAX_PATTERN: usize = 256;
/// Most `*` in one pattern, which bounds backtracking.
const MAX_STARS: usize = 16;
/// Deepest nesting of scripts inside scripts, such as `bash -c "$(...)"`.
const MAX_DEPTH: usize = 8;
/// Most allow and deny patterns a host's policy may gather from its groups.
/// Past it every line asks: checking would cost too much to promise an answer.
pub const MAX_POLICY_PATTERNS: usize = 400;
/// Most simple commands one line may hold before it is flagged unread.
const MAX_COMMANDS: usize = 4096;
/// Longest command shown in a finding, in characters.
const MAX_SHOWN: usize = 200;
/// Findings after which the guard stops reading: the line needs
/// confirmation either way, and the dialog lists only so many.
const MAX_FINDINGS: usize = 64;
/// Arguments read after a word that is only possibly a command name. Keeps
/// reading every word as a name O(w) rather than O(w²).
const SCAN_ARGS: usize = 64;
/// Characters the guard may lex for one line, across every nested script.
/// Past it the line is flagged unread, so no input costs more than O(8n).
const MAX_WORK: usize = 8 * MAX_LINE;
/// Character comparisons pattern matching may make for one line: tens of
/// milliseconds at most.
const MAX_MATCH_WORK: usize = 64 * 1024 * 1024;
/// What a pattern match costs before it reads a character, in characters.
const PAIR_COST: usize = 256;

/// Why a command needs confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rule {
    /// `rm -r`, `rm --no-preserve-root`, `find -delete`.
    RecursiveDelete,
    /// SQL `DROP`, MongoDB `dropDatabase()`.
    DropData,
    /// SQL `TRUNCATE`, Redis `FLUSHALL` and `FLUSHDB`.
    EmptyData,
    /// `DELETE FROM` without `WHERE`, MongoDB `deleteMany({})`.
    DeleteAll,
    /// `kubectl delete`, `helm uninstall`.
    KubernetesDelete,
    /// `terraform destroy`, `terraform apply -destroy`, `pulumi destroy`.
    InfraDestroy,
    /// `shutdown`, `reboot`, `systemctl poweroff`, `init 0`, sysrq.
    PowerOff,
    /// `mkfs`, `mkswap`, `wipefs`.
    FormatDisk,
    /// `dd of=/dev/sda`, `> /dev/sda`, `tee /dev/sda`.
    DiskWrite,
    /// `chmod -R`, `chown -R`, or `chgrp -R` on `/` or a top-level directory.
    OpenPermissions,
    /// Text piped or fed into a shell, which runs whatever it says.
    PipeToShell,
    /// The command's name, or a script it runs, is only known when the
    /// shell expands it: `$X`, `$(...)`, a glob, history.
    Opaque,
    /// Too long or too deeply nested to read.
    Unreadable,
    /// Matched one of the host's deny patterns.
    Denied,
}

impl Rule {
    /// A stable name for the view and for tests.
    pub fn id(self) -> &'static str {
        match self {
            Self::RecursiveDelete => "recursive-delete",
            Self::DropData => "drop-data",
            Self::EmptyData => "empty-data",
            Self::DeleteAll => "delete-all",
            Self::KubernetesDelete => "kubernetes-delete",
            Self::InfraDestroy => "infra-destroy",
            Self::PowerOff => "power-off",
            Self::FormatDisk => "format-disk",
            Self::DiskWrite => "disk-write",
            Self::OpenPermissions => "open-permissions",
            Self::PipeToShell => "pipe-to-shell",
            Self::Opaque => "opaque",
            Self::Unreadable => "unreadable",
            Self::Denied => "denied",
        }
    }

    /// What the rule protects against, for the confirmation dialog.
    pub fn reason(self) -> &'static str {
        match self {
            Self::RecursiveDelete => "deletes files recursively",
            Self::DropData => "drops a database object",
            Self::EmptyData => "empties a table or database",
            Self::DeleteAll => "deletes every row",
            Self::KubernetesDelete => "deletes Kubernetes resources",
            Self::InfraDestroy => "destroys infrastructure",
            Self::PowerOff => "shuts down or reboots the machine",
            Self::FormatDisk => "formats a disk",
            Self::DiskWrite => "writes to a disk device",
            Self::OpenPermissions => "changes permissions or owners recursively from /",
            Self::PipeToShell => "runs piped text as a script",
            Self::Opaque => "runs a command whose name is only known when the shell expands it",
            Self::Unreadable => "is too long or too nested to check",
            Self::Denied => "matches a deny pattern",
        }
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PatternError {
    #[error("a pattern must be 1 to 256 characters with no control characters and at most 16 *")]
    Invalid,
    #[error("at most {MAX_PATTERNS} allow and {MAX_PATTERNS} deny patterns")]
    TooMany,
    #[error("an allow pattern must start with a command name, such as `rm -rf /tmp/*`")]
    Broad,
}

/// A command pattern such as `rm -rf /tmp/build/*`. `*` matches any run of
/// characters and `?` one character; everything else matches itself.
/// Patterns match the command as the shell would run it, after quotes are
/// removed and `sudo`, `env`, and similar wrappers, so `sudo "rm" -rf x`
/// matches `rm -rf x`.
#[derive(Debug, Clone)]
pub struct Pattern {
    text: String,
    chars: Vec<char>,
    /// The literal runs between `*`, for matching without backtracking when
    /// the pattern has no `?`.
    segments: Vec<String>,
    question: bool,
    /// The pattern split at spaces, for matching word by word.
    words: Vec<Vec<char>>,
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl Eq for Pattern {}

impl PartialOrd for Pattern {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Pattern {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.text.cmp(&other.text)
    }
}

impl std::hash::Hash for Pattern {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.text.hash(state);
    }
}

impl Pattern {
    pub fn parse(text: &str) -> Result<Self, PatternError> {
        let text = text.trim();
        if text.is_empty()
            || text.chars().count() > MAX_PATTERN
            || text.chars().any(char::is_control)
            || text.matches('*').count() > MAX_STARS
        {
            return Err(PatternError::Invalid);
        }
        Ok(Self {
            text: text.to_owned(),
            chars: text.chars().collect(),
            segments: text.split('*').map(str::to_owned).collect(),
            question: text.contains('?'),
            words: text
                .split_whitespace()
                .map(|w| w.chars().collect())
                .collect(),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Glob match over the whole of `command`, where `*` may cross words.
    /// Deny patterns match this way, so they match more rather than less.
    pub fn matches(&self, command: &str) -> bool {
        self.literal_match(command)
            .unwrap_or_else(|| glob(&self.chars, &command.chars().collect::<Vec<_>>()))
    }

    /// Matches a pattern of literal runs and `*` without backtracking: the
    /// first run is a prefix, the last a suffix, and each run between is
    /// found leftmost after the one before, which is the leftmost match any
    /// `*` could make. O(n·k) for k ≤ 17 runs. `None` when the pattern has
    /// a `?`.
    fn literal_match(&self, text: &str) -> Option<bool> {
        if self.question {
            return None;
        }
        let (first, rest) = self.segments.split_first()?;
        let Some((last, middle)) = rest.split_last() else {
            return Some(text == first);
        };
        if text.len() < first.len() + last.len()
            || !text.starts_with(first.as_str())
            || !text.ends_with(last.as_str())
        {
            return Some(false);
        }
        let mut inner = &text[first.len()..text.len() - last.len()];
        for run in middle.iter().filter(|run| !run.is_empty()) {
            match inner.find(run.as_str()) {
                Some(at) => inner = &inner[at + run.len()..],
                None => return Some(false),
            }
        }
        Some(true)
    }

    /// Characters a match against a text of `text_len` bytes may compare.
    /// Each pattern and text pair also costs a fixed amount, counted as
    /// `PAIR_COST` characters, from setting up each search.
    fn cost(&self, text_len: usize) -> usize {
        if self.question {
            text_len
                .saturating_add(PAIR_COST)
                .saturating_mul(self.chars.len())
        } else {
            text_len
                .saturating_add(PAIR_COST)
                .saturating_mul(self.segments.len())
        }
    }

    /// Word-by-word match: the same number of words, each matching, so `*`
    /// never crosses a space. Allow patterns match this way, against words
    /// whose paths are already normalized, so `rm -rf /tmp/*` allows neither
    /// `rm -rf /tmp/x /` nor `rm -rf /tmp/../etc`.
    fn matches_words(&self, words: &[Vec<char>]) -> bool {
        self.words.len() == words.len() && self.words.iter().zip(words).all(|(p, w)| glob(p, w))
    }

    /// The first word names the command, with no wildcard.
    fn names_command(&self) -> bool {
        self.words
            .first()
            .is_some_and(|w| !w.iter().any(|c| matches!(c, '*' | '?')))
    }
}

/// Glob match of `text` against `pattern`. O(n·m) worst case for n text and
/// m pattern characters, from backtracking to the last `*`.
fn glob(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == '?' || c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    p = sp + 1;
                    t = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// Guard settings a host or group may set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuardSettings {
    /// `None` inherits; with no level setting it, the guard is on for
    /// hosts tagged `prod`.
    pub enabled: Option<bool>,
    /// Commands that never need confirmation from the built-in rules.
    allow: Vec<Pattern>,
    /// Commands that always need confirmation.
    deny: Vec<Pattern>,
}

impl GuardSettings {
    /// Sorted and without repeats. Fails past [`MAX_PATTERNS`] each, and
    /// on an allow pattern that does not start with a command name: `*`
    /// alone would turn every rule off.
    pub fn new(
        enabled: Option<bool>,
        mut allow: Vec<Pattern>,
        mut deny: Vec<Pattern>,
    ) -> Result<Self, PatternError> {
        if !allow.iter().all(Pattern::names_command) {
            return Err(PatternError::Broad);
        }
        for list in [&mut allow, &mut deny] {
            list.sort();
            list.dedup();
            if list.len() > MAX_PATTERNS {
                return Err(PatternError::TooMany);
            }
        }
        Ok(Self {
            enabled,
            allow,
            deny,
        })
    }

    pub fn allow(&self) -> &[Pattern] {
        &self.allow
    }

    pub fn deny(&self) -> &[Pattern] {
        &self.deny
    }

    pub fn is_empty(&self) -> bool {
        self.enabled.is_none() && self.allow.is_empty() && self.deny.is_empty()
    }

    /// Adds `parent`'s patterns and its `enabled` if unset, as a host
    /// inherits them. Fails, leaving `self` as it was, when a list would
    /// pass [`MAX_PATTERNS`].
    pub(crate) fn inherit(&mut self, parent: &Self) -> Result<(), PatternError> {
        let merge = |mine: &[Pattern], theirs: &[Pattern]| {
            let mut all: Vec<Pattern> = mine.iter().chain(theirs).cloned().collect();
            all.sort();
            all.dedup();
            if all.len() > MAX_PATTERNS {
                Err(PatternError::TooMany)
            } else {
                Ok(all)
            }
        };
        let allow = merge(&self.allow, &parent.allow)?;
        let deny = merge(&self.deny, &parent.deny)?;
        self.allow = allow;
        self.deny = deny;
        if self.enabled.is_none() {
            self.enabled = parent.enabled;
        }
        Ok(())
    }
}

/// The guard as it applies to one host: whether it is on, and the patterns
/// of the host and all its groups.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    pub active: bool,
    pub allow: Vec<Pattern>,
    pub deny: Vec<Pattern>,
}

/// One reason a line needs confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: Rule,
    /// The command the rule matched, as the shell would run it. At most 200
    /// characters.
    pub command: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdict {
    /// Empty when the line may run without asking.
    pub findings: Vec<Finding>,
    /// The shell will ask for more before it runs anything: the next line
    /// continues this one.
    pub incomplete: bool,
}

impl Policy {
    /// Checks `line`, which may hold several lines of a script. O(n·d) for n
    /// characters and nesting depth d ≤ 8, plus pattern matching.
    pub fn check(&self, line: &str) -> Verdict {
        self.check_all(&[line])
    }

    /// Checks several readings of one line, such as the line as typed and
    /// the whole screen line, under one shared budget: findings from all of
    /// them count, and `incomplete` comes from the first. The cost of the
    /// whole call is bounded as one line's is.
    pub fn check_all(&self, lines: &[&str]) -> Verdict {
        if !self.active || lines.is_empty() {
            return Verdict::default();
        }
        let patterns = self.allow.len() + self.deny.len();
        if patterns > MAX_POLICY_PATTERNS {
            return Verdict {
                findings: vec![Finding {
                    rule: Rule::Unreadable,
                    command: format!(
                        "this host's guard has {patterns} patterns; at most {MAX_POLICY_PATTERNS} can be checked"
                    ),
                }],
                incomplete: false,
            };
        }
        let mut analysis = Analysis {
            policy: self,
            findings: Vec::new(),
            commands: 0,
            quoted: false,
            lenient: false,
            work: 0,
            matching: 0,
            done: false,
        };
        let mut incomplete = false;
        for (index, line) in lines.iter().enumerate() {
            if line.len() > MAX_LINE {
                analysis.flag(Rule::Unreadable, line);
                break;
            }
            let open = analysis.script(line, 0);
            if open {
                // The screen may show a line the shell does not hold as
                // typed, such as bash's reverse search; read it again with
                // quotes ignored, so nothing inside an unclosed quote hides.
                analysis.script(&unquoted(line), 0);
            }
            if index == 0 {
                incomplete = open;
            }
        }
        let mut findings = analysis.findings;
        findings.sort_by(|a, b| (a.rule, &a.command).cmp(&(b.rule, &b.command)));
        findings.dedup();
        Verdict {
            findings,
            incomplete,
        }
    }
}

struct Analysis<'a> {
    policy: &'a Policy,
    findings: Vec<Finding>,
    commands: usize,
    /// Reading a quoted argument as a possible script: only rules that need
    /// arguments apply, as for a word in the middle of a command.
    quoted: bool,
    /// Reading code whose words may be expansions by design, such as an awk
    /// program or a hook: a name the shell expands is not flagged there.
    lenient: bool,
    /// Characters lexed so far, against `MAX_WORK`.
    work: usize,
    /// Characters compared by pattern matching, against `MAX_MATCH_WORK`.
    matching: usize,
    /// Set once the line is flagged unread: the verdict cannot change.
    done: bool,
}

impl Analysis<'_> {
    /// Records a finding no allow pattern can silence: a redirect, a whole
    /// line, an opaque name.
    fn flag(&mut self, rule: Rule, command: &str) {
        self.record(rule, command, None);
    }

    /// Records a finding about the command `words`, unless an allow pattern
    /// matches it. Only literal words can match: an expansion, a glob, `~`,
    /// or a `..` part may name something other than the pattern says, as
    /// `rm -rf /tmp/${X:-../etc}` does.
    fn flag_command(&mut self, rule: Rule, words: &[Word]) {
        self.record(rule, &join(words), Some(words));
    }

    fn record(&mut self, rule: Rule, command: &str, words: Option<&[Word]>) {
        if self.findings.len() >= MAX_FINDINGS || self.done {
            return;
        }
        if rule == Rule::Unreadable {
            self.done = true;
        }
        // Backtracking compares each word with each pattern word.
        let allow_cost = words.map_or(0, |w| {
            let len: usize = w.iter().map(|w| w.text.len()).sum::<usize>() + PAIR_COST;
            self.policy
                .allow
                .iter()
                .filter(|p| p.words.len() == w.len())
                .map(|p| len.saturating_mul(p.chars.len()))
                .fold(0, usize::saturating_add)
        });
        let allowed = !matches!(rule, Rule::Denied | Rule::Unreadable)
            && !self.policy.allow.is_empty()
            && self.charge(allow_cost)
            && words.is_some_and(|words| {
                let literal = words.iter().all(|w| {
                    !w.opaque()
                        && !w.text.starts_with('~')
                        && !w.text.split('/').any(|part| part == "..")
                });
                let chars: Vec<Vec<char>> = words
                    .iter()
                    .map(|w| normalize_path(&w.text).chars().collect())
                    .collect();
                literal && self.policy.allow.iter().any(|p| p.matches_words(&chars))
            });
        let finding = Finding {
            rule,
            command: shown(command),
        };
        // Repeats would fill the cap and crowd out later reasons. O(f), f ≤ 64.
        if !allowed && !self.findings.contains(&finding) {
            self.findings.push(finding);
        }
    }

    /// Whether a deny pattern matches any of `commands`. Matching counts
    /// against `MAX_MATCH_WORK`; past it the line is flagged unread.
    fn denied(&mut self, commands: &[&str]) -> bool {
        if self.policy.deny.is_empty() {
            return false;
        }
        let cost: usize = commands
            .iter()
            .map(|c| {
                self.policy
                    .deny
                    .iter()
                    .map(|p| p.cost(c.len()))
                    .sum::<usize>()
            })
            .sum();
        if !self.charge(cost) {
            return false;
        }
        commands
            .iter()
            .any(|command| self.policy.deny.iter().any(|p| p.matches(command)))
    }

    /// Counts pattern matching against its budget. False once it is spent.
    fn charge(&mut self, cost: usize) -> bool {
        self.matching = self.matching.saturating_add(cost);
        if self.matching > MAX_MATCH_WORK {
            self.flag(
                Rule::Unreadable,
                "the host's patterns cost too much to match on this line",
            );
            return false;
        }
        !self.done
    }

    /// Counts `text` against the work budget. False once it is spent.
    fn spend(&mut self, text: &str, depth: usize) -> bool {
        if depth > MAX_DEPTH {
            self.flag(Rule::Unreadable, text);
            return false;
        }
        self.spend_size(text.len())
    }

    fn spend_size(&mut self, size: usize) -> bool {
        if self.done {
            return false;
        }
        self.work = self.work.saturating_add(size);
        if self.work > MAX_WORK {
            self.flag(Rule::Unreadable, "the line is too large to check");
            return false;
        }
        true
    }

    /// Reads `text` as a script. Returns whether it was incomplete.
    fn script(&mut self, text: &str, depth: usize) -> bool {
        if !self.spend(text, depth) {
            return false;
        }
        let script = lex::lex(text);
        let line = collapse(text);
        for rule in sql::scan(text) {
            self.flag(rule, &line);
        }
        if self.denied(&[&line]) {
            self.flag(Rule::Denied, &line);
        }
        for nested in &script.nested {
            self.script(nested, depth + 1);
        }
        for scanned in &script.scanned {
            self.substitutions(scanned, depth + 1);
        }
        for command in &script.commands {
            self.commands += 1;
            // Brace expansion can make the words longer than the text.
            let expanded: usize = command.words.iter().map(|w| w.text.len()).sum();
            if !self.spend_size(expanded) {
                break;
            }
            if self.findings.len() >= MAX_FINDINGS || self.done {
                break;
            }
            if self.commands > MAX_COMMANDS {
                self.flag(Rule::Unreadable, text);
                break;
            }
            for words in segments(&command.words) {
                self.command(command, words, depth);
            }
            self.redirects(command);
        }
        script.incomplete
    }

    /// Reads the substitutions inside `${...}` or `$((...))`, whose own
    /// words never run.
    fn substitutions(&mut self, text: &str, depth: usize) {
        if !self.spend(text, depth) {
            return;
        }
        let script = lex::lex(text);
        for nested in &script.nested {
            self.script(nested, depth + 1);
        }
        for scanned in &script.scanned {
            self.substitutions(scanned, depth + 1);
        }
    }

    /// Reads code for another language, such as `python -c`, as if it were
    /// shell with its quotes gone: `os.system('rm -rf /')` then shows
    /// `rm -rf /` as a command.
    fn foreign(&mut self, code: &str, depth: usize) {
        // Library calls that delete a tree: Python, Ruby, Node, Perl, Go.
        const TREE_DELETES: &[&str] = &[
            "rmtree",
            "rm_rf",
            "rm_r(",
            "rmsync",
            "rimraf",
            "remove_tree",
            "removeall(",
            "remove_dir_all",
            "recursive:true",
            "recursive: true",
        ];
        let folded = code.to_ascii_lowercase();
        if TREE_DELETES.iter().any(|call| folded.contains(call)) {
            self.flag(Rule::RecursiveDelete, code);
        }
        // Lists such as `["rm", "-rf", "/"]` hold one argument per item.
        let code: String = unquoted(code)
            .chars()
            .map(|c| if matches!(c, '[' | ']' | ',') { ' ' } else { c })
            .collect();
        let lenient = std::mem::replace(&mut self.lenient, true);
        self.script(&code, depth + 1);
        self.lenient = lenient;
    }

    fn redirects(&mut self, command: &Command) {
        for redirect in &command.redirects {
            if redirect.kind != RedirectKind::Write {
                continue;
            }
            let target = normalize_path(&redirect.target.text);
            let shown = format!("> {}", redirect.target.text);
            if target == "/proc/sysrq-trigger" {
                self.flag(Rule::PowerOff, &shown);
            } else if is_device(&target) {
                self.flag(Rule::DiskWrite, &shown);
            }
        }
    }

    fn command(&mut self, command: &Command, words: &[Word], depth: usize) {
        let start = command_start(words);
        let effective = unwrap(words, start);
        // Assignments alone, such as `PROMPT_COMMAND='...'`, still hold code.
        self.deferred(words, effective, depth);
        // `[[ ... ]]` is a test: its words never run, and substitutions
        // inside it were read already.
        if words.get(start).is_none_or(|w| w.raw == "[[") {
            return;
        }
        let joined = join(&words[start..]);
        for rule in sql::scan(&joined) {
            self.flag(rule, &joined);
        }
        if let Some(effective) = effective {
            let word = &words[effective];
            if names_unknown(word) && !self.quoted && !self.lenient {
                self.flag(Rule::Opaque, &join(&words[effective..]));
            }
            let from = join(&words[effective..]);
            if self.denied(&[&from, &joined]) {
                self.flag(Rule::Denied, &from);
            }
        }
        // Every word is read as a possible command name too, so a wrapper
        // the guard does not know, such as `docker exec web rm -rf /`,
        // still shows its command. A name that is dangerous with no
        // arguments, such as `reboot`, counts there only after a word that
        // runs what follows, or `grep reboot` would ask.
        let mut runs = false;
        for at in start..words.len() {
            if self.done {
                return;
            }
            let word = &words[at];
            let scanned = Some(at) != effective;
            // `git rm` is git's own command, not rm.
            let git = at > start && basename(&words[at - 1].text) == "git";
            if !word.opaque() && !git {
                let end = if scanned {
                    words.len().min(at + 1 + SCAN_ARGS)
                } else {
                    words.len()
                };
                self.at(command, &words[at..end], depth, scanned && !runs);
            }
            runs |= word.text == "--" || RUNNERS.contains(&basename(&word.text));
        }
    }

    /// Code the shell runs later or elsewhere, kept in a word: a hook such as
    /// `PROMPT_COMMAND='rm -rf /'`, substitutions inside quotes that may be
    /// expanded later (`PS1`, array subscripts), and quoted arguments with
    /// spaces, which programs such as `screen` or `at` may run.
    fn deferred(&mut self, words: &[Word], effective: Option<usize>, depth: usize) {
        // Commands whose quoted arguments are text, not scripts.
        const TEXT: &[&str] = &[
            "echo",
            "printf",
            "grep",
            "egrep",
            "fgrep",
            "rg",
            "ag",
            "ack",
            "git",
            "hg",
            "svn",
            "notify-send",
            "logger",
            "wall",
            "jq",
            "yq",
            "sed",
            "awk",
            "gawk",
            "man",
            "less",
        ];
        let prints = effective
            .and_then(|e| words.get(e))
            .is_some_and(|w| TEXT.contains(&basename(&w.text)));
        for word in words {
            // Hooks bash runs as scripts later.
            if let Some(("PROMPT_COMMAND" | "BASH_ENV" | "ENV", value)) = assignment(word) {
                // `$PROMPT_COMMAND` inside it is the old hook, not a command to guess.
                let lenient = std::mem::replace(&mut self.lenient, true);
                self.script(value, depth + 1);
                self.lenient = lenient;
            }
            if !word.expanded && (word.text.contains("$(") || word.text.contains('`')) {
                self.substitutions(&word.text, depth + 1);
            }
            if !prints && !word.opaque() && word.text.contains(char::is_whitespace) {
                let quoted = std::mem::replace(&mut self.quoted, true);
                self.script(&word.text, depth + 1);
                self.quoted = quoted;
            }
        }
    }

    /// Checks `words[0]` as a command name with the rest as its arguments.
    ///
    /// `bare` is set where the name is only an argument to something else,
    /// as in `man shutdown`: only rules that need arguments apply.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per command family reads better than a table of closures"
    )]
    fn at(&mut self, command: &Command, words: &[Word], depth: usize, bare: bool) {
        let bare = bare || self.quoted;
        let name = basename(&words[0].text);
        let args = &words[1..];
        let shown = join(words);
        let flag = |this: &mut Self, rule| this.flag_command(rule, words);
        match name {
            "rm" if rm_recursive(args) => flag(self, Rule::RecursiveDelete),
            "find" if args.iter().any(|w| w.text == "-delete") => {
                flag(self, Rule::RecursiveDelete);
            }
            "chmod" | "chown" | "chgrp" if opens_permissions(args) => {
                flag(self, Rule::OpenPermissions);
            }
            // Any `delete` word counts: the options before the subcommand
            // and which of them take a value change between versions.
            "kubectl" | "k" | "oc" | "kubecolor" => {
                let sub = subcommand(args, KUBECTL_VALUES);
                // `--dry-run` only prints what would go.
                let dry = args
                    .iter()
                    .any(|w| w.text.starts_with("--dry-run") && w.text != "--dry-run=none");
                let deletes =
                    sub.is_some_and(Word::opaque) || args.iter().any(|w| w.text == "delete");
                if deletes && !dry {
                    flag(self, Rule::KubernetesDelete);
                }
            }
            "helm" => {
                let sub = subcommand(args, HELM_VALUES);
                let deletes = args
                    .iter()
                    .any(|w| matches!(w.text.as_str(), "uninstall" | "delete"))
                    || sub.is_some_and(|w| w.opaque() || matches!(w.text.as_str(), "del" | "un"));
                if deletes {
                    flag(self, Rule::KubernetesDelete);
                }
            }
            "terraform" | "tofu" | "terragrunt" | "pulumi" => {
                let has = |text: &str| args.iter().any(|w| w.text == text);
                // `-destroy`, `--destroy`, and `-destroy=true`.
                let destroy_flag = args.iter().any(|w| {
                    w.text
                        .trim_start_matches('-')
                        .split('=')
                        .next()
                        .is_some_and(|f| f == "destroy" && w.text.starts_with('-'))
                });
                let destroys = has("destroy")
                    || has("destroy-all")
                    || (has("apply") && destroy_flag)
                    || (name == "pulumi" && has("down"));
                let sub = subcommand(args, &[]);
                if destroys || sub.is_some_and(Word::opaque) {
                    flag(self, Rule::InfraDestroy);
                }
            }
            "shutdown" | "reboot" | "halt" | "poweroff" if !bare => flag(self, Rule::PowerOff),
            "systemctl" => {
                const POWER: &[&str] = &[
                    "poweroff",
                    "reboot",
                    "halt",
                    "kexec",
                    "soft-reboot",
                    "poweroff.target",
                    "reboot.target",
                    "halt.target",
                    "kexec.target",
                    "rescue",
                    "emergency",
                    "rescue.target",
                    "emergency.target",
                ];
                if args.iter().any(|w| POWER.contains(&w.text.as_str())) {
                    flag(self, Rule::PowerOff);
                }
            }
            "init" | "telinit" => {
                if args
                    .iter()
                    .any(|w| matches!(w.text.as_str(), "0" | "1" | "6" | "s" | "S"))
                {
                    flag(self, Rule::PowerOff);
                }
            }
            "mke2fs" | "mkswap" | "wipefs" | "mkntfs" | "mkdosfs" | "mkexfatfs" if !bare => {
                flag(self, Rule::FormatDisk);
            }
            _ if name.starts_with("mkfs") && !bare => flag(self, Rule::FormatDisk),
            "dd" => {
                let writes = args.iter().any(|w| {
                    w.text
                        .strip_prefix("of=")
                        .is_some_and(|target| w.opaque() || is_device(&normalize_path(target)))
                });
                if writes {
                    flag(self, Rule::DiskWrite);
                }
            }
            "tee" | "cp" | "shred" | "blkdiscard" | "pv" => {
                if args
                    .iter()
                    .any(|w| normalize_path(&w.text) == "/proc/sysrq-trigger")
                {
                    flag(self, Rule::PowerOff);
                }
                if args.iter().any(|w| is_device(&normalize_path(&w.text))) {
                    flag(self, Rule::DiskWrite);
                }
            }
            _ if SHELLS.contains(&name) => self.shell(command, args, &shown, depth),
            "eval" if !bare => {
                if args.iter().any(Word::opaque) {
                    flag(self, Rule::Opaque);
                }
                self.script(&join(args), depth + 1);
            }
            "su" | "runuser" | "sg" | "flock" | "script" => {
                if let Some(script) = option_value(args, &["-c", "--command"], "--command=") {
                    if script.opaque() {
                        flag(self, Rule::Opaque);
                    }
                    self.script(&script.text, depth + 1);
                }
            }
            "tmux" => {
                // These tmux commands hand their last words to a shell.
                const RUNS: &[&str] = &[
                    "new",
                    "new-session",
                    "new-window",
                    "neww",
                    "split-window",
                    "splitw",
                    "respawn-pane",
                    "respawnp",
                    "respawn-window",
                    "respawnw",
                    "run-shell",
                    "run",
                    "send-keys",
                    "send",
                    "if-shell",
                    "if",
                ];
                // Options of those commands that take a value.
                const VALUES: &[&str] = &[
                    "-c", "-e", "-F", "-f", "-n", "-s", "-t", "-x", "-y", "-l", "-p", "-T",
                ];
                if let Some(at) = args.iter().position(|w| RUNS.contains(&w.text.as_str())) {
                    let rest = &args[at + 1..];
                    let rest = &rest[skip_options(rest, VALUES).min(rest.len())..];
                    self.script(&join(rest), depth + 1);
                }
            }
            "complete" if !bare => {
                if let Some(script) = option_value(args, &["-C"], "-C=") {
                    self.script(&script.text, depth + 1);
                }
            }
            "alias" if !bare => {
                for word in args {
                    if let Some((_, value)) = word.text.split_once('=') {
                        self.script(value, depth + 1);
                    }
                }
            }
            "trap" if !bare => {
                let at = usize::from(args.first().is_some_and(|w| w.text == "--"));
                if let Some(action) = args.get(at).filter(|w| !w.text.starts_with('-')) {
                    if action.opaque() {
                        flag(self, Rule::Opaque);
                    }
                    self.script(&action.text, depth + 1);
                }
            }
            // `xargs sh -c '...'` runs what arrives on standard input.
            "xargs" | "parallel" if args.iter().any(|w| SHELLS.contains(&basename(&w.text))) => {
                flag(self, Rule::PipeToShell);
            }
            "at" | "batch" if !bare => {
                if let Some(input) = &command.input {
                    self.script(input, depth + 1);
                } else if command.stage > 0 {
                    flag(self, Rule::PipeToShell);
                }
            }
            "ssh" => {
                // The remote shell reads the arguments after the host,
                // joined by spaces.
                let at = skip_options(args, SSH_VALUES);
                if let Some(rest) = args.get(at + 1..).filter(|rest| !rest.is_empty()) {
                    if rest.iter().any(Word::opaque) {
                        flag(self, Rule::Opaque);
                    }
                    self.script(&join(rest), depth + 1);
                } else if let Some(input) = &command.input {
                    // With no command, the remote shell reads standard input.
                    self.script(input, depth + 1);
                }
            }
            "watch" => {
                let at = skip_options(args, &["-n", "--interval", "-d", "--differences"]);
                if let Some(rest) = args.get(at..).filter(|rest| !rest.is_empty()) {
                    self.script(&join(rest), depth + 1);
                }
            }
            "env" => {
                if let Some(split) =
                    option_value(args, &["-S", "--split-string"], "--split-string=")
                {
                    self.script(&split.text, depth + 1);
                }
            }
            "source" | "." if !bare => {
                if args.first().is_some_and(|w| {
                    w.process
                        || w.expanded
                        || matches!(w.text.as_str(), "/dev/stdin" | "-")
                        || w.text.starts_with("/dev/fd/")
                }) {
                    flag(self, Rule::PipeToShell);
                }
            }
            _ => {
                if let Some(code) = inline_code(name, args) {
                    self.foreign(&code, depth);
                }
            }
        }
    }

    /// A shell: `-c` runs its script; no script operand means it reads
    /// standard input.
    fn shell(&mut self, command: &Command, args: &[Word], shown: &str, depth: usize) {
        let mut runs_string = false;
        let mut reads_input = false;
        let mut at = 0;
        while let Some(word) = args.get(at) {
            let text = word.text.as_str();
            if text == "--" || text == "-" {
                at += 1;
                break;
            }
            if text.starts_with("--") {
                at += if matches!(text, "--rcfile" | "--init-file") {
                    2
                } else {
                    1
                };
                continue;
            }
            if (text.starts_with('-') || text.starts_with('+')) && text.len() > 1 {
                runs_string |= text.starts_with('-') && text.contains('c');
                reads_input |= text.starts_with('-') && text.contains('s');
                // `-o name` and `-O name` take a value.
                at += if text.ends_with('o') || text.ends_with('O') {
                    2
                } else {
                    1
                };
                continue;
            }
            break;
        }
        match args.get(at) {
            Some(script) if runs_string => {
                if script.opaque() {
                    self.flag(Rule::Opaque, shown);
                }
                self.script(&script.text, depth + 1);
            }
            Some(file)
                if !reads_input
                    && (file.process
                        || file.expanded
                        || file.text == "/dev/stdin"
                        || file.text.starts_with("/dev/fd/")) =>
            {
                self.flag(Rule::PipeToShell, shown);
            }
            Some(_) if !reads_input => {}
            _ => {
                if let Some(input) = &command.input {
                    self.script(input, depth + 1);
                } else if command.stage > 0
                    || command
                        .redirects
                        .iter()
                        .any(|r| r.kind == RedirectKind::Read && r.target.process)
                {
                    self.flag(Rule::PipeToShell, shown);
                }
            }
        }
    }
}

const SHELLS: &[&str] = &[
    "sh", "bash", "dash", "zsh", "ksh", "mksh", "ash", "yash", "rbash", "fish", "csh", "tcsh",
];

/// Words that run the command after them: `docker exec web reboot`,
/// `kubectl exec pod -- reboot`.
const RUNNERS: &[&str] = &[
    "exec",
    "screen",
    "run",
    "sshpass",
    "parallel",
    "nsenter",
    "xargs",
    "chroot",
    "systemd-run",
    "ip",
];

/// Global options of kubectl that take a value as the next word.
const KUBECTL_VALUES: &[&str] = &[
    "-n",
    "--namespace",
    "-s",
    "--server",
    "--context",
    "--cluster",
    "--user",
    "--kubeconfig",
    "--token",
    "--as",
    "--as-group",
    "--request-timeout",
    "--certificate-authority",
    "--client-certificate",
    "--client-key",
    "--tls-server-name",
    "-v",
];
const HELM_VALUES: &[&str] = &[
    "-n",
    "--namespace",
    "--kube-context",
    "--kubeconfig",
    "--kube-apiserver",
    "--kube-token",
    "--registry-config",
    "--repository-config",
    "--repository-cache",
];
/// ssh options that take a value, from `ssh(1)`.
const SSH_VALUES: &[&str] = &[
    "-B", "-b", "-c", "-D", "-E", "-e", "-F", "-I", "-i", "-J", "-L", "-l", "-m", "-O", "-o", "-P",
    "-p", "-Q", "-R", "-S", "-W", "-w",
];

/// Options before the first operand, skipping the values of `values`.
/// Returns the operand's index, or `args.len()`.
fn skip_options(args: &[Word], values: &[&str]) -> usize {
    let mut at = 0;
    while let Some(word) = args.get(at) {
        let text = word.text.as_str();
        if text == "--" {
            return at + 1;
        }
        if !text.starts_with('-') || text.len() < 2 {
            return at;
        }
        // A cluster such as `-iu` ends in the option that takes the value.
        let last = (!text.starts_with("--") && text.len() > 2)
            .then(|| text.chars().last().map(|c| format!("-{c}")))
            .flatten();
        let takes_value =
            values.contains(&text) || last.is_some_and(|last| values.contains(&last.as_str()));
        at += if takes_value { 2 } else { 1 };
    }
    at
}

fn subcommand<'a>(args: &'a [Word], values: &[&str]) -> Option<&'a Word> {
    args.get(skip_options(args, values))
}

/// The value of an option given as `-c value`, `--command value`, or
/// `--command=value`.
fn option_value(args: &[Word], names: &[&str], joined: &str) -> Option<Word> {
    for (at, word) in args.iter().enumerate() {
        if names.contains(&word.text.as_str()) {
            return args.get(at + 1).cloned();
        }
        if let Some(value) = word.text.strip_prefix(joined) {
            return Some(Word {
                text: value.to_owned(),
                ..word.clone()
            });
        }
        // A cluster such as `-lc` ends in the option that takes the value.
        if names.first().is_some_and(|short| {
            short.len() == 2
                && word.text.len() > 2
                && word.text.starts_with('-')
                && !word.text.starts_with("--")
                && word.text.ends_with(&short[1..])
        }) {
            return args.get(at + 1).cloned();
        }
    }
    None
}

/// The code a language interpreter runs from its arguments.
fn inline_code(name: &str, args: &[Word]) -> Option<String> {
    let flags: &[&str] = match name {
        n if n.starts_with("python") || n == "pypy" || n == "pypy3" => &["-c"],
        "perl" | "ruby" | "lua" | "luajit" => &["-e", "-E"],
        "node" | "nodejs" | "deno" | "bun" => &["-e", "-p", "--eval", "--print"],
        "php" => &["-r"],
        "awk" | "gawk" | "mawk" | "nawk" => {
            return args
                .get(skip_options(args, &["-f", "-v", "-F"]))
                .map(|w| w.text.clone());
        }
        _ => return None,
    };
    let code: Vec<&str> = args
        .windows(2)
        .filter(|pair| flags.contains(&pair[0].text.as_str()))
        .map(|pair| pair[1].text.as_str())
        .collect();
    (!code.is_empty()).then(|| code.join("\n"))
}

/// The word's value starts with an expansion, so its first character, and
/// whether it is an option, is only known when the shell runs it.
fn leads_with_expansion(word: &Word) -> bool {
    let start = word.raw.trim_start_matches(['"', '\'']);
    word.expanded && (start.starts_with('$') || start.starts_with('`'))
}

fn rm_recursive(args: &[Word]) -> bool {
    for word in args {
        let text = word.text.as_str();
        if text == "--" {
            return false;
        }
        // An expansion may hold options: `rm $FLAGS`, and also `rm "$opt"`
        // or `rm "$@"`, since rm reads any word starting with `-` as one.
        // A literal start such as `"./$f"` cannot.
        if word.split || word.history || leads_with_expansion(word) {
            return true;
        }
        if long_option(text, "--recursive", 3) || long_option(text, "--no-preserve-root", 4) {
            return true;
        }
        if text.starts_with('-')
            && !text.starts_with("--")
            && (text.contains('r') || text.contains('R'))
        {
            return true;
        }
    }
    false
}

fn opens_permissions(args: &[Word]) -> bool {
    let mut recursive = false;
    let mut root = false;
    for word in args {
        let text = word.text.as_str();
        if long_option(text, "--no-preserve-root", 4) {
            return true;
        }
        if long_option(text, "--recursive", 5)
            || (text.len() > 1
                && text.starts_with('-')
                && !text.starts_with("--")
                && text.contains('R')
                && text[1..].chars().all(|c| "RcfvhHLP".contains(c)))
        {
            recursive = true;
        } else if word.split || word.history || leads_with_expansion(word) {
            recursive = true;
            root = true;
        } else if is_top_level(&normalize_path(text)) {
            root = true;
        }
    }
    recursive && root
}

/// `text` abbreviates `option` as GNU getopt allows, with at least `min`
/// characters: `--recur` is `--recursive`.
fn long_option(text: &str, option: &str, min: usize) -> bool {
    text.len() >= min && option.starts_with(text.split('=').next().unwrap_or(text))
}

/// `/`, `/*`, or a directory right under it such as `/etc`.
fn is_top_level(path: &str) -> bool {
    if path == "/*" {
        return true;
    }
    let path = path.strip_suffix("/*").unwrap_or(path);
    path.starts_with('/') && !path[1..].contains('/')
}

/// A device file other than the ones that only read, discard, or reach a
/// terminal.
fn is_device(path: &str) -> bool {
    const HARMLESS: &[&str] = &[
        "null", "zero", "full", "random", "urandom", "tty", "console", "ptmx", "stdin", "stdout",
        "stderr", "kmsg", "log",
    ];
    let Some(name) = path.strip_prefix("/dev/") else {
        return false;
    };
    !(name.is_empty()
        || HARMLESS.contains(&name)
        || name.starts_with("tty")
        || name.starts_with("pts/")
        || name.starts_with("fd/")
        || name.starts_with("shm/")
        || name.starts_with("mqueue/"))
}

/// Resolves `.`, `..`, and repeated slashes in a path, without touching the
/// file system. A relative path keeps the `..` that climb out of it, so
/// `build/../../x` becomes `../x`. Words without `/` come back unchanged.
fn normalize_path(path: &str) -> String {
    if !path.contains('/') {
        return path.to_owned();
    }
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|p| *p != "..") => {
                parts.pop();
            }
            ".." if absolute => {}
            _ => parts.push(part),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_owned()
    } else {
        joined
    }
}

/// Words the shell reads before the command name: reserved words such as
/// `if` and `do`, and assignments such as `LC_ALL=C`.
fn command_start(words: &[Word]) -> usize {
    const RESERVED: &[&str] = &[
        "if", "then", "else", "elif", "fi", "do", "done", "while", "until", "for", "case", "esac",
        "in", "select", "function", "time", "!", "coproc",
    ];
    words
        .iter()
        .position(|w| !(RESERVED.contains(&w.raw.as_str()) || is_assignment(w)))
        .unwrap_or(words.len())
}

/// A command name whose value the shell decides: an expansion, history,
/// or a glob that could match a program, such as `/bin/r?`. A glob with no
/// letter and no `/`, such as the test command `[`, SQL's `count(*)`, or
/// `(( 2*3 ))`, names nothing the guard could miss.
fn names_unknown(word: &Word) -> bool {
    if is_assignment(word) {
        return false;
    }
    if word.expanded || word.history {
        return true;
    }
    // `([k]=v)` assigns array elements.
    word.glob
        && !word.text.contains('=')
        && word
            .text
            .chars()
            .any(|c| c.is_ascii_alphabetic() || c == '/')
}

/// The name and value of `NAME=value`, as an assignment or an argument
/// of `export` and `declare`.
fn assignment(word: &Word) -> Option<(&str, &str)> {
    let (name, value) = word.text.split_once('=')?;
    // `NAME+=value` appends and `NAME[0]=value` sets an element.
    let name = name.strip_suffix('+').unwrap_or(name);
    let name = name.split('[').next().unwrap_or(name);
    let valid = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then_some((name, value))
}

/// `NAME=value` with an unquoted name, which bash reads as an assignment.
fn is_assignment(word: &Word) -> bool {
    let Some(eq) = word.raw.find('=') else {
        return false;
    };
    let name = word.raw[..eq].strip_suffix('+').unwrap_or(&word.raw[..eq]);
    let name = name.split('[').next().unwrap_or(name);
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Splits at unquoted `{` and `}` words, which open and close groups and
/// function bodies: `f() { rm -rf /; }`.
fn segments(words: &[Word]) -> impl Iterator<Item = &[Word]> {
    words.split(|w| w.raw == "{" || w.raw == "}")
}

/// Wrappers that run the command after their options: `sudo`, `env`,
/// `nice`, and the like. Returns the index of the command they run, or
/// `None` when no command follows. Bounded by the word count.
fn unwrap(words: &[Word], start: usize) -> Option<usize> {
    let mut at = start;
    loop {
        let word = words.get(at)?;
        if word.opaque() {
            return Some(at);
        }
        let rest = &words[at + 1..];
        // Options with a value, then how many operands come before the command.
        let (values, operands): (&[&str], usize) = match basename(&word.text) {
            "sudo" => (
                &[
                    "-u",
                    "-g",
                    "-h",
                    "-p",
                    "-C",
                    "-D",
                    "-r",
                    "-t",
                    "-T",
                    "-U",
                    "--user",
                    "--group",
                    "--host",
                    "--prompt",
                    "--close-from",
                    "--chdir",
                    "--role",
                    "--type",
                    "--command-timeout",
                    "--other-user",
                ],
                0,
            ),
            "doas" => (&["-u", "-C"], 0),
            "pkexec" | "run0" => (&["--user", "-u"], 0),
            "env" => (&["-u", "--unset", "-C", "--chdir"], 0),
            "nice" => (&["-n", "--adjustment"], 0),
            "ionice" => (&["-c", "-n", "--class", "--classdata"], 0),
            "time" => (&["-f", "-o", "--format", "--output"], 0),
            "timeout" => (&["-s", "-k", "--signal", "--kill-after"], 1),
            "xargs" => (
                &[
                    "-a",
                    "-d",
                    "-E",
                    "-I",
                    "-L",
                    "-n",
                    "-P",
                    "-s",
                    "--arg-file",
                    "--delimiter",
                    "--max-lines",
                    "--max-args",
                    "--max-procs",
                    "--max-chars",
                ],
                0,
            ),
            "exec" => (&["-a"], 0),
            "stdbuf" => (&["-i", "-o", "-e"], 0),
            "chroot" => (&[], 1),
            "chrt" | "taskset" | "flock" => (&["-w", "-E"], 1),
            "strace" | "ltrace" => (&["-e", "-o", "-p", "-s", "-u", "-E", "-a", "-b", "-I"], 0),
            "sshpass" => (&["-p", "-f", "-d", "-e"], 0),
            "nohup" | "command" | "builtin" | "setsid" | "unbuffer" | "busybox" | "toybox"
            | "caffeinate" | "catchsegv" | "firejail" | "proxychains" | "proxychains4"
            | "torsocks" | "microk8s" | "k3s" => (&[], 0),
            _ => return Some(at),
        };
        let mut next = at + 1 + skip_options(rest, values);
        if basename(&word.text) == "env" {
            while words.get(next).is_some_and(is_assignment) {
                next += 1;
            }
        }
        next += operands;
        at = next;
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn join(words: &[Word]) -> String {
    let mut out = String::new();
    for word in words {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&word.text);
    }
    out
}

/// Runs of whitespace become one space.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` with quote characters and backslashes removed.
fn unquoted(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '\'' | '"' | '`' | '\\'))
        .collect()
}

fn shown(command: &str) -> String {
    let mut out: String = collapse(command)
        .chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .take(MAX_SHOWN)
        .collect();
    if collapse(command).chars().count() > MAX_SHOWN {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests;
