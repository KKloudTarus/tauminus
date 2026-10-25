// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Duration;

use super::*;

const TRIGGER: &[u8] = b"\x1b[s::TRZSZ:TRANSFER:R:1.2.0:1729812345600:41234\r\n";

#[test]
fn a_whole_trigger_is_found_with_what_it_says() {
    let mut output = b"$ trz\r\n".to_vec();
    output.extend_from_slice(TRIGGER);
    let found = find_trigger(&output).unwrap();
    assert_eq!(found.mode, Mode::Receive);
    assert_eq!(found.version, "1.2.0");
    assert_eq!(found.id, "1729812345600");
    assert!(!found.windows && !found.tmux_control);
    assert_eq!(&output[found.at..found.at + 2], b"::", "held from the mark");
    assert_eq!(found.end, output.len());
    // Python's tsz, no port, and a Windows host's id.
    let python = find_trigger(b"\x1b7::TRZSZ:TRANSFER:S:1.1.7:0001234567800\r\n").unwrap();
    assert_eq!((python.mode, python.windows), (Mode::Send, false));
    let windows = find_trigger(b"::TRZSZ:TRANSFER:R:1.2.0:1729812345610:0\r\n").unwrap();
    assert!(windows.windows);
    let folders = find_trigger(b"::TRZSZ:TRANSFER:D:1.2.0:1729812345600:0\r\n").unwrap();
    assert_eq!(folders.mode, Mode::Folders);
}

#[test]
fn a_trigger_cut_anywhere_or_wrong_anywhere_is_not_found() {
    for cut in 0..TRIGGER.len() {
        assert_eq!(find_trigger(&TRIGGER[..cut]), None, "cut at {cut}");
    }
    for bad in [
        &b"::TRZSZ:TRANSFER:X:1.2.0:1729812345600:1\r\n"[..],
        b"::TRZSZ:TRANSFER:R:1.2:1729812345600:1\r\n",
        b"::TRZSZ:TRANSFER:R:1.2.0:172981234560:1\r\n",
        b"::TRZSZ:TRANSFER:R:1.2.0:1729812345600:1\n",
        b"::TRZSZ:TRANSFER:R:1.2.0:1729812345600:123456\r\n",
        b"echo '::TRZSZ:TRANSFER:R:1.2.0:1729812345600'",
    ] {
        assert_eq!(find_trigger(bad), None, "{}", String::from_utf8_lossy(bad));
    }
    // The longest one fits what is kept across reads.
    let longest = b"::TRZSZ:TRANSFER:R:99999.99999.99999:1729812345600:65535\r\n";
    assert_eq!(longest.len(), TRIGGER_KEEP);
    assert!(find_trigger(longest).is_some());
    assert_eq!(
        find_trigger(b"::TRZSZ:TRANSFER:R:1.2.0:17298123456001:1\r\n"),
        None,
        "a longer id"
    );
}

#[test]
fn a_trigger_in_a_tmux_control_line_is_passed_over() {
    let line = b"%output %3 \x1b[s::TRZSZ:TRANSFER:R:1.2.0:1729812345600:0\r\n";
    assert_eq!(find_trigger(line), None);
    let mut then = line.to_vec();
    then.extend_from_slice(TRIGGER);
    assert_eq!(
        find_trigger(&then).unwrap().at,
        line.len() + 3,
        "the next one counts"
    );
}

#[test]
fn payloads_read_back_and_match_what_trzsz_writes() {
    // From trzsz-go's transfer_test.go and its stop message.
    let act = decode(
        b"eJyrVspJzEtXslJQKqhU0lFQSs7PS8ssygUKlBSVpgIFylKLijPz80AqDPUM9AxAiopLCwryi0riUzKLEAoLivJL8pPzc4AiBrUAlAQbEA==",
        MAX_LINE,
    )
    .unwrap();
    assert!(String::from_utf8(act).unwrap().contains("\"lang\": \"py\""));
    assert_eq!(decode(b"eJwLLskvKEhNAQALbQLg", 64).unwrap(), b"Stopped");
    let text = b"#!/bin/sh\necho hello\n".repeat(50);
    assert_eq!(decode(encode(&text).as_bytes(), text.len()).unwrap(), text);
    for bad in [&b"not base64!"[..], b"AAAA", b"eJwLLskvKEhNAQ"] {
        assert!(decode(bad, 64).is_err(), "{}", String::from_utf8_lossy(bad));
    }
}

#[test]
fn a_payload_that_inflates_past_its_bound_stops_there() {
    let bomb = encode(&vec![0u8; 8 << 20]);
    assert!(bomb.len() < 64 << 10, "small on the wire");
    assert_eq!(
        decode(bomb.as_bytes(), 1 << 20),
        Err(TrzszError::TooLong(1 << 20))
    );
}

