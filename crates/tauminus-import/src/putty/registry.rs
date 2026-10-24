// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! PuTTY's sessions straight from the current user's registry. Read only:
//! nothing here writes or changes a key.

#![expect(unsafe_code, reason = "the registry API has no safe wrapper here")]

use std::collections::HashMap;

use tauminus_app::ports::ConfigHost;
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD, REG_EXPAND_SZ, REG_SZ, RegCloseKey,
    RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW,
};

use super::{MAX_SESSIONS, Value, session, unescape};

/// PuTTY's and KiTTY's session keys under the current user.
pub const PUTTY: &str = r"Software\SimonTatham\PuTTY\Sessions";
pub const KITTY: &str = r"Software\9bis.com\KiTTY\Sessions";

/// Name and value buffers: key and value names are at most 255
/// characters, and PuTTY's values far shorter than 8 KiB.
const NAME: usize = 256;
const NAME_LEN: u32 = 256;
const DATA: usize = 8192;
const DATA_LEN: u32 = 8192;

/// An open key, closed when dropped.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from RegOpenKeyExW and is closed once.
        unsafe { RegCloseKey(self.0) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open(parent: HKEY, path: &str) -> std::io::Result<Option<Key>> {
    let path = wide(path);
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: `path` is NUL-terminated and outlives the call; `key` is a
    // valid out pointer.
    let status = unsafe { RegOpenKeyExW(parent, path.as_ptr(), 0, KEY_READ, &raw mut key) };
    match status {
        ERROR_SUCCESS => Ok(Some(Key(key))),
        2 => Ok(None), // ERROR_FILE_NOT_FOUND: no such key, so no sessions.
        status => Err(std::io::Error::from_raw_os_error(status.cast_signed())),
    }
}

/// The sessions under `HKCU\<path>`; none when the key does not exist.
/// O(s·v) for s sessions of v values.
///
/// # Errors
/// The registry refusing to open or list the key.
pub fn read_registry(path: &str) -> std::io::Result<Vec<ConfigHost>> {
    let Some(root) = open(HKEY_CURRENT_USER, path)? else {
        return Ok(Vec::new());
    };
    let mut hosts = Vec::new();
    for index in 0..u32::try_from(MAX_SESSIONS).unwrap_or(u32::MAX) {
        // Key names are at most 255 characters.
        let mut name = [0u16; NAME];
        let mut len = NAME_LEN;
        // SAFETY: `name` holds `len` characters; the other pointers may be
        // null, as the API allows.
        let status = unsafe {
            RegEnumKeyExW(
                root.0,
                index,
                name.as_mut_ptr(),
                &raw mut len,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(status.cast_signed()));
        }
        let escaped = String::from_utf16_lossy(&name[..len as usize]);
        let Some(key) = open(root.0, &escaped)? else {
            continue;
        };
        if let Some(host) = session(&unescape(&escaped), &values(&key)) {
            hosts.push(host);
        }
    }
    Ok(hosts)
}

/// A session key's string and number values; any other kind is left out.
fn values(key: &Key) -> HashMap<String, Value> {
    let mut values = HashMap::new();
    for index in 0..1024 {
        let mut name = [0u16; NAME];
        let mut name_len = NAME_LEN;
        // Values PuTTY writes are short; a longer one is skipped.
        let mut data = vec![0u8; DATA];
        let mut data_len = DATA_LEN;
        let mut kind = 0u32;
        // SAFETY: the buffers hold the lengths passed; the reserved
        // pointer is null.
        let status = unsafe {
            RegEnumValueW(
                key.0,
                index,
                name.as_mut_ptr(),
                &raw mut name_len,
                std::ptr::null(),
                &raw mut kind,
                data.as_mut_ptr(),
                &raw mut data_len,
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status != ERROR_SUCCESS {
            continue;
        }
        let name = String::from_utf16_lossy(&name[..name_len as usize]);
        let data = &data[..data_len as usize];
        let value = match kind {
            REG_DWORD if data.len() == 4 => {
                Value::Number(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
            }
            REG_SZ | REG_EXPAND_SZ => {
                let units: Vec<u16> = data
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&p| u16::from_le_bytes(p))
                    .take_while(|&u| u != 0)
                    .collect();
                Value::Text(String::from_utf16_lossy(&units))
            }
            _ => continue,
        };
        values.insert(name, value);
    }
    values
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::System::Registry::{
        KEY_WRITE, REG_OPTION_NON_VOLATILE, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW,
    };

    use super::*;

    /// Writes a session under a key of this test's own, never PuTTY's.
    fn write(path: &str, name: &str, strings: &[(&str, &str)], numbers: &[(&str, u32)]) {
        let full = wide(&format!(r"{path}\{name}"));
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: valid NUL-terminated path and out pointer.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                full.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                std::ptr::null(),
                &raw mut key,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, ERROR_SUCCESS);
        let key = Key(key);
        for (name, value) in strings {
            let name = wide(name);
            let value = wide(value);
            // SAFETY: the data pointer and length describe `value`.
            unsafe {
                RegSetValueExW(
                    key.0,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    value.as_ptr().cast(),
                    u32::try_from(value.len() * 2).unwrap(),
                )
            };
        }
        for (name, value) in numbers {
            let name = wide(name);
            let bytes = value.to_le_bytes();
            // SAFETY: four bytes of a DWORD.
            unsafe { RegSetValueExW(key.0, name.as_ptr(), 0, REG_DWORD, bytes.as_ptr(), 4) };
        }
    }

    #[test]
    fn sessions_in_the_registry_read_as_putty_wrote_them() {
        let path = format!(r"Software\Tauminus-test-{}\Sessions", std::process::id());
        write(
            &path,
            "prod%20db",
            &[
                ("HostName", "10.0.0.5"),
                ("UserName", "deploy"),
                ("Protocol", "ssh"),
                ("ProxyHost", "bastion"),
            ],
            &[("PortNumber", 2222), ("ProxyMethod", 6), ("ProxyPort", 22)],
        );
        write(&path, "Default%20Settings", &[("HostName", "x")], &[]);
        let hosts = read_registry(&path).unwrap();
        let parent = wide(&path[..path.rfind('\\').unwrap()]);
        // SAFETY: deletes this test's own key tree.
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, parent.as_ptr()) };
        assert_eq!(hosts.len(), 1, "{hosts:?}");
        assert_eq!(hosts[0].alias, "prod db");
        assert_eq!(hosts[0].hostname, "10.0.0.5");
        assert_eq!(hosts[0].port, Some(2222));
        assert_eq!(hosts[0].user.as_deref(), Some("deploy"));
        assert_eq!(hosts[0].proxy_jump.as_deref(), Some("bastion"));
        assert_eq!(
            read_registry(r"Software\Tauminus-test-none\Sessions").unwrap(),
            Vec::<ConfigHost>::new()
        );
    }
}
