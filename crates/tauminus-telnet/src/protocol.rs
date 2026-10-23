// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The Telnet protocol without the network: bytes in, data and replies
//! out. RFC 854 framing, option negotiation as RFC 1143's Q method keeps
//! it from looping, and the options a terminal needs: ECHO (857), SGA
//! (858), BINARY (856), TTYPE (1091), and NAWS (1073). Every other option
//! is refused.

use tauminus_domain::TerminalSize;

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;

const BINARY: u8 = 0;
const ECHO: u8 = 1;
const SGA: u8 = 3;
const TTYPE: u8 = 24;
const NAWS: u8 = 31;

const TTYPE_IS: u8 = 0;
const TTYPE_SEND: u8 = 1;

/// Longest subnegotiation kept; past it the rest is dropped, so a server
/// cannot grow memory without end.
const MAX_SUBNEGOTIATION: usize = 1024;

/// What the server's bytes became.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Output {
    /// For the screen.
    pub data: Vec<u8>,
    /// For the server: answers to its negotiation.
    pub reply: Vec<u8>,
}

/// One side of an option, as RFC 1143 tracks it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Side {
    #[default]
    No,
    Yes,
    /// Asked to turn it on, waiting for the answer.
    WantYes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Data,
    /// After a CR in data, outside BINARY: the NUL or LF that follows is
    /// part of the line end.
    Cr,
    Iac,
    /// After IAC and a verb, waiting for the option.
    Option(u8),
    Sub,
    SubIac,
}

/// A connection's protocol state. O(1) per byte, at most
/// [`MAX_SUBNEGOTIATION`] bytes held.
#[derive(Debug)]
pub struct Telnet {
    state: State,
    /// Whether the server does each option (index by option code).
    them: [Side; 256],
    /// Whether this side does each option.
    us: [Side; 256],
    sub: Vec<u8>,
    terminal: String,
    size: TerminalSize,
}

impl Telnet {
    /// `terminal` answers TTYPE; `size` is sent once NAWS is on.
    pub fn new(terminal: &str, size: TerminalSize) -> Self {
        Self {
            state: State::Data,
            them: [Side::No; 256],
            us: [Side::No; 256],
            sub: Vec::new(),
            terminal: terminal.to_owned(),
            size,
        }
    }

    /// What to send first: offers this side makes before the server asks.
    pub fn start(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        for option in [NAWS, TTYPE] {
            self.us[usize::from(option)] = Side::WantYes;
            out.extend([IAC, WILL, option]);
        }
        // The server echoes, and nobody needs go-aheads.
        for option in [ECHO, SGA] {
            self.them[usize::from(option)] = Side::WantYes;
            out.extend([IAC, DO, option]);
        }
        out
    }

    /// Whether the server echoes what is typed; a terminal shows nothing
    /// itself either way, so this is for tests and status.
    pub fn server_echoes(&self) -> bool {
        self.them[usize::from(ECHO)] == Side::Yes
    }

    /// Splits the server's bytes into screen data and negotiation replies.
    pub fn receive(&mut self, bytes: &[u8]) -> Output {
        let mut out = Output::default();
        for &byte in bytes {
            self.step(byte, &mut out);
        }
        out
    }

    fn step(&mut self, byte: u8, out: &mut Output) {
        match self.state {
            State::Data => match byte {
                IAC => self.state = State::Iac,
                b'\r' if !self.binary_from_them() => {
                    out.data.push(b'\r');
                    self.state = State::Cr;
                }
                _ => out.data.push(byte),
            },
            State::Cr => {
                self.state = State::Data;
                match byte {
                    // CR NUL is a bare carriage return.
                    0 => {}
                    IAC => self.state = State::Iac,
                    b'\r' => {
                        out.data.push(b'\r');
                        self.state = State::Cr;
                    }
                    _ => out.data.push(byte),
                }
            }
            State::Iac => match byte {
                IAC => {
                    out.data.push(IAC);
                    self.state = State::Data;
                }
                WILL | WONT | DO | DONT => self.state = State::Option(byte),
                SB => {
                    self.sub.clear();
                    self.state = State::Sub;
                }
                // NOP, GA, and the rest carry nothing a terminal shows.
                _ => self.state = State::Data,
            },
            State::Option(verb) => {
                self.state = State::Data;
                self.negotiate(verb, byte, &mut out.reply);
            }
            State::Sub => match byte {
                IAC => self.state = State::SubIac,
                _ if self.sub.len() < MAX_SUBNEGOTIATION => self.sub.push(byte),
                _ => {}
            },
            State::SubIac => match byte {
                SE => {
                    self.state = State::Data;
                    let sub = std::mem::take(&mut self.sub);
                    self.subnegotiation(&sub, &mut out.reply);
                }
                IAC => {
                    if self.sub.len() < MAX_SUBNEGOTIATION {
                        self.sub.push(IAC);
                    }
                    self.state = State::Sub;
                }
                // Anything else ends it badly; drop what was gathered.
                _ => {
                    self.sub.clear();
                    self.state = State::Data;
                }
            },
        }
    }

