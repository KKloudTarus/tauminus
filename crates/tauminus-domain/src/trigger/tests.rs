// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::{Duration, Instant};

use super::*;

fn id(text: &str) -> TriggerSetId {
    TriggerSetId::parse(text).unwrap()
}

fn rule(pattern: &str, line: bool) -> TriggerRule {
    TriggerRule {
        name: Label::parse(pattern).unwrap(),
        pattern: RulePattern::parse(pattern).unwrap(),
        action: Action::Highlight(Highlight::new(Some(Colour::Palette(1)), None, line).unwrap()),
        enabled: true,
    }
}

fn set(name: &str, rules: Vec<TriggerRule>) -> TriggerSet {
    TriggerSet::new(id(name), Label::parse(name).unwrap(), rules).unwrap()
}

fn spans(matcher: &Matcher, lines: &[&str]) -> Vec<(u32, u16, u32, u32)> {
    matcher
        .scan(lines)
        .into_iter()
        .map(|h| (h.line, h.style, h.start, h.end))
        .collect()
}

#[test]
fn patterns_are_checked_and_errors_say_what_is_wrong() {
    assert_eq!(RulePattern::parse(""), Err(TriggerError::EmptyPattern));
    assert_eq!(
        RulePattern::parse(&"a".repeat(MAX_RULE_PATTERN + 1)),
        Err(TriggerError::LongPattern)
    );
    let Err(TriggerError::BadPattern(why)) = RulePattern::parse("error(") else {
        panic!("an open group is an error");
    };
    assert!(why.contains("unclosed group"), "{why}");
    assert!(!why.contains('\n'));
    // Backreferences and lookaround are not in the syntax: they are what
    // makes a pattern slow on text chosen to make it so.
    assert!(RulePattern::parse(r"(a)\1").is_err());
    assert!(RulePattern::parse("a(?=b)").is_err());
    // A pattern that compiles too large is refused, not kept to stall later.
    assert!(matches!(
        RulePattern::checked(r"\w{1000}\w{1000}"),
        Err(TriggerError::BadPattern(_))
    ));
    // Read from a file, only the syntax is checked: compiling waits.
    assert!(RulePattern::parse(r"\w{1000}\w{1000}").is_ok());
    // Nesting deeper than the limit is refused without compiling.
    assert!(RulePattern::parse(&format!("{}a{}", "(".repeat(100), ")".repeat(100))).is_err());
}

#[test]
fn colours_read_back_as_written_and_nothing_else_reads() {
    for colour in [
        Colour::Palette(0),
        Colour::Palette(15),
        Colour::Rgb(1, 0xab, 0xff),
    ] {
        assert_eq!(Colour::parse(&colour.to_string()), Ok(colour));
    }
    assert_eq!(Colour::Rgb(1, 0xab, 0xff).to_string(), "#01abff");
    for bad in [
        "16", "+1", "-1", "", "007", "#12345", "#gggggg", "#1234567", "red",
    ] {
        assert_eq!(Colour::parse(bad), Err(TriggerError::BadColour), "{bad}");
    }
}

#[test]
fn a_highlight_needs_a_colour_and_palette_colours_are_the_sixteen() {
    assert_eq!(
        Highlight::new(None, None, false),
        Err(TriggerError::NoColour)
    );
    assert_eq!(Colour::palette(15), Ok(Colour::Palette(15)));
    assert_eq!(Colour::palette(16), Err(TriggerError::BadColour));
}

#[test]
fn hits_count_code_points_so_the_view_can_find_cells() {
    let matcher = Matcher::new(&[&rule("ERROR", false)]).unwrap();
    // "é" is two bytes and "漢" three, each one code point.
    assert_eq!(
        spans(&matcher, &["é漢 ERROR x ERROR", "fine", "ERROR"]),
        vec![(0, 0, 3, 8), (0, 0, 11, 16), (2, 0, 0, 5)]
    );
}

#[test]
fn an_earlier_rule_wins_text_both_match_and_whole_lines_are_their_own_layer() {
    let first = rule("disk full", false);
    let second = rule("full", false);
    let line = rule("disk", true);
    let matcher = Matcher::new(&[&first, &second, &line]).unwrap();
    let hits = spans(&matcher, &["disk full, full"]);
    // "full" inside "disk full" is the first rule's; the second "full" is
    // the second's; the whole line is the line rule's.
    assert_eq!(hits, vec![(0, 2, 0, 15), (0, 0, 0, 9), (0, 1, 11, 15)]);
}

#[test]
fn empty_matches_colour_nothing() {
    let matcher = Matcher::new(&[&rule("x*", false)]).unwrap();
    assert_eq!(spans(&matcher, &["abc", "axxb"]), vec![(1, 0, 1, 3)]);
}

#[test]
fn a_scan_is_bounded_in_lines_bytes_and_hits() {
    let matcher = Matcher::new(&[&rule("a", false)]).unwrap();
    let long = "a".repeat(MAX_SCAN_LINE * 2);
    let lines: Vec<&str> = vec![long.as_str(); 3];
    let hits = matcher.scan(&lines);
    assert_eq!(hits.len(), MAX_HITS);
    // A line cut at the limit is cut on a character boundary.
    assert_eq!(cut("éé", 3), "é");
    assert_eq!(cut("éé", 4), "éé");
    // A line past the last one read is not matched.
    let matcher = Matcher::new(&[&rule("b", false)]).unwrap();
    let mut many: Vec<&str> = vec!["a"; MAX_SCAN_LINES];
    many.push("b");
    assert_eq!(matcher.scan(&many), vec![]);
    many[MAX_SCAN_LINES - 1] = "b";
    assert_eq!(matcher.scan(&many).len(), 1);
    // Nor is one past the bytes a scan reads in all.
    let full = format!("{}b", "x".repeat(MAX_SCAN_LINE - 1));
    let per_line = full.len();
    let read = MAX_SCAN_BYTES / per_line;
    let lines: Vec<&str> = vec![full.as_str(); read + 4];
    assert_eq!(matcher.scan(&lines).len(), read);
}

