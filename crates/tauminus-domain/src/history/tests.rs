// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

fn query(text: &str) -> HistoryQuery {
    HistoryQuery {
        text: text.into(),
        limit: 100,
        ..HistoryQuery::default()
    }
}

#[test]
fn search_words_never_become_fts_operators() {
    assert_eq!(query("").fts_query(), None);
    assert_eq!(query("   ").fts_query(), None);
    assert_eq!(query("kubectl").fts_query().unwrap(), "\"kubectl\"*");
    assert_eq!(query("git push").fts_query().unwrap(), "\"git\" \"push\"*");
    // Operators, columns, and quotes stay plain words.
    assert_eq!(
        query("OR command: NEAR(x \"y").fts_query().unwrap(),
        "\"OR\" \"command:\" \"NEAR(x\" \"\"\"y\"*"
    );
}

#[test]
fn queries_are_bounded() {
    assert!(query("x").check().is_ok());
    assert!(query(&"x".repeat(600)).check().is_err());
    let mut q = query("x");
    q.limit = 0;
    assert!(q.check().is_err());
    q.limit = MAX_RESULTS + 1;
    assert!(q.check().is_err());
    let mut q = query("x");
    q.from_ms = Some(10);
    q.to_ms = Some(5);
    assert!(q.check().is_err());
}

#[test]
fn entries_are_cut_to_their_limits_without_breaking_characters() {
    let entry = HistoryEntry {
        host: None,
        label: "local".into(),
        at_ms: 1,
        command: format!("  echo {}  ", "é".repeat(MAX_COMMAND)),
        output: format!("ok\u{1b}[31m\r\n{}", "x".repeat(MAX_OUTPUT)),
        exit: Some(0),
    }
    .bounded();
    assert!(entry.command.len() <= MAX_COMMAND);
    assert!(entry.command.starts_with("echo é"));
    assert!(entry.output.len() <= MAX_OUTPUT);
    // Escape and carriage return go; the newline stays.
    assert!(entry.output.starts_with("ok[31m\n"));
    assert_eq!(
        entry.size(),
        (entry.command.len() + entry.output.len() + 5) as u64
    );
}

#[test]
fn a_command_keeps_no_newline_that_would_run_it() {
    let entry = HistoryEntry {
        host: None,
        label: String::new(),
        at_ms: 0,
        command: "echo a\nrm -rf ~\tx".into(),
        output: "a\nb".into(),
        exit: None,
    }
    .bounded();
    assert_eq!(entry.command, "echo a rm -rf ~ x");
    assert_eq!(entry.output, "a\nb");
}

#[test]
fn a_one_letter_last_word_is_a_word_not_a_prefix() {
    assert_eq!(query("a").fts_query().unwrap(), "\"a\"");
    assert_eq!(query("ls -a").fts_query().unwrap(), "\"ls\" \"-a\"*");
}

#[test]
fn a_search_reads_at_most_eight_words_and_an_eight_letter_prefix() {
    let many = query("a1 b2 c3 d4 e5 f6 g7 h8 i9 j10");
    assert_eq!(
        many.fts_query().unwrap(),
        "\"a1\" \"b2\" \"c3\" \"d4\" \"e5\" \"f6\" \"g7\" \"h8\"*"
    );
    assert_eq!(query("kubernetes").fts_query().unwrap(), "\"kubernet\"*");
    assert_eq!(query("đăngnhậpxyz").fts_query().unwrap(), "\"đăngnhập\"*");
    assert_eq!(query("--------x").fts_query().unwrap(), "\"--------x\"*");
}

#[test]
fn a_time_range_needs_its_start() {
    let mut q = query("x");
    q.to_ms = Some(5);
    assert!(q.check().is_err());
    q.from_ms = Some(1);
    assert!(q.check().is_ok());
}

#[test]
fn an_excerpt_marks_the_words_around_the_first_one() {
    let q = query("Error conn");
    let output = "starting\nupstream ERROR: connection refused by peer\ndone";
    assert_eq!(
        q.excerpt(output),
        "upstream \u{1}ERROR\u{2}: \u{1}conn\u{2}ection refused by peer"
    );
    assert_eq!(q.excerpt("nothing here"), "");
    let long = format!("{}error{}", "x".repeat(100), "y".repeat(300));
    let excerpt = query("error").excerpt(&long);
    assert!(
        excerpt.starts_with('…') && excerpt.ends_with('…'),
        "{excerpt}"
    );
    assert!(excerpt.chars().count() < 200);
    // A word with diacritics matches as typed, on character boundaries.
    assert_eq!(
        query("nhập").excerpt("đăng nhập thành công"),
        "đăng \u{1}nhập\u{2} thành công"
    );
}
