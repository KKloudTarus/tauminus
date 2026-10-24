// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

/// As MobaXterm 23 writes it: the root, then a folder two levels deep.
const INI: &str = "[Misc]\r\n\
Hostname=ignored%a%b\r\n\
\r\n\
[Bookmarks]\r\n\
SubRep=\r\n\
ImgNum=42\r\n\
web= #109#0%web.internal%22%deploy%%-1%-1%%bastion.internal%2222%ops%0%0%0%_ProfileDir_\\.ssh\\id_web%%-1%0%0%0%%1080%%0%0%1#Courier New%10%0%0%-1%15%236,236,236%30,30,30%180,180,192%0%-1%0%%xterm%-1%0%_Std_Colors_0_%80%24%0%1%-1%<none>%%0%0%-1#0# #-1\r\n\
desk= #91#4%desk.internal%3389%%-1%-1%%%%#MobaFont%10#0# #-1\r\n\
\r\n\
[Bookmarks_1]\r\n\
SubRep=Prod\\EU\r\n\
ImgNum=41\r\n\
db__PTVIRG__primary= #109#0%10.0.0.7%22%<default>%%-1%-1%%a.internal__PIPE__b.internal%22__PIPE__2200%u1__PIPE__<default>%0%0%0%_CurrentDrive_:\\keys\\db.ppk%%-1%0%0%0%%1080%%0%0%1#Courier New%10#0# #-1\r\n\
old= #109#0%old.internal%22%root\r\n\
socks= #109#0%far.internal%22%u%%-1%-1%%%%%0%0%0%%%-1%0%0%2%proxy%1080%%0%0%1#Courier New#0# #-1\r\n";

#[test]
fn bookmarks_read_with_their_folders_jump_hosts_and_keys() {
    let hosts = bookmarks(INI, Path::new("/home/me"));
    let names: Vec<&str> = hosts.iter().map(|h| h.alias.as_str()).collect();
    assert_eq!(names, ["web", "desk", "db;primary", "old", "socks"]);

    let web = &hosts[0];
    assert_eq!(web.hostname, "web.internal");
    assert_eq!(web.port, Some(22));
    assert_eq!(web.user.as_deref(), Some("deploy"));
    assert_eq!(web.proxy_jump.as_deref(), Some("ops@bastion.internal:2222"));
    assert_eq!(
        web.identity_files,
        [PathBuf::from("/home/me\\.ssh\\id_web")]
    );
    assert_eq!(web.folder, Vec::<String>::new());
    assert_eq!(web.problem, None);

    assert_eq!(hosts[1].problem.as_deref(), Some("a RDP session, not SSH"));

    let db = &hosts[2];
    assert_eq!(db.folder, ["Prod", "EU"]);
    assert_eq!(db.user, None, "<default> is no user");
    assert_eq!(
        db.proxy_jump.as_deref(),
        Some("u1@a.internal,b.internal:2200"),
        "a chain of two hops"
    );
    assert_eq!(
        db.identity_files,
        Vec::<PathBuf>::new(),
        "a PuTTY key is left to Pageant"
    );

    // A line an old version wrote, cut short after the user.
    let old = &hosts[3];
    assert_eq!(
        (old.hostname.as_str(), old.user.as_deref()),
        ("old.internal", Some("root"))
    );
    assert_eq!(old.problem, None);

    assert_eq!(
        hosts[4].problem.as_deref(),
        Some("goes through a SOCKS 5 proxy, which is not imported")
    );
}

#[test]
fn a_file_with_no_bookmarks_is_no_hosts() {
    assert_eq!(
        bookmarks("[Misc]\nx=#1#0%a%22\n", Path::new("/h")),
        Vec::<ConfigHost>::new()
    );
    assert_eq!(bookmarks("", Path::new("/h")), Vec::<ConfigHost>::new());
}

#[test]
fn a_deep_folder_is_cut_and_a_bad_port_reported() {
    let deep = format!(
        "[Bookmarks_1]\nSubRep={}\na= #109#0%a.internal%22%u\nb= #109#0%b.internal%99999%u\n",
        vec!["x"; 10_000].join("\\")
    );
    let hosts = bookmarks(&deep, Path::new("/h"));
    assert_eq!(hosts[0].folder.len(), 15);
    assert_eq!(
        hosts[1].problem.as_deref(),
        Some("the port 99999 is not a number")
    );
}
