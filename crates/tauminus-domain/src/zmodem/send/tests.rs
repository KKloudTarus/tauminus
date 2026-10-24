// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use zmodem2::{Action as Wire, Event as WireEvent, Receiver};

use super::*;

/// What the host does with each file it is offered.
#[derive(Clone, Copy, PartialEq)]
enum Host {
    Take,
    /// Skips the second file, as `rz` does for a name that exists.
    SkipSecond,
    /// Cancels after the first data.
    Cancel,
}

/// Files the host got, by name, and the steps the sender reported.
struct Run {
    files: Vec<(String, Vec<u8>)>,
    sent: Vec<Sent>,
    send: Transmit,
    /// Every byte sent to the host.
    wire: Vec<u8>,
}

/// A host running `rz` against a [`Transmit`] of `files`, in reads of
/// `chunk` bytes each way.
fn run(files: &[(&str, Vec<u8>)], host: Host, chunk: usize) -> Run {
    let mut rz = Receiver::new().unwrap();
    rz.set_manual_file_accept(true);
    let mut send = Transmit::new();
    let mut out = TransmitOutput::default();
    let mut index = 0;
    send.start(files[0].0, files[0].1.len() as u64).unwrap();
    let mut got: Vec<(String, Vec<u8>)> = Vec::new();
    let mut steps_seen = Vec::new();
    let mut to_rz: Vec<u8> = Vec::new();
    let mut wire: Vec<u8> = Vec::new();
    let mut to_send: Vec<u8> = Vec::new();
    let mut cancelled = false;
    for _ in 0..1_000_000 {
        // The sender's side.
        match send.poll(&mut out) {
            Need::Read { offset, max_len } => {
                let data = &files[index].1;
                let from = usize::try_from(offset).unwrap();
                let to = (from + max_len).min(data.len());
                send.provide(&data[from..to], &mut out);
                continue;
            }
            Need::Ended => {
                steps_seen.append(&mut out.sent);
                break;
            }
            Need::Wait => {}
        }
        let steps = std::mem::take(&mut out.sent);
        for step in steps {
            if step == Sent::FileDone {
                index += 1;
                if let Some((name, data)) = files.get(index) {
                    send.start(name, data.len() as u64).unwrap();
                } else {
                    send.finish(&mut out);
                }
            }
            steps_seen.push(step);
        }
        wire.extend_from_slice(&out.wire);
        to_rz.append(&mut out.wire);
        // The host's side, `chunk` bytes at a time.
        let take = to_rz.len().min(chunk);
        let mut at = 0;
        loop {
            let mut acted = false;
            loop {
                match rz.poll() {
                    Wire::WriteWire(bytes) => {
                        let n = bytes.len();
                        to_send.extend_from_slice(bytes);
                        rz.wire_written(n);
                    }
                    Wire::Event(WireEvent::FileStarted(info)) => {
                        let name = String::from_utf8_lossy(info.name).into_owned();
                        if host == Host::SkipSecond && got.len() == 1 && name == "b" {
                            rz.skip_file().unwrap();
                        } else {
                            got.push((name, Vec::new()));
                            rz.accept_file_at(0).unwrap();
                        }
                    }
                    Wire::WriteFile(bytes) => {
                        let n = bytes.len();
                        got.last_mut().unwrap().1.extend_from_slice(bytes);
                        rz.file_written(n).unwrap();
                        if host == Host::Cancel && !cancelled {
                            cancelled = true;
                            rz.abort().unwrap();
                        }
                    }
                    Wire::Idle => break,
                    _ => {}
                }
                acted = true;
            }
            if at >= take {
                break;
            }
            match rz.submit_wire(&to_rz[at..take]) {
                Ok(n) if n > 0 => at += n,
                _ if acted => {}
                _ => break,
            }
        }
        to_rz.drain(..at);
        let give = to_send.len().min(chunk);
        send.feed(&to_send[..give], &mut out);
        to_send.drain(..give);
        if take == 0 && give == 0 && to_rz.is_empty() {
            send.timeout(&mut out);
        }
    }
    Run {
        files: got,
        sent: steps_seen,
        send,
        wire,
    }
}

fn bytes(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 131 % 251).to_le_bytes()[0]).collect()
}

#[test]
fn every_size_arrives_whole() {
    for len in [0, 1, 1023, 1024, 1025, 3 << 20] {
        let file = bytes(len);
        let run = run(&[("a.bin", file.clone())], Host::Take, 32 << 10);
        assert_eq!(run.files, vec![("a.bin".to_owned(), file)], "{len}");
        assert_eq!(run.sent.last(), Some(&Sent::Done), "{len}");
        assert!(run.send.completed());
    }
}

#[test]
fn several_files_go_in_order_through_small_reads() {
    let files = [
        ("one", bytes(5000)),
        ("two", bytes(70_000)),
        ("three", bytes(7)),
    ];
    let run = run(&files, Host::Take, 7);
    let want: Vec<(String, Vec<u8>)> = files
        .iter()
        .map(|(n, d)| ((*n).to_owned(), d.clone()))
        .collect();
    assert_eq!(run.files, want);
    assert_eq!(run.sent.iter().filter(|s| **s == Sent::FileDone).count(), 3);
}

#[test]
fn a_skipped_file_is_reported_done_and_the_next_goes() {
    let files = [("a", bytes(10)), ("b", bytes(20_000)), ("c", bytes(30))];
    let run = run(&files, Host::SkipSecond, 4096);
    let names: Vec<&str> = run.files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["a", "c"]);
    assert_eq!(run.files[1].1, bytes(30));
    assert_eq!(run.sent.last(), Some(&Sent::Done));
}

