// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Where the sealed objects live. An object is written once and never
//! changed; each device writes only its own, so no backend has to merge.
//! Names are `key` and `devices/<device>/<seq>`, checked before use.

use std::io;
use std::path::{Path, PathBuf};

/// A sync target. Blocking: callers run it off the async workers.
pub trait Target: Send + Sync + 'static {
    /// Brings the local view up to date (a git pull); nothing elsewhere.
    ///
    /// # Errors
    /// The target cannot be reached.
    fn refresh(&self) -> io::Result<()>;
    /// Every object's name.
    ///
    /// # Errors
    /// The target cannot be read.
    fn list(&self) -> io::Result<Vec<String>>;
    /// An object's bytes, at most `limit` of them.
    ///
    /// # Errors
    /// No such object, or one larger than `limit`.
    fn get(&self, name: &str, limit: u64) -> io::Result<Vec<u8>>;
    /// Writes a new object; an existing one is an error.
    ///
    /// # Errors
    /// The name is taken, or the target cannot be written.
    fn put_new(&self, name: &str, bytes: &[u8]) -> io::Result<()>;
    /// Removes one of this device's objects.
    ///
    /// # Errors
    /// The target cannot be written.
    fn remove(&self, name: &str) -> io::Result<()>;
    /// Sends what was written (a git commit and push); nothing elsewhere.
    ///
    /// # Errors
    /// The target cannot be reached.
    fn flush(&self) -> io::Result<()>;
}

/// The key file's name.
pub const KEY: &str = "key";
/// Largest key file read.
pub const MAX_KEY_FILE: u64 = 4096;
/// Largest object read, sealed: compressed, the state at 10 000 hosts
/// takes about 1 MB.
pub const MAX_OBJECT: u64 = 64 * 1024 * 1024;
const DEVICES: &str = "devices";

/// `devices/<device>/<seq>`, the seq zero-padded so names sort in order.
#[must_use]
pub fn object_name(device: &str, seq: u64) -> String {
    format!("{DEVICES}/{device}/{seq:020}")
}

/// The device and seq an object name holds, if it is one.
#[must_use]
pub fn parse_object(name: &str) -> Option<(&str, u64)> {
    let rest = name.strip_prefix("devices/")?;
    let (device, seq) = rest.split_once('/')?;
    if !valid_device(device) || seq.len() != 20 || !seq.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((device, seq.parse().ok()?))
}

/// A device id: 32 lowercase hex digits.
#[must_use]
pub fn valid_device(device: &str) -> bool {
    device.len() == 32
        && device
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether `name` is one a target may hold: the key file or an object.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    name == KEY || parse_object(name).is_some()
}

fn invalid(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("not an object name: {name}"),
    )
}

/// A folder, as a sync tool (Syncthing, Dropbox, a network share) copies
/// it between devices.
#[derive(Debug, Clone)]
pub struct Folder {
    root: PathBuf,
}

impl Folder {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, name: &str) -> io::Result<PathBuf> {
        if !valid_name(name) {
            return Err(invalid(name));
        }
        Ok(self.root.join(name))
    }

    /// Fails unless every folder from the root down to `name`'s is a real
    /// folder: a link there, which a target can plant, would lead reads
    /// and writes out of it.
    fn check_dirs(&self, name: &str) -> io::Result<()> {
        let mut dir = self.root.clone();
        let mut parts: Vec<&str> = name.split('/').collect();
        parts.pop();
        for part in std::iter::once("").chain(parts) {
            if !part.is_empty() {
                dir.push(part);
            }
            match std::fs::symlink_metadata(&dir) {
                Ok(meta) if meta.file_type().is_dir() => {}
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "a folder of the sync target is a link",
                    ));
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// Opens a file for reading without following a link, and without waiting
/// on a pipe; only a regular file passes, checked on the open handle.
fn open_regular(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        // FILE_FLAG_OPEN_REPARSE_POINT: a link opens as itself.
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path).map_err(|e| {
        // A link (refused by the open) or a socket is no object.
        match std::fs::symlink_metadata(path) {
            Ok(meta) if !meta.file_type().is_file() => {
                io::Error::new(io::ErrorKind::InvalidData, "not a regular file")
            }
            _ => e,
        }
    })?;
    if !file.metadata()?.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a regular file",
        ));
    }
    Ok(file)
}

impl Target for Folder {
    fn refresh(&self) -> io::Result<()> {
        Ok(())
    }

