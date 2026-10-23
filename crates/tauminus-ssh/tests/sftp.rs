// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SFTP against a real OpenSSH server running `internal-sftp`. sshd runs
//! as the current user, so "remote" paths are on this machine too.

#![cfg(unix)]

mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use support::*;
use tauminus_app::ports::{
    Download, EntryKind, SftpError, SftpPort, SshConnection, SshConnector, TransferProgress, Upload,
};
use tauminus_domain::Route;
use tauminus_ssh::{AgentSource, HostKeyStore, RusshConnector};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

const MIB: usize = 1024 * 1024;

struct Setup {
    sshd: Sshd,
    scratch: Scratch,
    connector: RusshConnector,
    target: tauminus_domain::SshTarget,
    _connection: Arc<dyn SshConnection>,
    sftp: Arc<dyn SftpPort>,
}

impl Setup {
    /// SFTP on a new connection, as after a reconnect.
    async fn again(&self) -> (Arc<dyn SshConnection>, Arc<dyn SftpPort>) {
        let connection = self
            .connector
            .connection(
                &self.target,
                ScriptedPrompter::new(vec![]),
                Arc::default(),
                false,
            )
            .await
            .expect("logs in again");
        let sftp = connection.open_sftp().await.expect("sftp starts again");
        (connection, sftp)
    }
}

impl Setup {
    /// A path in the scratch directory, as a string for the server.
    fn remote(&self, name: &str) -> String {
        self.scratch.path(name).to_string_lossy().into_owned()
    }
}

async fn setup() -> Option<Setup> {
    setup_with("internal-sftp").await
}

async fn setup_with(subsystem: &str) -> Option<Setup> {
    let binary = sshd_binary()?;
    let scratch = Scratch::new("sftp");
    keygen(&scratch.0, &["-q", "-t", "ed25519", "-N", "", "-f", "id"]);
    let sshd = Sshd::start(
        &binary,
        &[&scratch.path("id.pub")],
        None,
        SshdOptions {
            extra: vec![format!("Subsystem sftp {subsystem}")],
            ..SshdOptions::default()
        },
    );
    std::fs::write(
        scratch.path("known_hosts"),
        format!("[127.0.0.1]:{} {}", sshd.port, sshd.host_public_key()),
    )
    .unwrap();
    let store = HostKeyStore::new(
        vec![scratch.path("known_hosts")],
        scratch.path("tauminus_known_hosts"),
    );
    let connector = RusshConnector::new(store, Some(scratch.0.clone()), AgentSource::None);
    let target = target(
        sshd.endpoint(),
        vec![],
        Route::Direct,
        key_auth(&[scratch.path("id")]),
    );
    let connection = connector
        .connection(
            &target,
            ScriptedPrompter::new(vec![]),
            Arc::default(),
            false,
        )
        .await
        .expect("logs in");
    let sftp = connection.open_sftp().await.expect("sftp starts");
    Some(Setup {
        sshd,
        scratch,
        connector,
        target,
        _connection: connection,
        sftp,
    })
}

/// Writes `size` bytes no two MiB of which are alike, so a block copied to
/// the wrong place changes the checksum.
async fn write_pattern(path: &Path, size: usize) {
    let mut file = tokio::io::BufWriter::new(tokio::fs::File::create(path).await.unwrap());
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut block = vec![0_u8; MIB];
    let mut left = size;
    while left > 0 {
        for chunk in block.as_chunks_mut::<8>().0 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            chunk.copy_from_slice(&state.to_le_bytes());
        }
        let n = left.min(MIB);
        file.write_all(&block[..n]).await.unwrap();
        left -= n;
    }
    file.flush().await.unwrap();
}

async fn sha256(path: &Path) -> Vec<u8> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut file = std::fs::File::open(path).unwrap();
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; MIB];
        loop {
            let read = file.read(&mut buffer).unwrap();
            if read == 0 {
                break hasher.finalize().to_vec();
            }
            hasher.update(&buffer[..read]);
        }
    })
    .await
    .unwrap()
}

fn progress() -> (
    watch::Sender<TransferProgress>,
    watch::Receiver<TransferProgress>,
) {
    watch::channel(TransferProgress::default())
}

fn never() -> watch::Receiver<()> {
    let (stop, receiver) = watch::channel(());
    std::mem::forget(stop);
    receiver
}

fn download(setup: &Setup, remote: &str, local: &str) -> Download {
    Download {
        remote: setup.remote(remote),
        local: setup.scratch.path(local),
        host: "test".into(),
        overwrite: true,
        resume: true,
        max: None,
    }
}

