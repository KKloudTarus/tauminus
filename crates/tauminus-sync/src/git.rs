// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! A git remote as the target, through the `git` command, which signs in
//! with the user's own SSH keys, agent and credential helpers. The
//! objects live on a branch of their own, `tauminus-sync`, in a local
//! clone. Each publish adds a commit on the fetched tip, so a push sends
//! one object, and every few the branch starts over with one commit, so the
//! remote keeps a few states, not one per sync: the push takes a lease on
//! the tip it built on, and one that lost a race fetches, applies this
//! device's changes again, and goes again.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Mutex, PoisonError};

use crate::target::{Folder, KEY, MAX_KEY_FILE, MAX_OBJECT, Target, valid_name};

/// The branch the objects live on.
pub const BRANCH: &str = "tauminus-sync";
/// Pushes tried before giving up on a remote other devices keep moving.
const PUSHES: usize = 3;
/// Files a fetched branch may hold, and their bytes all told: what a
/// remote sends is checked out only within these.
const MAX_FILES: usize = 4096;
const MAX_BYTES: u64 = 1024 * 1024 * 1024;
/// The remote's name, for the address given in the environment: one no
/// user's settings name, since they would add their address to it.
const REMOTE: &str = "tauminus-sync-6b1f0c3e";
/// Commits the branch keeps: each publish builds on the last, so a push
/// sends its own object only, and every so many the branch starts over
/// with one commit, so the remote holds at most this many states.
const HISTORY: u32 = 8;

/// A change this device made since the last refresh, applied again over a
/// remote that moved.
#[derive(Debug)]
enum Change {
    Put(String, Vec<u8>),
    Remove(String),
}

/// A git remote, cloned at `dir`.
pub struct Git {
    url: String,
    dir: PathBuf,
    files: Folder,
    /// The commit the branch was at when last fetched; `None` when the
    /// remote has no branch yet.
    head: Mutex<Option<String>>,
    changes: Mutex<Vec<Change>>,
    /// The tip last refused: not fetched again while the remote holds it.
    refused: Mutex<Option<String>>,
    /// A test's own remote on disk, and its own settings file.
    local: bool,
    global: Option<PathBuf>,
}

