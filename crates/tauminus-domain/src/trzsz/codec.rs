// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Payloads as trzsz writes them: base64, padded and standard, of a zlib
//! stream.

use std::io::{Read as _, Write as _};

use data_encoding::BASE64;
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use super::TrzszError;

/// `bytes` as base64 of zlib. O(n).
#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    let mut zlib = ZlibEncoder::new(
        Vec::with_capacity(bytes.len() / 2 + 64),
        Compression::default(),
    );
    // Writing to a Vec does not fail.
    let _ = zlib.write_all(bytes);
    let compressed = zlib.finish().unwrap_or_default();
    BASE64.encode(&compressed)
}

/// The bytes `payload` holds, at most `max` of them: a stream that would
/// inflate past it stops there, so a small line cannot fill memory. O(n +
/// max).
///
/// # Errors
/// Not base64, not zlib, or more than `max` bytes.
pub fn decode(payload: &[u8], max: usize) -> Result<Vec<u8>, TrzszError> {
    let compressed = BASE64
        .decode(payload)
        .map_err(|_| TrzszError::Protocol("base64".into()))?;
    let mut out = Vec::new();
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    ZlibDecoder::new(compressed.as_slice())
        .take(limit)
        .read_to_end(&mut out)
        .map_err(|_| TrzszError::Protocol("zlib".into()))?;
    if out.len() > max {
        return Err(TrzszError::TooLong(max));
    }
    Ok(out)
}