fn upload(setup: &Setup, local: &str, remote: &str) -> Upload {
    Upload {
        local: setup.scratch.path(local),
        remote: setup.remote(remote),
        host: "test".into(),
        overwrite: true,
        state: setup.scratch.path("state"),
        resume: true,
    }
}

#[tokio::test]
async fn files_are_listed_made_moved_and_changed() {
    let Some(setup) = setup().await else { return };
    let dir = setup.remote("tree");
    setup.sftp.mkdir(&dir).await.unwrap();
    std::fs::write(setup.scratch.path("tree/a.txt"), b"hello").unwrap();
    std::os::unix::fs::symlink("a.txt", setup.scratch.path("tree/link")).unwrap();
    let mut entries = setup.sftp.list(&dir).await.unwrap();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let summary: Vec<_> = entries.iter().map(|e| (e.name.as_str(), e.kind)).collect();
    assert_eq!(
        summary,
        [("a.txt", EntryKind::File), ("link", EntryKind::Link)]
    );
    assert_eq!(entries[0].size, 5);

    let a = format!("{dir}/a.txt");
    let b = format!("{dir}/b.txt");
    setup.sftp.chmod(&a, 0o640).await.unwrap();
    assert_eq!(setup.sftp.stat(&a).await.unwrap().mode, Some(0o640));
    // A rename never replaces what is there.
    std::fs::write(setup.scratch.path("tree/b.txt"), b"old").unwrap();
    assert!(setup.sftp.rename(&a, &b).await.is_err());
    assert_eq!(
        std::fs::read(setup.scratch.path("tree/b.txt")).unwrap(),
        b"old"
    );
    std::fs::remove_file(setup.scratch.path("tree/b.txt")).unwrap();
    setup.sftp.rename(&a, &b).await.unwrap();
    assert_eq!(
        std::fs::read(setup.scratch.path("tree/b.txt")).unwrap(),
        b"hello"
    );
    setup.sftp.remove_file(&b).await.unwrap();
    setup
        .sftp
        .remove_file(&format!("{dir}/link"))
        .await
        .unwrap();
    setup.sftp.remove_dir(&dir).await.unwrap();
    assert!(setup.sftp.stat(&dir).await.is_err());
    let home = setup.sftp.canonicalize(".").await.unwrap();
    assert!(home.starts_with('/'), "{home}");
    let missing = setup.sftp.list(&setup.remote("nothing")).await;
    assert!(matches!(missing, Err(SftpError::Server(_))), "{missing:?}");
}

/// A release-build CI step runs this; in a debug build it takes minutes.
#[tokio::test]
#[ignore = "1 GiB; CI runs it in release with --ignored"]
async fn a_gibibyte_goes_up_and_comes_back_unchanged() {
    let Some(setup) = setup().await else { return };
    let size = 1024 * MIB;
    write_pattern(&setup.scratch.path("source"), size).await;
    let (tx, rx) = progress();
    let sent = setup
        .sftp
        .upload(&upload(&setup, "source", "uploaded"), tx, never())
        .await
        .unwrap();
    assert_eq!(sent, size as u64);
    assert_eq!(rx.borrow().done, size as u64);
    let (tx, _) = progress();
    let got = setup
        .sftp
        .download(&download(&setup, "uploaded", "downloaded"), tx, never())
        .await
        .unwrap();
    assert_eq!(got, size as u64);
    let expected = sha256(&setup.scratch.path("source")).await;
    assert_eq!(sha256(&setup.scratch.path("uploaded")).await, expected);
    assert_eq!(sha256(&setup.scratch.path("downloaded")).await, expected);
    // Nothing is left behind.
    assert!(!setup.scratch.path("downloaded.tauminus-part").exists());
    assert!(!setup.scratch.path("uploaded.tauminus-part").exists());
}

