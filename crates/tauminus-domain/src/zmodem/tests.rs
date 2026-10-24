// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use zmodem2::{FileInfo, Position, Sender};

use super::*;

/// What a host running `sz` sends for `files`, against a [`Receive`]: the
/// steps received, and the host's bytes after the receive ended.
fn send(files: &[(&[u8], &[u8])]) -> (Vec<Received>, Vec<u8>) {
    let mut host = Sender::new().unwrap();
    let mut receive = Receive::new(LocalOs::Unix);
    let mut out = Output::default();
    let mut index = 0;
    host.start_file(FileInfo::new(
        files[0].0,
        Some(Position::new(u32::try_from(files[0].1.len()).unwrap())),
    ))
    .unwrap();
    let mut wire: Vec<u8> = Vec::new();
    let mut to_host: Vec<u8> = Vec::new();
    let mut rest = Vec::new();
    let mut finished = false;
    for _ in 0..100_000 {
        match host.poll() {
            Action::WriteWire(bytes) => {
                wire.extend_from_slice(bytes);
                let n = bytes.len();
                host.wire_written(n);
            }
            Action::ReadFile { offset, max_len } => {
                let data = files[index].1;
                let from = offset.get() as usize;
                let to = (from + max_len).min(data.len());
                host.submit_file(&data[from..to]).unwrap();
            }
            Action::Event(Event::FileCompleted) => {
                index += 1;
                if let Some((name, data)) = files.get(index) {
                    host.start_file(FileInfo::new(
                        name,
                        Some(Position::new(u32::try_from(data.len()).unwrap())),
                    ))
                    .unwrap();
                } else {
                    host.finish().unwrap();
                }
            }
            Action::Event(Event::SessionCompleted) => finished = true,
            _ => {
                let used = receive.feed(&wire, &mut out);
                if receive.ended() {
                    rest.extend_from_slice(&wire[used..]);
                }
                wire.clear();
                if finished {
                    break;
                }
                // What the host does not take yet waits for its next turn.
                to_host.extend(std::mem::take(&mut out.wire));
                let taken = host.submit_wire(&to_host).unwrap();
                to_host.drain(..taken);
            }
        }
    }
    assert!(finished, "the host finished");
    (out.received, rest)
}

fn data(received: &[Received]) -> Vec<u8> {
    received
        .iter()
        .flat_map(|r| match r {
            Received::Data(bytes) => bytes.clone(),
            _ => Vec::new(),
        })
        .collect()
}

#[test]
fn receives_every_file_with_its_name_and_size() {
    let big: Vec<u8> = (0..200_000u32).map(|i| (i * 31 % 251) as u8).collect();
    let (received, _) = send(&[(b"a.txt", b"hello\n"), (b"big.bin", &big)]);
    assert_eq!(
        received[0],
        Received::File {
            name: "a.txt".into(),
            size: Some(6)
        }
    );
    assert!(received.contains(&Received::File {
        name: "big.bin".into(),
        size: Some(200_000)
    }));
    let mut expected = b"hello\n".to_vec();
    expected.extend_from_slice(&big);
    assert_eq!(data(&received), expected);
    assert_eq!(
        received
            .iter()
            .filter(|r| **r == Received::FileDone)
            .count(),
        2
    );
    assert_eq!(received.last(), Some(&Received::Done));
}

#[test]
fn a_name_is_saved_as_its_last_part_or_skipped() {
    let (received, _) = send(&[
        (b"../../etc/passwd", b"x"),
        (b"/abs/.bashrc", b"y"),
        (b"evil\xe2\x80\xaetxt.exe", b"z"),
        ("invoice.pdf\u{3164}\u{3164}.exe".as_bytes(), b"v"),
        (b"ok", b"w"),
    ]);
    let names: Vec<&Received> = received
        .iter()
        .filter(|r| matches!(r, Received::File { .. } | Received::Skipped { .. }))
        .collect();
    assert_eq!(
        names,
        vec![
            &Received::File {
                name: "passwd".into(),
                size: Some(1)
            },
            &Received::File {
                name: "_bashrc".into(),
                size: Some(1)
            },
            // Shown without what reorders it.
            &Received::Skipped {
                name: "eviltxt.exe".into()
            },
            &Received::Skipped {
                name: "invoice.pdf.exe".into()
            },
            &Received::File {
                name: "ok".into(),
                size: Some(1)
            },
        ]
    );
    // The skipped file's data never comes.
    assert_eq!(data(&received), b"xyw");
}

#[test]
fn what_follows_the_transfer_is_the_terminal_s() {
    let (received, rest) = send(&[(b"a", b"1")]);
    assert_eq!(received.last(), Some(&Received::Done));
    // `sz`'s `OO`, then the shell's prompt.
    assert!(rest.ends_with(b"OO"), "{rest:?}");
    let mut receive = Receive::new(LocalOs::Unix);
    receive.abort(&mut Output::default(), "test");
    assert_eq!(receive.after(&rest), b"");
    assert_eq!(receive.after(b"\r\n$ "), b"$ ");
    // Once the shell's own output came, it passes as it is.
    assert_eq!(receive.after(b"\r\nOK"), b"\r\nOK");
    // Only `sz`'s two: a third `O` is the shell's.
    let mut receive = Receive::new(LocalOs::Unix);
    receive.abort(&mut Output::default(), "test");
    assert_eq!(receive.after(b"\r\nOOOK"), b"OK");
}

#[test]
fn an_offer_is_found_whole_and_only_whole() {
    let offer = b"rz\r**\x18B00000000000000\r\x8a\x11";
    assert_eq!(find_offer(offer), Some(3));
    assert_eq!(find_offer(b"plain text"), None);
    // Cut before its flags and CRC: not yet.
    assert_eq!(find_offer(b"rz\r**\x18B0000"), None);
    // Not hex: text that only looks like it.
    assert_eq!(find_offer(b"**\x18B00zzzzzzzzzzzz\r"), None);
    // A second, whole one after a broken one.
    assert_eq!(find_offer(b"**\x18B00zz**\x18B00000000000000\r"), Some(8));
    assert_eq!(OFFER_KEEP, 17);
}

#[test]
fn an_abort_tells_the_host_to_stop() {
    let mut receive = Receive::new(LocalOs::Unix);
    let mut out = Output::default();
    receive.feed(b"**\x18B00000000000000\r\x8a\x11", &mut out);
    out.wire.clear();
    receive.abort(&mut out, "cancelled");
    assert!(receive.ended());
    assert!(
        out.wire.windows(5).any(|w| w == [0x18; 5]),
        "{:?}",
        out.wire
    );
    assert_eq!(
        out.received.last(),
        Some(&Received::Aborted {
            reason: "cancelled"
        })
    );
}

#[test]
fn a_host_that_stops_answering_ends_the_receive() {
    let mut receive = Receive::new(LocalOs::Unix);
    let mut out = Output::default();
    receive.feed(b"**\x18B00000000000000\r\x8a\x11", &mut out);
    for _ in 0..100 {
        if receive.ended() {
            break;
        }
        receive.timeout(&mut out);
    }
    assert!(receive.ended());
}
