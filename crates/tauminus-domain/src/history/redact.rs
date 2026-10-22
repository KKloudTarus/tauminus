// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Secrets with a shape no ordinary text has: cloud and chat API keys,
//! bearer tokens and JWTs, private key blocks, passwords in arguments,
//! assignments, and URLs. They are replaced before a command or its output
//! is stored, whatever prompt they came after.

/// What a secret is replaced with.
const HIDDEN: &str = "•••";

/// A token's prefix, the shortest run of token characters after it, and
/// whether the run must hold one part of that length with a digit, as
/// random keys do (`sk-payments-eu` and `sk-ecdsa-sha2-...` are no keys).
struct Prefix {
    text: &'static str,
    min: usize,
    digit: bool,
}

const fn prefix(text: &'static str, min: usize, digit: bool) -> Prefix {
    Prefix { text, min, digit }
}

const PREFIXES: &[Prefix] = &[
    prefix("AKIA", 16, false),
    prefix("ASIA", 16, false),
    prefix("ghp_", 20, false),
    prefix("gho_", 20, false),
    prefix("ghu_", 20, false),
    prefix("ghs_", 20, false),
    prefix("ghr_", 20, false),
    prefix("github_pat_", 20, false),
    prefix("glpat-", 20, false),
    prefix("xoxb-", 10, false),
    prefix("xoxp-", 10, false),
    prefix("xoxa-", 10, false),
    prefix("xoxs-", 10, false),
    prefix("xoxe-", 10, false),
    prefix("xoxe.", 10, false),
    prefix("sk-", 20, true),
    prefix("sk_live_", 16, false),
    prefix("rk_live_", 16, false),
    prefix("sk_test_", 16, false),
    prefix("rk_test_", 16, false),
    prefix("AIza", 30, false),
    prefix("npm_", 30, false),
    prefix("hvs.", 20, false),
    prefix("dckr_pat_", 20, false),
];

/// Parts of a variable's or a flag's name that say its value is a secret.
const SECRET_WORDS: &[&str] = &["PASS", "PW", "CREDENTIAL", "CREDENTIALS"];
/// Words that make a `KEY` a secret's: `api_key`, `AWS_SECRET_ACCESS_KEY`.
const KEY_OWNERS: &[&str] = &[
    "API",
    "ACCESS",
    "PRIVATE",
    "SECRET",
    "SIGNING",
    "ENCRYPTION",
    "MASTER",
    "LICENSE",
    "AUTH",
    "APP",
    "SSH",
    "HMAC",
    "JWT",
    "CLIENT",
    "SESSION",
    "DEPLOY",
    "CRYPT",
    "CIPHER",
    "SHARED",
];
/// Last parts that describe a secret rather than hold one.
const NOT_SECRET_LAST: &[&str] = &[
    "TYPE",
    "TYPES",
    "COUNT",
    "ID",
    "IDS",
    "NAME",
    "FILE",
    "PATH",
    "URL",
    "URI",
    "LENGTH",
    "SIZE",
    "EXPIRES",
    "EXPIRY",
    "FORMAT",
    "ALGORITHM",
];
/// First parts that make a key public.
const NOT_SECRET_FIRST: &[&str] = &["PUBLIC", "PRIMARY", "FOREIGN", "SORT", "PARTITION"];
/// Endings of such parts.
const SECRET_ENDINGS: &[&str] = &[
    "PASSWORD",
    "PASSWD",
    "PASSPHRASE",
    "SECRET",
    "TOKEN",
    "APIKEY",
];
/// Programs and the flags whose value is a password, as the next word.
const PASSWORD_FLAGS: &[(&str, &[&str])] = &[
    ("mysql", &["-p"]),
    ("mariadb", &["-p"]),
    ("mysqldump", &["-p"]),
    ("mysqladmin", &["-p"]),
    ("mysqlsh", &["-p"]),
    ("sshpass", &["-p"]),
    ("redis-cli", &["-a"]),
    ("ssh-keygen", &["-P", "-N"]),
    ("openssl", &["-k"]),
];
/// Programs whose `-p` is a password only after `login`; elsewhere it is
/// a port (`docker run -p 8080:80`).
const LOGIN_P: &[&str] = &["docker", "podman", "az", "oc", "helm"];
/// Programs that come after a password tool and have their own `-p`
/// (`sshpass -p x ssh -p 2222 host`): their flags are no password's.
const AFTER_PASSWORD: &[&str] = &["ssh", "scp", "sftp", "rsync", "mosh"];
/// Programs that take their password attached to `-p` (`-phunter2`).
const ATTACHED_P: &[&str] = &["mysql", "mariadb", "mysqldump", "mysqladmin", "mysqlsh"];
/// Programs whose `-U user%password` holds a password.
const PERCENT_USERS: &[&str] = &["smbclient", "rpcclient", "smbmap", "smbget"];

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '/' | '=')
}