/// Starts a transfer and cancels it once `at_least` bytes are in place.
async fn interrupted<F, Fut>(at_least: u64, run: F)
where
    F: FnOnce(watch::Sender<TransferProgress>, watch::Receiver<()>) -> Fut,
    Fut: Future<Output = Result<u64, SftpError>>,
{
    let (tx, mut rx) = progress();
    let (stop, cancel) = watch::channel(());
    let watcher = tokio::spawn(async move {
        let _ = rx.wait_for(|p| p.done >= at_least).await;
        stop.send_replace(());
        // Keep the sender until the transfer saw it.
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let result = run(tx, cancel).await;
    assert!(matches!(result, Err(SftpError::Cancelled)), "{result:?}");
    watcher.abort();
}

#[tokio::test]
async fn an_interrupted_download_resumes_where_it_stopped() {
    let Some(setup) = setup().await else { return };
    let size = 64 * MIB;
    write_pattern(&setup.scratch.path("remote"), size).await;
    let job = download(&setup, "remote", "local");
    interrupted(16 * MIB as u64, |tx, cancel| {
        setup.sftp.download(&job, tx, cancel)
    })
    .await;
    assert!(setup.scratch.path("local.tauminus-part").exists());
    assert!(!setup.scratch.path("local").exists());
    let (tx, rx) = progress();
    let moved = setup.sftp.download(&job, tx, never()).await.unwrap();
    let resumed = rx.borrow().resumed_from;
    assert!(resumed >= 16 * MIB as u64, "resumed from {resumed}");
    assert_eq!(moved + resumed, size as u64);
    assert_eq!(
        sha256(&setup.scratch.path("local")).await,
        sha256(&setup.scratch.path("remote")).await
    );
}

#[tokio::test]
async fn an_interrupted_upload_resumes_where_it_stopped() {
    let Some(setup) = setup().await else { return };
    let size = 64 * MIB;
    write_pattern(&setup.scratch.path("local"), size).await;
    let job = upload(&setup, "local", "remote");
    interrupted(16 * MIB as u64, |tx, cancel| {
        setup.sftp.upload(&job, tx, cancel)
    })
    .await;
    assert!(
        !setup.scratch.path("remote").exists(),
        "not in place until whole"
    );
    let (tx, rx) = progress();
    let moved = setup.sftp.upload(&job, tx, never()).await.unwrap();
    let resumed = rx.borrow().resumed_from;
    assert!(resumed > 0, "resumed from {resumed}");
    assert_eq!(moved + resumed, size as u64);
    assert_eq!(
        sha256(&setup.scratch.path("remote")).await,
        sha256(&setup.scratch.path("local")).await
    );
}

#[tokio::test]
async fn a_source_changed_since_the_partial_copy_starts_over() {
    let Some(setup) = setup().await else { return };
    write_pattern(&setup.scratch.path("remote"), 32 * MIB).await;
    let job = download(&setup, "remote", "local");
    interrupted(8 * MIB as u64, |tx, cancel| {
        setup.sftp.download(&job, tx, cancel)
    })
    .await;
    // The file on the server is now a different one of the same size.
    let mut bytes = std::fs::read(setup.scratch.path("remote")).unwrap();
    bytes[0] ^= 0xff;
    std::fs::write(setup.scratch.path("remote"), &bytes).unwrap();
    let modified = std::time::SystemTime::now() + Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(setup.scratch.path("remote"))
        .unwrap()
        .set_modified(modified)
        .unwrap();
    let (tx, rx) = progress();
    setup.sftp.download(&job, tx, never()).await.unwrap();
    assert_eq!(rx.borrow().resumed_from, 0);
    assert_eq!(std::fs::read(setup.scratch.path("local")).unwrap(), bytes);
}

#[tokio::test]
async fn a_link_planted_where_the_partial_copy_goes_is_not_followed() {
    let Some(setup) = setup().await else { return };
    std::fs::write(setup.scratch.path("remote"), b"from the server").unwrap();
    std::fs::write(setup.scratch.path("victim"), b"keep me").unwrap();
    std::os::unix::fs::symlink(
        setup.scratch.path("victim"),
        setup.scratch.path("local.tauminus-part"),
    )
    .unwrap();
    let (tx, _) = progress();
    setup
        .sftp
        .download(&download(&setup, "remote", "local"), tx, never())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(setup.scratch.path("victim")).unwrap(),
        b"keep me"
    );
    assert_eq!(
        std::fs::read(setup.scratch.path("local")).unwrap(),
        b"from the server"
    );
}

#[tokio::test]
async fn an_existing_file_is_kept_unless_asked() {
    let Some(setup) = setup().await else { return };
    std::fs::write(setup.scratch.path("remote"), b"new").unwrap();
    std::fs::write(setup.scratch.path("local"), b"mine").unwrap();
    let mut job = download(&setup, "remote", "local");
    job.overwrite = false;
    let (tx, _) = progress();
    let result = setup.sftp.download(&job, tx, never()).await;
    assert!(matches!(result, Err(SftpError::Exists(_))), "{result:?}");
    assert_eq!(std::fs::read(setup.scratch.path("local")).unwrap(), b"mine");
    let mut job = upload(&setup, "local", "remote");
    job.overwrite = false;
    let (tx, _) = progress();
    let result = setup.sftp.upload(&job, tx, never()).await;
    assert!(matches!(result, Err(SftpError::Exists(_))), "{result:?}");
    // An empty file goes both ways too.
    std::fs::write(setup.scratch.path("empty"), b"").unwrap();
    let (tx, _) = progress();
    setup
        .sftp
        .upload(&upload(&setup, "empty", "empty-remote"), tx, never())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(setup.scratch.path("empty-remote")).unwrap(),
        b""
    );
}

