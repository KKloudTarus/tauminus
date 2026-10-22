// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The prod guard against a corpus of dangerous and ordinary command lines.
//! Every dangerous line must be flagged with its rule. Ordinary lines that
//! are flagged count as false positives, and their rate must stay under the
//! bound recorded in docs/prod-guard.md.

#![expect(
    clippy::print_stdout,
    reason = "the test reports the false positive rate"
)]

use tauminus_domain::Policy;

/// False positives allowed on the safe corpus, in percent.
const MAX_FALSE_POSITIVE_PERCENT: usize = 3;

fn entries(text: &str) -> impl Iterator<Item = String> + '_ {
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.replace("<NL>", "\n"))
}

fn policy() -> Policy {
    Policy {
        active: true,
        ..Policy::default()
    }
}

#[test]
fn every_dangerous_line_is_flagged_with_its_rule() {
    let corpus = include_str!("guard/dangerous.txt");
    let mut misses = Vec::new();
    let mut count = 0;
    for entry in entries(corpus) {
        count += 1;
        let (rule, line) = entry.split_once('\t').expect("rule<TAB>command");
        let verdict = policy().check(line);
        if !verdict.findings.iter().any(|f| f.rule.id() == rule) {
            let got: Vec<_> = verdict.findings.iter().map(|f| f.rule.id()).collect();
            misses.push(format!("{line:?}: want {rule}, got {got:?}"));
        }
    }
    assert!(count >= 200, "the corpus has {count} dangerous lines");
    assert!(misses.is_empty(), "missed:\n{}", misses.join("\n"));
}

#[test]
fn ordinary_lines_rarely_ask() {
    let corpus = include_str!("guard/safe.txt");
    let lines: Vec<String> = entries(corpus).collect();
    let flagged: Vec<String> = lines
        .iter()
        .filter_map(|line| {
            let verdict = policy().check(line);
            let first = verdict.findings.first()?;
            Some(format!("{line:?}: {}", first.rule))
        })
        .collect();
    let percent = flagged.len() * 100 / lines.len();
    println!(
        "false positives: {} of {} ({percent}%)\n{}",
        flagged.len(),
        lines.len(),
        flagged.join("\n")
    );
    assert!(
        percent <= MAX_FALSE_POSITIVE_PERCENT,
        "{percent}% false positives:\n{}",
        flagged.join("\n")
    );
}
