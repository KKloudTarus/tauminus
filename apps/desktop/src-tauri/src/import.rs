// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Hosts from other SSH clients: the user picks the file or folder in the
//! system's dialog, opened here, so no path crosses IPC; PuTTY's own
//! sessions are read where PuTTY keeps them. What is read goes through the
//! same import as `~/.ssh/config`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauminus_import::securecrt::File as CrtFile;
use tauminus_import::{Parsed, csv, decode, mobaxterm, putty, securecrt};
use tauri::State;

use crate::files::Home;
use crate::hosts::{Hosts, ImportResult, import_result};
use crate::ipc::{CommandError, ErrorCode};
use crate::zmodem::Choosing;

/// Where the hosts come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ImportKind {
    /// A CSV host list the user picks.
    Csv,
    /// The sessions PuTTY (or KiTTY) keeps on this machine: the registry
    /// on Windows, `~/.putty/sessions` elsewhere.
    Putty,
    /// A `.reg` file of PuTTY's sessions `regedit` exported.
    PuttyExport,
    /// A `MobaXterm.ini` or `.mxtsessions` file the user picks.
    MobaXterm,
    /// SecureCRT's `Config` or `Sessions` folder the user picks.
    SecureCrt,
}

/// Largest file read, and the most a session folder may hold in all.
const MAX_FILE: u64 = 16 * 1024 * 1024;
const MAX_TREE: u64 = 64 * 1024 * 1024;
/// Most files and folder levels a session folder is read to.
const MAX_FILES: usize = 20_000;
const MAX_DEPTH: usize = 16;

/// Imports hosts from `kind`: from the file or folder the user picks, or
/// for [`ImportKind::Putty`] from PuTTY's own store. `None` when the user
/// picked nothing.
#[tauri::command]
#[specta::specta]
pub(crate) async fn hosts_import_from<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    hosts: State<'_, Hosts>,
    choosing: State<'_, Choosing>,
    home: State<'_, Home>,
    kind: ImportKind,
) -> Result<Option<ImportResult>, CommandError> {
    let home = home.0.clone().unwrap_or_default();
    let parsed = if kind == ImportKind::Putty {
        let home = home.clone();
        blocking(move || putty_here(&home)).await??
    } else {
        let Some(path) = pick(app, &choosing, kind).await? else {
            return Ok(None);
        };
        blocking(move || read(kind, &path, &home)).await??
    };
    let (report, inventory) = hosts.book()?.import(&parsed).await?;
    Ok(Some(import_result(report, &inventory)))
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, CommandError> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))
}

/// The file or folder the user picks for `kind`, in the one dialog the app
/// may have open.
async fn pick<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    choosing: &Choosing,
    kind: ImportKind,
) -> Result<Option<PathBuf>, CommandError> {
    if choosing.0.swap(true, Ordering::AcqRel) {
        return Err(CommandError::new(
            ErrorCode::NotAllowed,
            "a file dialog is open already",
        ));
    }
    let picked = tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        let dialog = app.dialog().file();
        match kind {
            ImportKind::Csv => dialog
                .set_title("Import hosts from a CSV file")
                .add_filter("CSV", &["csv", "txt", "tsv"])
                .blocking_pick_file(),
            ImportKind::PuttyExport => dialog
                .set_title("Import PuTTY sessions exported with regedit")
                .add_filter("Registry export", &["reg"])
                .blocking_pick_file(),
            ImportKind::MobaXterm => dialog
                .set_title("Import MobaXterm sessions")
                .add_filter("MobaXterm", &["ini", "mxtsessions"])
                .blocking_pick_file(),
            ImportKind::SecureCrt | ImportKind::Putty => dialog
                .set_title("Choose SecureCRT's Config or Sessions folder")
                .blocking_pick_folder(),
        }
    })
    .await;
    choosing.0.store(false, Ordering::Release);
    Ok(picked
        .map_err(|e| CommandError::new(ErrorCode::Internal, e.to_string()))?
        .and_then(|path| path.into_path().ok()))
}

fn read(kind: ImportKind, path: &Path, home: &Path) -> Result<Parsed, CommandError> {
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    // A `.reg` export is UTF-16: twice the bytes of the same text.
    let limit = if kind == ImportKind::PuttyExport {
        MAX_TREE
    } else {
        MAX_FILE
    };
    let file = || read_capped(path, limit).map(|bytes| decode(&bytes));
    Ok(match kind {
        ImportKind::Csv => Parsed::new("CSV import", name, csv::hosts(&file()?)),
        ImportKind::PuttyExport => Parsed::new("PuTTY", name, putty::registry_export(&file()?)),
        ImportKind::MobaXterm => {
            Parsed::new("MobaXterm", name, mobaxterm::bookmarks(&file()?, home))
        }
        ImportKind::SecureCrt => {
            let (config, files) = session_tree(path)?;
            Parsed::new("SecureCRT", name, securecrt::sessions(files, &config))
        }
        ImportKind::Putty => putty_here(home)?,
    })
}

