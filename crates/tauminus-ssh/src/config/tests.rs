// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::{AtomicUsize, Ordering};

use super::parse::{Item, Line, aliases, read, tokenize};
use super::*;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("tauminus-sshcfg-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn line(keyword: &str, args: &[&str]) -> Line {
    Line {
        keyword: keyword.into(),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        rest: args.join(" "),
    }
}

#[test]
fn lines_split_on_space_or_equals_and_honour_quotes() {
    // Keyword and arguments only; `rest` is checked on its own below.
    let parsed = |raw: &str| {
        tokenize(raw).map(|l| {
            line(
                &l.keyword,
                &l.args.iter().map(String::as_str).collect::<Vec<_>>(),
            )
        })
    };
    assert_eq!(
        parsed("  HostName db.internal "),
        Some(line("hostname", &["db.internal"]))
    );
    assert_eq!(parsed("Port=2222"), Some(line("port", &["2222"])));
    assert_eq!(parsed("Port = 2222"), Some(line("port", &["2222"])));
    assert_eq!(
        parsed("IdentityFile \"~/.ssh/my key\" other"),
        Some(line("identityfile", &["~/.ssh/my key", "other"]))
    );
    assert_eq!(parsed("Host\ta\tb"), Some(line("host", &["a", "b"])));
    assert_eq!(parsed("   # comment"), None);
    // An unquoted # starting an argument ends the line, as argv_split does.
    assert_eq!(parsed("Host web # main box"), Some(line("host", &["web"])));
    assert_eq!(
        parsed("Host a#b \"#c\""),
        Some(line("host", &["a#b", "#c"]))
    );
    assert_eq!(
        tokenize("ProxyCommand = sh -c \"nc %h %p\"").unwrap().rest,
        "sh -c \"nc %h %p\""
    );
    assert_eq!(parsed(""), None);
    // argv_split: single quotes, escapes, only space and tab separate.
    assert_eq!(
        parsed("HostName 'x.example'"),
        Some(line("hostname", &["x.example"]))
    );
    assert_eq!(
        parsed("IdentityFile my\\ key"),
        Some(line("identityfile", &["my key"]))
    );
    assert_eq!(parsed("Host a\u{a0}b"), Some(line("host", &["a\u{a0}b"])));
    assert_eq!(
        parsed("HostName \"abc"),
        None,
        "an unclosed quote is rejected"
    );
}

#[test]
fn includes_expand_relative_to_dot_ssh_with_globs_in_order() {
    let home = Dir::new();
    home.write(
        ".ssh/config",
        "Host first\nInclude config.d/*.conf ~/extra\nHost last\n",
    );
    home.write(".ssh/config.d/b.conf", "Host b\n");
    home.write(
        ".ssh/config.d/a.conf",
        "Host a\nInclude ../config.d/*.conf\n",
    );
    home.write(".ssh/config.d/.hidden.conf", "Host hidden\n");
    home.write(".ssh/config.d/skip.txt", "Host skipped\n");
    home.write("extra", "Host extra\n");
    let lines = read(&home.0.join(".ssh/config"), &home.0);
    let names = aliases(&lines);
    // a.conf includes the directory again: the recursion stops at the
    // depth limit and every alias is listed once.
    assert_eq!(names, ["first", "a", "b", "extra", "last"]);
}

#[test]
fn aliases_skip_patterns_and_negations_and_repeats() {
    let lines = vec![
        Item::Line(line("host", &["web", "web-*", "!bastion", "db?", "*"])),
        Item::Include(vec![Item::Line(line("host", &["web", "cache"]))]),
        Item::Line(line("hostname", &["ignored"])),
    ];
    assert_eq!(aliases(&lines), ["web", "cache"]);
}