fn word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// `text` with every secret of a known shape replaced. O(n · p) for p
/// prefixes.
pub(crate) fn redact(text: &str) -> String {
    let text = redact_key_blocks(text);
    let text = redact_structured(&text);
    let text = redact_tokens(&text);
    redact_words(&text)
}

/// Values of secret-named fields in JSON (`"password": "x"`, spaces or
/// not), in JSON escaped inside a shell string (`\"password\":\"x\"`), and
/// in XML (`<password>x</password>`, attributes or not). O(n).
fn redact_structured(text: &str) -> String {
    redact_xml(&redact_json(&redact_json(text, "\""), "\\\""))
}

/// JSON fields whose strings are quoted by `quote`: `"`, or `\"` inside
/// another string. Each character is read once or twice.
fn redact_json(text: &str, quote: &str) -> String {
    let escaped = quote.len() > 1;
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(quote) {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        // A string; a key when a colon follows it.
        let Some(end) = string_end(rest, quote) else {
            out.push_str(rest);
            return out;
        };
        let key = &rest[quote.len()..end];
        out.push_str(&rest[..end + quote.len()]);
        rest = &rest[end + quote.len()..];
        let gap = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        if !rest[gap..].starts_with(':') || key.len() > 64 || !secret_field(key) {
            continue;
        }
        let colon = gap + 1;
        let after = &rest[colon..];
        let gap = colon + (after.len() - after.trim_start_matches([' ', '\t']).len());
        out.push_str(&rest[..gap]);
        rest = &rest[gap..];
        if rest.starts_with(['{', '[']) {
            // An object or a list: its own fields are judged by their names.
            continue;
        }
        if rest.starts_with(quote) {
            let end = string_end(rest, quote).unwrap_or(rest.len() - quote.len());
            out.push_str(quote);
            out.push_str(HIDDEN);
            out.push_str(quote);
            rest = rest.get(end + quote.len()..).unwrap_or("");
        } else {
            let stops: &[char] = if escaped {
                &[',', '}', ']', '\n', ' ', '\\', '"']
            } else {
                &[',', '}', ']', '\n', ' ']
            };
            let end = rest.find(stops).unwrap_or(rest.len());
            if end > 0 {
                out.push_str(HIDDEN);
            }
            rest = &rest[end..];
        }
    }
    out.push_str(rest);
    out
}