#[tokio::test]
async fn a_download_cut_off_by_a_dropped_connection_resumes_on_the_next_one() {
    let Some(setup) = setup().await else { return };
    let size = 64 * MIB;
    write_pattern(&setup.scratch.path("remote"), size).await;
    let job = download(&setup, "remote", "local");
    let (tx, mut rx) = progress();
    let killer = async {
        let _ = rx.wait_for(|p| p.done >= 16 * MIB as u64).await;
        setup.sshd.kill_sessions();
    };
    let (result, ()) = tokio::join!(setup.sftp.download(&job, tx, never()), killer);
    assert!(matches!(result, Err(SftpError::Closed)), "{result:?}");
    let (_connection, sftp) = setup.again().await;
    let (tx, rx) = progress();
    let moved = sftp.download(&job, tx, never()).await.unwrap();
    let resumed = rx.borrow().resumed_from;
    assert!(resumed >= 16 * MIB as u64, "resumed from {resumed}");
    assert_eq!(moved + resumed, size as u64);
    assert_eq!(
        sha256(&setup.scratch.path("local")).await,
        sha256(&setup.scratch.path("remote")).await
    );
}

#[tokio::test]
async fn an_upload_replaces_a_link_at_the_target_and_never_writes_through_it() {
    let Some(setup) = setup().await else { return };
    std::fs::write(setup.scratch.path("victim"), b"keep me").unwrap();
    std::os::unix::fs::symlink(setup.scratch.path("victim"), setup.scratch.path("target")).unwrap();
    std::fs::write(setup.scratch.path("local"), b"new content").unwrap();
    let (tx, _) = progress();
    setup
        .sftp
        .upload(&upload(&setup, "local", "target"), tx, never())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(setup.scratch.path("victim")).unwrap(),
        b"keep me"
    );
    let target = std::fs::symlink_metadata(setup.scratch.path("target")).unwrap();
    assert!(target.is_file(), "the link was replaced by the file");
    assert_eq!(
        std::fs::read(setup.scratch.path("target")).unwrap(),
        b"new content"
    );
}

#[tokio::test]
async fn an_upload_keeps_the_targets_mode_without_setuid() {
    use std::os::unix::fs::PermissionsExt;
    let Some(setup) = setup().await else { return };
    std::fs::write(setup.scratch.path("tool"), b"old").unwrap();
    std::fs::set_permissions(
        setup.scratch.path("tool"),
        std::fs::Permissions::from_mode(0o4750),
    )
    .unwrap();
    std::fs::write(setup.scratch.path("local"), b"new").unwrap();
    let (tx, _) = progress();
    setup
        .sftp
        .upload(&upload(&setup, "local", "tool"), tx, never())
        .await
        .unwrap();
    let mode = std::fs::metadata(setup.scratch.path("tool"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o750);
    // No partial copy is left beside it.
    let leftovers: Vec<_> = std::fs::read_dir(&setup.scratch.0)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains("tauminus-part"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[tokio::test]
async fn a_source_that_changes_during_the_copy_is_reported_not_kept() {
    let Some(setup) = setup().await else { return };
    write_pattern(&setup.scratch.path("remote"), 64 * MIB).await;
    let job = download(&setup, "remote", "local");
    let (tx, mut rx) = progress();
    let remote = setup.scratch.path("remote");
    let grow = async {
        let _ = rx.wait_for(|p| p.done >= 16 * MIB as u64).await;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&remote)
            .unwrap();
        std::io::Write::write_all(&mut file, b"more").unwrap();
    };
    let (result, ()) = tokio::join!(setup.sftp.download(&job, tx, never()), grow);
    assert!(matches!(result, Err(SftpError::Changed)), "{result:?}");
    assert!(
        !setup.scratch.path("local").exists(),
        "nothing mixed is kept"
    );
}

#[tokio::test]
async fn a_partial_copy_whose_tail_differs_starts_over() {
    let Some(setup) = setup().await else { return };
    write_pattern(&setup.scratch.path("remote"), 32 * MIB).await;
    let job = download(&setup, "remote", "local");
    interrupted(8 * MIB as u64, |tx, cancel| {
        setup.sftp.download(&job, tx, cancel)
    })
    .await;
    // The record still matches, but the bytes before the resume point do
    // not: someone wrote into the partial copy.
    let part = setup.scratch.path("local.tauminus-part");
    let mut bytes = std::fs::read(&part).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&part, &bytes).unwrap();
    let (tx, rx) = progress();
    setup.sftp.download(&job, tx, never()).await.unwrap();
    assert_eq!(rx.borrow().resumed_from, 0);
    assert_eq!(
        sha256(&setup.scratch.path("local")).await,
        sha256(&setup.scratch.path("remote")).await
    );
}

#[tokio::test]
async fn a_link_planted_at_a_guessed_partial_name_is_never_written() {
    let Some(setup) = setup().await else { return };
    std::fs::write(setup.scratch.path("victim"), b"keep me").unwrap();
    std::os::unix::fs::symlink(
        setup.scratch.path("victim"),
        setup.scratch.path("target.tauminus-part"),
    )
    .unwrap();
    std::fs::write(setup.scratch.path("local"), b"uploaded").unwrap();
    let (tx, _) = progress();
    setup
        .sftp
        .upload(&upload(&setup, "local", "target"), tx, never())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(setup.scratch.path("victim")).unwrap(),
        b"keep me"
    );
    assert_eq!(
        std::fs::read(setup.scratch.path("target")).unwrap(),
        b"uploaded"
    );
}