    fn list(&self) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        if self.root.join(KEY).is_file() {
            names.push(KEY.to_owned());
        }
        let devices = self.root.join(DEVICES);
        let Ok(entries) = std::fs::read_dir(&devices) else {
            return Ok(names);
        };
        for device in entries.flatten() {
            let device_name = device.file_name().to_string_lossy().into_owned();
            if !valid_device(&device_name) || !device.file_type()?.is_dir() {
                continue;
            }
            for object in std::fs::read_dir(device.path())?.flatten() {
                let name = format!(
                    "{DEVICES}/{device_name}/{}",
                    object.file_name().to_string_lossy()
                );
                // Whatever holds an object's name is listed, file or not: a
                // folder planted there is then found forged, and removed.
                if parse_object(&name).is_some() {
                    names.push(name);
                }
            }
        }
        names.sort();
        Ok(names)
    }

    fn get(&self, name: &str, limit: u64) -> io::Result<Vec<u8>> {
        use std::io::Read as _;
        let path = self.path(name)?;
        self.check_dirs(name)?;
        let file = open_regular(&path)?;
        if file.metadata()?.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "object too large",
            ));
        }
        let mut bytes = Vec::new();
        file.take(limit + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "object too large",
            ));
        }
        Ok(bytes)
    }

    fn put_new(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write as _;
        let path = self.path(name)?;
        let dir = path.parent().ok_or_else(|| invalid(name))?;
        self.check_dirs(name)?;
        std::fs::create_dir_all(dir)?;
        self.check_dirs(name)?;
        if std::fs::symlink_metadata(&path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                name.to_owned(),
            ));
        }
        // Written beside it under a fresh random name, created new (never
        // through a link a target planted), then moved into place in one
        // step, so a sync tool never copies half an object.
        let mut suffix = [0u8; 8];
        rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut suffix)
            .map_err(|_| io::Error::other("no randomness from the system"))?;
        let partial = dir.join(format!(".partial-{}", u64::from_le_bytes(suffix)));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
        }
        let mut file = options.open(&partial)?;
        let written = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        if let Err(e) = written {
            let _ = std::fs::remove_file(&partial);
            return Err(e);
        }
        // A link fails on a name taken meanwhile, where a rename would
        // replace it: two devices creating one folder's key at once both
        // see an empty folder. A file system with no links renames.
        match std::fs::hard_link(&partial, &path) {
            Ok(()) => std::fs::remove_file(&partial),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let _ = std::fs::remove_file(&partial);
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    name.to_owned(),
                ))
            }
            // No links here (FAT, some shares): a rename, which a name
            // taken in the instant between check and rename would lose.
            Err(_) if std::fs::symlink_metadata(&path).is_ok() => {
                let _ = std::fs::remove_file(&partial);
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    name.to_owned(),
                ))
            }
            Err(_) => std::fs::rename(&partial, &path).inspect_err(|_| {
                let _ = std::fs::remove_file(&partial);
            }),
        }
    }

    fn remove(&self, name: &str) -> io::Result<()> {
        let path = self.path(name)?;
        self.check_dirs(name)?;
        // A folder planted at an object's name goes with what is in it;
        // a link is removed as itself, never followed.
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_dir()) {
            return std::fs::remove_dir_all(path);
        }
        match std::fs::remove_file(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    fn flush(&self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn names_are_checked_so_none_leaves_the_folder() {
        assert_eq!(parse_object(&object_name(DEVICE, 7)), Some((DEVICE, 7)));
        for bad in [
            "../x",
            "devices/../../x/00000000000000000001",
            "devices/AB/00000000000000000001",
            "key2",
            "/etc/passwd",
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn links_a_target_planted_are_neither_written_through_nor_read() {
        let root = std::env::temp_dir().join(format!("tauminus-sync-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let outside = root.with_extension("outside");
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&outside).unwrap();
        let victim = outside.join("victim");
        std::fs::write(&victim, b"keep me").unwrap();
        // A link where this device's folder goes, and one at `key`.
        std::fs::create_dir_all(root.join("devices")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("devices").join(DEVICE)).unwrap();
        std::os::unix::fs::symlink(&victim, root.join(KEY)).unwrap();
        let folder = Folder::new(root.clone());
        assert!(folder.put_new(&object_name(DEVICE, 1), b"x").is_err());
        assert!(folder.get(KEY, 100).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
        assert_eq!(
            std::fs::read_dir(&outside).unwrap().count(),
            1,
            "nothing written there"
        );
        // A link where an object's partial file might go is never followed:
        // the name is fresh and created new.
        std::fs::remove_file(root.join("devices").join(DEVICE)).unwrap();
        std::fs::create_dir_all(root.join("devices").join(DEVICE)).unwrap();
        for n in 0..50 {
            std::os::unix::fs::symlink(
                &victim,
                root.join("devices")
                    .join(DEVICE)
                    .join(format!(".partial-{n}")),
            )
            .unwrap();
        }
        folder.put_new(&object_name(DEVICE, 1), b"x").unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn a_folder_keeps_objects_once_and_lists_only_its_own_names() {
        let root =
            std::env::temp_dir().join(format!("tauminus-sync-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let folder = Folder::new(root.clone());
        let name = object_name(DEVICE, 1);
        folder.put_new(&name, b"one").unwrap();
        assert_eq!(
            folder.put_new(&name, b"two").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        folder.put_new(KEY, b"k").unwrap();
        let partials = std::fs::read_dir(root.join("devices").join(DEVICE))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".partial-"))
            .count();
        assert_eq!(partials, 0, "no partial file left behind");
        std::fs::write(root.join("devices").join(DEVICE).join("stray.txt"), b"x").unwrap();
        assert_eq!(folder.list().unwrap(), [name.clone(), KEY.to_owned()]);
        assert_eq!(folder.get(&name, 3).unwrap(), b"one");
        assert!(folder.get(&name, 2).is_err(), "over the limit");
        folder.remove(&name).unwrap();
        folder.remove(&name).unwrap();
        assert_eq!(folder.list().unwrap(), [KEY.to_owned()]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
