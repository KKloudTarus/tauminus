// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt::Write as _;

use super::*;

#[test]
fn a_termius_style_list_reads_with_its_groups() {
    let text = "Groups,Label,Tags,Hostname/IP,Protocol,Port,Username,Password\r\n\
                prod/eu,web,\"a,b\",web.internal,ssh,2222,deploy,secret\r\n\
                ,lab,,10.0.0.9,,,,\r\n";
    let hosts = hosts(text);
    assert_eq!(hosts.len(), 2);
    let web = &hosts[0];
    assert_eq!(web.alias, "web");
    assert_eq!(web.hostname, "web.internal");
    assert_eq!(web.port, Some(2222));
    assert_eq!(web.user.as_deref(), Some("deploy"));
    assert_eq!(web.folder, ["prod", "eu"]);
    assert_eq!(web.problem, None);
    let lab = &hosts[1];
    assert_eq!(
        (lab.alias.as_str(), lab.hostname.as_str()),
        ("lab", "10.0.0.9")
    );
    assert_eq!(lab.folder, Vec::<String>::new());
    // The password column is never read into the host.
    assert!(!format!("{hosts:?}").contains("secret"));
}

#[test]
fn quotes_separators_and_line_breaks_follow_rfc_4180() {
    let text = "name;host;user\n\"a \"\"quoted\"\" name\";\"h;1\";\"line\nbreak\"\n";
    let hosts = hosts(text);
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].alias, "a \"quoted\" name");
    assert_eq!(hosts[0].hostname, "h;1");
    assert_eq!(hosts[0].user.as_deref(), Some("line\nbreak"));
}

#[test]
fn tabs_a_bom_and_a_missing_label_are_taken() {
    let hosts = hosts("\u{feff}Host\tPort\ndb.internal\t22\n");
    assert_eq!(hosts[0].alias, "db.internal", "the host names it");
    assert_eq!(hosts[0].port, Some(22));
}

#[test]
fn other_protocols_bad_ports_and_empty_hosts_are_reported() {
    let hosts = hosts(
        "Name,Host,Port,Protocol\n\
         desk,desk.internal,3389,RDP\n\
         odd,odd.internal,ssh,SSH2\n\
         blank,,22,ssh\n\
         ,,,\n",
    );
    assert_eq!(hosts.len(), 3, "the empty row is dropped");
    assert_eq!(hosts[0].problem.as_deref(), Some("a RDP session, not SSH"));
    assert_eq!(
        hosts[1].problem.as_deref(),
        Some("the port ssh is not a number")
    );
    assert_eq!(hosts[2].problem.as_deref(), Some("no host name is set"));
}

#[test]
fn mremoteng_containers_become_folders() {
    let text = "Name;Id;Parent;NodeType;Username;Hostname;Port;Protocol\n\
                Prod;c1;;Container;;;;\n\
                EU;c2;c1;Container;;;;\n\
                web;h1;c2;Connection;deploy;web.internal;22;SSH2\n\
                top;h2;;Connection;;top.internal;22;SSH2\n\
                looped;h3;c9;Connection;;l.internal;22;SSH2\n\
                C9;c9;c9;Container;;;;\n";
    let hosts = hosts(text);
    assert_eq!(hosts.len(), 3, "containers are not hosts");
    assert_eq!(hosts[0].folder, ["Prod", "EU"]);
    assert_eq!(hosts[1].folder, Vec::<String>::new());
    // A container that is its own parent stops instead of looping.
    assert!(hosts[2].folder.len() <= 3);
}

#[test]
fn a_header_alone_or_nothing_is_no_hosts() {
    assert_eq!(hosts(""), Vec::<ConfigHost>::new());
    assert_eq!(hosts("name,host\n"), Vec::<ConfigHost>::new());
}

#[test]
fn bare_cr_lines_a_leading_blank_and_the_most_used_separator_read() {
    let mac = hosts("name,host\rweb,web.internal\rdb,db.internal\r");
    assert_eq!(mac.len(), 2);
    assert_eq!(mac[1].hostname, "db.internal");
    let blank = hosts("\n\nname,host\nweb,web.internal\n");
    assert_eq!(blank[0].hostname, "web.internal");
    // A comma inside a header name does not make it the separator.
    let tabs = hosts("Name\tHost\tNotes, misc\nweb\tweb.internal\tx, y\n");
    assert_eq!(tabs[0].hostname, "web.internal");
}

#[test]
fn a_deep_mremoteng_chain_stops_at_fifteen_folders() {
    let mut text = String::from("Name;Id;Parent;NodeType;Hostname\n");
    for i in 0..100 {
        let parent = if i == 0 {
            String::new()
        } else {
            format!("c{}", i - 1)
        };
        let _ = writeln!(text, "C{i};c{i};{parent};Container;");
    }
    text.push_str("web;h;c99;Connection;web.internal\n");
    let hosts = hosts(&text);
    assert_eq!(hosts[0].folder.len(), 15);
    assert_eq!(
        hosts[0].folder.last().map(String::as_str),
        Some("C99"),
        "the innermost kept"
    );
}

#[test]
fn only_the_first_256_fields_of_a_row_are_read() {
    let header = format!("name,host{}", ",x".repeat(10_000));
    let hosts = hosts(&format!("{header}\nweb,web.internal\n"));
    assert_eq!(hosts[0].hostname, "web.internal");
}

#[test]
fn long_container_names_and_folder_paths_are_cut() {
    let long = "n".repeat(100_000);
    let text =
        format!("Name;Id;Parent;NodeType;Hostname\n{long};c1;;Container;\nweb;h;c1;Connection;w\n");
    let found = hosts(&text);
    assert_eq!(found[0].folder[0].chars().count(), 128);
    let deep = format!(
        "name,host,group\nweb,w,{}\n",
        vec!["g".repeat(100); 15].join("/")
    );
    let found = hosts(&deep);
    let total: usize = found[0].folder.iter().map(|f| f.chars().count()).sum();
    assert!(total <= 512, "{total}");
    assert_eq!(found[0].folder.len(), 5);
}

#[test]
fn bidi_controls_leave_imported_names() {
    let found = hosts("name,host,group\nab\u{202e}cd,w,f\u{2066}x\n");
    assert_eq!(found[0].alias, "abcd");
    assert_eq!(found[0].folder, ["fx"]);
}

#[test]
fn a_long_alias_is_kept_whole_for_the_label_check() {
    let long = "a".repeat(130);
    let found = hosts(&format!("name,host\n{long}-one,one\n{long}-two,two\n"));
    assert_ne!(found[0].alias, found[1].alias);
}
