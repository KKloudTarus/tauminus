// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

/// A parser past tmux's first reply, as it is once a client starts.
fn framed() -> ControlParser {
    let mut parser = ControlParser::default();
    parser.push(b"%begin 0 0 1\n%end 0 0 1\n").unwrap();
    parser
}

fn all(bytes: &[u8]) -> Vec<ControlEvent> {
    framed().push(bytes).unwrap()
}

#[test]
fn replies_keep_their_lines_and_notifications_come_between() {
    let events = all(b"%begin 1700000000 12 1\n3.4\n%end 1700000000 12 1\n\
        %window-add @3\n\
        %begin 1700000000 13 1\nno such pane\n%error 1700000000 13 1\n");
    assert_eq!(
        events,
        vec![
            ControlEvent::Reply {
                ok: true,
                lines: vec!["3.4".into()],
                dropped: 0
            },
            ControlEvent::WindowAdd {
                window: WindowId(3)
            },
            ControlEvent::Reply {
                ok: false,
                lines: vec!["no such pane".into()],
                dropped: 0
            },
        ]
    );
}

#[test]
fn a_reply_line_that_looks_like_a_notification_stays_in_the_reply() {
    let events = all(b"%begin 1 2 1\n%output %1 not output\n%exit\n%end 1 2 1\n");
    assert_eq!(
        events,
        vec![ControlEvent::Reply {
            ok: true,
            lines: vec!["%output %1 not output".into(), "%exit".into()],
            dropped: 0
        }]
    );
    // A captured screen with guard-looking rows: only the real guard ends it.
    let events = all(b"%begin 1792803516 283 1\n%end 1 2 1\n%error 1 1 1\n\
        %output %1 INJECTED\n%end 1792803516 284 1\n%end 1792803516 283 1\n%exit\n");
    assert_eq!(
        events,
        vec![
            ControlEvent::Reply {
                ok: true,
                lines: vec![
                    "%end 1 2 1".into(),
                    "%error 1 1 1".into(),
                    "%output %1 INJECTED".into(),
                    "%end 1792803516 284 1".into()
                ],
                dropped: 0
            },
            ControlEvent::Exit { reason: None },
        ]
    );
}

#[test]
fn output_is_unescaped_as_bytes() {
    let events = all(b"%output %7 a\\033[1mb\\134c\\015\\012\xe2\x82\xac\n");
    assert_eq!(
        events,
        vec![ControlEvent::Output {
            pane: PaneId(7),
            data: b"a\x1b[1mb\\c\r\n\xe2\x82\xac".to_vec()
        }]
    );
    assert_eq!(
        all(b"%extended-output %2 1200 : x\\011y\n"),
        vec![ControlEvent::Output {
            pane: PaneId(2),
            data: b"x\ty".to_vec()
        }]
    );
    // An empty value, and a broken escape kept as it came.
    assert_eq!(unescape(b""), b"");
    assert_eq!(unescape(b"\\9\\01"), b"\\9\\01");
    assert_eq!(unescape(b"\\777"), b"\\777");
}

#[test]
fn lines_split_across_reads_and_crlf_are_read_whole() {
    let mut parser = framed();
    assert_eq!(parser.push(b"%out").unwrap(), []);
    assert_eq!(parser.push(b"put %1 wor").unwrap(), []);
    assert_eq!(
        parser.push(b"ld\r\n%pause %1\n%continue %1\r\n").unwrap(),
        vec![
            ControlEvent::Output {
                pane: PaneId(1),
                data: b"world".to_vec()
            },
            ControlEvent::Pause { pane: PaneId(1) },
            ControlEvent::Continue { pane: PaneId(1) },
        ]
    );
}

#[test]
fn windows_layouts_sessions_and_exit_are_read() {
    let events = all(b"%window-renamed @1 my editor\n\
        %window-close @1\n\
        %layout-change @2 b25f,80x24,0,0,3 b25f,80x24,0,0,3 *\n\
        %session-changed $1 work\n\
        %sessions-changed\n\
        %exit\n\
        %exit detached\n");
    assert_eq!(
        events,
        vec![
            ControlEvent::WindowRenamed {
                window: WindowId(1),
                name: "my editor".into()
            },
            ControlEvent::WindowClose {
                window: WindowId(1)
            },
            ControlEvent::LayoutChange {
                window: WindowId(2),
                layout: "b25f,80x24,0,0,3".into()
            },
            ControlEvent::SessionChanged {
                name: "work".into()
            },
            ControlEvent::Exit { reason: None },
            ControlEvent::Exit {
                reason: Some("detached".into())
            },
        ]
    );
}

#[test]
fn malformed_notifications_are_skipped() {
    assert_eq!(
        all(b"%output nopane x\n%window-add 3\n%pause\n%layout-change @1\n"),
        []
    );
}

