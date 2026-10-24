// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

fn file(folder: &[&str], name: &str, text: &str) -> File {
    File {
        folder: folder.iter().map(|f| (*f).to_owned()).collect(),
        name: name.to_owned(),
        text: text.to_owned(),
    }
}

const WEB: &str = "S:\"Protocol Name\"=SSH2\r\n\
S:\"Hostname\"=web.internal\r\n\
S:\"Username\"=deploy\r\n\
D:\"[SSH2] Port\"=00000016\r\n\
D:\"Port\"=00000017\r\n\
D:\"Use Global Public Key\"=00000000\r\n\
S:\"Identity Filename V2\"=${VDS_CONFIG_PATH}/keys/web::rawkey\r\n\
S:\"Firewall Name\"=Session:Prod/bastion\r\n\
S:\"Password V2\"=02:abcdef\r\n\
B:\"Keyword Set\"=00000002\r\n 00 00\r\n\
Z:\"Port Forward Table V2\"=00000001\r\n some line\r\n\
D:\"Is Session\"=00000001\r\n";

const BASTION: &str = "S:\"Protocol Name\"=SSH2\nS:\"Hostname\"=bastion.internal\n\
S:\"Username\"=ops\nD:\"[SSH2] Port\"=000008ae\nS:\"Firewall Name\"=None\n";

#[test]
fn a_session_tree_reads_with_folders_keys_and_session_jumps() {
    let hosts = sessions(
        [
            file(&["Prod", "EU"], "web.ini", WEB),
            file(&["Prod"], "bastion.ini", BASTION),
            file(&[], "__FolderData__.ini", "S:\"Folder List\"=Prod:\n"),
            file(&[], "Default.ini", "S:\"Hostname\"=x\n"),
            file(
                &[],
                "router.ini",
                "S:\"Protocol Name\"=Telnet\nS:\"Hostname\"=r\nD:\"Port\"=00000017\n",
            ),
            file(
                &[],
                "fw.ini",
                "S:\"Hostname\"=fw.internal\nS:\"Firewall Name\"=Office\n",
            ),
            file(
                &[],
                "lost.ini",
                "S:\"Hostname\"=l.internal\nS:\"Firewall Name\"=Session:gone\n",
            ),
            file(&[], "notes.txt", "S:\"Hostname\"=no\n"),
        ],
        Path::new("/home/me/.vandyke/SecureCRT/Config"),
    );
    let names: Vec<&str> = hosts.iter().map(|h| h.alias.as_str()).collect();
    assert_eq!(names, ["web", "bastion", "router", "fw", "lost"]);
    let web = &hosts[0];
    assert_eq!(web.hostname, "web.internal");
    assert_eq!(web.port, Some(22), "the SSH2 port, not the Telnet one");
    assert_eq!(web.user.as_deref(), Some("deploy"));
    assert_eq!(web.folder, ["Prod", "EU"]);
    assert_eq!(
        web.identity_files,
        [PathBuf::from("/home/me/.vandyke/SecureCRT/Config/keys/web")]
    );
    assert_eq!(web.proxy_jump.as_deref(), Some("ops@bastion.internal:2222"));
    assert_eq!(web.problem, None);
    assert!(!format!("{hosts:?}").contains("abcdef"), "no password read");
    assert_eq!(hosts[1].folder, ["Prod"]);
    assert_eq!(
        hosts[2].problem.as_deref(),
        Some("a Telnet session, not SSH2")
    );
    assert_eq!(
        hosts[3].problem.as_deref(),
        Some("goes through the firewall Office, which is not imported")
    );
    assert_eq!(
        hosts[4].problem.as_deref(),
        Some("jumps through the session gone, which is not here")
    );
}

#[test]
fn the_global_key_leaves_the_session_s_own_unused() {
    let hosts = sessions(
        [file(
            &[],
            "a.ini",
            "S:\"Hostname\"=a\nD:\"Use Global Public Key\"=00000001\nS:\"Identity Filename V2\"=/k\n",
        )],
        Path::new("/c"),
    );
    assert_eq!(hosts[0].identity_files, Vec::<PathBuf>::new());
}

fn jump(name: &str, host: &str, firewall: &str) -> File {
    file(
        &[],
        &format!("{name}.ini"),
        &format!("S:\"Hostname\"={host}\nS:\"Firewall Name\"={firewall}\n"),
    )
}

#[test]
fn a_chain_of_session_jumps_is_followed_and_its_faults_named() {
    let hosts = sessions(
        [
            jump("a", "a.internal", "Session:b"),
            jump("b", "b.internal", "Session:c"),
            jump("c", "c.internal", "None"),
            jump("loop1", "l1", "Session:loop2"),
            jump("loop2", "l2", "Session:loop1"),
            jump("me", "me.internal", "Session:me"),
            jump("t", "t.internal", "Session:tel"),
            file(
                &[],
                "tel.ini",
                "S:\"Protocol Name\"=Telnet\nS:\"Hostname\"=tel\n",
            ),
        ],
        Path::new("/c"),
    );
    let by = |name: &str| hosts.iter().find(|h| h.alias == name).unwrap();
    assert_eq!(by("a").proxy_jump.as_deref(), Some("c.internal,b.internal"));
    assert_eq!(by("a").problem, None);
    assert_eq!(
        by("loop1").problem.as_deref(),
        Some("jumps through the session loop1 in a loop")
    );
    assert_eq!(
        by("me").problem.as_deref(),
        Some("jumps through the session me in a loop")
    );
    assert_eq!(
        by("t").problem.as_deref(),
        Some("jumps through the session tel (a Telnet session, not SSH2)")
    );
}

#[test]
fn a_key_named_by_its_public_half_points_at_the_private_one() {
    let hosts = sessions(
        [file(
            &[],
            "k.ini",
            "S:\"Hostname\"=k\nD:\"Use Global Public Key\"=00000000\nS:\"Identity Filename V2\"=/keys/Identity.pub::rawkey\n",
        )],
        Path::new("/c"),
    );
    assert_eq!(hosts[0].identity_files, [PathBuf::from("/keys/Identity")]);
}

#[test]
fn a_long_chain_is_named_as_long_and_backslash_paths_resolve() {
    let mut files: Vec<File> = (0..11)
        .map(|i| {
            jump(
                &format!("s{i}"),
                &format!("s{i}.internal"),
                &format!("Session:s{}", i + 1),
            )
        })
        .collect();
    files.push(jump("s11", "s11.internal", "None"));
    files.push(file(&["Lab"], "bastion.ini", "S:\"Hostname\"=b.internal\n"));
    files.push(jump("w", "w.internal", "Session:Lab\\bastion"));
    let hosts = sessions(files, Path::new("/c"));
    let by = |name: &str| hosts.iter().find(|h| h.alias == name).unwrap();
    assert_eq!(
        by("s0").problem.as_deref(),
        Some("jumps through more than 8 sessions")
    );
    assert_eq!(by("s3").problem, None, "eight hops is the limit");
    assert_eq!(by("w").proxy_jump.as_deref(), Some("b.internal"));
}