/// PuTTY's and KiTTY's sessions in the registry.
#[cfg(windows)]
fn putty_here(_home: &Path) -> Result<Parsed, CommandError> {
    let failed = |e: std::io::Error| CommandError::new(ErrorCode::Internal, e.to_string());
    let mut hosts = putty::read_registry(putty::PUTTY).map_err(failed)?;
    hosts.extend(putty::read_registry(putty::KITTY).map_err(failed)?);
    Ok(Parsed::new("PuTTY", "PuTTY's sessions".into(), hosts))
}

/// PuTTY's session files, from the first folder PuTTY would use that
/// exists.
#[cfg(not(windows))]
fn putty_here(home: &Path) -> Result<Parsed, CommandError> {
    let dirs = putty::session_dirs(home, |name| std::env::var(name).ok());
    let Some(dir) = dirs.into_iter().find(|d| d.is_dir()) else {
        return Err(CommandError::new(
            ErrorCode::NotFound,
            "PuTTY has no saved sessions here (~/.putty/sessions)",
        ));
    };
    let mut files = Vec::new();
    let mut total = 0;
    for entry in std::fs::read_dir(&dir)
        .map_err(|e| io(&e))?
        .flatten()
        .take(MAX_FILES)
    {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let bytes = read_capped(&path, MAX_FILE)?;
        total += bytes.len() as u64;
        if total > MAX_TREE {
            return Err(too_big());
        }
        files.push((
            entry.file_name().to_string_lossy().into_owned(),
            decode(&bytes),
        ));
    }
    Ok(Parsed::new(
        "PuTTY",
        dir.display().to_string(),
        putty::session_files(files.iter().map(|(n, t)| (n.as_str(), t.as_str()))),
    ))
}

fn io(error: &std::io::Error) -> CommandError {
    CommandError::new(ErrorCode::InvalidArgument, error.to_string())
}

fn too_big() -> CommandError {
    CommandError::new(
        ErrorCode::InvalidArgument,
        "the sessions are larger than an import reads (64 MiB)",
    )
}

/// The bytes of the regular file at `path`, at most `limit` of them. The
/// type is checked before opening, so a pipe does not hang the import,
/// and again on the open file.
fn read_capped(path: &Path, limit: u64) -> Result<Vec<u8>, CommandError> {
    let not_file = || {
        CommandError::new(
            ErrorCode::InvalidArgument,
            "only a regular file can be imported",
        )
    };
    if !std::fs::metadata(path).map_err(|e| io(&e))?.is_file() {
        return Err(not_file());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // A pipe swapped in after the check opens without waiting.
        options.custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOCTTY);
    }
    let file = options.open(path).map_err(|e| io(&e))?;
    let meta = file.metadata().map_err(|e| io(&e))?;
    if !meta.is_file() {
        return Err(CommandError::new(
            ErrorCode::InvalidArgument,
            "only a regular file can be imported",
        ));
    }
    if meta.len() > limit {
        return Err(CommandError::new(
            ErrorCode::InvalidArgument,
            format!(
                "the file is larger than an import reads ({} MiB)",
                limit >> 20
            ),
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io(&e))?;
    if bytes.len() as u64 > limit {
        return Err(too_big());
    }
    Ok(bytes)
}

/// SecureCRT's session files below `picked`, its `Config` folder or the
/// `Sessions` folder in it, with the `Config` folder key paths name.
/// Links are not followed; at most 20 000 files, 16 levels and 64 MiB.
fn session_tree(picked: &Path) -> Result<(PathBuf, Vec<CrtFile>), CommandError> {
    // `Sessions` counts only as a folder of its own, not a link elsewhere.
    let sessions_dir =
        std::fs::symlink_metadata(picked.join("Sessions")).is_ok_and(|m| m.file_type().is_dir());
    let (config, root) = if sessions_dir {
        (picked.to_path_buf(), picked.join("Sessions"))
    } else {
        (
            picked.parent().unwrap_or(picked).to_path_buf(),
            picked.to_path_buf(),
        )
    };
    let mut files = Vec::new();
    let mut total = 0u64;
    let mut stack = vec![(root, Vec::<String>::new())];
    while let Some((dir, folder)) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| io(&e))?
            .flatten()
            .collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            // Not followed: a link could lead out of the folder, or loop.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if kind.is_dir() && folder.len() + 1 < MAX_DEPTH {
                let mut inner = folder.clone();
                inner.push(name);
                stack.push((entry.path(), inner));
            } else if kind.is_file() && securecrt::is_session_file(&name) {
                if files.len() >= MAX_FILES {
                    return Ok((config, files));
                }
                let bytes = read_capped(&entry.path(), MAX_FILE)?;
                total += bytes.len() as u64;
                if total > MAX_TREE {
                    return Err(too_big());
                }
                files.push(CrtFile {
                    folder: folder.clone(),
                    name,
                    text: decode(&bytes),
                });
            }
        }
    }
    Ok((config, files))
}

#[cfg(test)]
mod tests;
