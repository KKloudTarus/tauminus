// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Text a file holds, whatever Windows tool wrote it: UTF-8 with or
//! without a BOM, UTF-16LE (with a BOM, as `regedit` writes, or without
//! one), or the ANSI code page, read as Windows-1252.

use std::borrow::Cow;

/// The text in `bytes`. O(n).
#[must_use]
pub fn decode_file(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16le(rest);
    }
    if looks_like_utf16le(bytes) {
        return utf16le(bytes);
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    let (text, _, _) = encoding_rs::WINDOWS_1252.decode(bytes);
    Cow::into_owned(text)
}

fn utf16le(bytes: &[u8]) -> String {
    let (text, _) = encoding_rs::UTF_16LE.decode_without_bom_handling(bytes);
    text.into_owned()
}

/// ASCII in UTF-16LE has a zero in every odd byte. The files read here
/// start with ASCII (`Windows Registry Editor`, `[Bookmarks]`, a header),
/// so the first four characters tell.
fn looks_like_utf16le(bytes: &[u8]) -> bool {
    bytes.len() >= 8
        && bytes[..8]
            .as_chunks::<2>()
            .0
            .iter()
            .all(|&[low, high]| high == 0 && low != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_encoding_a_windows_tool_writes_reads_back() {
        let text = "Host=prod–db é";
        let mut bom8 = vec![0xEF, 0xBB, 0xBF];
        bom8.extend_from_slice(text.as_bytes());
        assert_eq!(decode_file(&bom8), text);
        assert_eq!(decode_file(text.as_bytes()), text);
        let wide: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut bom16 = vec![0xFF, 0xFE];
        bom16.extend_from_slice(&wide);
        assert_eq!(decode_file(&bom16), text);
        assert_eq!(decode_file(&wide), text);
        // `é` in Windows-1252 is one byte, not valid UTF-8.
        assert_eq!(decode_file(b"na\xefve"), "naïve");
    }
}