/// XML elements with a secret's name, attributes or not, hide their text.
fn redact_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        // A tag is short: look no further, so a run of `<` stays O(n).
        let window = &rest[1..floor_boundary(rest, rest.len().min(130))];
        let tag = window.find('>').map(|close| &window[..close]);
        let name = tag.map(|tag| tag.split([' ', '\t']).next().unwrap_or(tag));
        match (tag, name) {
            (Some(tag), Some(name))
                if !name.is_empty()
                    && name.len() <= 64
                    && name.chars().all(|c| {
                        c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.')
                    })
                    && secret_field(name) =>
            {
                out.push_str(&rest[..tag.len() + 2]);
                rest = &rest[tag.len() + 2..];
                let end = rest.find("</").unwrap_or(rest.len());
                if end > 0 {
                    out.push_str(HIDDEN);
                }
                rest = &rest[end..];
            }
            _ => {
                out.push('<');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The nearest character boundary at or before `at`.
fn floor_boundary(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Where the quote that ends the string `text` starts. With `"`, escaped
/// quotes are skipped; with `\"`, the next `\"` ends it.
fn string_end(text: &str, quote: &str) -> Option<usize> {
    if quote.len() > 1 {
        return text[quote.len()..].find(quote).map(|n| n + quote.len());
    }
    let bytes = text.as_bytes();
    let mut at = 1;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 2,
            b'"' => return Some(at),
            _ => at += 1,
        }
    }
    None
}

/// Tokens by their prefix, bearer and basic credentials, and JWTs. Each
/// run of token characters is measured once, so a long run costs O(n · p)
/// and never O(n²).
fn redact_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    // The end of the run of token characters `pos` is in, and how many
    // dots are left in it.
    let mut run_end = 0;
    let mut dots = 0;
    'outer: while pos < text.len() {
        let rest = &text[pos..];
        if pos >= run_end {
            run_end = pos + rest.find(|c: char| !token_char(c)).unwrap_or(rest.len());
            dots = text[pos..run_end].bytes().filter(|&b| b == b'.').count();
        }
        // A token starts at a word boundary.
        if out.chars().next_back().is_none_or(|c| !word_char(c)) {
            for (scheme, min) in [("bearer ", 8), ("token ", 16), ("basic ", 16)] {
                if let Some(head) = rest.get(..scheme.len())
                    && head.eq_ignore_ascii_case(scheme)
                {
                    let after = &rest[scheme.len()..];
                    let run = after.find(|c: char| !token_char(c)).unwrap_or(after.len());
                    if run >= min {
                        out.push_str(head);
                        out.push_str(HIDDEN);
                        pos += scheme.len() + run;
                        continue 'outer;
                    }
                }
            }
            // Token characters are ASCII: any byte offset in a run is a
            // character boundary.
            let word = &text[pos..run_end];
            if word.starts_with("eyJ") && word.len() >= 20 && dots >= 2 {
                out.push_str(HIDDEN);
                pos = run_end;
                continue;
            }
            for p in PREFIXES {
                if let Some(after) = word.strip_prefix(p.text)
                    && after.len() >= p.min
                    && (!p.digit || random_part(&after[..after.len().min(256)], p.min))
                {
                    out.push_str(HIDDEN);
                    pos = run_end;
                    continue 'outer;
                }
            }
        }
        let c = rest.chars().next().unwrap_or_default();
        if c == '.' {
            dots = dots.saturating_sub(1);
        }
        out.push(c);
        pos += c.len_utf8();
    }
    out
}

/// Whether `run` holds a part between `-` and `_` of at least `min`
/// characters with a digit.
fn random_part(run: &str, min: usize) -> bool {
    run.split(['-', '_'])
        .any(|part| part.len() >= min && part.chars().any(|c| c.is_ascii_digit()))
}

/// Whether a variable's or flag's name says its value is a secret. Its
/// parts, split at `-`, `_`, `.` and camelCase (`SecretAccessKey`), hold a
/// secret's word (`PASS`) or end with one (`PGPASSWORD`, `GITHUB_TOKEN`),
/// so `--keyserver` and `--passive` are no secrets' names. A last part
/// `KEY` is a secret's (`APP_KEY`, `SSH_KEY`); see [`secret_field`] for
/// fields, where `key` alone is common.
fn secret_name(name: &str) -> bool {
    named_secret(name, false)
}

/// As [`secret_name`], for a JSON or XML field: `Key` alone names a label
/// there (`{"Key":"Name"}`), so `KEY` counts only with a word saying
/// whose (`api_key`, `SecretAccessKey`).
fn secret_field(name: &str) -> bool {
    named_secret(name, true)
}

fn named_secret(name: &str, field: bool) -> bool {
    let parts = name_parts(name);
    let (Some(first), Some(last)) = (parts.first(), parts.last()) else {
        return false;
    };
    // What describes a secret is no secret: `token_type`, `key_id`,
    // `public_key`, `primary_key`.
    if NOT_SECRET_LAST.contains(&last.as_str()) || NOT_SECRET_FIRST.contains(&first.as_str()) {
        return false;
    }
    let owned = parts.iter().any(|p| KEY_OWNERS.contains(&p.as_str()));
    parts.iter().enumerate().any(|(n, part)| {
        SECRET_WORDS.contains(&part.as_str())
            || SECRET_ENDINGS.iter().any(|end| part.ends_with(end))
            || (part == "KEY" && (owned || (!field && n + 1 == parts.len() && parts.len() > 1)))
    })
}

/// A name's parts, upper case, split at non-alphanumerics and where a
/// lower-case letter meets an upper-case one. O(n).
fn name_parts(name: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut part = String::new();
    let mut lower = false;
    for c in name.chars() {
        if !c.is_ascii_alphanumeric() {
            if !part.is_empty() {
                parts.push(std::mem::take(&mut part));
            }
            lower = false;
            continue;
        }
        if c.is_ascii_uppercase() && lower && !part.is_empty() {
            parts.push(std::mem::take(&mut part));
        }
        lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        part.push(c.to_ascii_uppercase());
    }
    if !part.is_empty() {
        parts.push(part);
    }
    parts
}