#[test]
fn lines_are_cut_as_they_come_and_bounded() {
    let mut lines = Lines::new();
    lines.push(b"#CFG:abc");
    assert_eq!(lines.next("CFG", MAX_LINE), Ok(None));
    lines.push(b"def\n#SUCC:5\n");
    let line = lines.next("CFG", MAX_LINE).unwrap().unwrap();
    assert_eq!(
        (line.kind.as_str(), line.payload.as_slice()),
        ("CFG", &b"abcdef"[..])
    );
    let line = lines.next("SUCC", MAX_LINE).unwrap().unwrap();
    assert_eq!(number(&line.payload), Ok(5));
    assert_eq!(lines.pending(), 0);

    let mut long = Lines::new();
    long.push(&[b'A'; 100]);
    assert_eq!(long.next("DATA", 64), Err(TrzszError::TooLong(64)));
    let mut ctrl_c = Lines::new();
    ctrl_c.push(b"#SUCC:\x035");
    assert_eq!(ctrl_c.next("SUCC", 64), Err(TrzszError::Interrupted));
    let mut noise = Lines::new();
    noise.push(b"bash: trz: command not found\n");
    assert!(matches!(
        noise.next("CFG", 64),
        Err(TrzszError::Protocol(_))
    ));
}

#[test]
fn a_long_line_in_small_reads_is_scanned_once() {
    let mut lines = Lines::new();
    lines.push(b"#DATA:");
    for _ in 0..(1 << 14) {
        lines.push(&[b'A'; 64]);
        assert_eq!(lines.next("DATA", MAX_DATA_LINE), Ok(None));
    }
    // scanned keeps up: no byte is looked at twice.
    assert_eq!(lines.scanned_for_test(), lines.pending());
    lines.push(b"\n");
    let line = lines.next("DATA", MAX_DATA_LINE).unwrap().unwrap();
    assert_eq!(line.payload.len(), 64 << 14);
}

#[test]
fn under_tmux_lines_are_joined_and_read_from_their_frame() {
    let mut lines = Lines::new();
    lines.set_junk(true);
    lines.push(b"\x1bP=1s\x1b\\junk #CFG:ab\r\ncd\x1bP=2s\x1b\\\n");
    let line = lines.next("CFG", MAX_LINE).unwrap().unwrap();
    assert_eq!(
        (line.kind.as_str(), line.payload.as_slice()),
        ("CFG", &b"abcd"[..])
    );
}

#[test]
fn numbers_are_plain_digits() {
    assert_eq!(number(b"18446744073709551615"), Ok(u64::MAX));
    for bad in [
        &b""[..],
        b"-1",
        b"1e3",
        b" 1",
        b"184467440737095516150",
        b"18446744073709551616",
    ] {
        assert!(number(bad).is_err(), "{}", String::from_utf8_lossy(bad));
    }
}

#[test]
fn the_answer_says_protocol_1_base64_and_files_only() {
    let act = action(true);
    for part in [
        "\"protocol\":1",
        "\"binary\":false",
        "\"support_dir\":false",
        "\"tmuxcc\":false",
        "\"confirm\":true",
    ] {
        assert!(act.contains(part), "{act}");
    }
    assert!(action(false).contains("\"confirm\":false"));
    assert_eq!(frame("ACT", "x"), b"#ACT:x\n");
}

#[test]
fn the_host_s_configuration_is_read_and_held_to_what_this_side_does() {
    let go = br#"{"lang":"go","timeout":20,"bufsize":10485760,"protocol":1,
        "escape_chars":[["\u00ee","\u00ee\u00ee"],["~","\u00ee1"]],"tmux_pane_width":80}"#;
    let config = Config::read(go).unwrap();
    assert_eq!(
        config,
        Config {
            timeout: Duration::from_secs(20),
            bufsize: 10 << 20,
            junk: false
        }
    );
    assert_eq!(
        Config::read(b"{}").unwrap().timeout,
        Duration::from_secs(20)
    );
    assert_eq!(
        Config::read(br#"{"timeout":0}"#).unwrap().timeout,
        Duration::from_secs(60)
    );
    assert_eq!(
        Config::read(br#"{"timeout":1}"#).unwrap().timeout,
        Duration::from_secs(5)
    );
    assert!(Config::read(br#"{"tmux_output_junk":true}"#).unwrap().junk);
    for refused in [
        &br#"{"binary":true}"#[..],
        br#"{"directory":true}"#,
        br#"{"newline":"!\n"}"#,
        br#"{"protocol":2}"#,
        br#"{"bufsize":10485761}"#,
    ] {
        assert!(
            matches!(Config::read(refused), Err(TrzszError::Unsupported(_))),
            "{}",
            String::from_utf8_lossy(refused)
        );
    }
    for bad in [
        &b""[..],
        b"[]",
        b"{\"a\":}",
        b"{\"a\":1} x",
        b"{\"a\":\"\\ud800\"}",
        br#"{"bufsize":-1}"#,
    ] {
        assert!(
            Config::read(bad).is_err(),
            "{}",
            String::from_utf8_lossy(bad)
        );
    }
    let deep = format!("{{\"a\":{}{}}}", "[".repeat(40), "]".repeat(40));
    assert!(Config::read(deep.as_bytes()).is_err());
    let big = format!("{{\"a\":\"{}\"}}", "x".repeat(70 << 10));
    assert_eq!(
        Config::read(big.as_bytes()),
        Err(TrzszError::TooLong(64 << 10))
    );
}

#[test]
fn a_host_message_is_shown_as_plain_text() {
    let stack = format!("panic: x\n\x1b[31mgoroutine 1\u{202e}{}", "y".repeat(600));
    let shown = shown_message(&stack);
    assert!(shown.len() <= 512);
    assert!(shown.starts_with("panic: x [31mgoroutine 1y"));
}
