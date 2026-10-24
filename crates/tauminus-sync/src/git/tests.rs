// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Two devices syncing through a bare git repository, as through any
//! remote: `git` must be installed, as it is in CI.

#![expect(
    clippy::disallowed_methods,
    reason = "a test runs git and waits for it"
)]

use std::sync::Arc;

use tauminus_app::ports::{SyncData, SyncPart, SyncRemote};
use tauminus_domain::{Host, HostId, HostName, Label};

use super::*;
use crate::engine::{Remote, create, join};
use crate::seal::Kdf;

fn has_git() -> bool {
    let found = Command::new("git").arg("--version").output().is_ok();
    if !found {
        assert!(
            std::env::var_os("CI").is_none(),
            "git must be installed in CI"
        );
    }
    found
}

fn data_with(hosts: &[(&str, &str)]) -> SyncData {
    let mut data = SyncData::default();
    for (id, label) in hosts {
        data.hosts
            .put_host(Host::new(
                HostId::parse(id).unwrap(),
                Label::parse(label).unwrap(),
                HostName::parse("canary-git.internal").unwrap(),
            ))
            .unwrap();
    }
    data
}

#[tokio::test]
async fn two_devices_sync_through_a_bare_repository_that_holds_only_ciphertext() {
    if !has_git() {
        return;
    }
    let root = std::env::temp_dir().join(format!("tauminus-sync-git-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let bare = root.join("remote.git");
    let status = Command::new("git")
        .args(["init", "--quiet", "--bare"])
        .arg(&bare)
        .status()
        .unwrap();
    assert!(status.success());
    let url = bare.to_string_lossy().into_owned();
    let a_git = Arc::new(Git::local(&url, root.join("a-clone"), None));
    let b_git = Arc::new(Git::local(&url, root.join("b-clone"), None));
    let key = create(a_git.as_ref(), "correct horse", Kdf::LEAST).unwrap();
    let b_key = join(b_git.as_ref(), "correct horse").unwrap();
    let a = Remote::new(a_git.clone(), key, root.join("a-state.json"));
    let b = Remote::new(b_git.clone(), b_key, root.join("b-state.json"));

    // A merges, then B syncs whole and pushes first: A's push loses the
    // race, fetches, applies its object again, and goes again.
    let merged_a = a
        .merge(data_with(&[("h1", "canary-label-a")]))
        .await
        .unwrap();
    let merged_b = b
        .merge(data_with(&[("h2", "canary-label-b")]))
        .await
        .unwrap();
    assert_eq!(
        merged_b.data.hosts.hosts().count(),
        1,
        "A has not pushed yet"
    );
    b.adopt(SyncPart::Hosts).await.unwrap();
    b.adopt(SyncPart::Snippets).await.unwrap();
    b.publish().await.unwrap();
    a.adopt(SyncPart::Hosts).await.unwrap();
    a.adopt(SyncPart::Snippets).await.unwrap();
    a.publish().await.unwrap();
    assert_eq!(merged_a.data.hosts.hosts().count(), 1);

    // Both now hold both hosts.
    let on_b = b.merge(merged_b.data.clone()).await.unwrap();
    b.adopt(SyncPart::Hosts).await.unwrap();
    b.adopt(SyncPart::Snippets).await.unwrap();
    b.publish().await.unwrap();
    let on_a = a.merge(merged_a.data.clone()).await.unwrap();
    a.adopt(SyncPart::Hosts).await.unwrap();
    a.adopt(SyncPart::Snippets).await.unwrap();
    a.publish().await.unwrap();
    assert_eq!(on_a.data.hosts.hosts().count(), 2);
    assert_eq!(on_b.data.hosts.hosts().count(), 2);

    // Every object the remote holds, blobs and commits: no plaintext.
    let all = Command::new("git")
        .arg("-C")
        .arg(&bare)
        .args(["cat-file", "--batch-all-objects", "--batch"])
        .output()
        .unwrap();
    assert!(all.status.success());
    assert_ne!(all.stdout.len(), 0);
    for secret in ["canary-git.internal", "canary-label-a", "canary-label-b"] {
        assert!(
            !all.stdout
                .windows(secret.len())
                .any(|w| w == secret.as_bytes()),
            "{secret} is in the remote"
        );
    }
    // Commits name no one.
    let log = Command::new("git")
        .arg("-C")
        .arg(&bare)
        .args(["log", "--format=%an <%ae>", BRANCH])
        .output()
        .unwrap();
    let authors = String::from_utf8_lossy(&log.stdout);
    assert!(
        authors
            .lines()
            .all(|l| l == "Tauminus <sync@tauminus.invalid>"),
        "{authors}"
    );
    // A few syncs, a few commits; never more than the history kept.
    let count = Command::new("git")
        .arg("-C")
        .arg(&bare)
        .args(["rev-list", "--count", BRANCH])
        .output()
        .unwrap();
    let count: u32 = String::from_utf8_lossy(&count.stdout)
        .trim()
        .parse()
        .unwrap();
    assert!((1..=8).contains(&count), "{count}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_address_git_would_read_as_an_option_is_refused() {
    for bad in ["--upload-pack=touch /tmp/x", "-oProxyCommand=x", "", "a\nb"] {
        assert!(Git::new(bad, std::env::temp_dir()).is_err(), "{bad:?}");
    }
    // On disk, where `git` would run the repository's own hooks.
    for local in [
        "/srv/sync.git",
        "file:///srv/sync.git",
        "./sync.git",
        "~/sync.git",
        "C:\\sync.git",
        "C:/sync.git",
        "\\\\server\\share\\sync.git",
        "//server/share/sync.git",
        "sync.git",
        "git://example.com/sync.git",
        "ext::sh -c touch% /tmp/x",
        "fd::3",
    ] {
        assert!(Git::new(local, std::env::temp_dir()).is_err(), "{local:?}");
    }
    for remote in [
        "git@example.com:me/sync.git",
        "example.com:sync.git",
        "ssh://git@example.com/me/sync.git",
        "https://example.com/me/sync.git",
    ] {
        assert!(Git::new(remote, std::env::temp_dir()).is_ok(), "{remote:?}");
    }
}

#[test]
fn a_token_in_the_address_is_hidden_in_errors() {
    let git = Git::new(
        "https://me:s3cret@example.com/sync.git",
        std::env::temp_dir(),
    )
    .unwrap();
    let shown = git.redact("fatal: unable to access 'https://me:s3cret@example.com/sync.git/'");
    assert!(!shown.contains("s3cret"), "{shown}");
}

/// Runs `git` in `dir` as a third party would, for the hostile remote.
fn plain_git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=x", "-c", "user.email=x@x"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "{args:?}");
}

/// What the remote commits cannot run a hook, check out a link, or have
/// its bytes rewritten, whatever the user's own git settings say.
#[cfg(unix)]
#[test]
fn a_hostile_remote_runs_nothing_and_changes_no_byte() {
    use std::os::unix::fs::PermissionsExt as _;
    if !has_git() {
        return;
    }
    let root = std::env::temp_dir().join(format!("tauminus-sync-hostile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // The user's own settings: hooks that leave a mark, CRLF, and an
    // ignore list that ignores everything.
    let hooks = root.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let mark = root.join("hook-ran");
    for hook in [
        "post-checkout",
        "pre-push",
        "post-commit",
        "reference-transaction",
    ] {
        let path = hooks.join(hook);
        std::fs::write(&path, format!("#!/bin/sh\ntouch '{}'\n", mark.display())).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let excludes = root.join("ignore-all");
    std::fs::write(&excludes, "*\n").unwrap();
    let global = root.join("gitconfig");
    std::fs::write(
        &global,
        format!(
            "[core]\n\thooksPath = {}\n\tautocrlf = true\n\texcludesFile = {}\n",
            hooks.display(),
            excludes.display()
        ),
    )
    .unwrap();

    // A remote whose sync branch holds a link at `key`, attributes asking
    // for CRLF and a filter, and an ignore list: refused before checkout.
    plain_git(&root, &["init", "--quiet", "--bare", "evil.git"]);
    let evil = root.join("evil");
    std::fs::create_dir_all(&evil).unwrap();
    plain_git(&evil, &["init", "--quiet", "-b", BRANCH]);
    std::os::unix::fs::symlink("/etc/hostname", evil.join("key")).unwrap();
    std::fs::write(evil.join(".gitattributes"), "* text eol=crlf filter=evil\n").unwrap();
    std::fs::write(evil.join(".gitignore"), "devices/\nkey\n").unwrap();
    plain_git(&evil, &["add", "-A", "--force"]);
    plain_git(&evil, &["commit", "--quiet", "--no-verify", "-m", "x"]);
    let evil_url = root.join("evil.git").to_string_lossy().into_owned();
    plain_git(&evil, &["push", "--quiet", &evil_url, BRANCH]);
    let refused = Git::local(&evil_url, root.join("evil-clone"), Some(global.clone()));
    assert!(
        refused.refresh().is_err(),
        "a branch Tauminus did not write"
    );
    assert!(
        std::fs::symlink_metadata(root.join("evil-clone").join("key")).is_err(),
        "nothing was checked out"
    );

    // A plain remote, under those settings: every byte goes out as written.
    // The remote on disk runs its own hooks, the user's global ones among
    // them, which is why a real remote is never on disk; here it gets none.
    plain_git(&root, &["init", "--quiet", "--bare", "remote.git"]);
    let none = root.join("remote-hooks");
    std::fs::create_dir_all(&none).unwrap();
    plain_git(
        &root.join("remote.git"),
        &["config", "core.hooksPath", &none.to_string_lossy()],
    );
    let url = root.join("remote.git").to_string_lossy().into_owned();
    let target = Git::local(&url, root.join("clone"), Some(global.clone()));
    target.refresh().unwrap();
    let bytes: Vec<u8> = (0u8..=255).chain(*b"\n\r\n").collect();
    let name = crate::target::object_name("0123456789abcdef0123456789abcdef", 1);
    target.put_new(&name, &bytes).unwrap();
    target.flush().unwrap();
    let other = Git::local(&url, root.join("clone2"), Some(global));
    other.refresh().unwrap();
    assert_eq!(
        other.get(&name, 1024).unwrap(),
        bytes,
        "every byte as written"
    );
    assert!(!mark.exists(), "a hook ran");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn one_blob_under_many_names_is_refused_before_checkout() {
    if !has_git() {
        return;
    }
    let root = std::env::temp_dir().join(format!("tauminus-sync-bomb-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    plain_git(&root, &["init", "--quiet", "--bare", "remote.git"]);
    let src = root.join("src");
    std::fs::create_dir_all(src.join("devices").join("0123456789abcdef0123456789abcdef")).unwrap();
    plain_git(&src, &["init", "--quiet", "-b", BRANCH]);
    // One blob under more names than a target holds. A big blob under as
    // many fills the disk on checkout; the count is checked like the size.
    for seq in 1..=5000 {
        std::fs::write(
            src.join(crate::target::object_name(
                "0123456789abcdef0123456789abcdef",
                seq,
            )),
            b"x",
        )
        .unwrap();
    }
    plain_git(&src, &["add", "-A"]);
    plain_git(&src, &["commit", "--quiet", "--no-verify", "-m", "x"]);
    let url = root.join("remote.git").to_string_lossy().into_owned();
    plain_git(&src, &["push", "--quiet", &url, BRANCH]);
    let git = Git::local(&url, root.join("clone"), None);
    assert!(git.refresh().is_err());
    assert!(
        std::fs::read_dir(root.join("clone"))
            .unwrap()
            .flatten()
            .all(|e| e.file_name() == ".git"),
        "nothing was checked out"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn the_clone_keeps_one_copy_however_many_syncs() {
    if !has_git() {
        return;
    }
    let root =
        std::env::temp_dir().join(format!("tauminus-sync-git-copies-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let bare = root.join("remote.git");
    plain_git(&root, &["init", "--quiet", "--bare", "remote.git"]);
    let url = bare.to_string_lossy().into_owned();
    let git = Arc::new(Git::local(&url, root.join("clone"), None));
    let key = create(git.as_ref(), "correct horse", Kdf::LEAST).unwrap();
    let remote = Remote::new(git, key, root.join("state.json"));
    for n in 0..20 {
        remote
            .merge(data_with(&[("h1", &format!("label-{n}"))]))
            .await
            .unwrap();
        remote.adopt(SyncPart::Hosts).await.unwrap();
        remote.adopt(SyncPart::Snippets).await.unwrap();
        remote.publish().await.unwrap();
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(root.join("clone"))
        .args(["count-objects", "-v"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let loose: usize = text
        .lines()
        .find_map(|l| l.strip_prefix("count: "))
        .unwrap()
        .parse()
        .unwrap();
    // One commit's worth, give or take the last few: a commit, its
    // trees, the key and one object, loose or packed.
    let packed: usize = text
        .lines()
        .find_map(|l| l.strip_prefix("in-pack: "))
        .unwrap()
        .parse()
        .unwrap();
    assert!(loose + packed <= 40, "{text}");
    // However many syncs, the remote keeps at most so many states.
    let count = Command::new("git")
        .arg("-C")
        .arg(root.join("remote.git"))
        .args(["rev-list", "--count", BRANCH])
        .output()
        .unwrap();
    let count: u32 = String::from_utf8_lossy(&count.stdout)
        .trim()
        .parse()
        .unwrap();
    assert!((1..=8).contains(&count), "{count}");
    // The pushed commit stays, so the next refresh fetches nothing.
    let head = Command::new("git")
        .arg("-C")
        .arg(root.join("remote.git"))
        .args(["rev-parse", BRANCH])
        .output()
        .unwrap();
    let head = String::from_utf8_lossy(&head.stdout).trim().to_owned();
    let kept = Command::new("git")
        .arg("-C")
        .arg(root.join("clone"))
        .args(["cat-file", "-e", &head])
        .status()
        .unwrap();
    assert!(kept.success());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn git_runs_in_the_clone_whatever_the_environment_names() {
    let dir = std::env::temp_dir().join("tauminus-sync-env");
    let git = Git::new("git@example.com:me/sync.git", dir.clone()).unwrap();
    let command = git.command(&["status"]);
    let removed: Vec<_> = command
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_ALLOW_PROTOCOL",
    ] {
        assert!(removed.iter().any(|r| r == var), "{var}");
    }
    let args: Vec<_> = command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(args.contains(&format!("--git-dir={}", dir.join(".git").display())));
    assert!(args.contains(&format!("--work-tree={}", dir.display())));
    assert!(args.iter().all(|a| !a.contains("example.com")), "{args:?}");
}

#[test]
fn a_tip_whose_message_is_not_ours_starts_the_branch_over() {
    if !has_git() {
        return;
    }
    let root = std::env::temp_dir().join(format!("tauminus-sync-tip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    plain_git(&root, &["init", "--quiet", "--bare", "remote.git"]);
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    plain_git(&src, &["init", "--quiet", "-b", BRANCH]);
    std::fs::write(src.join("key"), b"k").unwrap();
    plain_git(&src, &["add", "-A"]);
    plain_git(
        &src,
        &["commit", "--quiet", "--no-verify", "-m", "sync 4294967295"],
    );
    let url = root.join("remote.git").to_string_lossy().into_owned();
    plain_git(&src, &["push", "--quiet", &url, BRANCH]);
    let git = Git::local(&url, root.join("clone"), None);
    git.refresh().unwrap();
    let name = crate::target::object_name("0123456789abcdef0123456789abcdef", 1);
    git.put_new(&name, b"x").unwrap();
    git.flush().unwrap();
    let log = Command::new("git")
        .arg("-C")
        .arg(root.join("remote.git"))
        .args(["log", "--format=%s", BRANCH])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "sync 0");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_branch_deleted_on_the_remote_starts_again() {
    if !has_git() {
        return;
    }
    let root = std::env::temp_dir().join(format!("tauminus-sync-deleted-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    plain_git(&root, &["init", "--quiet", "--bare", "remote.git"]);
    let url = root.join("remote.git").to_string_lossy().into_owned();
    let git = Git::local(&url, root.join("clone"), None);
    git.refresh().unwrap();
    let name = crate::target::object_name("0123456789abcdef0123456789abcdef", 1);
    git.put_new(&name, b"x").unwrap();
    git.flush().unwrap();
    plain_git(
        &root.join("remote.git"),
        &["update-ref", "-d", &format!("refs/heads/{BRANCH}")],
    );
    git.refresh().unwrap();
    let name = crate::target::object_name("0123456789abcdef0123456789abcdef", 2);
    git.put_new(&name, b"y").unwrap();
    git.flush().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}