/// Hides the values in `name=value` pairs, joined by `&` as in a query or
/// a form, whose name is a secret's: `password=x&user=a`. Returns the text
/// and the quote a hidden value left open, or `None` when nothing is
/// hidden.
fn redact_pairs(word: &str) -> Option<(String, Option<char>)> {
    let body = word.trim_start_matches(['\'', '"']);
    let lead = &word[..word.len() - body.len()];
    let mut out = String::from(lead);
    let mut hidden = false;
    let mut open = None;
    let parts = body.split('&').count();
    for (n, part) in body.split('&').enumerate() {
        if n > 0 {
            out.push('&');
        }
        match part.split_once('=') {
            Some((name, value))
                if !value.is_empty()
                    && !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
                    && secret_name(name) =>
            {
                out.push_str(name);
                out.push('=');
                out.push_str(HIDDEN);
                hidden = true;
                let last = n + 1 == parts;
                if lead.is_empty() {
                    // `PASS='a b c'`: hidden to the quote that closes it.
                    open = open_quote(value);
                } else if last && let Some(q) = lead.chars().next() {
                    // `'password=x'`: the quote around the word stays.
                    if value.ends_with(q) {
                        out.push(q);
                    } else {
                        open = Some(q);
                    }
                }
            }
            _ => out.push_str(part),
        }
    }
    hidden.then_some((out, open))
}

/// What a word needs done after it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Next {
    Keep,
    /// The next word is a flag's secret value.
    Hide,
    /// The next word is `user:password`.
    User,
    /// The next word is `user%password`.
    PercentUser,
}

