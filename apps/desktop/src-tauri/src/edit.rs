// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Editing a remote file in a local editor.
//!
//! The file is copied into a new private folder (0700) under a name the
//! domain checked, and opened with a text editor program given its path as
//! the last argument: never through the file's type association, which
//! could run a file the server named `x.command`. Once a second its
//! content is hashed; when the hash changes and the server's copy is still
//! the one downloaded (same size and time), it is uploaded. A server copy
//! changed by someone else stops the uploads and says so. The folder goes
//! when the edit ends, and leftovers go at start.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::Serialize;
use specta::Type;
use tauminus_app::Files;
use tauminus_app::ports::{Download, EntryKind, SftpPort, TransferProgress, Upload};
use tauminus_domain::{LocalOs, local_name};
use tauri::State;
use tokio::sync::watch;

use crate::files::UploadState;
use crate::ipc::{CommandError, ErrorCode};

/// Largest file opened for editing.
const MAX_EDIT: u64 = 64 * 1024 * 1024;
/// How often the local copy is checked for changes.
const EVERY: Duration = Duration::from_secs(1);
/// Marks an edit folder whose last save did not reach the server, so a
/// later start leaves it.
const KEPT: &str = ".kept";
/// Most files open for editing at once.
const MAX_EDITS: usize = 16;
/// Text editors tried on Linux, in order, when none is configured.
const LINUX_EDITORS: &[&str] = &[
    "gnome-text-editor",
    "gedit",
    "kate",
    "kwrite",
    "mousepad",
    "xed",
    "pluma",
    "code",
    "codium",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum EditState {
    /// Open in the editor; nothing saved since it opened.
    Watching,
    /// The last save went to the server.
    Saved,
    Uploading,
    /// Someone changed the server's copy; uploads stopped so it is not lost.
    Conflict,
    Failed {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EditView {
    pub(crate) id: u32,
    pub(crate) session: u32,
    pub(crate) remote: String,
    /// The copy being edited, for taking changes from after a conflict.
    pub(crate) local: String,
    pub(crate) state: EditState,
}

struct Edit {
    session: u32,
    remote: String,
    local: PathBuf,
    state: Arc<Mutex<EditState>>,
    /// Dropped to end the edit.
    _stop: watch::Sender<()>,
}

/// The edits under way and where their folders go.
pub(crate) struct Edits {
    root: PathBuf,
    next: AtomicU32,
    open: Mutex<HashMap<u32, Edit>>,
}

impl Edits {
    /// Removes leftovers in `root` from instances no longer running, once
    /// `root` is checked to be a private folder of this user.
    pub(crate) fn new(root: PathBuf) -> Self {
        if create_private(&root).is_ok()
            && let Ok(entries) = std::fs::read_dir(&root)
        {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let pid = name
                    .to_str()
                    .and_then(|n| n.rsplit_once('-'))
                    .and_then(|(_, pid)| pid.parse::<u32>().ok());
                // A copy kept because its save did not reach the server
                // stays until the user removes it.
                let kept = entry.path().join(KEPT).exists();
                if pid.is_some_and(|pid| !alive(pid)) && !kept {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
        Self {
            root,
            next: AtomicU32::new(1),
            open: Mutex::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u32, Edit>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Whether a process with this pid runs: another Tauminus window's edits
/// stay. Where that cannot be told, it is taken as running.
#[cfg(unix)]
fn alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return true;
    };
    if pid == i32::try_from(std::process::id()).unwrap_or(0) {
        return false;
    }
    !matches!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
        Err(nix::errno::Errno::ESRCH)
    )
}

#[cfg(not(unix))]
fn alive(pid: u32) -> bool {
    pid != std::process::id()
}

/// Where edit folders go: the per-user runtime folder when there is one,
/// which only the user can enter, else the temp folder.
pub(crate) fn edit_root() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    base.join(format!("tauminus-edit-{}", user_tag()))
}

#[cfg(unix)]
fn user_tag() -> String {
    nix::unistd::getuid().to_string()
}

#[cfg(not(unix))]
fn user_tag() -> String {
    std::env::var("USERNAME").unwrap_or_else(|_| "user".into())
}

/// A new folder only this user can enter. `create_dir` fails if the name
/// exists, so another user cannot have made it first.
fn private_dir(root: &Path, id: u32) -> std::io::Result<PathBuf> {
    create_private(root)?;
    let dir = root.join(format!("{id}-{}", std::process::id()));
    create_new_private(&dir)?;
    Ok(dir)
}

#[cfg(unix)]
fn create_private(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // The root may exist from before: it must be ours, a real
            // folder, and closed to others.
            let metadata = std::fs::symlink_metadata(dir)?;
            let ours = metadata.uid() == nix::unistd::getuid().as_raw();
            if !metadata.is_dir() || !ours || metadata.permissions().mode() & 0o077 != 0 {
                return Err(std::io::Error::other(format!(
                    "{} is not a private folder",
                    dir.display()
                )));
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// A folder that must not exist yet, closed to others.
fn create_new_private(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(dir)
    }
    #[cfg(not(unix))]
    std::fs::create_dir(dir)
}

#[cfg(not(unix))]
fn create_private(dir: &Path) -> std::io::Result<()> {
    // Under the user's own profile, which other users cannot enter.
    match std::fs::create_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        other => other,
    }
}

/// The editor program and its leading arguments: `TAUMINUS_EDITOR`, split
/// on spaces with no shell, else the platform's text editor.
fn editor(env: Option<String>, on_path: impl Fn(&str) -> bool) -> Option<Vec<String>> {
    if let Some(configured) = env.filter(|e| !e.trim().is_empty()) {
        return Some(configured.split_whitespace().map(str::to_owned).collect());
    }
    if cfg!(target_os = "macos") {
        // `-t` opens with the default text editor, whatever the file's type.
        return Some(vec!["open".into(), "-t".into()]);
    }
    if cfg!(windows) {
        return Some(vec!["notepad.exe".into()]);
    }
    LINUX_EDITORS
        .iter()
        .find(|name| on_path(name))
        .map(|name| vec![(*name).to_owned()])
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

/// The file's hash, read 1 MiB at a time; off the async workers.
fn hash_file(path: &Path) -> Option<[u8; 32]> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            return Some(hasher.finalize().into());
        }
        hasher.update(&buffer[..read]);
    }
}

/// What one check of the local copy leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Nothing,
    Upload,
    Conflict,
}

/// Uploads only a changed copy, and only over the server copy it came
/// from.
fn step(last: &[u8; 32], now: &[u8; 32], server_unchanged: bool) -> Step {
    if last == now {
        Step::Nothing
    } else if server_unchanged {
        Step::Upload
    } else {
        Step::Conflict
    }
}

fn failed(error: impl std::fmt::Display) -> CommandError {
    CommandError::new(ErrorCode::LaunchFailed, error.to_string())
}

/// Opens a remote file in a local text editor and uploads each save.
#[tauri::command]
#[specta::specta]
pub(crate) async fn sftp_edit(
    files: State<'_, Arc<Files>>,
    edits: State<'_, Arc<Edits>>,
    uploads: State<'_, UploadState>,
    id: u32,
    remote: String,
) -> Result<u32, CommandError> {
    let invalid =
        |e: &dyn std::fmt::Display| CommandError::new(ErrorCode::InvalidArgument, e.to_string());
    let remote = crate::files::checked(&remote)?.to_owned();
    if edits.lock().len() >= MAX_EDITS {
        return Err(CommandError::new(
            ErrorCode::LimitReached,
            format!("at most {MAX_EDITS} files open for editing"),
        ));
    }
    let sftp = files.sftp(u64::from(id))?;
    // Saves go to the file a link points at, not over the link.
    let remote = sftp
        .canonicalize(&remote)
        .await
        .map_err(|e| CommandError::from(tauminus_app::FilesError::Sftp(e)))?;
    let entry = sftp
        .stat(&remote)
        .await
        .map_err(|e| CommandError::from(tauminus_app::FilesError::Sftp(e)))?;
    if entry.kind != EntryKind::File {
        return Err(invalid(&"only a file can be edited"));
    }
    if entry.size > MAX_EDIT {
        return Err(invalid(&format!(
            "files over {} MiB are not opened for editing",
            MAX_EDIT / (1024 * 1024)
        )));
    }
    let name = local_name(&entry.name, LocalOs::current())
        .map_err(|e| invalid(&e))?
        .to_owned();
    let program = editor(std::env::var("TAUMINUS_EDITOR").ok(), on_path).ok_or_else(|| {
        CommandError::new(
            ErrorCode::Unavailable,
            "no text editor found; set TAUMINUS_EDITOR to one",
        )
    })?;
    let edit = edits.next.fetch_add(1, Ordering::Relaxed);
    let dir = private_dir(&edits.root, edit).map_err(failed)?;
    let local = dir.join(&name);
    let job = Download {
        remote: remote.clone(),
        local: local.clone(),
        host: "edit".into(),
        overwrite: true,
        // A server that said it is small must not fill this folder.
        resume: false,
        max: Some(MAX_EDIT),
    };
    let (progress, _) = watch::channel(TransferProgress::default());
    let (_keep, never) = watch::channel(());
    if let Err(error) = sftp.download(&job, progress, never.clone()).await {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(CommandError::from(tauminus_app::FilesError::Sftp(error)));
    }
    let (program, args) = program.split_first().expect("editor has a program");
    if let Err(error) = tokio::process::Command::new(program)
        .args(args)
        .arg(&local)
        .current_dir(std::env::temp_dir())
        .spawn()
    {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(failed(format!("could not start {program}: {error}")));
    }
    let state = Arc::new(Mutex::new(EditState::Watching));
    let (stop, stopped) = watch::channel(());
    let current: CurrentSftp = {
        let files = Arc::clone(&files);
        Box::new(move || files.sftp(u64::from(id)).ok())
    };
    let watcher = Watcher {
        sftp: current,
        remote: remote.clone(),
        local: local.clone(),
        dir,
        state: Arc::clone(&state),
        baseline: (entry.size, entry.modified),
        uploads: uploads.0.clone(),
    };
    tauri::async_runtime::spawn(watcher.run(stopped));
    edits.lock().insert(
        edit,
        Edit {
            session: id,
            remote,
            local,
            state,
            _stop: stop,
        },
    );
    Ok(edit)
}

/// The session's SFTP as it is now: after a reconnect, the new one.
type CurrentSftp = Box<dyn Fn() -> Option<Arc<dyn SftpPort>> + Send + Sync>;

struct Watcher {
    sftp: CurrentSftp,
    remote: String,
    local: PathBuf,
    dir: PathBuf,
    state: Arc<Mutex<EditState>>,
    /// The server copy's size and time as last downloaded or uploaded.
    baseline: (u64, Option<u64>),
    uploads: PathBuf,
}

impl Watcher {
    fn set(&self, state: EditState) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = state;
    }

    /// The local copy's size and time, and its hash when either changed
    /// since `seen`; `None` when it cannot be read or is too large.
    async fn look(
        &self,
        seen: &mut Option<(u64, std::time::SystemTime)>,
    ) -> Option<Option<[u8; 32]>> {
        let metadata = tokio::fs::metadata(&self.local).await.ok()?;
        if metadata.len() > MAX_EDIT {
            self.set(EditState::Failed {
                reason: format!(
                    "the file grew past {} MiB; not saved",
                    MAX_EDIT / (1024 * 1024)
                ),
            });
            return None;
        }
        let modified = metadata.modified().ok()?;
        let stamp = (metadata.len(), modified);
        // Coarse clocks (FAT, HFS+) can hide a same-size save in the same
        // second, so a time this recent is hashed anyway, as git does.
        let recent = modified
            .elapsed()
            .map_or(true, |age| age < Duration::from_secs(2));
        if *seen == Some(stamp) && !recent {
            return Some(None);
        }
        *seen = Some(stamp);
        let path = self.local.clone();
        tokio::task::spawn_blocking(move || hash_file(&path))
            .await
            .ok()?
            .map(Some)
    }

    async fn run(mut self, mut stopped: watch::Receiver<()>) {
        let mut seen = None;
        let Some(Some(mut last)) = self.look(&mut seen).await else {
            self.set(EditState::Failed {
                reason: "the copy to edit could not be read".into(),
            });
            let _ = tokio::fs::remove_dir_all(&self.dir).await;
            return;
        };
        loop {
            let ended = tokio::select! {
                () = tokio::time::sleep(EVERY) => false,
                _ = stopped.changed() => true,
            };
            // One last look after the edit ends, so a final save goes too.
            // Hashed only when its size or time moved.
            if let Some(Some(now)) = self.look(&mut seen).await {
                last = self.check(last, now).await;
            }
            if ended {
                break;
            }
            let conflict =
                *self.state.lock().unwrap_or_else(PoisonError::into_inner) == EditState::Conflict;
            if conflict {
                // Uploads stop, but the local copy stays until the edit
                // ends, so the user can take their changes from it.
                let _ = stopped.changed().await;
                break;
            }
        }
        // A copy whose last save did not reach the server stays, so the
        // changes in it are not lost; the view shows where it is.
        let kept = matches!(
            *self.state.lock().unwrap_or_else(PoisonError::into_inner),
            EditState::Failed { .. } | EditState::Conflict
        );
        if kept {
            let _ = tokio::fs::write(self.dir.join(KEPT), b"").await;
        } else {
            let _ = tokio::fs::remove_dir_all(&self.dir).await;
        }
    }

    /// Uploads a changed copy if the server's is unchanged. Returns the
    /// hash now on the server.
    async fn check(&mut self, last: [u8; 32], now: [u8; 32]) -> [u8; 32] {
        if last == now {
            return last;
        }
        let Some(sftp) = (self.sftp)() else {
            self.set(EditState::Failed {
                reason: "the file session closed before the save went up".into(),
            });
            return last;
        };
        let server = sftp.stat(&self.remote).await.ok();
        let unchanged = server
            .as_ref()
            .is_some_and(|s| (s.size, s.modified) == self.baseline);
        match step(&last, &now, unchanged) {
            Step::Nothing => last,
            Step::Conflict => {
                self.set(EditState::Conflict);
                last
            }
            Step::Upload => {
                self.set(EditState::Uploading);
                let job = Upload {
                    local: self.local.clone(),
                    remote: self.remote.clone(),
                    host: "edit".into(),
                    overwrite: true,
                    state: self.uploads.clone(),
                    // A save is never spliced onto an older one.
                    resume: false,
                };
                let (progress, _) = watch::channel(TransferProgress::default());
                let (_keep, never) = watch::channel(());
                match sftp.upload(&job, progress, never).await {
                    Ok(_) => {
                        if let Ok(after) = sftp.stat(&self.remote).await {
                            self.baseline = (after.size, after.modified);
                        }
                        self.set(EditState::Saved);
                        now
                    }
                    Err(error) => {
                        self.set(EditState::Failed {
                            reason: error.to_string(),
                        });
                        last
                    }
                }
            }
        }
    }
}

