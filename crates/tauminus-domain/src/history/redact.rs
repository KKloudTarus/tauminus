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
const SECRET_WORDS: &[&str] = &["KEY", "PASS", "PW", "PWD", "CREDENTIAL", "CREDENTIALS"];
/// Endings of such parts.
const SECRET_ENDINGS: &[&str] = &[
    "PASSWORD",
    "PASSWD",
    "PASSPHRASE",
    "SECRET",
    "TOKEN",
    "APIKEY",
];
/// Clients whose `-p<password>` holds a password.
const PASSWORD_CLIENTS: &[&str] = &[
    "mysql",
    "mariadb",
    "mysqldump",
    "mysqladmin",
    "mysqlsh",
    "sshpass",
    "docker",
    "podman",
];

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
    let text = redact_tokens(&text);
    redact_words(&text)
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
            for scheme in ["bearer ", "basic "] {
                if let Some(head) = rest.get(..scheme.len())
                    && head.eq_ignore_ascii_case(scheme)
                {
                    let after = &rest[scheme.len()..];
                    let run = after.find(|c: char| !token_char(c)).unwrap_or(after.len());
                    if run >= 16 {
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

/// Whether a variable's or flag's name says its value is a secret.
/// One of its parts, split at `-`, `_`, and the like, is a secret's word
/// (`KEY`, `PASS`) or ends with one (`PGPASSWORD`, `GITHUB_TOKEN`), so
/// `--keyserver` and `--passive` are no secrets' names.
fn secret_name(name: &str) -> bool {
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|part| {
            let part = part.to_ascii_uppercase();
            SECRET_WORDS.contains(&part.as_str())
                || SECRET_ENDINGS.iter().any(|end| part.ends_with(end))
        })
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
}

/// Secrets by their place on a line: `NAME=value` and `--name value` with
/// a secret's name, `-u user:password`, a URL's password, and a MySQL
/// client's `-p<password>`. A quoted value with spaces is hidden to its
/// closing quote.
fn redact_words(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // Inside a hidden quoted value: the quote that ends it, on this line or
    // a later one.
    let mut quote: Option<char> = None;
    for line in text.split_inclusive('\n') {
        let mut next = Next::Keep;
        let mut client = false;
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
                Next::Hide => {
                    out.push_str(HIDDEN);
                    quote = open_quote(word);
                    continue;
                }
                Next::User => {
                    if let Some(colon) = word.find(':') {
                        out.push_str(&word[..=colon]);
                        out.push_str(HIDDEN);
                        quote = open_quote(word);
                        continue;
                    }
                }
                Next::Keep => {}
            }
            let base = word.rsplit('/').next().unwrap_or(word);
            client |= PASSWORD_CLIENTS.contains(&base);
            if word == "-u" || word == "--user" {
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
            } else if client && word == "-p" {
                next = Next::Hide;
                out.push_str(word);
            } else if let Some(name) = word.strip_suffix(':')
                && secret_name(name.trim_start_matches(['\'', '"']))
            {
                // A header or a field: `X-Api-Key: value`, `Token: value`.
                next = Next::Hide;
                out.push_str(word);
            } else if client && word.len() > 2 && word.starts_with("-p") {
                out.push_str("-p");
                out.push_str(HIDDEN);
                quote = open_quote(&word[2..]);
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
    const BEGIN: &str = "-----BEGIN ";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(BEGIN) {
        let header_end = rest[at..].find('\n').map_or(rest.len(), |n| at + n);
        if !rest[at..header_end].contains("PRIVATE KEY-----") {
            out.push_str(&rest[..at + BEGIN.len()]);
            rest = &rest[at + BEGIN.len()..];
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
            ("{\"api_key\": \"abc\"}", "{\"api_key\": •••"),
            ("PGPASSWORD=hunter2 psql", "PGPASSWORD=••• psql"),
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
            "curl -u alice https://x",
            "open https://example.com/a:b@c",
        ] {
            assert_eq!(redact(text), text, "{text}");
        }
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
    fn long_runs_take_linear_time() {
        for unit in ["/", "=", ".", "a/", "sk-", "eyJ."] {
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
