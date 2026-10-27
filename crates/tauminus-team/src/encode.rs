// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The bytes that are signed and hashed. Every field has a fixed width or
//! a length before it, so two different values never encode alike.

use sha2::{Digest as _, Sha256};

use crate::Hash;

#[derive(Debug)]
pub(crate) struct Encoder(Vec<u8>);

impl Encoder {
    /// Starts with `context`, so bytes signed for one purpose never pass
    /// for another.
    pub(crate) fn new(context: &[u8]) -> Self {
        let mut encoder = Self(Vec::with_capacity(256));
        encoder.bytes(context);
        encoder
    }

    pub(crate) fn u8(&mut self, value: u8) -> &mut Self {
        self.0.push(value);
        self
    }

    pub(crate) fn u32(&mut self, value: u32) -> &mut Self {
        self.0.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub(crate) fn u64(&mut self, value: u64) -> &mut Self {
        self.0.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub(crate) fn fixed(&mut self, value: &[u8]) -> &mut Self {
        self.0.extend_from_slice(value);
        self
    }

    /// A field of any length, its length first.
    pub(crate) fn bytes(&mut self, value: &[u8]) -> &mut Self {
        self.u64(value.len() as u64).fixed(value)
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.0
    }
}

pub(crate) fn sha256(parts: &[&[u8]]) -> Hash {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}