impl std::fmt::Debug for Git {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The address may hold a token.
        f.debug_struct("Git")
            .field("url", &self.redact(&self.url))
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl Git {
    /// `url` as `git clone` takes it; the clone lives at `dir`.
    ///
    /// # Errors
    /// A URL that `git` would read as an option.
    pub fn new(url: &str, dir: PathBuf) -> io::Result<Self> {
        let url = url.trim();
        if url.is_empty() || url.starts_with('-') || url.chars().any(char::is_control) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a git remote address",
            ));
        }
        if !over_network(url) {
            // A repository on disk is pushed to by `git` here, running its
            // hooks: a shared folder is the folder target's.
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a git remote is reached over https or ssh; for a shared folder, sync to the folder",
            ));
        }
        Ok(Self::with(url, dir, false, None))
    }

    fn with(url: &str, dir: PathBuf, local: bool, global: Option<PathBuf>) -> Self {
        Self {
            url: url.to_owned(),
            files: Folder::new(dir.clone()),
            dir,
            head: Mutex::new(None),
            changes: Mutex::new(Vec::new()),
            refused: Mutex::new(None),
            local,
            global,
        }
    }

    /// A remote on disk, for tests, with `global` as the user's settings.
    #[cfg(test)]
    pub(crate) fn local(url: &str, dir: PathBuf, global: Option<PathBuf>) -> Self {
        Self::with(url, dir, true, global)
    }

    /// `git` in the clone, with no prompts: a remote that wants a password
    /// fails instead of waiting on a terminal nobody sees. The `ext`
    /// transport, which runs a command, is off.
    #[expect(
        clippy::disallowed_methods,
        reason = "a Target blocks; the sync adapter calls it on a blocking thread"
    )]
    fn git(&self, args: &[&str]) -> io::Result<Output> {
        self.command(args).output()
    }

    /// The `git` command for `args`, as [`Git::git`] runs it.
    fn command(&self, args: &[&str]) -> Command {
        // The remote writes this worktree: nothing it holds may run or
        // rewrite a file. No hooks (an empty folder of its own, whatever
        // the user's global setting), no links, no line-ending or filter
        // conversion, no fsmonitor, no submodules.
        let git_dir = self.dir.join(".git");
        let hooks = format!("core.hooksPath={}", git_dir.join("no-hooks").display());
        // Nothing the user ignores everywhere keeps an object back.
        let excludes = format!(
            "core.excludesFile={}",
            git_dir.join("no-excludes").display()
        );
        let mut command = Command::new("git");
        // This clone and no other, whatever the environment names.
        command
            .arg("-C")
            .arg(&self.dir)
            .arg(format!("--git-dir={}", git_dir.display()))
            .arg(format!("--work-tree={}", self.dir.display()));
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_COMMON_DIR",
            "GIT_NAMESPACE",
            "GIT_CEILING_DIRECTORIES",
            "GIT_CONFIG_PARAMETERS",
            "GIT_ALLOW_PROTOCOL",
            "GIT_PROTOCOL_FROM_USER",
            "GIT_EXEC_PATH",
            "GIT_SSL_NO_VERIFY",
        ] {
            command.env_remove(var);
        }
        for setting in [
            // Transports by name: none that runs a command.
            "protocol.allow=never",
            "protocol.https.allow=always",
            "protocol.ssh.allow=always",
            if self.local {
                "protocol.file.allow=always"
            } else {
                "protocol.file.allow=never"
            },
            "transfer.fsckObjects=true",
            &hooks,
            &excludes,
            "core.symlinks=false",
            "core.autocrlf=false",
            "core.fsmonitor=false",
            "submodule.recurse=false",
            "commit.gpgSign=false",
            // Commits carry no name of the user's or the device's.
            "user.name=Tauminus",
            "user.email=sync@tauminus.invalid",
            // No reflog keeps a replaced commit's objects from `prune`.
            "core.logAllRefUpdates=false",
            // No gc of git's own choosing: with nothing kept, one after a
            // fetch would drop the commit just fetched, before checkout,
            // and trees a push that lost a race wrote, which the index
            // still names. This clone is cleaned after each push instead.
            "gc.auto=0",
            "maintenance.auto=false",
            "gc.reflogExpire=now",
            "gc.reflogExpireUnreachable=now",
            "gc.pruneExpire=now",
        ] {
            command.arg("-c").arg(setting);
        }
        command
            .args(args)
            // The address, which may hold a token, is in no command line.
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", format!("remote.{REMOTE}.url"))
            .env("GIT_CONFIG_VALUE_0", &self.url)
            // Messages in English, which `fetch` is read for.
            .env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "")
            .env("SSH_ASKPASS", "")
            // Commit times tell the target when, not where: no time zone.
            .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00+0000")
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00+0000");
        if let Some(global) = &self.global {
            command.env("GIT_CONFIG_GLOBAL", global);
        }
        command
    }

    /// `text` with any `user:secret@` of the remote's address hidden.
    fn redact(&self, text: &str) -> String {
        let Some((scheme, rest)) = self.url.split_once("://") else {
            return text.to_owned();
        };
        let Some((_, host)) = rest.split_once('@') else {
            return text.to_owned();
        };
        text.replace(&self.url, &format!("{scheme}://…@{host}"))
    }

    fn run(&self, args: &[&str]) -> io::Result<String> {
        let out = self.git(args)?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        // The arguments hold no address: that is in the environment.
        Err(io::Error::other(self.redact(&format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))))
    }

    /// The clone, made if missing, with its guards in place each time: a
    /// first run cut short leaves none missing.
    fn ensure_clone(&self) -> io::Result<()> {
        if !self.dir.join(".git").is_dir() {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            builder.create(&self.dir)?;
            self.run(&["init", "--quiet"])?;
        }
        // Ahead of any `.gitattributes` the remote commits: every file is
        // bytes, as written.
        let info = self.dir.join(".git").join("info");
        std::fs::create_dir_all(&info)?;
        std::fs::write(
            info.join("attributes"),
            // Sealed bytes never delta: none is looked for.
            "* -text -eol -filter -ident -working-tree-encoding -diff -merge -delta\n",
        )?;
        std::fs::create_dir_all(self.dir.join(".git").join("no-hooks"))?;
        Ok(())
    }

    /// Brings the worktree to the remote's branch, or empties it when the
    /// remote has none. Three processes, one of them on the network.
    fn fetch(&self, dirty: bool) -> io::Result<()> {
        let refspec = format!("refs/heads/{BRANCH}");
        // A tip refused before is not downloaded again: one round trip.
        let refused = self
            .refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(refused) = refused {
            let tip = self.run(&["ls-remote", REMOTE, &refspec])?;
            // The line for this ref exactly: `ls-remote` matches suffixes.
            let tip = tip.lines().find_map(|l| {
                let (sha, name) = l.split_once('\t')?;
                (name == refspec).then_some(sha)
            });
            if tip == Some(refused.as_str()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the sync branch holds files Tauminus did not write",
                ));
            }
        }
        // The tip only: what the branch held before is not this device's to
        // read.
        let out = self.git(&[
            "fetch",
            "--quiet",
            "--no-tags",
            "--depth=1",
            REMOTE,
            &refspec,
        ])?;
        let head = if out.status.success() {
            let fetched = std::fs::read_to_string(self.dir.join(".git").join("FETCH_HEAD"))?;
            let sha: String = fetched
                .chars()
                .take_while(char::is_ascii_hexdigit)
                .collect();
            if sha.len() < 40 {
                return Err(io::Error::other("git fetch: no commit fetched"));
            }
            let current = self
                .head
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            // The worktree is that commit already: one process, not three.
            if !dirty && current.as_deref() == Some(sha.as_str()) {
                *self.refused.lock().unwrap_or_else(PoisonError::into_inner) = None;
                return Ok(());
            }
            if let Err(refused) = self.check_fetched(&sha) {
                *self.refused.lock().unwrap_or_else(PoisonError::into_inner) = Some(sha.clone());
                // What the remote sent goes, or each refused fetch would
                // stay in the clone.
                let _ = self.run(&["repack", "-a", "-d", "-q"]);
                let _ = self.run(&["prune", "--expire=now"]);
                return Err(refused);
            }
            *self.refused.lock().unwrap_or_else(PoisonError::into_inner) = None;
            self.run(&["checkout", "--quiet", "--force", "-B", BRANCH, &sha])?;
            Some(sha)
        } else if String::from_utf8_lossy(&out.stderr)
            .lines()
            .any(|l| l.trim() == format!("fatal: couldn't find remote ref {refspec}"))
        {
            *self.refused.lock().unwrap_or_else(PoisonError::into_inner) = None;
            // A branch once fetched here and since deleted there goes too.
            let _ = self.run(&["update-ref", "-d", &refspec]);
            self.run(&["checkout", "--quiet", "--force", "--orphan", BRANCH])?;
            self.run(&["rm", "-r", "-q", "--cached", "--ignore-unmatch", "--", "."])?;
            None
        } else {
            return Err(io::Error::other(self.redact(&format!(
                "git fetch: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))));
        };
        // Files the branch does not track (one a cut-short sync left) go.
        self.run(&["clean", "-q", "-d", "-x", "--force", "--", "."])?;
        *self.head.lock().unwrap_or_else(PoisonError::into_inner) = head;
        Ok(())
    }

    /// Commits the worktree on the fetched tip, or alone every [`HISTORY`]
    /// commits, and pushes it if the branch is still at that tip.
    fn push(&self) -> io::Result<String> {
        // Forced: a `.gitignore` the remote commits hides no object.
        self.run(&["add", "--all", "--force", "--", "."])?;
        let tree = self.run(&["write-tree"])?;
        self.check_tree(tree.trim())?;
        let head = self
            .head
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        // How deep the branch is, from the last commit's message.
        let depth = match &head {
            Some(head) => self
                .run(&["log", "-1", "--format=%s", head])?
                .trim()
                .strip_prefix("sync ")
                .and_then(|n| n.parse::<u32>().ok())
                .filter(|d| *d < HISTORY)
                .unwrap_or(HISTORY),
            None => HISTORY,
        };
        let commit = match &head {
            Some(head) if depth.saturating_add(1) < HISTORY => {
                let message = format!("sync {}", depth + 1);
                self.run(&["commit-tree", tree.trim(), "-p", head, "-m", &message])?
            }
            _ => self.run(&["commit-tree", tree.trim(), "-m", "sync 0"])?,
        };
        let lease = format!(
            "--force-with-lease=refs/heads/{BRANCH}:{}",
            head.as_deref().unwrap_or_default()
        );
        self.run(&[
            "push",
            "--quiet",
            "--no-verify",
            &lease,
            REMOTE,
            &format!("{}:refs/heads/{BRANCH}", commit.trim()),
        ])?;
        Ok(commit.trim().to_owned())
    }

    /// Fails unless the fetched commit holds only what a sync target
    /// holds, within bounds: the key and objects, each a plain file of at
    /// most its size, few enough. A remote's one blob under a million
    /// names would fill the disk on checkout.
    fn check_fetched(&self, sha: &str) -> io::Result<()> {
        let listing = self.run(&["ls-tree", "-r", "-l", "-z", "--full-tree", sha])?;
        let refuse = |why: &str| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the sync branch holds {why}; it was not written by Tauminus"),
            )
        };
        let (mut files, mut bytes) = (0usize, 0u64);
        for entry in listing.split('\0').filter(|e| !e.is_empty()) {
            let (meta, path) = entry
                .split_once('\t')
                .ok_or_else(|| refuse("an odd entry"))?;
            let mut meta = meta.split_whitespace();
            let (mode, kind, size) = (meta.next(), meta.next(), meta.nth(1));
            if mode != Some("100644") || kind != Some("blob") || !valid_name(path) {
                return Err(refuse("a file of another kind"));
            }
            let size: u64 = size
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| refuse("an odd entry"))?;
            let limit = if path == KEY {
                MAX_KEY_FILE
            } else {
                MAX_OBJECT
            };
            files += 1;
            bytes = bytes.saturating_add(size);
            if size > limit || files > MAX_FILES || bytes > MAX_BYTES {
                return Err(refuse("more than a sync target holds"));
            }
        }
        Ok(())
    }

    /// Fails unless every object this device wrote is a plain file in
    /// `tree`, and every one it removed is gone: what the remote commits
    /// (a gitlink, a file where a folder goes) cannot make a write vanish.
    fn check_tree(&self, tree: &str) -> io::Result<()> {
        let listing = self.run(&["ls-tree", "-r", "--full-tree", tree])?;
        let files: std::collections::BTreeMap<&str, &str> = listing
            .lines()
            .filter_map(|l| {
                let (meta, path) = l.split_once('\t')?;
                Some((path, meta))
            })
            .collect();
        let changes = self.changes.lock().unwrap_or_else(PoisonError::into_inner);
        for change in changes.iter() {
            let ok = match change {
                Change::Put(name, _) => files
                    .get(name.as_str())
                    .is_some_and(|meta| meta.starts_with("100644 blob ")),
                Change::Remove(name) => !files.contains_key(name.as_str()),
            };
            if !ok {
                return Err(io::Error::other(
                    "the sync branch's files are not as written; another program changed them",
                ));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Whether `url` reaches a remote over the network: https, ssh, or ssh's
/// `host:path`. A path, `file://`, or a Windows share is on disk.
fn over_network(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("ssh://") {
        return true;
    }
    // `<helper>::<address>` runs a helper.
    if url.contains("://") || url.contains("::") || url.starts_with(['/', '\\', '.', '~']) {
        return false;
    }
    // `host:path`, the host without a slash, and longer than a drive
    // letter.
    url.split_once(':')
        .is_some_and(|(host, _)| host.len() > 1 && !host.contains(['/', '\\']))
}

impl Target for Git {
    fn refresh(&self) -> io::Result<()> {
        self.ensure_clone()?;
        // Changes a publish that failed left in the worktree, kept until a
        // fetch has put the worktree back.
        let dirty = !self
            .changes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty();
        self.fetch(dirty)?;
        self.changes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        Ok(())
    }

    fn list(&self) -> io::Result<Vec<String>> {
        self.files.list()
    }

    fn get(&self, name: &str, limit: u64) -> io::Result<Vec<u8>> {
        self.files.get(name, limit)
    }

    fn put_new(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.files.put_new(name, bytes)?;
        self.changes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Change::Put(name.to_owned(), bytes.to_vec()));
        Ok(())
    }

    fn remove(&self, name: &str) -> io::Result<()> {
        self.files.remove(name)?;
        self.changes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Change::Remove(name.to_owned()));
        Ok(())
    }

    fn flush(&self) -> io::Result<()> {
        let mut last = None;
        for attempt in 0..PUSHES {
            if attempt > 0 {
                // Another device pushed first: take its branch, then this
                // device's changes again. Each device touches only its own
                // files, so they apply as they did.
                self.fetch(true)?;
                let changes = self.changes.lock().unwrap_or_else(PoisonError::into_inner);
                for change in changes.iter() {
                    match change {
                        // A name taken meanwhile (two devices creating
                        // one key) fails: never one device's file for
                        // another's.
                        Change::Put(name, bytes) => self.files.put_new(name, bytes)?,
                        Change::Remove(name) => self.files.remove(name)?,
                    }
                }
            }
            match self.push() {
                Ok(commit) => {
                    // The worktree is the pushed commit: the next refresh
                    // need not check it out.
                    *self.head.lock().unwrap_or_else(PoisonError::into_inner) = Some(commit);
                    self.changes
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clear();
                    // What the replaced commits held is unreachable now:
                    // packed objects by a repack, loose ones by a prune,
                    // so the clone holds one copy.
                    // Kept by a ref, the pushed commit is not fetched again.
                    let branch = format!("refs/heads/{BRANCH}");
                    let pushed = self
                        .head
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone();
                    if let Some(pushed) = pushed {
                        let _ = self.run(&["update-ref", &branch, &pushed]);
                    }
                    // All is repacked, which drops what no commit holds; at
                    // a few states it costs little.
                    let _ = self.run(&["repack", "-a", "-d", "-q"]);
                    let _ = self.run(&["prune", "--expire=now"]);
                    return Ok(());
                }
                Err(error) => last = Some(error),
            }
        }
        Err(last.unwrap_or_else(|| io::Error::other("git push failed")))
    }
}

#[cfg(test)]
mod tests;