#[test]
fn text_built_to_make_a_backtracking_engine_slow_scans_in_linear_time() {
    // Catastrophic for a backtracking engine: 2^n steps on a's with no
    // match at the end.
    let matcher = Matcher::new(&[&rule(r"^(a+)+$", false), &rule(r"(\w+\s?)+:", false)]).unwrap();
    let evil = format!("{}!", "a".repeat(MAX_SCAN_LINE - 1));
    let lines: Vec<&str> = vec![evil.as_str(); 64];
    let began = Instant::now();
    assert_eq!(matcher.scan(&lines), vec![]);
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "{:?}",
        began.elapsed()
    );
}

#[test]
fn rules_that_together_are_too_large_are_refused_at_once() {
    let rules = [
        rule(r"\w{20}a", false),
        rule(r"\w{20}b", false),
        rule(r"\w{20}c", false),
    ];
    let refs: Vec<&TriggerRule> = rules.iter().collect();
    // Each fits; together they pass the limit the set is held to.
    let one = Matcher::within(&refs[..1], usize::MAX).unwrap();
    assert!(!one.is_empty());
    let limit = (1..1024)
        .map(|k: usize| k * 64 * 1024)
        .find(|&size| Matcher::within(&refs[..1], size).is_ok())
        .unwrap();
    assert!(matches!(
        Matcher::within(&refs, limit),
        Err(TriggerError::TooLarge(_))
    ));
    // A set is checked so when it is made: one that could never match is
    // refused on save, not found out by a pane.
    assert!(matches!(
        TriggerSet::within(
            id("big"),
            Label::parse("big").unwrap(),
            rules.to_vec(),
            limit
        ),
        Err(TriggerError::TooLarge(_))
    ));
}

#[test]
fn sets_apply_nearest_first_then_defaults_each_once_and_only_enabled_rules() {
    let mut off = rule("off", false);
    off.enabled = false;
    let library = TriggerLibrary::new(
        vec![
            set("host", vec![rule("h", false), off]),
            set("group", vec![rule("g", false)]),
            set("default", vec![rule("d", false)]),
        ],
        vec![id("default"), id("gone")],
    )
    .unwrap();
    // A default naming no set is dropped.
    assert_eq!(library.defaults(), &[id("default")]);
    let applied: Vec<&str> = library
        .applied(&[id("host"), id("gone"), id("group"), id("default")])
        .iter()
        .map(|r| r.pattern.as_str())
        .collect();
    assert_eq!(applied, vec!["h", "g", "d"]);
}

#[test]
fn removing_a_set_takes_it_out_of_the_defaults() {
    let mut library = TriggerLibrary::new(
        vec![set("a", vec![]), set("b", vec![])],
        vec![id("a"), id("b")],
    )
    .unwrap();
    library.remove(&id("a")).unwrap();
    assert_eq!(library.defaults(), &[id("b")]);
    assert_eq!(library.remove(&id("a")), Err(TriggerError::NoSuch(id("a"))));
}

#[test]
fn the_library_is_bounded() {
    let rules: Vec<TriggerRule> = (0..=MAX_SET_RULES).map(|_| rule("a", false)).collect();
    assert_eq!(
        TriggerSet::new(id("s"), Label::parse("s").unwrap(), rules),
        Err(TriggerError::TooManyRules)
    );
    let mut library = TriggerLibrary::default();
    for i in 0..MAX_SETS {
        library.put(set(&format!("s{i}"), vec![])).unwrap();
    }
    assert_eq!(
        library.put(set("more", vec![])),
        Err(TriggerError::TooManySets)
    );
    // Replacing one is not adding one.
    library.put(set("s0", vec![rule("a", false)])).unwrap();
    let defaults: Vec<TriggerSetId> = (0..=MAX_LEVEL_SETS).map(|i| id(&format!("s{i}"))).collect();
    assert_eq!(library.set_defaults(defaults), Err(TriggerError::Defaults));
    assert_eq!(
        library.set_defaults(vec![id("s0"), id("s0")]),
        Err(TriggerError::Defaults)
    );
}

#[test]
fn many_rules_matching_everywhere_scan_in_time_linear_in_the_line() {
    // Each rule matches every number on the line, and all overlap: with a
    // check against every span taken, this was quadratic (1.2 s in release).
    let rules: Vec<TriggerRule> = (0..MAX_SET_RULES)
        .map(|i| rule(&format!(r"\d+|tok{i}"), false))
        .collect();
    let refs: Vec<&TriggerRule> = rules.iter().collect();
    let matcher = Matcher::new(&refs).unwrap();
    let line = "1 ".repeat(MAX_SCAN_LINE / 2);
    let began = Instant::now();
    let hits = matcher.scan(&[line.as_str()]);
    // The first rule takes every number; the rest find nothing free.
    assert_eq!(hits.len(), MAX_HITS);
    assert!(hits.iter().all(|h| h.style == 0));
    assert!(
        began.elapsed() < Duration::from_secs(1),
        "{:?}",
        began.elapsed()
    );
}

#[test]
fn a_match_that_overlaps_an_earlier_rule_s_is_left_out_whole() {
    let matcher = Matcher::new(&[&rule("bc", false), &rule("abcd", false)]).unwrap();
    assert_eq!(spans(&matcher, &["abcd"]), vec![(0, 0, 1, 3)]);
}