/// Ends an edit: a last save goes up, then its folder is removed.
#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn edit_done(edits: State<'_, Arc<Edits>>, id: u32) -> bool {
    edits.lock().remove(&id).is_some()
}

#[tauri::command]
#[specta::specta]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects State by value into command handlers"
)]
pub(crate) fn edits_status(edits: State<'_, Arc<Edits>>) -> Vec<EditView> {
    let mut views: Vec<EditView> = edits
        .lock()
        .iter()
        .map(|(id, edit)| EditView {
            id: *id,
            session: edit.session,
            remote: edit.remote.clone(),
            local: edit.local.to_string_lossy().into_owned(),
            state: edit
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        })
        .collect();
    views.sort_by_key(|v| v.id);
    views
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha256(bytes: &[u8]) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes).into()
    }

    #[test]
    fn a_configured_editor_wins_and_is_split_without_a_shell() {
        let chosen = editor(Some("code --wait".into()), |_| false).unwrap();
        assert_eq!(chosen, ["code", "--wait"]);
        let quoted = editor(Some("ed; rm -rf ~".into()), |_| false).unwrap();
        assert_eq!(
            quoted,
            ["ed;", "rm", "-rf", "~"],
            "words, never a shell line"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_takes_the_first_text_editor_found() {
        assert_eq!(
            editor(None, |name| name == "mousepad" || name == "code").unwrap(),
            ["mousepad"]
        );
        assert_eq!(editor(None, |_| false), None);
        assert_eq!(editor(Some("  ".into()), |_| false), None);
    }

    #[test]
    fn only_a_changed_copy_over_the_same_server_copy_goes_up() {
        let a = sha256(b"a");
        let b = sha256(b"b");
        assert_eq!(step(&a, &a, true), Step::Nothing);
        assert_eq!(step(&a, &b, true), Step::Upload);
        assert_eq!(step(&a, &b, false), Step::Conflict);
    }

    #[cfg(unix)]
    #[test]
    fn edit_folders_are_private_and_new() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("tauminus-edit-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = private_dir(&root, 1).unwrap();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(private_dir(&root, 1).is_err(), "never reuses a folder");
        // A root others can enter is refused.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_dir(&root, 2).is_err());
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        // Leftovers of an instance that ended go at start; those of one
        // still running, and anything else, stay.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let gone = child.id();
        child.wait().unwrap();
        create_private(&root).unwrap();
        std::fs::create_dir(root.join(format!("1-{gone}"))).unwrap();
        std::fs::create_dir(root.join(format!("2-{}", std::process::id()))).unwrap();
        std::fs::create_dir(root.join("other")).unwrap();
        std::fs::create_dir(root.join(format!("3-{gone}"))).unwrap();
        std::fs::write(root.join(format!("3-{gone}")).join(KEPT), b"").unwrap();
        let _edits = Edits::new(root.clone());
        assert!(!root.join(format!("1-{gone}")).exists());
        assert!(root.join("other").exists());
        assert!(root.join(format!("3-{gone}")).exists(), "a kept copy stays");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_root_someone_else_made_or_linked_is_left_alone() {
        let base = std::env::temp_dir().join(format!("tauminus-edit-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("victim/keep")).unwrap();
        let root = base.join("root");
        std::os::unix::fs::symlink(base.join("victim"), &root).unwrap();
        let _edits = Edits::new(root.clone());
        assert!(
            base.join("victim/keep").exists(),
            "nothing deleted through the link"
        );
        assert!(
            private_dir(&root, 1).is_err(),
            "and no edit folder made in it"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    use std::sync::atomic::AtomicU64;

    use tauminus_app::ports::{BoxFuture, RemoteEntry, SftpError};

    /// A server file whose time moves on each upload, or when `touch` is
    /// called, as someone else saving would.
    #[derive(Default)]
    struct OneFile {
        modified: AtomicU64,
        uploads: Mutex<Vec<Vec<u8>>>,
    }

    impl OneFile {
        fn entry(&self) -> RemoteEntry {
            RemoteEntry {
                name: "app.conf".into(),
                kind: EntryKind::File,
                size: 1,
                modified: Some(self.modified.load(Ordering::SeqCst)),
                mode: Some(0o644),
            }
        }
    }

    impl SftpPort for OneFile {
        fn list<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Vec<RemoteEntry>, SftpError>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn stat<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<RemoteEntry, SftpError>> {
            let entry = self.entry();
            Box::pin(async move { Ok(entry) })
        }
        fn canonicalize<'a>(&'a self, p: &'a str) -> BoxFuture<'a, Result<String, SftpError>> {
            Box::pin(async move { Ok(p.to_owned()) })
        }
        fn mkdir<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
            Box::pin(async { Ok(()) })
        }
        fn remove_file<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
            Box::pin(async { Ok(()) })
        }
        fn remove_dir<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
            Box::pin(async { Ok(()) })
        }
        fn rename<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, Result<(), SftpError>> {
            Box::pin(async { Ok(()) })
        }
        fn chmod<'a>(&'a self, _: &'a str, _: u32) -> BoxFuture<'a, Result<(), SftpError>> {
            Box::pin(async { Ok(()) })
        }
        fn download<'a>(
            &'a self,
            _: &'a Download,
            _: watch::Sender<TransferProgress>,
            _: watch::Receiver<()>,
        ) -> BoxFuture<'a, Result<u64, SftpError>> {
            Box::pin(async { Ok(0) })
        }
        fn upload<'a>(
            &'a self,
            job: &'a Upload,
            _: watch::Sender<TransferProgress>,
            _: watch::Receiver<()>,
        ) -> BoxFuture<'a, Result<u64, SftpError>> {
            Box::pin(async move {
                let bytes = std::fs::read(&job.local).unwrap();
                self.uploads.lock().unwrap().push(bytes);
                self.modified.fetch_add(1, Ordering::SeqCst);
                Ok(1)
            })
        }
    }

    #[tokio::test]
    async fn each_save_goes_up_until_someone_else_changes_the_server_copy() {
        let dir = std::env::temp_dir().join(format!("tauminus-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let local = dir.join("app.conf");
        std::fs::write(&local, b"v1").unwrap();
        let server = Arc::new(OneFile::default());
        let state = Arc::new(Mutex::new(EditState::Watching));
        let mut watcher = Watcher {
            sftp: {
                let server = Arc::clone(&server);
                Box::new(move || Some(Arc::clone(&server) as Arc<dyn SftpPort>))
            },
            remote: "/etc/app.conf".into(),
            local: local.clone(),
            dir: dir.clone(),
            state: Arc::clone(&state),
            baseline: (1, Some(0)),
            uploads: dir.join("state"),
        };
        let v1 = sha256(b"v1");
        // Unchanged: nothing goes up.
        assert_eq!(watcher.check(v1, v1).await, v1);
        assert!(server.uploads.lock().unwrap().is_empty());
        // A save goes up and becomes the new baseline.
        std::fs::write(&local, b"v2").unwrap();
        let v2 = sha256(b"v2");
        assert_eq!(watcher.check(v1, v2).await, v2);
        assert_eq!(*server.uploads.lock().unwrap(), [b"v2".to_vec()]);
        assert_eq!(*state.lock().unwrap(), EditState::Saved);
        // Someone else saves on the server; the next local save stops.
        server.modified.fetch_add(10, Ordering::SeqCst);
        std::fs::write(&local, b"v3").unwrap();
        assert_eq!(watcher.check(v2, sha256(b"v3")).await, v2);
        assert_eq!(*state.lock().unwrap(), EditState::Conflict);
        assert_eq!(
            server.uploads.lock().unwrap().len(),
            1,
            "theirs was not overwritten"
        );
        // After a conflict, ending the edit keeps the copy with the changes.
        let (stop, stopped) = watch::channel(());
        drop(stop);
        watcher.run(stopped).await;
        assert_eq!(std::fs::read(&local).unwrap(), b"v3");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn watcher_for(
        dir: &Path,
        server: Option<Arc<OneFile>>,
        state: &Arc<Mutex<EditState>>,
    ) -> Watcher {
        Watcher {
            sftp: Box::new(move || server.clone().map(|s| s as Arc<dyn SftpPort>)),
            remote: "/etc/app.conf".into(),
            local: dir.join("app.conf"),
            dir: dir.to_owned(),
            state: Arc::clone(state),
            baseline: (1, Some(0)),
            uploads: dir.join("state"),
        }
    }

    #[tokio::test]
    async fn a_clean_end_removes_the_folder_and_an_unsent_save_keeps_it() {
        let base = std::env::temp_dir().join(format!("tauminus-watch-end-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for (name, server, kept) in [
            ("saved", Some(Arc::new(OneFile::default())), false),
            ("closed", None, true),
        ] {
            let dir = base.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("app.conf"), b"v1").unwrap();
            let state = Arc::new(Mutex::new(EditState::Watching));
            let watcher = watcher_for(&dir, server, &state);
            let (stop, stopped) = watch::channel(());
            let running = tokio::spawn(watcher.run(stopped));
            // Saved in the editor just before the edit ends.
            tokio::time::sleep(Duration::from_millis(50)).await;
            std::fs::write(dir.join("app.conf"), b"v2 longer").unwrap();
            drop(stop);
            running.await.unwrap();
            assert_eq!(dir.exists(), kept, "{name}: {:?}", state.lock().unwrap());
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