#[test]
fn includes_that_multiply_stop_at_the_budget() {
    let home = Dir::new();
    // Each level reads itself twice: 2^17 reads without a budget.
    let config = home.write(".ssh/config", "Include config config\nHost loop\n");
    let started = std::time::Instant::now();
    let names = aliases(&read(&config, &home.0));
    assert_eq!(names, ["loop"]);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn aliases_with_an_at_sign_are_left_out() {
    let lines = vec![Item::Line(line("host", &["me@box", "box"]))];
    assert_eq!(aliases(&lines), ["box"]);
}

#[test]
fn missing_and_oversized_files_are_skipped() {
    let home = Dir::new();
    let config = home.write(".ssh/config", "Include nowhere/*\nInclude big\nHost ok\n");
    let big = std::fs::File::create(home.0.join(".ssh/big")).unwrap();
    big.set_len(2 * 1024 * 1024).unwrap();
    assert_eq!(aliases(&read(&config, &home.0)), ["ok"]);
}

#[test]
fn defaults_are_dropped_so_hosts_inherit_them() {
    let config = OpenSshConfig::file("x".into(), "/home/me".into(), "me".into(), None);
    let mut host = ConfigHost {
        alias: "a".into(),
        hostname: "a".into(),
        user: Some("me".into()),
        identity_files: DEFAULT_IDENTITIES
            .iter()
            .map(|n| PathBuf::from(format!("~/.ssh/{n}")))
            .collect(),
        ..ConfigHost::default()
    };
    let tidy = config.tidy(host.clone());
    assert_eq!(tidy.user, None);
    assert_eq!(tidy.identity_files, Vec::<PathBuf>::new());
    host.identities_only = true;
    assert_eq!(
        config.tidy(host).identity_files.len(),
        DEFAULT_IDENTITIES.len()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_alias_ssh_rejects_is_reported_not_guessed() {
    let Some(ssh) = find_ssh() else { return };
    let dir = Dir::new();
    let file = dir.write("config", "Host p0\n  Port 0\nHost fine\n");
    let config = OpenSshConfig::file(file, dir.0.clone(), "me".into(), Some(ssh));
    let hosts = config.read().await.unwrap();
    assert_eq!(hosts.len(), 2);
    // ssh rejects the whole config, so neither alias resolves.
    for host in &hosts {
        let problem = host.problem.as_deref().unwrap_or_default();
        assert!(
            problem.to_lowercase().contains("port"),
            "{}: {problem:?}",
            host.alias
        );
    }
}

#[test]
fn ssh_g_output_is_read() {
    let host = parse_ssh_g(
        "web",
        "user deploy\nhostname web.internal\nport 2222\nidentitiesonly yes\n\
         identityfile ~/.ssh/id_work\nproxyjump ops@bastion:2200,edge\nproxycommand none\n",
    );
    assert_eq!(host.hostname, "web.internal");
    assert_eq!(host.user.as_deref(), Some("deploy"));
    assert_eq!(host.port, Some(2222));
    assert_eq!(host.proxy_jump.as_deref(), Some("ops@bastion:2200,edge"));
    assert_eq!(host.proxy_command, None);
    assert!(host.identities_only);
}

/// The built-in resolver against `ssh -G -F`, the reference, on a corpus of
/// configs. Skipped without `ssh`, except in CI.
#[cfg(unix)]
mod corpus {
    use super::*;

    fn corpus(dir: &Dir) -> Vec<(&'static str, String)> {
        let inc = dir.write(
            "inc/part.conf",
            "Host included\n  HostName inc.example.com\n  User inc\n",
        );
        dir.write("inc/d/10-a.conf", "Host globbed-a\n  Port 2001\n");
        dir.write("inc/d/20-b.conf", "Host globbed-b\n  Port 2002\n");
        let glob = dir.0.join("inc/d/*.conf");
        vec![
            ("basic", "Host web\n  HostName web.internal\n  User deploy\n  Port 2222\n".into()),
            ("several aliases", "Host a b c\n  HostName shared.internal\n".into()),
            ("specific then wildcard", "Host web\n  User web\nHost *\n  User everyone\n  Port 2200\n".into()),
            ("wildcard first wins", "Host *\n  User everyone\nHost web\n  User web\n".into()),
            (
                "negation",
                "Host bastion\n  HostName b.internal\nHost * !bastion\n  ProxyJump bastion\nHost app\n".into(),
            ),
            (
                "proxyjump none overrides",
                "Host direct\n  ProxyJump none\nHost *\n  ProxyJump ops@jump:2200\nHost other\n".into(),
            ),
            (
                "proxycommand",
                "Host legacy\n  ProxyCommand ssh -W %h:%p jump.internal\n".into(),
            ),
            ("equals", "Host eq\n  HostName=eq.internal\n  Port = 2201\n  User=root\n".into()),
            (
                "quoted",
                "Host q\n  IdentityFile \"~/.ssh/my key\"\n  IdentitiesOnly yes\n".into(),
            ),
            (
                "identity files accumulate",
                "Host acc\n  IdentityFile ~/.ssh/id_one\nHost a*\n  IdentityFile ~/.ssh/id_two\n".into(),
            ),
            (
                "identities only with defaults",
                "Host only\n  IdentitiesOnly yes\n".into(),
            ),
            (
                "keyword case",
                "HOST up\n  HOSTNAME up.internal\n  user Upper\n  PORT 2203\n".into(),
            ),
            ("hostname token", "Host short\n  HostName %h.example.com\n".into()),
            (
                "comments and blanks",
                "# top\n\nHost c1\n\n  # inside\n  HostName c1.internal\n\n".into(),
            ),
            ("include in a block", format!("Host before\nInclude {}\nHost after\n", inc.display())),
            ("include at the top", format!("Include {}\nHost after\n", inc.display())),
            (
                "include restores the outer block",
                format!("Host outer\nInclude {}\n  Port 2209\n", inc.display()),
            ),
            ("include glob", format!("Include {}\n", glob.display())),
            ("match all", "Host m\n  HostName m.internal\nMatch all\n  User matched\n".into()),
            (
                "repeat block",
                "Host twice\n  Port 2204\nHost twice\n  Port 2205\n  User second\n".into(),
            ),
            ("question mark", "Host w?b\n  Port 2206\nHost web wab\n".into()),
            ("user with at", "Host corp\n  User first.last@corp.example\n".into()),
            ("tabs", "Host\ttabbed\n\tHostName\ttab.internal\n\tPort\t2207\n".into()),
            ("trailing comment", "Host tc # main box\n  HostName tc.internal # the box\n".into()),
            (
                "quoted proxycommand",
                "Host qp\n  ProxyCommand sh -c \"nc %h %p\"\n".into(),
            ),
            (
                "jump then command",
                "Host both\n  ProxyJump j.internal\n  ProxyCommand nc %h %p\n".into(),
            ),
            (
                "command then jump",
                "Host both2\n  ProxyCommand nc %h %p\nHost *\n  ProxyJump bastion\n".into(),
            ),
            (
                "command none blocks a later jump",
                "Host direct2\n  ProxyCommand none\nHost *\n  ProxyJump bastion\n".into(),
            ),
            ("uppercase alias token", "Host Mixed\n  HostName %h.example.com\n".into()),
            (
                "host case matters",
                "Host web\n  User lower\nHost WEB\n  User upper\n".into(),
            ),
            ("single quotes", "Host sq\n  HostName 'sq.internal'\n".into()),
            (
                "duplicate identity file",
                "Host dupid\n  IdentityFile ~/.ssh/k1\n  IdentityFile ~/.ssh/k1\n".into(),
            ),
            (
                "ipv6 hostname",
                "Host six\n  HostName 2001:db8::10\n  Port 2208\n".into(),
            ),
        ]
    }

    fn ssh() -> Option<PathBuf> {
        let found = find_ssh();
        if found.is_none() {
            assert!(
                std::env::var_os("CI").is_none(),
                "ssh must be installed in CI"
            );
        }
        found
    }

    /// `ssh -G -F file alias`, which must succeed: the reference never
    /// falls back to the resolver it checks.
    async fn reference(ssh: &Path, file: &Path, alias: &str, config: &OpenSshConfig) -> ConfigHost {
        let output = Command::new(ssh)
            .arg("-G")
            .arg("-F")
            .arg(file)
            .arg("--")
            .arg(alias)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .await
            .unwrap();
        assert!(output.status.success(), "ssh -G {alias} failed");
        config.tidy(parse_ssh_g(alias, &String::from_utf8_lossy(&output.stdout)))
    }

    #[tokio::test]
    async fn built_in_resolver_agrees_with_ssh_g() {
        let Some(ssh) = ssh() else { return };
        let dir = Dir::new();
        // The built-in reader gets a scratch home, so it cannot reach the
        // real ~/.ssh; corpus includes use absolute paths.
        let home = dir.0.clone();
        let user = std::env::var("USER").unwrap();
        let cases = corpus(&dir);
        assert!(cases.len() >= 20);
        let mut checked = 0;
        for (name, text) in cases {
            let file = dir.write(&format!("{}.conf", name.replace(' ', "-")), &text);
            let built_in = OpenSshConfig::file(file.clone(), home.clone(), user.clone(), None);
            let actual = built_in.read().await.unwrap();
            let names = aliases(&read(&file, &home));
            assert!(!names.is_empty(), "{name}: no aliases");
            let mut expected = Vec::new();
            for alias in &names {
                expected.push(reference(&ssh, &file, alias, &built_in).await);
            }
            assert_eq!(actual, expected, "{name}");
            checked += expected.len();
        }
        assert!(checked >= 30, "{checked} aliases compared");
    }
}