#[test]
fn the_host_s_cancel_ends_the_send() {
    let run = run(&[("a", bytes(500_000))], Host::Cancel, 8192);
    assert!(run.send.ended());
    assert!(!run.send.completed());
    assert_eq!(
        run.sent.last(),
        Some(&Sent::Aborted {
            reason: "the host stopped the transfer"
        })
    );
}

#[test]
fn no_line_of_a_text_file_reaches_the_host_raw() {
    // Lines that would run, were a shell to read them.
    let lines = b"touch /tmp/pwned\r\n".repeat(20_000);
    let run = run(&[("lines.txt", lines.clone())], Host::Take, 32 << 10);
    assert_eq!(run.files, vec![("lines.txt".to_owned(), lines)]);
    // Line ends only end hex headers (`**` ZDLE `B` ... CR LF): a few,
    // whatever the file holds.
    let raw = run
        .wire
        .iter()
        .filter(|&&b| b == b'\n' || b == 0x8a)
        .count();
    let headers = run.wire.windows(4).filter(|w| *w == b"**\x18B").count();
    assert_eq!(raw, headers);
    assert!(!run.wire.contains(&0x04), "no raw Ctrl+D");
}

#[test]
fn what_cannot_be_sent_is_refused_before_anything_goes() {
    let mut send = Transmit::new();
    assert_eq!(send.start("big", 1 << 32), Err(TransmitError::TooLarge));
    assert_eq!(
        send.start(&"n".repeat(256), 1),
        Err(TransmitError::NameTooLong)
    );
    assert_eq!(send.start("", 1), Err(TransmitError::NoName));
    send.start("ok", (1 << 32) - 1).unwrap();
}

#[test]
fn an_abort_tells_the_host_to_stop() {
    let mut send = Transmit::new();
    let mut out = TransmitOutput::default();
    send.start("a", 10).unwrap();
    assert_eq!(send.poll(&mut out), Need::Wait);
    // It begins with ZRQINIT, never file data.
    assert!(out.wire.starts_with(b"**\x18B00"), "{:?}", out.wire);
    out.wire.clear();
    send.abort(&mut out, "cancelled");
    assert!(out.wire.windows(5).any(|w| w == [0x18; 5]));
    // Then Ctrl+E Ctrl+U: a shell that reads it drops the line.
    assert!(out.wire.ends_with(&[0x05, 0x15]));
    assert_eq!(
        out.sent.last(),
        Some(&Sent::Aborted {
            reason: "cancelled"
        })
    );
    assert_eq!(send.poll(&mut out), Need::Ended);
}

#[test]
fn a_host_that_says_nothing_ends_the_send() {
    let mut send = Transmit::new();
    let mut out = TransmitOutput::default();
    send.start("a", 10).unwrap();
    for _ in 0..20 {
        send.poll(&mut out);
        send.timeout(&mut out);
    }
    assert!(send.ended());
}

#[test]
fn a_host_whose_rz_is_gone_is_given_up_at_once() {
    let mut send = Transmit::new();
    let mut out = TransmitOutput::default();
    send.start("a", 10).unwrap();
    send.poll(&mut out);
    // `rz` gave up before the user chose: the shell answers instead, and
    // the first timeout ends it, with nothing but ZRQINIT sent.
    send.feed(
        b"bash: **B00000000000000: command not found\r\n$ ",
        &mut out,
    );
    assert_eq!(send.poll(&mut out), Need::Wait);
    send.timeout(&mut out);
    assert!(send.ended());
    assert!(matches!(out.sent.last(), Some(Sent::Aborted { .. })));
}

#[test]
fn a_host_that_asks_again_without_end_is_stopped() {
    let mut send = Transmit::new();
    let mut out = TransmitOutput::default();
    send.start("a", 100_000).unwrap();
    send.poll(&mut out);
    // `rz`: ZRINIT, then ZRPOS 0 after every window.
    let zrinit = b"**\x18B0100000023be50\r\x8a\x11";
    send.feed(zrinit, &mut out);
    let mut asked = 0;
    for _ in 0..10_000 {
        match send.poll(&mut out) {
            Need::Read { max_len, .. } => {
                asked += 1;
                send.provide(&vec![0; max_len], &mut out);
            }
            Need::Ended => break,
            Need::Wait => {
                // ZRPOS 0, as hex header with its CRC.
                send.feed(b"**\x18B0900000000a87c\r\x8a\x11", &mut out);
            }
        }
    }
    assert!(send.ended(), "asked {asked} times");
    assert_eq!(
        out.sent.last(),
        Some(&Sent::Aborted {
            reason: "the host asked for the same bytes again and again"
        })
    );
}

#[test]
#[ignore = "prints the wire's size against the file's"]
#[expect(clippy::print_stderr, reason = "a measure to read")]
fn overhead() {
    let text = b"2026-10-24 04:12:24 INFO request served in 3 ms\n".repeat(20_000);
    let random: Vec<u8> = (0..1_000_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13).to_le_bytes()[0])
        .collect();
    for (what, file) in [("text", text), ("random", random)] {
        let run = run(&[("f", file.clone())], Host::Take, 32 << 10);
        #[expect(clippy::cast_precision_loss, reason = "a ratio to print")]
        let ratio = run.wire.len() as f64 / file.len() as f64;
        eprintln!("{what}: {ratio:.3}");
    }
}