#[test]
fn an_endless_line_or_reply_is_bounded() {
    let mut parser = ControlParser::default();
    let long = vec![b'x'; MAX_CONTROL_LINE + 1];
    assert_eq!(parser.push(&long), Err(ControlError::LineTooLong));
    let mut parser = ControlParser::default();
    let mut reply = b"%begin 1 1 1\n".to_vec();
    for _ in 0..MAX_REPLY_LINES + 5 {
        reply.extend_from_slice(b"row\n");
    }
    reply.extend_from_slice(b"%end 1 1 1\n");
    match parser.push(&reply).unwrap().as_slice() {
        [ControlEvent::Reply { lines, dropped, .. }] => {
            assert_eq!(lines.len(), MAX_REPLY_LINES);
            assert_eq!(*dropped, 5);
        }
        other => panic!("{other:?}"),
    }
    // Long lines stop at the byte cap well before the line cap.
    let mut parser = ControlParser::default();
    let row = format!("{}\n", "x".repeat(MAX_CONTROL_LINE - 1));
    let mut reply = b"%begin 1 1 1\n".to_vec();
    for _ in 0..20 {
        reply.extend_from_slice(row.as_bytes());
    }
    reply.extend_from_slice(b"%end 1 1 1\n");
    match parser.push(&reply).unwrap().as_slice() {
        [ControlEvent::Reply { lines, dropped, .. }] => {
            assert_eq!(lines.len(), 8);
            assert_eq!(*dropped, 12);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn versions_compare_by_major_and_minor() {
    assert_eq!(
        TmuxVersion::parse("3.2a"),
        Some(TmuxVersion { major: 3, minor: 2 })
    );
    assert_eq!(
        TmuxVersion::parse("next-3.6\n"),
        Some(TmuxVersion { major: 3, minor: 6 })
    );
    assert!(TmuxVersion::parse("3.1c").unwrap() < TmuxVersion::MIN);
    assert!(TmuxVersion::parse("3.10").unwrap() > TmuxVersion::MIN);
    assert!(TmuxVersion::parse("2.7").unwrap() < TmuxVersion::MIN);
    assert_eq!(TmuxVersion::parse("master"), None);
}

#[test]
fn window_and_cursor_lines_are_read() {
    assert_eq!(
        WindowInfo::parse("@4 %9 1 vim notes.md"),
        Some(WindowInfo {
            window: WindowId(4),
            pane: PaneId(9),
            panes: 1,
            name: "vim notes.md".into()
        })
    );
    assert_eq!(WindowInfo::parse("@4 %9"), None);
    assert_eq!(
        PaneCursor::parse("12 3 1"),
        Some(PaneCursor {
            x: 12,
            y: 3,
            alternate: true,
            keypad: false,
            mouse_any: false,
            mouse_button: false,
            mouse_standard: false,
            mouse_sgr: false,
        })
    );
    let vim = PaneCursor::parse("0 5 1 1 0 1 0 1").unwrap();
    assert!(vim.keypad && vim.mouse_button && vim.mouse_sgr && !vim.mouse_any);
}

#[test]
fn commands_encode_to_lines_tmux_reads() {
    assert_eq!(
        Command::Keys {
            pane: PaneId(3),
            bytes: b"ls\r".to_vec()
        }
        .encode(),
        vec!["send-keys -H -t %3 6c 73 0d\n".to_owned()]
    );
    let long = Command::Keys {
        pane: PaneId(3),
        bytes: vec![0x61; 2000],
    }
    .encode();
    assert!(long.len() > 1);
    assert!(
        long.iter()
            .all(|line| line.len() <= 1000 && line.ends_with('\n'))
    );
    let sent: usize = long.iter().map(|line| line.matches(" 61").count()).sum();
    assert_eq!(sent, 2000);
    assert_eq!(
        Command::Keys {
            pane: PaneId(1),
            bytes: vec![]
        }
        .encode(),
        Vec::<String>::new()
    );
    assert_eq!(
        Command::Size {
            cols: 120,
            rows: 40
        }
        .encode(),
        vec!["refresh-client -C 120x40\n".to_owned()]
    );
    assert_eq!(
        Command::Continue { pane: PaneId(5) }.encode(),
        vec!["refresh-client -A '%5:continue'\n".to_owned()]
    );
    assert_eq!(
        Command::Pause { pane: PaneId(5) }.encode(),
        vec!["refresh-client -A '%5:pause'\n".to_owned()]
    );
    assert_eq!(
        Command::Capture {
            pane: PaneId(5),
            history: 2000
        }
        .encode(),
        vec!["capture-pane -p -e -t %5 -S -2000\n".to_owned()]
    );
    for command in [
        Command::Version,
        Command::ListWindows,
        Command::Cursor { pane: PaneId(1) },
        Command::PauseAfter { seconds: 2 },
        Command::NewWindow,
        Command::KillWindow {
            window: WindowId(2),
        },
        Command::Detach,
    ] {
        let lines = command.encode();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].ends_with('\n') && lines[0].matches('\n').count() == 1);
    }
}

#[test]
fn a_layout_names_its_panes() {
    assert_eq!(layout_panes("b25f,80x24,0,0,3"), vec![PaneId(3)]);
    assert_eq!(
        layout_panes("bb62,159x48,0,0{79x48,0,0,1,79x48,80,0[79x24,80,0,2,79x23,80,25,4]}"),
        vec![PaneId(1), PaneId(2), PaneId(4)]
    );
    assert_eq!(layout_panes(""), []);
    assert_eq!(layout_panes("nonsense,a,b"), []);
}

#[test]
fn what_the_login_shell_printed_before_tmux_is_skipped() {
    let events = ControlParser::default()
        .push(b"Welcome to db%begin 1 1 1\n%end 1 1 1\n%window-add @1\n")
        .unwrap();
    assert_eq!(
        events,
        vec![
            ControlEvent::Reply {
                ok: true,
                lines: vec![],
                dropped: 0
            },
            ControlEvent::WindowAdd {
                window: WindowId(1)
            },
        ]
    );
    let mut fresh = ControlParser::default();
    assert_eq!(fresh.push(b"motd line\n%window-add @1\n").unwrap(), []);
}

#[test]
fn a_snapshot_is_one_line_of_two_commands() {
    assert_eq!(
        Command::Snapshot {
            pane: PaneId(2),
            history: 0
        }
        .encode(),
        vec![format!(
            "display-message -p -t %2 '{CURSOR_FORMAT}' ; capture-pane -p -e -t %2 -S -0\n"
        )]
    );
    assert_eq!(TmuxVersion::parse("openbsd-7.4"), Some(TmuxVersion::MIN));
}
