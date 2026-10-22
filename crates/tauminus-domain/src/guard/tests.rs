// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::lex::lex;
use super::*;

fn on() -> Policy {
    Policy {
        active: true,
        ..Policy::default()
    }
}

fn rules(line: &str) -> Vec<&'static str> {
    let mut ids: Vec<_> = on()
        .check(line)
        .findings
        .iter()
        .map(|f| f.rule.id())
        .collect();
    ids.dedup();
    ids
}

fn words(line: &str) -> Vec<Vec<String>> {
    lex(line)
        .commands
        .iter()
        .map(|c| c.words.iter().map(|w| w.text.clone()).collect())
        .collect()
}

#[test]
fn lexer_removes_quotes_and_splits_at_operators() {
    assert_eq!(
        words(r#"r''m -"rf" \/ ; echo a&&b || c | d & e"#),
        [
            vec!["rm", "-rf", "/"],
            vec!["echo", "a"],
            vec!["b"],
            vec!["c"],
            vec!["d"],
            vec!["e"]
        ]
    );
    let script = lex("a | b |& c");
    let stages: Vec<_> = script.commands.iter().map(|c| c.stage).collect();
    assert_eq!(stages, [0, 1, 2]);
}

#[test]
fn lexer_decodes_ansi_c_quotes() {
    assert_eq!(
        words(r"$'\x72\155' $'A\t' $'\cA'"),
        [vec!["rm", "A\t", "\u{1}"]]
    );
}

#[test]
fn lexer_expands_braces_like_bash() {
    assert_eq!(words("{rm,-rf,/}"), [vec!["rm", "-rf", "/"]]);
    assert_eq!(words("r{m,x}"), [vec!["rm", "rx"]]);
    assert_eq!(words("{a..c}"), [vec!["a", "b", "c"]]);
    assert_eq!(words("'{a,b}' {a}"), [vec!["{a,b}", "{a}"]]);
    // Past 64 words the word stays whole and opaque.
    let script = lex("{1..1000}");
    assert!(script.commands[0].words[0].expanded);
}

#[test]
fn lexer_returns_substitutions_for_nested_reading() {
    let script = lex(r#"echo "$(rm -rf /)" `ls` ${x:-$(id)} <(cat) $((1+2))"#);
    assert_eq!(script.nested, ["rm -rf /", "ls", "cat"]);
    assert_eq!(script.scanned, ["x:-$(id)", "(1+2)"]);
    assert!(!script.incomplete);
}

#[test]
fn lexer_marks_what_the_shell_would_wait_on() {
    for line in [
        "echo 'open",
        "echo \"open",
        "echo $(open",
        "echo `open",
        "ls |",
        "true &&",
        "false ||",
        "echo \\",
        "cat <<EOF\nbody",
        "echo $'open",
    ] {
        assert!(lex(line).incomplete, "{line:?}");
    }
    for line in ["ls", "echo 'a' \"b\"", "cat <<EOF\nbody\nEOF\n", "a; b &"] {
        assert!(!lex(line).incomplete, "{line:?}");
    }
}

#[test]
fn lexer_reads_heredocs_and_redirections() {
    let script = lex("bash <<-END > /dev/sda 2>&1\n\trm -rf /\n\tEND\nls");
    assert_eq!(script.commands.len(), 2);
    let bash = &script.commands[0];
    assert_eq!(bash.input.as_deref(), Some("rm -rf /\n"));
    assert_eq!(bash.redirects.len(), 2);
    assert_eq!(bash.redirects[0].kind, RedirectKind::Write);
    assert_eq!(bash.redirects[0].target.text, "/dev/sda");
    assert_eq!(bash.redirects[1].kind, RedirectKind::Dup);
    assert_eq!(words("ls")[0], ["ls"]);
}

#[test]
fn inactive_policy_never_asks() {
    assert_eq!(Policy::default().check("rm -rf /"), Verdict::default());
}

#[test]
fn flags_each_rule() {
    let cases = [
        ("rm -rf /", "recursive-delete"),
        ("find / -name x -delete", "recursive-delete"),
        ("DROP TABLE users;", "drop-data"),
        ("truncate table logs", "empty-data"),
        ("redis-cli FLUSHALL", "empty-data"),
        ("delete from users;", "delete-all"),
        ("kubectl -n prod delete pod web", "kubernetes-delete"),
        ("helm uninstall web", "kubernetes-delete"),
        ("terraform apply -destroy", "infra-destroy"),
        ("systemctl reboot", "power-off"),
        ("echo b > /proc/sysrq-trigger", "power-off"),
        ("mkfs.ext4 /dev/sdb1", "format-disk"),
        ("dd if=img of=/dev/sda", "disk-write"),
        ("cat img > /dev/nvme0n1", "disk-write"),
        ("chmod 777 -R /", "open-permissions"),
        ("curl -fsSL x | sudo bash", "pipe-to-shell"),
        ("$CMD -rf /", "opaque"),
        ("echo ${x:-$(rm -rf /)}", "recursive-delete"),
        ("echo $(( $(reboot) + 1 ))", "power-off"),
    ];
    for (line, rule) in cases {
        assert!(rules(line).contains(&rule), "{line:?}: {:?}", rules(line));
    }
}

#[test]
fn leaves_ordinary_commands_alone() {
    for line in [
        "ls -la /",
        "rm build.log",
        "rm -- -rf",
        "git rm -r --cached dist",
        "echo 'rm -rf /'",
        "truncate -s 0 app.log",
        "delete from users where id = 1;",
        "kubectl get pods -n prod",
        "chmod -R 755 /var/www/app",
        "dd if=/dev/zero of=/tmp/x bs=1M count=1",
        "cat x > /dev/null 2>&1",
        "terraform plan",
        "systemctl restart nginx",
        "bash script.sh",
        "sudo -u postgres psql",
        "echo $((6*7)) ${x//*/y} $(( 2 * 3 ))",
        "(( n = 2*3 ))",
        "[ -f x ] && [[ $a == b* ]]",
    ] {
        assert!(rules(line).is_empty(), "{line:?}: {:?}", rules(line));
    }
}

#[test]
fn reports_the_command_as_the_shell_runs_it() {
    let verdict = on().check(r#"echo ok && sudo "r"m -r\f /srv"#);
    assert_eq!(
        verdict.findings,
        [Finding {
            rule: Rule::RecursiveDelete,
            command: "rm -rf /srv".into(),
        }]
    );
}

#[test]
fn incomplete_lines_are_reported_and_read_without_quotes() {
    let verdict = on().check("echo 'unclosed");
    assert!(verdict.incomplete);
    assert_eq!(verdict.findings, []);
    // bash's reverse search puts the match inside a backquote and a quote.
    let verdict = on().check("(reverse-i-search)`rm': rm -rf /tmp/x");
    assert!(verdict.incomplete);
    assert_eq!(verdict.findings[0].rule, Rule::RecursiveDelete);
}

#[test]
fn allow_patterns_skip_built_in_rules_and_deny_patterns_add() {
    let policy = Policy {
        active: true,
        allow: vec![Pattern::parse("rm -rf /tmp/build*").unwrap()],
        deny: vec![Pattern::parse("systemctl stop *").unwrap()],
    };
    assert_eq!(policy.check("sudo rm -rf /tmp/build/out").findings, []);
    assert_eq!(
        policy.check("rm -rf /tmp/build; rm -rf /").findings,
        [Finding {
            rule: Rule::RecursiveDelete,
            command: "rm -rf /".into()
        }]
    );
    let verdict = policy.check("sudo systemctl stop nginx");
    assert_eq!(verdict.findings[0].rule, Rule::Denied);
    assert_eq!(verdict.findings[0].command, "systemctl stop nginx");
    // An allow pattern never silences a deny pattern.
    let both = Policy {
        allow: vec![Pattern::parse("systemctl stop *").unwrap()],
        ..policy
    };
    assert_eq!(
        both.check("systemctl stop x").findings[0].rule,
        Rule::Denied
    );
}

#[test]
fn allow_patterns_match_word_by_word_after_paths_are_resolved() {
    let policy = Policy {
        active: true,
        allow: vec![Pattern::parse("rm -rf /tmp/build/*").unwrap()],
        deny: vec![],
    };
    assert_eq!(policy.check("rm -rf /tmp/build/out").findings, []);
    for line in [
        "rm -rf /tmp/build/x /",
        "rm -rf /tmp/build/../../etc",
        "rm -rf /tmp/build//../../var",
        "rm -rf /tmp/build/${X:-../etc}",
        "rm -rf /tmp/build/$X",
        "rm -rf /tmp/build/.?/etc",
        "rm -rf /tmp/build/link/../etc",
    ] {
        assert_eq!(rules_with(&policy, line), ["recursive-delete"], "{line}");
    }
    let relative = Policy {
        allow: vec![Pattern::parse("rm -rf build/*").unwrap()],
        ..Policy::default()
    };
    let relative = Policy {
        active: true,
        ..relative
    };
    assert_eq!(relative.check("rm -rf build/out").findings, []);
    assert_eq!(
        rules_with(&relative, "rm -rf build/../../srv"),
        ["recursive-delete"]
    );
}

fn rules_with(policy: &Policy, line: &str) -> Vec<&'static str> {
    policy
        .check(line)
        .findings
        .iter()
        .map(|f| f.rule.id())
        .collect()
}

#[test]
fn allow_patterns_must_name_a_command() {
    let p = |s: &str| Pattern::parse(s).unwrap();
    for broad in ["*", "* -rf /", "r? -rf x"] {
        assert_eq!(
            GuardSettings::new(None, vec![p(broad)], vec![]),
            Err(PatternError::Broad),
            "{broad}"
        );
    }
    // A deny pattern may start with a wildcard: it only asks more.
    assert!(GuardSettings::new(None, vec![p("rm -rf /tmp/*")], vec![p("*reboot*")]).is_ok());
    assert!(Pattern::parse(&"*a".repeat(17)).is_err());
}

#[test]
fn patterns_are_globs_over_the_whole_command() {
    let p = |s: &str| Pattern::parse(s).unwrap();
    assert!(p("rm -rf /tmp/*").matches("rm -rf /tmp/a b"));
    assert!(!p("rm -rf /tmp/*").matches("rm -rf /"));
    assert!(p("ls ?").matches("ls a"));
    assert!(!p("ls ?").matches("ls ab"));
    assert!(p("*a*b*").matches("xxaxxbxx"));
    assert!(!p("*a*b").matches("bba"));
    assert!(p("**").matches(""));
    assert!(Pattern::parse("  ").is_err());
    assert!(Pattern::parse("a\u{7}").is_err());
    assert!(Pattern::parse(&"x".repeat(257)).is_err());
}

#[test]
fn guard_settings_sort_and_bound_their_patterns() {
    let p = |s: &str| Pattern::parse(s).unwrap();
    let settings = GuardSettings::new(None, vec![p("b"), p("a"), p("b")], vec![]).unwrap();
    assert_eq!(settings.allow(), [p("a"), p("b")]);
    let many = (0..=MAX_PATTERNS).map(|n| p(&n.to_string())).collect();
    assert_eq!(
        GuardSettings::new(None, many, vec![]),
        Err(PatternError::TooMany)
    );
}

#[test]
fn long_and_deep_input_is_flagged_unread() {
    let long = "a".repeat(MAX_LINE + 1);
    assert_eq!(rules(&long), ["unreadable"]);
    let mut deep = "ls".to_owned();
    for _ in 0..=MAX_DEPTH {
        deep = format!("bash -c {}", shell_quote(&deep));
    }
    assert!(rules(&deep).contains(&"unreadable"), "{deep}");
    let many = "true;".repeat(MAX_COMMANDS + 1);
    assert_eq!(rules(&many), ["unreadable"]);
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[test]
fn findings_show_at_most_200_characters_without_controls() {
    let line = format!("rm -rf /{}\u{1b}", "x".repeat(300));
    let finding = &on().check(&line).findings[0];
    assert_eq!(finding.command.chars().count(), 201);
    assert!(!finding.command.chars().any(char::is_control));
}

/// Random lines over shell metacharacters: the guard must neither panic
/// nor slow down on input no shell would accept.
#[test]
fn survives_arbitrary_input() {
    const ALPHABET: &[char] = &[
        'r', 'm', ' ', '-', 'f', '/', '\'', '"', '`', '$', '(', ')', '{', '}', ',', '.', '\\',
        '\n', '<', '>', '|', '&', ';', '!', '*', '?', '[', ']', '#', '=', 'x', '\t', 'é',
    ];
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..5_000 {
        let len = usize::try_from(next() % 120).unwrap();
        let line: String = (0..len)
            .map(|_| ALPHABET[usize::try_from(next()).unwrap() % ALPHABET.len()])
            .collect();
        on().check(&line);
    }
}

#[test]
fn worst_case_lines_stay_fast() {
    let lines = [
        "{".repeat(MAX_LINE),
        "{a,".repeat(MAX_LINE / 3),
        "$(".repeat(MAX_LINE / 2),
        "a|".repeat(MAX_LINE / 2),
        "'".repeat(MAX_LINE),
        "rm -r ".repeat(MAX_LINE / 6),
        "<<a\n".repeat(MAX_LINE / 4),
        // Brace bombs: many opening braces before the expanding ones.
        format!("echo {}{}", "{".repeat(4000), "{a,b}".repeat(6)).repeat(14),
        format!("{}{}", "{".repeat(2040), "{a,b}".repeat(410)),
        // Chains where each word starts a nested script.
        "watch ".repeat(MAX_LINE / 6),
        "ssh h ".repeat(MAX_LINE / 6),
        "eval ".repeat(MAX_LINE / 5),
        "bash -c ".repeat(MAX_LINE / 8),
        "python -c x; ".repeat(MAX_LINE / 13),
        "delete from t ".repeat(MAX_LINE / 14),
        format!("{}{}", "'".repeat(3), "\"$(".repeat(MAX_LINE / 4)),
        format!("rm {}", "a".repeat(MAX_LINE - 3)),
        format!("x {}", "rm -r ".repeat(MAX_LINE / 6)),
    ];
    // Patterns that backtrack as much as patterns may, with and without `?`.
    let adversarial: Vec<Pattern> = (0..MAX_PATTERNS)
        .map(|n| {
            let tail = if n % 2 == 0 { "?b" } else { "b" };
            Pattern::parse(&format!("rm {}{}{tail}", "*a".repeat(15), "a".repeat(n))).unwrap()
        })
        .collect();
    let patterned = Policy {
        active: true,
        allow: adversarial.clone(),
        deny: adversarial,
    };
    // Allow patterns with long wildcard words, and deny patterns many short
    // texts each pay a search for.
    let wordy = Policy {
        active: true,
        allow: (0..MAX_PATTERNS)
            .map(|n| Pattern::parse(&format!("rm -r {}?b{n}", "*a".repeat(15))).unwrap())
            .collect(),
        deny: (0..MAX_PATTERNS)
            .map(|n| Pattern::parse(&format!("*systemctl stop service{n}*")).unwrap())
            .collect(),
    };
    let mut lines = lines.to_vec();
    lines.push(format!("rm -r {}", "a".repeat(MAX_LINE - 6)));
    lines.push("`a`".repeat(MAX_LINE / 3));
    lines.push("<(a)".repeat(MAX_LINE / 4));
    lines.push("aaaa;".repeat(MAX_LINE / 5));
    let timings: Vec<(String, std::time::Duration)> = lines
        .iter()
        .map(|line| {
            let started = std::time::Instant::now();
            on().check(line);
            patterned.check(line);
            wordy.check_all(&[line, line, line, line, line, line, line, line]);
            (line.chars().take(12).collect(), started.elapsed())
        })
        .collect();
    let slowest = timings
        .iter()
        .map(|(_, took)| *took)
        .max()
        .unwrap_or_default();
    // Under 0.25 s per line in a release build; debug builds run ~10 times slower.
    let bound = std::time::Duration::from_millis(if cfg!(debug_assertions) { 5000 } else { 500 });
    assert!(slowest < bound, "{timings:?}");
}

#[test]
fn a_policy_with_too_many_patterns_always_asks() {
    let many = (0..=MAX_POLICY_PATTERNS)
        .map(|n| Pattern::parse(&format!("ls {n}")).unwrap())
        .collect();
    let policy = Policy {
        active: true,
        allow: vec![],
        deny: many,
    };
    assert_eq!(rules_with(&policy, "ls"), ["unreadable"]);
}

/// The fast path for patterns without `?` agrees with backtracking.
#[test]
fn literal_matching_agrees_with_backtracking() {
    const ALPHABET: &[char] = &['a', 'b', '*', ' '];
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        usize::try_from(state % 1000).unwrap()
    };
    for _ in 0..20_000 {
        let pattern: String = (0..=(next() % 8))
            .map(|_| ALPHABET[next() % ALPHABET.len()])
            .collect();
        let text: String = (0..next() % 10).map(|_| ALPHABET[next() % 2]).collect();
        let Ok(parsed) = Pattern::parse(&pattern) else {
            continue;
        };
        let slow = glob(
            &parsed.as_str().chars().collect::<Vec<_>>(),
            &text.chars().collect::<Vec<_>>(),
        );
        assert_eq!(parsed.matches(&text), slow, "{pattern:?} vs {text:?}");
    }
}