    fn negotiate(&mut self, verb: u8, option: u8, reply: &mut Vec<u8>) {
        let index = usize::from(option);
        match verb {
            WILL => {
                let wanted = matches!(option, ECHO | SGA | BINARY);
                match (self.them[index], wanted) {
                    (Side::Yes, _) => {}
                    (Side::WantYes, _) => self.them[index] = Side::Yes,
                    (Side::No, true) => {
                        self.them[index] = Side::Yes;
                        reply.extend([IAC, DO, option]);
                    }
                    (Side::No, false) => reply.extend([IAC, DONT, option]),
                }
            }
            WONT => {
                if self.them[index] == Side::Yes {
                    reply.extend([IAC, DONT, option]);
                }
                self.them[index] = Side::No;
            }
            DO => {
                let offered = matches!(option, NAWS | TTYPE | SGA | BINARY);
                let was = self.us[index];
                match (self.us[index], offered) {
                    (Side::Yes, _) => {}
                    (Side::WantYes, _) => self.us[index] = Side::Yes,
                    (Side::No, true) => {
                        self.us[index] = Side::Yes;
                        reply.extend([IAC, WILL, option]);
                    }
                    (Side::No, false) => reply.extend([IAC, WONT, option]),
                }
                // Only on the change: a request for what is already on gets
                // no answer (RFC 1143).
                if option == NAWS && was != Side::Yes && self.us[index] == Side::Yes {
                    self.push_size(reply);
                }
            }
            DONT => {
                if self.us[index] == Side::Yes {
                    reply.extend([IAC, WONT, option]);
                }
                self.us[index] = Side::No;
            }
            _ => {}
        }
    }

    fn subnegotiation(&mut self, sub: &[u8], reply: &mut Vec<u8>) {
        if sub == [TTYPE, TTYPE_SEND] && self.us[usize::from(TTYPE)] == Side::Yes {
            reply.extend([IAC, SB, TTYPE, TTYPE_IS]);
            reply.extend(self.terminal.bytes().filter(|&b| b != IAC));
            reply.extend([IAC, SE]);
        }
    }

    /// Records the window size; returns what to send when NAWS is on.
    pub fn resize(&mut self, size: TerminalSize) -> Vec<u8> {
        self.size = size;
        let mut out = Vec::new();
        if self.us[usize::from(NAWS)] == Side::Yes {
            self.push_size(&mut out);
        }
        out
    }

    fn push_size(&self, out: &mut Vec<u8>) {
        out.extend([IAC, SB, NAWS]);
        for value in [self.size.cols(), self.size.rows()] {
            for byte in value.to_be_bytes() {
                // A 255 in the size is doubled, as any data byte is.
                out.push(byte);
                if byte == IAC {
                    out.push(IAC);
                }
            }
        }
        out.extend([IAC, SE]);
    }

    /// What typed `bytes` become on the wire: IAC doubled, and outside
    /// BINARY a lone CR sent as CR NUL. O(n).
    pub fn send(&self, bytes: &[u8]) -> Vec<u8> {
        let binary = self.us[usize::from(BINARY)] == Side::Yes;
        let mut out = Vec::with_capacity(bytes.len() + 8);
        let mut iter = bytes.iter().peekable();
        while let Some(&byte) = iter.next() {
            match byte {
                IAC => out.extend([IAC, IAC]),
                b'\r' if !binary && iter.peek() != Some(&&b'\n') => out.extend([b'\r', 0]),
                _ => out.push(byte),
            }
        }
        out
    }

    fn binary_from_them(&self) -> bool {
        self.them[usize::from(BINARY)] == Side::Yes
    }
}

#[cfg(test)]
mod tests {
    use tauminus_domain::TerminalSize;

    use super::*;

    fn telnet() -> Telnet {
        Telnet::new("xterm-256color", TerminalSize::new(80, 24).unwrap())
    }

