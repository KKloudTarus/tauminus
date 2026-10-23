// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Converts a shell's bytes to UTF-8 for the terminal view, and typed text
//! back, for hosts whose character set is not UTF-8.

use encoding_rs::{CoderResult, Decoder, EncoderResult};
use tauminus_domain::Encoding;

/// One session's converter. UTF-8 passes through untouched.
pub struct Transcoder {
    codec: Option<(&'static encoding_rs::Encoding, Decoder)>,
    /// The last chunk was decoded; the decoder takes no more.
    finished: bool,
}

impl std::fmt::Debug for Transcoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transcoder")
            .field("encoding", &self.codec.as_ref().map(|(e, _)| e.name()))
            .finish_non_exhaustive()
    }
}

impl Transcoder {
    pub fn new(encoding: Encoding) -> Self {
        let codec = match encoding {
            Encoding::Utf8 => None,
            other => {
                let codec = charset(other);
                Some((codec, codec.new_decoder_without_bom_handling()))
            }
        };
        Self {
            codec,
            finished: false,
        }
    }

    /// Output to show. A character split across chunks is held until its
    /// last byte arrives; a byte sequence the charset has no character for
    /// shows as U+FFFD, and so does a C1 control: ISO-8859-2 and the
    /// Windows sets map single bytes to U+0080..U+009F, which xterm.js acts
    /// on (U+009B is CSI), so a stray byte in a file would drive the
    /// terminal. O(n).
    pub fn decode(&mut self, bytes: &[u8], last: bool) -> Vec<u8> {
        let Some((_, decoder)) = &mut self.codec else {
            return bytes.to_vec();
        };
        if self.finished {
            return Vec::new();
        }
        self.finished = last;
        let mut out = String::with_capacity(
            decoder
                .max_utf8_buffer_length(bytes.len())
                .unwrap_or(bytes.len() * 3),
        );
        let mut input = bytes;
        loop {
            let (result, read, _) = decoder.decode_to_string(input, &mut out, last);
            input = &input[read..];
            match result {
                CoderResult::InputEmpty => break,
                // Only when `out` ran short, which its capacity rules out
                // but this keeps correct regardless.
                CoderResult::OutputFull => out.reserve(input.len() * 3 + 16),
            }
        }
        if out.chars().any(is_c1) {
            out = out
                .chars()
                .map(|c| if is_c1(c) { '\u{fffd}' } else { c })
                .collect();
        }
        out.into_bytes()
    }

    /// Typed text to send. A character the charset cannot hold, or a C1
    /// control, is sent as `_`: never as an HTML entity, which a shell
    /// would take as typed text, and never as `?`, which a shell takes as
    /// a pattern, so `rm ~/tmp/✓` cannot become `rm ~/tmp/?`. O(n).
    pub fn encode(&self, text: &str) -> Vec<u8> {
        let Some((codec, _)) = &self.codec else {
            return text.as_bytes().to_vec();
        };
        let cleaned;
        let text = if text.chars().any(is_c1) {
            cleaned = text
                .chars()
                .map(|c| if is_c1(c) { UNSENDABLE } else { c })
                .collect::<String>();
            cleaned.as_str()
        } else {
            text
        };
        let mut encoder = codec.new_encoder();
        let mut out = Vec::with_capacity(
            encoder
                .max_buffer_length_from_utf8_without_replacement(text.len())
                .unwrap_or(text.len() * 4),
        );
        let mut input = text;
        loop {
            let (result, read) =
                encoder.encode_from_utf8_to_vec_without_replacement(input, &mut out, true);
            input = &input[read..];
            match result {
                EncoderResult::InputEmpty => break,
                EncoderResult::Unmappable(_) => out.push(UNSENDABLE as u8),
                EncoderResult::OutputFull => out.reserve(input.len() * 4 + 16),
            }
        }
        out
    }
}

/// What stands for a character a host's charset cannot carry: in every
/// offered set, and inert in a shell.
const UNSENDABLE: char = '_';

fn is_c1(c: char) -> bool {
    ('\u{80}'..='\u{9f}').contains(&c)
}

fn charset(encoding: Encoding) -> &'static encoding_rs::Encoding {
    match encoding {
        Encoding::Utf8 => encoding_rs::UTF_8,
        Encoding::Windows1252 => encoding_rs::WINDOWS_1252,
        Encoding::Windows1251 => encoding_rs::WINDOWS_1251,
        Encoding::Koi8R => encoding_rs::KOI8_R,
        Encoding::Iso8859_2 => encoding_rs::ISO_8859_2,
        Encoding::ShiftJis => encoding_rs::SHIFT_JIS,
        Encoding::EucJp => encoding_rs::EUC_JP,
        Encoding::Gb18030 => encoding_rs::GB18030,
        Encoding::Big5 => encoding_rs::BIG5,
        Encoding::EucKr => encoding_rs::EUC_KR,
    }
}