/// Secrets by their place on a line: `NAME=value` and `--name value` with
/// a secret's name, `-u user:password`, a URL's password, and a MySQL
/// client's `-p<password>`. A quoted value with spaces is hidden to its
/// closing quote.
#[expect(
    clippy::too_many_lines,
    reason = "one pass over the words, a branch per rule, in the order they apply"
)]
fn redact_words(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // Inside a hidden quoted value: the quote that ends it, on this line or
    // a later one.
    let mut quote: Option<char> = None;
    for line in text.split_inclusive('\n') {
        let mut next = Next::Keep;
        // The flags whose value is a password, for the program this line
        // runs.
        let mut flags: &[&str] = &[];
        let mut percent = false;
        let mut attached_p = false;
        let mut login_p = false;
        // `.netrc`: `machine h login u password p`, on one line or several.
        let netrc = {
            let mut words = line.split_whitespace();
            match (words.next(), words.next()) {
                (Some("machine" | "default" | "login"), _) => true,
                // `password hunter2`, not `password = x` or `password: x`.
                (Some("password" | "passwd"), Some(value)) => !value.starts_with(['=', ':']),
                _ => false,
            }
        };
        let mut first = true;
        // The last word was a secret's bare name, as in `password = x`.
        let mut named = false;
        let mut rest = line;
        while !rest.is_empty() {
            let space = rest
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(rest.len());
            if quote.is_none() {
                out.push_str(&rest[..space]);
            } else if rest[..space].contains('\n') {
                // The rows stay; what is on them does not.
                out.push('\n');
            }
            rest = &rest[space..];
            let len = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let word = &rest[..len];
            rest = &rest[len..];
            if word.is_empty() {
                continue;
            }
            if let Some(q) = quote {
                if word.ends_with(q) {
                    quote = None;
                }
                continue;
            }
            match std::mem::replace(&mut next, Next::Keep) {
                // Hidden already, by the JSON pass: keep what closes it.
                Next::Hide if word.starts_with(HIDDEN) || word.starts_with("\"•••\"") => {
                    out.push_str(word);
                    continue;
                }
                Next::Hide => {
                    out.push_str(HIDDEN);
                    quote = open_quote(word);
                    continue;
                }
                user @ (Next::User | Next::PercentUser) => {
                    let split = if user == Next::PercentUser { '%' } else { ':' };
                    if let Some(at) = word.find(split) {
                        out.push_str(&word[..=at]);
                        out.push_str(HIDDEN);
                        quote = open_quote(word);
                        continue;
                    }
                }
                Next::Keep => {}
            }
            let leading = std::mem::replace(&mut first, false);
            // `password = x`, `password =x`, `password= x`.
            if std::mem::replace(&mut named, false) {
                if word == "=" || word == ":" {
                    out.push_str(word);
                    next = Next::Hide;
                    continue;
                }
                if let Some(value) = word.strip_prefix('=')
                    && !value.is_empty()
                {
                    out.push('=');
                    out.push_str(HIDDEN);
                    quote = open_quote(value);
                    continue;
                }
            }
            let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
            let base = base.strip_suffix(".exe").unwrap_or(base);
            if let Some((_, program)) = PASSWORD_FLAGS.iter().find(|(name, _)| *name == base) {
                flags = program;
            } else if AFTER_PASSWORD.contains(&base) {
                flags = &[];
            }
            login_p |= LOGIN_P.contains(&base);
            if login_p && word == "login" {
                flags = &["-p"];
            }
            percent |= PERCENT_USERS.contains(&base);
            attached_p |= ATTACHED_P.contains(&base);
            let percent_attached = if percent {
                word.strip_prefix("--user=")
                    .or_else(|| word.strip_prefix("-U").filter(|v| !v.is_empty()))
                    .and_then(|value| value.find('%').map(|at| word.len() - value.len() + at))
            } else {
                None
            };
            if let Some(value) = word.strip_prefix("pass:")
                && !value.is_empty()
            {
                // openssl's `-pass pass:x`, `-passin pass:x`.
                out.push_str("pass:");
                out.push_str(HIDDEN);
                quote = open_quote(value);
            } else if let Some(at) = percent_attached {
                out.push_str(&word[..=at]);
                out.push_str(HIDDEN);
                quote = open_quote(word);
            } else if percent && (word == "-U" || word == "--user") {
                next = Next::PercentUser;
                out.push_str(word);
            } else if flags.contains(&word) {
                next = Next::Hide;
                out.push_str(word);
            } else if attached_p && word.len() > 2 && word.starts_with("-p") {
                out.push_str("-p");
                out.push_str(HIDDEN);
                quote = open_quote(&word[2..]);
            } else if netrc && matches!(word, "password" | "passwd") {
                next = Next::Hide;
                out.push_str(word);
            } else if leading && is_bare_name(word) && secret_name(word.trim_end_matches('=')) {
                // A secret's name: the value follows it, or follows `=`.
                if word.ends_with('=') {
                    next = Next::Hide;
                } else {
                    named = true;
                }
                out.push_str(word);
            } else if word == "-u" || word == "--user" {
                next = Next::User;
                out.push_str(word);
            } else if let Some(colon) = word.find(':')
                && (word.starts_with("-u") || word.starts_with("--user="))
                && word.len() > colon + 1
            {
                out.push_str(&word[..=colon]);
                out.push_str(HIDDEN);
                quote = open_quote(word);
            } else if let Some(flag) = word.strip_prefix("--") {
                match flag.split_once('=') {
                    Some((name, value)) if secret_name(name) && !value.is_empty() => {
                        out.push_str(&word[..=(2 + name.len())]);
                        out.push_str(HIDDEN);
                        quote = open_quote(value);
                    }
                    // `--env=DB_PASSWORD=x`
                    Some((name, value)) if let Some((hidden, open)) = redact_pairs(value) => {
                        out.push_str(&word[..=(2 + name.len())]);
                        out.push_str(&hidden);
                        quote = open;
                    }
                    None if secret_name(flag) && !flag.ends_with("-file") => {
                        next = Next::Hide;
                        out.push_str(word);
                    }
                    _ => out.push_str(&redact_url(word)),
                }
            } else if let Some(name) = word.strip_suffix(':')
                && secret_name(name.trim_start_matches(['\'', '"']))
            {
                // A header or a field: `X-Api-Key: value`, `Token: value`.
                next = Next::Hide;
                out.push_str(word);
            } else if let Some((hidden, open)) = redact_pairs(word) {
                out.push_str(&hidden);
                quote = open;
            } else {
                out.push_str(&redact_url(word));
            }
        }
    }
    out
}