    #[test]
    fn it_offers_naws_and_ttype_and_asks_the_server_to_echo() {
        let mut t = telnet();
        assert_eq!(
            t.start(),
            [
                IAC, WILL, NAWS, IAC, WILL, TTYPE, IAC, DO, ECHO, IAC, DO, SGA
            ]
        );
        // The server agrees: no answer to answers.
        let out = t.receive(&[IAC, WILL, ECHO, IAC, WILL, SGA, IAC, DO, TTYPE]);
        assert_eq!(out.reply, []);
        assert!(t.server_echoes());
    }

    #[test]
    fn naws_sends_the_size_on_agreement_and_on_every_resize() {
        let mut t = telnet();
        t.start();
        let out = t.receive(&[IAC, DO, NAWS]);
        assert_eq!(out.reply, [IAC, SB, NAWS, 0, 80, 0, 24, IAC, SE]);
        let resized = t.resize(TerminalSize::new(255, 300).unwrap());
        assert_eq!(resized, [IAC, SB, NAWS, 0, 255, 255, 1, 44, IAC, SE]);
    }

    #[test]
    fn a_server_that_never_agrees_to_naws_gets_no_size() {
        let mut t = telnet();
        t.start();
        t.receive(&[IAC, DONT, NAWS]);
        assert_eq!(t.resize(TerminalSize::new(100, 30).unwrap()), []);
    }

    #[test]
    fn ttype_answers_with_the_terminal_name() {
        let mut t = telnet();
        t.start();
        t.receive(&[IAC, DO, TTYPE]);
        let out = t.receive(&[IAC, SB, TTYPE, TTYPE_SEND, IAC, SE]);
        let mut expected = vec![IAC, SB, TTYPE, TTYPE_IS];
        expected.extend(b"xterm-256color");
        expected.extend([IAC, SE]);
        assert_eq!(out.reply, expected);
    }

    #[test]
    fn options_it_does_not_know_are_refused_once() {
        let mut t = telnet();
        let out = t.receive(&[IAC, DO, 39, IAC, WILL, 34]);
        assert_eq!(out.reply, [IAC, WONT, 39, IAC, DONT, 34]);
        // Saying it again changes nothing and gets no second answer loop.
        let again = t.receive(&[IAC, WONT, 34, IAC, DONT, 39]);
        assert_eq!(again.reply, []);
    }

    #[test]
    fn data_comes_out_unescaped_with_line_ends_as_sent() {
        let mut t = telnet();
        let out = t.receive(&[b'a', IAC, IAC, b'b', b'\r', 0, b'c', b'\r', b'\n', IAC, 241]);
        assert_eq!(out.data, [b'a', IAC, b'b', b'\r', b'c', b'\r', b'\n']);
        // Split anywhere, the same.
        let mut split = telnet();
        let bytes = [
            b'x', IAC, IAC, b'\r', 0, IAC, SB, TTYPE, TTYPE_SEND, IAC, SE, b'y',
        ];
        let mut data = Vec::new();
        for byte in bytes {
            data.extend(split.receive(&[byte]).data);
        }
        assert_eq!(data, [b'x', IAC, b'\r', b'y']);
    }

    #[test]
    fn typed_bytes_double_iac_and_send_a_lone_cr_as_cr_nul() {
        let t = telnet();
        assert_eq!(t.send(b"ls\r"), b"ls\r\0");
        assert_eq!(t.send(b"a\r\nb"), b"a\r\nb");
        assert_eq!(t.send(&[1, IAC, 2]), [1, IAC, IAC, 2]);
    }

    #[test]
    fn a_repeated_do_naws_gets_no_second_size_and_cr_cr_nul_is_two_returns() {
        let mut t = telnet();
        t.start();
        assert_ne!(t.receive(&[IAC, DO, NAWS]).reply, []);
        assert_eq!(t.receive(&[IAC, DO, NAWS, IAC, DO, NAWS]).reply, []);
        assert_eq!(t.receive(b"\r\r\0b").data, b"\r\rb");
    }

    #[test]
    fn a_long_subnegotiation_is_cut_and_parsing_goes_on() {
        let mut t = telnet();
        let mut bytes = vec![IAC, SB, 99];
        bytes.extend(std::iter::repeat_n(7, 10_000));
        bytes.extend([IAC, SE, b'k']);
        let out = t.receive(&bytes);
        assert_eq!(out.data, b"k");
        assert!(t.sub.capacity() <= 4 * MAX_SUBNEGOTIATION);
    }
}
