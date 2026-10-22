// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Hands SQLCipher its key through `sqlite3_key_v2` and `sqlite3_rekey_v2`,
//! not a `PRAGMA` statement: SQLite copies a statement's text into buffers
//! it frees without wiping, and this way the key passes through none of
//! them. The key text lives in a buffer wiped when dropped.

#![expect(
    unsafe_code,
    reason = "SQLCipher's key calls take a raw connection handle and a key buffer"
)]

use std::fmt::Write as _;

use rusqlite::Connection;
use zeroize::Zeroizing;

use crate::vault::SecretKey;

/// `x'` + 64 hex digits + `'`.
const RAW_KEY_LEN: usize = 67;
/// The same, as SQLCipher takes it.
const RAW_KEY_LEN_C: std::ffi::c_int = 67;

/// `x'<hex>'`: SQLCipher's form for a raw 256-bit key, which skips its own
/// key derivation. Exactly [`RAW_KEY_LEN`] bytes, written into a buffer of
/// that capacity, so it is never moved and left unwiped.
fn raw_key(key: &SecretKey) -> Zeroizing<String> {
    let mut text = Zeroizing::new(String::with_capacity(RAW_KEY_LEN));
    text.push_str("x'");
    for b in key.as_bytes() {
        let _ = write!(text, "{b:02x}");
    }
    text.push('\'');
    debug_assert_eq!(text.len(), RAW_KEY_LEN);
    text
}

/// Keys `conn` for its main database. `false` when SQLCipher refused.
pub(super) fn key(conn: &Connection, key: &SecretKey) -> bool {
    let text = raw_key(key);
    // SAFETY: the handle is the live connection's, used on this thread; the
    // name is a NUL-terminated literal; the key buffer outlives the call,
    // which copies it.
    let rc = unsafe {
        rusqlite::ffi::sqlite3_key_v2(
            conn.handle(),
            c"main".as_ptr(),
            text.as_ptr().cast(),
            RAW_KEY_LEN_C,
        )
    };
    rc == rusqlite::ffi::SQLITE_OK
}

/// Re-encrypts `conn`'s main database under `key`. `false` when SQLCipher
/// refused. SQLCipher rolls back a rewrite that failed partway (a full disk)
/// and still answers OK: the caller checks which key opens the file.
pub(super) fn rekey(conn: &Connection, key: &SecretKey) -> bool {
    let text = raw_key(key);
    // SAFETY: as in `key`.
    let rc = unsafe {
        rusqlite::ffi::sqlite3_rekey_v2(
            conn.handle(),
            c"main".as_ptr(),
            text.as_ptr().cast(),
            RAW_KEY_LEN_C,
        )
    };
    rc == rusqlite::ffi::SQLITE_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_raw_key_is_exactly_the_length_sqlcipher_is_told() {
        let text = raw_key(&SecretKey::random().unwrap());
        assert_eq!(text.len(), RAW_KEY_LEN);
        assert_eq!(usize::try_from(RAW_KEY_LEN_C), Ok(RAW_KEY_LEN));
        assert!(text.starts_with("x'") && text.ends_with('\''));
    }
}