/// A config key as it stands alone on a line: letters, digits, `_`, `-`,
/// `.`, maybe ending in `=`.
fn is_bare_name(word: &str) -> bool {
    let name = word.trim_end_matches('=');
    !name.is_empty()
        && !name.starts_with('-')
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// The quote a value opens and does not close in the same word.
fn open_quote(value: &str) -> Option<char> {
    let q = value.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    (value.len() == 1 || !value.ends_with(q)).then_some(q)
}

/// A URL's password, `scheme://user:password@host`, hidden.
fn redact_url(word: &str) -> String {
    let Some(scheme) = word.find("://") else {
        return word.to_owned();
    };
    let start = scheme + 3;
    let authority = &word[start..];
    let end = authority.find(['/', '?', '#']).unwrap_or(authority.len());
    let Some(at) = authority[..end].rfind('@') else {
        return word.to_owned();
    };
    let Some(colon) = authority[..at].find(':') else {
        // `https://<token>@host`: a long user is a token.
        if at >= 32 {
            return format!("{}{HIDDEN}{}", &word[..start], &word[start + at..]);
        }
        return word.to_owned();
    };
    format!(
        "{}{HIDDEN}{}",
        &word[..=(start + colon)],
        &word[start + at..]
    )
}

/// Private key blocks, from `-----BEGIN ... PRIVATE KEY-----` to its END
/// line, or to the end of the text when the END line is missing.
fn redact_key_blocks(text: &str) -> String {
    redact_ppk(&redact_pem(text))
}

/// A PuTTY key's private part: from `Private-Lines:` through its
/// `Private-MAC:` line, or to the end when that is missing.
fn redact_ppk(text: &str) -> String {
    const HEADER: &str = "PuTTY-User-Key-File-";
    const PRIVATE: &str = "Private-Lines:";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    // Only inside a PuTTY key: from its header on.
    while let Some(header) = rest.find(HEADER)
        && let Some(at) = rest[header..].find(PRIVATE).map(|n| header + n)
    {
        out.push_str(&rest[..at]);
        out.push_str(HIDDEN);
        let after = &rest[at..];
        rest = match after.find("Private-MAC:") {
            Some(mac) => &after[after[mac..].find('\n').map_or(after.len(), |n| mac + n)..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

fn redact_pem(text: &str) -> String {
    const BEGIN: &str = "-----BEGIN ";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(BEGIN) {
        let header_end = rest[at..].find('\n').map_or(rest.len(), |n| at + n);
        if !rest[at..header_end].contains("PRIVATE KEY-----") {
            // The whole line is no key's header: past it, so a line of
            // headers is read once.
            out.push_str(&rest[..header_end]);
            rest = &rest[header_end..];
            continue;
        }
        out.push_str(&rest[..at]);
        out.push_str(HIDDEN);
        let after = &rest[header_end..];
        rest = match after.find("-----END ") {
            Some(end) => {
                let line_end = after[end..].find('\n').map_or(after.len(), |n| end + n);
                &after[line_end..]
            }
            None => "",
        };
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_of_known_shapes_are_hidden() {
        for (input, expected) in [
            ("aws s3 ls # AKIAIOSFODNN7EXAMPLE", "aws s3 ls # •••"),
            (
                "git clone https://ghp_abcdefghijklmnopqrstuvwxyz0123@github.com/x",
                "git clone https://•••@github.com/x",
            ),
            (
                "curl -H 'Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload'",
                "curl -H 'Authorization: Bearer •••'",
            ),
            (
                "curl -H 'authorization: bearer abcdefghijklmnopqrstu'",
                "curl -H 'authorization: bearer •••'",
            ),
            ("echo xoxb-123456789012-abcdef", "echo •••"),
            ("echo sk-proj-abcdefghijklmnopqrstuvwx1", "echo •••"),
            (
                "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl",
                "jwt •••",
            ),
            ("npm_abcdefghijklmnopqrstuvwxyz0123456789", "•••"),
        ] {
            assert_eq!(redact(input), expected, "{input}");
        }
        // Built at run time, so the source holds nothing shaped like a
        // real token for a secret scanner to stop.
        let vault = format!("vault login hvs.{}", "x1".repeat(12));
        assert_eq!(redact(&vault), "vault login •••");
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one row per shape the redaction knows"
    )]
    fn secrets_in_arguments_and_assignments_are_hidden() {
        for (input, expected) in [
            (
                "export AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI",
                "export AWS_SECRET_ACCESS_KEY=•••",
            ),
            ("DB_PASSWORD='a b c' ./run", "DB_PASSWORD=••• ./run"),
            ("mysql -uroot -phunter2 db", "mysql -uroot -p••• db"),
            ("mysql --password=hunter2", "mysql --password=•••"),
            (
                "tool --api-token hunter2 --verbose",
                "tool --api-token ••• --verbose",
            ),
            (
                "curl -u alice:hunter2 https://x",
                "curl -u alice:••• https://x",
            ),
            (
                "git clone https://alice:hunter2@example.com/r.git",
                "git clone https://alice:•••@example.com/r.git",
            ),
            ("sshpass -p hunter2 ssh x", "sshpass -p ••• ssh x"),
            (
                "docker login -u bob -p hunter2",
                "docker login -u bob -p •••",
            ),
            ("curl -H 'X-Api-Key: abc123' x", "curl -H 'X-Api-Key: ••• x"),
            (
                "curl -H 'Authorization: Basic YWxpY2U6aHVudGVyMjEyMw=='",
                "curl -H 'Authorization: Basic •••'",
            ),
            ("PASS='a b\nc d' rest\nnext", "PASS=•••\n rest\nnext"),
            ("curl --user=alice:hunter2 x", "curl --user=alice:••• x"),
            ("curl -ualice:hunter2 x", "curl -ualice:••• x"),
            (
                "docker run --env=DB_PASSWORD=hunter2 img",
                "docker run --env=DB_PASSWORD=••• img",
            ),
            (
                "curl -d 'password=hunter2&u=a' x",
                "curl -d 'password=•••&u=a' x",
            ),
            ("password: hunter2", "password: •••"),
            ("{\"api_key\": \"abc\"}", "{\"api_key\": \"•••\"}"),
            ("PGPASSWORD=hunter2 psql", "PGPASSWORD=••• psql"),
            // Config files and their spacing.
            (
                "aws_secret_access_key = wJalrXUtnFEMI",
                "aws_secret_access_key = •••",
            ),
            ("password =hunter2", "password =•••"),
            ("password= hunter2", "password= •••"),
            ("password: hunter2", "password: •••"),
            // JSON and XML.
            (
                "{\"user\":\"a\",\"password\":\"hun\\\"ter2\"}",
                "{\"user\":\"a\",\"password\":\"•••\"}",
            ),
            ("{\"api_key\": 12345}", "{\"api_key\": •••}"),
            ("<password>hunter2</password>", "<password>•••</password>"),
            // Programs' own password flags.
            (
                "openssl enc -pass pass:hunter2",
                "openssl enc -pass pass:•••",
            ),
            (
                "openssl rsa -passin pass:hunter2",
                "openssl rsa -passin pass:•••",
            ),
            ("openssl enc -k hunter2", "openssl enc -k •••"),
            ("redis-cli -h db -a hunter2", "redis-cli -h db -a •••"),
            ("APP_KEY=base64:2fl+Ktvkfl+Fuz4Qp=", "APP_KEY=•••"),
            (
                "export SSH_KEY=abc HMAC_KEY=def",
                "export SSH_KEY=••• HMAC_KEY=•••",
            ),
            (
                "{\"SecretAccessKey\":\"wJalrXUtnFEMI\",\"SessionToken\":\"abc\"}",
                "{\"SecretAccessKey\":\"•••\",\"SessionToken\":\"•••\"}",
            ),
            ("secretKey: minio123", "secretKey: •••"),
            (
                "{\"Credentials\":{\"AccessKeyId\":\"ASIAX\",\"SecretAccessKey\":\"w\"}}",
                "{\"Credentials\":{\"AccessKeyId\":\"ASIAX\",\"SecretAccessKey\":\"•••\"}}",
            ),
            (
                "machine h login bob password hunter2",
                "machine h login bob password •••",
            ),
            (
                "smbclient //h/s -Uadmin%hunter2",
                "smbclient //h/s -Uadmin%•••",
            ),
            (
                "smbclient //h/s --user=admin%hunter2",
                "smbclient //h/s --user=admin%•••",
            ),
            (
                "C:\\bin\\mysql.exe -uroot -phunter2",
                "C:\\bin\\mysql.exe -uroot -p•••",
            ),
            (
                "curl -d \"{\\\"password\\\":\\\"hunter2\\\"}\" x",
                "curl -d \"{\\\"password\\\":\\\"•••\\\"}\" x",
            ),
            (
                "<password type=\"x\">hunter2</password>",
                "<password type=\"x\">•••</password>",
            ),
            (
                "git push https://abcdefghijklmnopqrstuvwxyz0123456789@example.com/r",
                "git push https://•••@example.com/r",
            ),
            ("ssh-keygen -p -P old -N new", "ssh-keygen -p -P ••• -N •••"),
            (
                "smbclient //h/s -U alice%hunter2",
                "smbclient //h/s -U alice%•••",
            ),
            ("az login -u a -p hunter2", "az login -u a -p •••"),
            (
                "curl -H 'Authorization: token ghx0123456789abcdef'",
                "curl -H 'Authorization: token •••'",
            ),
            (
                "Authorization: Bearer abc12345",
                "Authorization: Bearer •••",
            ),
        ] {
            assert_eq!(redact(input), expected, "{input}");
        }
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        for text in [
            "ls -la /var/log",
            "task-runner ask-me sk-short",
            "maskAKIA",
            "Bearer short",
            "đăng nhập thành công",
            "ssh -o CertificateFile=sk-ecdsa-sha2-nistp256-cert-v01@openssh.com x",
            "kubectl -n sk-payments-production-eu get pods",
            "tar -pcf out.tar dir",
            "ssh-keygen --key-file ~/.ssh/id",
            "ftp --passive host",
            "gpg --keyserver keys.example.org",
            "make PREFIX=/usr",
            "Enter the password now",
            "curl -k https://x",
            "token expired",
            "basic configuration done",
            "ssh -p 2222 host",
            "{\"user\": \"alice\"}",
            "<name>prod</name>",
            "openssl req -new -newkey rsa:2048 -nodes -keyout server.key -out server.csr",
            "{\"Tags\":[{\"Key\":\"Name\",\"Value\":\"web-1\"}]}",
            "<Key>photos/2024/a.jpg</Key>",
            "{\"token_type\":\"Bearer\",\"input_token_count\":12}",
            "{\"public_key\":\"ssh-ed25519 AAAA\",\"primary_key\":\"id\"}",
            "{\"PWD\":\"/home/coder\"}",
            "    fix: rotate token = per session",
            "  + key   = \"Environment\"",
            "aws_access_key_id = AKIAEXAMPLE",
            "see Private-Lines: in the docs",
            "docker run -p 8080:80 nginx",
            "sshpass -f pwfile ssh -p 2222 host",
            "curl -u alice https://x",
            "open https://example.com/a:b@c",
        ] {
            assert_eq!(redact(text), text, "{text}");
        }
    }

    #[test]
    fn putty_private_lines_are_hidden() {
        let ppk = "PuTTY-User-Key-File-3: ssh-ed25519\nEncryption: none\nPublic-Lines: 2\nAAAA\nBBBB\nPrivate-Lines: 1\nSECRETBODY\nPrivate-MAC: 0123\ndone";
        let out = redact(ppk);
        assert!(!out.contains("SECRETBODY"), "{out}");
        assert!(
            out.contains("Public-Lines: 2\nAAAA") && out.ends_with("•••\ndone"),
            "{out}"
        );
    }

    #[test]
    fn private_key_blocks_are_hidden() {
        let text = "cat id\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3Blbn\nNzaC1\n-----END OPENSSH PRIVATE KEY-----\ndone";
        assert_eq!(redact(text), "cat id\n•••\ndone");
        let cut = "-----BEGIN RSA PRIVATE KEY-----\nMIIE";
        assert_eq!(redact(cut), "•••");
        let public = "-----BEGIN CERTIFICATE-----\nMIIB";
        assert_eq!(redact(public), public);
    }

    #[test]
    fn a_wide_character_where_a_tag_is_cut_does_not_panic() {
        for at in 60..70 {
            let text = format!("<{}é>x</a>", "a".repeat(at));
            let _ = redact(&text);
            let text = format!("\"{}é\": 1", "a".repeat(at));
            let _ = redact(&text);
        }
    }

    #[test]
    fn long_runs_take_linear_time() {
        for unit in [
            "-----BEGIN ",
            "/",
            "=",
            ".",
            "a/",
            "sk-",
            "eyJ.",
            "<",
            "\"",
            "\"a\":",
            "<password>",
            "pass:",
        ] {
            let text = unit.repeat(70 * 1024 / unit.len());
            let started = std::time::Instant::now();
            let _ = redact(&text);
            let took = started.elapsed();
            assert!(
                took < std::time::Duration::from_millis(500),
                "{unit:?} took {took:?}"
            );
        }
    }
}