#[tokio::test]
async fn a_new_file_keeps_the_local_files_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let Some(setup) = setup().await else { return };
    std::fs::write(setup.scratch.path("secret"), b"token").unwrap();
    std::fs::set_permissions(
        setup.scratch.path("secret"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let (tx, _) = progress();
    setup
        .sftp
        .upload(&upload(&setup, "secret", "uploaded-secret"), tx, never())
        .await
        .unwrap();
    let mode = std::fs::metadata(setup.scratch.path("uploaded-secret"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "never wider than the source");
}

#[tokio::test]
async fn an_upload_lands_where_the_server_refuses_to_set_modes() {
    let Some(setup) = setup_with("internal-sftp -P fsetstat,setstat").await else {
        return;
    };
    std::fs::write(setup.scratch.path("plain"), b"content").unwrap();
    let (tx, _) = progress();
    setup
        .sftp
        .upload(&upload(&setup, "plain", "landed"), tx, never())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(setup.scratch.path("landed")).unwrap(),
        b"content"
    );
    let left: Vec<_> = std::fs::read_dir(&setup.scratch.0)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".tauminus-part"))
        .collect();
    assert!(left.is_empty(), "no partial file left: {left:?}");
}

#[tokio::test]
async fn two_downloads_to_one_file_never_finish_with_the_other_ones_bytes() {
    let Some(setup) = setup().await else { return };
    let big: Vec<u8> = (0..16 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    let small = vec![7u8; 256 * 1024 * 1024];
    std::fs::write(setup.scratch.path("big"), &big).unwrap();
    std::fs::write(setup.scratch.path("small"), &small).unwrap();
    std::fs::create_dir(setup.scratch.path("d")).unwrap();
    std::os::unix::fs::symlink(setup.scratch.path("d"), setup.scratch.path("d2")).unwrap();
    let first = download(&setup, "big", "d/x");
    let second = download(&setup, "small", "d2/x");
    let (tx1, mut rx1) = progress();
    let (tx2, _) = progress();
    let sftp = Arc::clone(&setup.sftp);
    let one = tokio::spawn(async move { sftp.download(&first, tx1, never()).await });
    rx1.wait_for(|p| p.done >= 2 * 1024 * 1024).await.unwrap();
    let two = setup.sftp.download(&second, tx2, never()).await;
    let one = one.await.unwrap();
    let landed = std::fs::read(setup.scratch.path("d/x")).unwrap_or_default();
    // Whichever finished Ok, the file holds that one's bytes.
    match (one, two) {
        (Ok(_), Ok(_)) => assert!(landed == small || landed == big),
        (Ok(_), Err(_)) => assert!(landed == big, "the first said Done"),
        (Err(_), Ok(_)) => assert!(landed == small, "the second said Done"),
        (Err(a), Err(b)) => panic!("both failed: {a} / {b}"),
    }
}