#[cfg(test)]
mod tests {
    use tauminus_domain::Encoding;

    use super::Transcoder;

    #[test]
    fn utf8_passes_through_as_it_is() {
        let mut utf8 = Transcoder::new(Encoding::Utf8);
        let bytes = "héllo \u{1b}[31m".as_bytes().to_vec();
        assert_eq!(utf8.decode(&bytes, false), bytes);
        assert_eq!(utf8.encode("tiếng"), "tiếng".as_bytes());
    }

    #[test]
    fn a_character_split_across_chunks_comes_out_whole() {
        let mut sjis = Transcoder::new(Encoding::ShiftJis);
        // 日本 in Shift_JIS, cut inside the second character.
        let bytes = [0x93, 0xfa, 0x96, 0x7b];
        let mut shown = sjis.decode(&bytes[..3], false);
        shown.extend(sjis.decode(&bytes[3..], false));
        assert_eq!(String::from_utf8(shown).unwrap(), "日本");
        assert_eq!(sjis.encode("日本"), bytes);
    }

    #[test]
    fn escape_sequences_and_controls_are_untouched() {
        for encoding in Encoding::ALL {
            let mut codec = Transcoder::new(encoding);
            let ascii = b"\x1b[1;31mok\x1b[0m\r\n\x07\x03".to_vec();
            assert_eq!(codec.decode(&ascii, false), ascii, "{encoding:?}");
            assert_eq!(
                codec.encode("ls -la\r\u{3}"),
                b"ls -la\r\x03",
                "{encoding:?}"
            );
        }
    }

    #[test]
    fn what_the_charset_lacks_is_a_question_mark() {
        let latin = Transcoder::new(Encoding::Windows1252);
        assert_eq!(latin.encode("olé ✓"), b"ol\xe9 _");
        let mut cyrillic = Transcoder::new(Encoding::Koi8R);
        assert_eq!(
            String::from_utf8(cyrillic.decode(&[0xf0, 0xd2, 0xc9], false)).unwrap(),
            "При"
        );
    }

    #[test]
    fn an_unfinished_character_at_the_end_shows_as_a_replacement() {
        let mut big5 = Transcoder::new(Encoding::Big5);
        assert_eq!(big5.decode(&[0xa4], false), b"");
        assert_eq!(
            String::from_utf8(big5.decode(&[], true)).unwrap(),
            "\u{fffd}"
        );
    }

    #[test]
    fn no_c1_control_reaches_the_terminal_or_the_host() {
        for encoding in Encoding::ALL {
            if encoding == Encoding::Utf8 {
                continue;
            }
            let mut codec = Transcoder::new(encoding);
            for byte in 0x80..=0x9f_u8 {
                let shown = String::from_utf8(codec.decode(&[byte, b'2', b'J'], false)).unwrap();
                assert!(
                    !shown.chars().any(|c| ('\u{80}'..='\u{9f}').contains(&c)),
                    "{encoding:?} {byte:#x} -> {shown:?}"
                );
            }
            assert_eq!(codec.encode("a\u{9b}2J"), b"a_2J", "{encoding:?}");
        }
    }

    #[test]
    fn any_split_decodes_as_the_whole() {
        let text = "Ж日本語한국中文 Ünïcödé \u{1b}[0m ok";
        for encoding in Encoding::ALL {
            let bytes = Transcoder::new(encoding).encode(text);
            let mut whole = Transcoder::new(encoding);
            let expected = whole.decode(&bytes, true);
            // Cut at every point, and into single bytes.
            for cut in 0..bytes.len() {
                let mut split = Transcoder::new(encoding);
                let mut shown = split.decode(&bytes[..cut], false);
                shown.extend(split.decode(&bytes[cut..], true));
                assert_eq!(shown, expected, "{encoding:?} cut at {cut}");
            }
            let mut bytewise = Transcoder::new(encoding);
            let mut shown = Vec::new();
            for byte in &bytes {
                shown.extend(bytewise.decode(std::slice::from_ref(byte), false));
            }
            shown.extend(bytewise.decode(&[], true));
            assert_eq!(shown, expected, "{encoding:?} byte by byte");
        }
    }
}
