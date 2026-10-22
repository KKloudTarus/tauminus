// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! `cargo xtask flood`: the output the benchmark feeds a terminal, the same
//! on every platform, since Windows has no `yes` or `cat` of a big file.
//!
//! - `flood text <MiB>` writes MiB mebibytes of 120-column lines of base64
//!   letters, then `flood-done <seconds>`. Like `cat` of a file of that
//!   size, without the disk.
//! - `flood yes <seconds>` writes `y` lines for that long, then
//!   `flood-done`.
//! - `flood report <hex>` decodes hex and writes it to the file named by
//!   `TAUMINUS_BENCH_OUT`. The benchmark page types this to hand its
//!   results out; hex needs no quoting in any shell.
//!
//! Writes go through one 64 KiB buffer, so the program is never the
//! bottleneck: the terminal's flow control is.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

const LINE: usize = 120;
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(crate) fn run(args: &[String]) -> Result<()> {
    match args {
        [mode, size] if mode == "text" => text(size.parse().context("MiB must be a number")?),
        [mode, seconds] if mode == "yes" => yes(Duration::from_secs(
            seconds.parse().context("seconds must be a number")?,
        )),
        [mode, hex] if mode == "report" => report(hex),
        _ => bail!("usage: cargo xtask flood text <MiB> | yes <seconds> | report <hex>"),
    }
}

/// One line of `LINE` letters and a newline; line `n` starts at letter `n`,
/// so no two neighbouring lines are the same.
fn line(n: usize, out: &mut [u8; LINE + 1]) {
    for (i, byte) in out[..LINE].iter_mut().enumerate() {
        *byte = ALPHABET[(n + i) % ALPHABET.len()];
    }
    out[LINE] = b'\n';
}

fn text(mebibytes: u64) -> Result<()> {
    let total = mebibytes * 1024 * 1024;
    let started = Instant::now();
    let stdout = io::stdout();
    let mut out = io::BufWriter::with_capacity(64 * 1024, stdout.lock());
    let mut buffer = [0; LINE + 1];
    let mut written = 0_u64;
    let mut n = 0;
    while written < total {
        line(n, &mut buffer);
        out.write_all(&buffer)?;
        written += buffer.len() as u64;
        n += 1;
    }
    writeln!(out, "flood-done {:.3}", started.elapsed().as_secs_f64())?;
    out.flush()?;
    Ok(())
}

fn yes(duration: Duration) -> Result<()> {
    let started = Instant::now();
    let stdout = io::stdout();
    let mut out = io::BufWriter::with_capacity(64 * 1024, stdout.lock());
    let chunk = b"y\n".repeat(4096);
    while started.elapsed() < duration {
        out.write_all(&chunk)?;
    }
    writeln!(out, "flood-done")?;
    out.flush()?;
    Ok(())
}

fn report(hex: &str) -> Result<()> {
    let path = std::env::var_os("TAUMINUS_BENCH_OUT").context("TAUMINUS_BENCH_OUT is not set")?;
    std::fs::write(&path, decode_hex(hex)?).context("cannot write the report")?;
    Ok(())
}

fn decode_hex(hex: &str) -> Result<Vec<u8>> {
    let digits = hex.trim().as_bytes();
    if !digits.len().is_multiple_of(2) {
        bail!("hex has an odd length");
    }
    let (pairs, _) = digits.as_chunks::<2>();
    pairs
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair)?;
            Ok(u8::from_str_radix(text, 16)?)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_fixed_width_and_shift_by_one_letter() {
        let mut first = [0; LINE + 1];
        let mut second = [0; LINE + 1];
        line(0, &mut first);
        line(1, &mut second);
        assert_eq!(first[LINE], b'\n');
        assert_eq!(&first[1..LINE], &second[..LINE - 1]);
        assert!(first[..LINE].iter().all(|b| ALPHABET.contains(b)));
    }

    #[test]
    fn hex_round_trips_and_rejects_bad_input() {
        assert_eq!(decode_hex("7b7d0a").unwrap(), b"{}\n");
        assert!(decode_hex("abc").is_err());
        assert!(decode_hex("zz").is_err());
    }
}
