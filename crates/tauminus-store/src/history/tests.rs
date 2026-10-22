// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

/// When test rows are stored; rows start at or before it.
use std::time::{Duration, Instant};

const NOW: i64 = 1_000_000_000;

struct Dir(std::path::PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let mut suffix = [0u8; 8];
        rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut suffix).unwrap();
        let suffix = suffix.iter().fold(String::new(), |mut out, b| {
            let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{b:02x}"));
            out
        });
        Self(std::env::temp_dir().join(format!("tauminus-history-{name}-{suffix}")))
    }

    fn db(&self) -> std::path::PathBuf {
        self.0.join("history.db")
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn entry(
    host: Option<&str>,
    at_ms: i64,
    command: &str,
    output: &str,
    exit: Option<i32>,
) -> HistoryEntry {
    HistoryEntry {
        host: host.map(Into::into),
        label: host.unwrap_or("local").into(),
        at_ms,
        command: command.into(),
        output: output.into(),
        exit,
    }
}

fn query(text: &str) -> HistoryQuery {
    HistoryQuery {
        text: text.into(),
        limit: 100,
        ..HistoryQuery::default()
    }
}

fn commands(hits: &[HistoryHit]) -> Vec<&str> {
    hits.iter().map(|h| h.command.as_str()).collect()
}

fn sample(db: &HistoryDb) {
    db.insert(
        &[
            entry(Some("web"), 1_000, "systemctl restart nginx", "ok", Some(0)),
            entry(
                Some("db"),
                2_000,
                "psql -c 'select 1'",
                "ERROR: permission denied",
                Some(1),
            ),
            entry(
                None,
                3_000,
                "echo xin chào nghĩa",
                "xin chào nghĩa",
                Some(0),
            ),
            entry(
                Some("web"),
                4_000,
                "tail -f /var/log/nginx/error.log",
                "upstream timed out",
                None,
            ),
        ],
        NOW,
    )
    .unwrap();
}

#[test]
fn what_is_recorded_is_found_and_the_file_holds_no_text() {
    let dir = Dir::new("round");
    let key = SecretKey::random().unwrap();
    {
        let db = HistoryDb::open(&dir.db(), &key).unwrap();
        sample(&db);
        assert_eq!(db.count().unwrap(), 4);
    }
    let db = HistoryDb::open(&dir.db(), &key).unwrap();
    // Newest first, prefix on the last word, across command and output.
    assert_eq!(
        commands(&db.search(&query("ngin")).unwrap()),
        [
            "tail -f /var/log/nginx/error.log",
            "systemctl restart nginx"
        ]
    );
    assert_eq!(
        commands(&db.search(&query("permission")).unwrap()),
        ["psql -c 'select 1'"]
    );
    // Diacritics fold: plain letters find the accented words.
    assert_eq!(
        commands(&db.search(&query("chao nghia")).unwrap()),
        ["echo xin chào nghĩa"]
    );
    // Matches in the output are marked for the view.
    let hit = &db.search(&query("denied")).unwrap()[0];
    assert!(
        hit.snippet.contains("\u{1}denied\u{2}"),
        "{:?}",
        hit.snippet
    );
    drop(db);
    for file in std::fs::read_dir(&dir.0).unwrap() {
        let bytes = std::fs::read(file.unwrap().path()).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for word in ["nginx", "permission", "SQLite format", "psql"] {
            assert!(!text.contains(word), "{word} is readable on disk");
        }
    }
}

#[test]
fn another_key_does_not_open_the_file() {
    let dir = Dir::new("key");
    {
        let db = HistoryDb::open(&dir.db(), &SecretKey::random().unwrap()).unwrap();
        sample(&db);
    }
    assert!(matches!(
        HistoryDb::open(&dir.db(), &SecretKey::random().unwrap()),
        Err(HistoryError::WrongKey)
    ));
}

#[test]
fn filters_narrow_by_host_time_and_exit() {
    let dir = Dir::new("filters");
    let db = HistoryDb::open(&dir.db(), &SecretKey::random().unwrap()).unwrap();
    sample(&db);
    let search = |q: HistoryQuery| commands(&db.search(&q).unwrap()).join(" | ");
    let mut q = query("");
    q.host = Some("web".into());
    assert_eq!(
        search(q.clone()),
        "tail -f /var/log/nginx/error.log | systemctl restart nginx"
    );
    q.exit = ExitFilter::Success;
    assert_eq!(search(q), "systemctl restart nginx");
    let mut q = query("");
    q.exit = ExitFilter::Failure;
    assert_eq!(search(q), "psql -c 'select 1'");
    let mut q = query("");
    q.host = Some(String::new());
    assert_eq!(search(q), "echo xin chào nghĩa");
    let mut q = query("nginx");
    q.from_ms = Some(2_000);
    q.to_ms = Some(5_000);
    assert_eq!(search(q), "tail -f /var/log/nginx/error.log");
    let mut q = query("");
    q.limit = 1;
    assert_eq!(search(q), "tail -f /var/log/nginx/error.log");
}

#[test]
fn search_text_cannot_inject_fts_syntax() {
    let dir = Dir::new("inject");
    let db = HistoryDb::open(&dir.db(), &SecretKey::random().unwrap()).unwrap();
    sample(&db);
    for text in ["\"", "NEAR(", "output:nginx", "*", "a OR b", "') OR 1=1 --"] {
        assert!(db.search(&query(text)).is_ok(), "{text}");
    }
}

#[test]
fn pruning_drops_the_oldest_by_age_then_size() {
    let dir = Dir::new("prune");
    let db = HistoryDb::open(&dir.db(), &SecretKey::random().unwrap()).unwrap();
    let day = 24 * 60 * 60 * 1000;
    let now = 100 * day;
    db.insert(
        &[
            entry(None, now - 40 * day, "old one", "", None),
            entry(None, now - 2 * day, "aaaaaaaaaa", "", None),
            entry(None, now - day, "bbbbbbbbbb", "", None),
            entry(None, now, "cccccccccc", "", None),
        ],
        NOW,
    )
    .unwrap();
    let by_age = Retention {
        max_bytes: u64::MAX,
        max_days: 30,
    };
    assert_eq!(db.prune(by_age, now).unwrap(), 1);
    // Each entry counts its command plus the label "local": 15 bytes.
    let by_size = Retention {
        max_bytes: 30,
        max_days: 0,
    };
    assert_eq!(db.prune(by_size, now).unwrap(), 1);
    assert_eq!(
        commands(&db.search(&query("")).unwrap()),
        ["cccccccccc", "bbbbbbbbbb"]
    );
    // And the index forgot them.
    assert_eq!(db.search(&query("aaaaaaaaaa")).unwrap().len(), 0);
}

#[test]
fn clearing_forgets_one_host_or_everything() {
    let dir = Dir::new("clear");
    let db = HistoryDb::open(&dir.db(), &SecretKey::random().unwrap()).unwrap();
    sample(&db);
    assert_eq!(db.clear(Some("web")).unwrap(), 2);
    assert_eq!(db.search(&query("nginx")).unwrap().len(), 0);
    assert_eq!(db.clear(None).unwrap(), 2);
    assert_eq!(db.count().unwrap(), 0);
}

#[test]
fn a_full_output_is_read_back_by_id() {
    let dir = Dir::new("output");
    let db = HistoryDb::open(&dir.db(), &SecretKey::random().unwrap()).unwrap();
    sample(&db);
    let hit = &db.search(&query("psql")).unwrap()[0];
    assert_eq!(
        db.output(hit.id).unwrap().as_deref(),
        Some("ERROR: permission denied")
    );
    assert_eq!(db.output(9_999).unwrap(), None);
}

/// The v0.2 exit criterion: a search over a million recorded lines answers
/// in under 100 ms. 100,000 commands of ten output lines each; each query
/// runs 20 times and its 95th percentile must stay under the budget. CI
/// runs it in a release build: `cargo test -p tauminus-store --release --
/// --ignored a_million_lines`.
/// The longest a search may take.
const BUDGET: Duration = Duration::from_millis(100);

#[test]
#[ignore = "a benchmark: run in a release build"]
#[expect(clippy::print_stdout, reason = "the benchmark reports its timings")]
#[expect(
    clippy::too_many_lines,
    reason = "one benchmark: data, cases, and budget"
)]
fn a_million_lines_are_searched_in_under_100_ms() {
    const COMMANDS: usize = 100_000;
    let dir = Dir::new("bench");
    let db = HistoryDb::open(&dir.db(), &SecretKey::random().unwrap()).unwrap();
    let words = [
        "git",
        "status",
        "kubectl",
        "get",
        "pods",
        "prod",
        "docker",
        "ps",
        "ls",
        "cd",
        "/var/log",
        "tail",
        "nginx",
        "systemctl",
        "restart",
        "journalctl",
        "grep",
        "error",
        "psql",
        "select",
        "users",
        "where",
        "deploy",
        "make",
        "timeout",
        "upstream",
        "connection",
        "refused",
    ];
    let mut seed: u64 = 7;
    let mut pick = || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        words[usize::try_from(seed >> 33).unwrap() % words.len()]
    };
    let started = std::time::Instant::now();
    let hosts = ["web", "db", "cache", "edge", ""];
    // Ten commands a commit, closer to one a commit as the app records
    // them, so the index has as many segments as it would in use.
    for batch in 0..COMMANDS / 10 {
        let entries: Vec<HistoryEntry> = (0..10)
            .map(|n| {
                let i = batch * 10 + n;
                let command = format!("{} {} {} arg{}", pick(), pick(), pick(), i % 50_000);
                let output = (0..10)
                    .map(|_| format!("{} {} {} {}", pick(), pick(), pick(), pick()))
                    .collect::<Vec<_>>()
                    .join("\n");
                let host = hosts[i % hosts.len()];
                entry(
                    (!host.is_empty()).then_some(host),
                    i64::try_from(i).unwrap() * 1000,
                    &command,
                    &output,
                    Some(i32::try_from(i % 3).unwrap()),
                )
            })
            .collect();
        db.insert(&entries, NOW).unwrap();
    }
    println!(
        "inserted {COMMANDS} commands, 10 output lines each, in {:?}",
        started.elapsed()
    );
    let cases: [(&str, HistoryQuery); 13] = [
        ("common word, newest 100", query("kubectl")),
        ("two words", query("systemctl restart")),
        ("rare token", query("arg12345")),
        ("prefix while typing", query("journ")),
        ("recent commands, no text", query("")),
        ("a host with no match", {
            let mut q = query("kubectl");
            q.host = Some("nowhere".into());
            q
        }),
        ("no text, one host", {
            let mut q = query("");
            q.host = Some("cache".into());
            q
        }),
        ("since a recent time", {
            // The window the dialog offers: the newest tenth of the rows.
            let mut q = query("kubectl");
            q.from_ms = Some(90_000_000);
            q
        }),
        ("two letter prefix", query("gr")),
        ("five letter prefix", query("kubec")),
        ("whole long word as typed", query("journalctl")),
        (
            "many words",
            query("git status kubectl get pods prod docker ps ls cd tail nginx"),
        ),
        ("words + host + time + failed", {
            let mut q = query("nginx error");
            q.host = Some("web".into());
            q.from_ms = Some(50_000_000);
            q.exit = ExitFilter::Failure;
            q
        }),
    ];
    let mut worst = std::time::Duration::ZERO;
    for (name, q) in &cases {
        let mut times: Vec<_> = (0..20)
            .map(|_| {
                let t = std::time::Instant::now();
                let hits = db.search(q).unwrap();
                assert!(
                    !hits.is_empty() || name.contains("no match"),
                    "{name} finds something"
                );
                t.elapsed()
            })
            .collect();
        times.sort();
        let (p50, p95) = (times[10], times[18]);
        println!("{name:32} p50 {p50:>10.2?}  p95 {p95:>10.2?}");
        worst = worst.max(p95);
    }
    // The newest results with large outputs full of the words searched,
    // as after `journalctl` or `kubectl logs`.
    let big: Vec<HistoryEntry> = (0..60)
        .map(|n| {
            let output = (0..1600)
                .map(|_| format!("{} {} {} {}", pick(), pick(), pick(), pick()))
                .collect::<Vec<_>>()
                .join("\n");
            entry(
                Some("web"),
                200_000_000 + n,
                "kubectl logs pod",
                &output,
                Some(0),
            )
        })
        .collect();
    db.insert(&big, NOW).unwrap();
    for (name, text) in [
        ("50 large outputs, 3 words", "kubectl logs pod"),
        (
            "50 large outputs, 8 words",
            "kubectl logs pod git status get pods prod",
        ),
    ] {
        let mut q = query(text);
        q.limit = 50;
        let mut times: Vec<_> = (0..10)
            .map(|_| {
                let t = Instant::now();
                assert_eq!(db.search(&q).unwrap().len(), 50);
                t.elapsed()
            })
            .collect();
        times.sort();
        println!("{name:32} p50 {:>10.2?}  p95 {:>10.2?}", times[5], times[9]);
        worst = worst.max(times[9]);
    }
    let started = std::time::Instant::now();
    db.prune(Retention::default(), 1_000_000_000).unwrap();
    let prune = started.elapsed();
    println!("prune under the cap              {prune:>10.2?}");
    assert!(
        prune < BUDGET,
        "a prune with nothing to drop took {prune:?}"
    );
    // Records wait for the index merge at the end of a forget.
    let gone = while_recording(&db, "forget one host", Duration::from_secs(10), || {
        db.clear(Some("edge")).unwrap()
    });
    println!("  ({gone} commands)");
    // As at the cap: every 500 commands recorded, about 500 go.
    let (total, oldest): (i64, i64) = {
        let conn = db.lock();
        (
            conn.query_row("SELECT bytes FROM stats", [], |r| r.get(0))
                .unwrap(),
            conn.query_row(
                "SELECT sum(size) FROM (SELECT size FROM entries ORDER BY id LIMIT 500)",
                [],
                |r| r.get(0),
            )
            .unwrap(),
        )
    };
    let half = Retention {
        max_bytes: u64::try_from(total - oldest).unwrap(),
        max_days: 0,
    };
    let gone = while_recording(
        &db,
        "prune 500 over the cap",
        Duration::from_secs(1),
        || db.prune(half, NOW).unwrap(),
    );
    println!("  ({gone} commands)");
    assert!(worst < BUDGET, "slowest p95 {worst:?} is over {BUDGET:?}");
}

/// Runs `job` while another thread records a command and searches every
/// 5 ms. Fails when a search waits more than [`BUDGET`], or when a record
/// waits longer than `record_budget`.
#[expect(clippy::print_stdout, reason = "the benchmark reports its timings")]
fn while_recording<T: Send>(
    db: &HistoryDb,
    name: &str,
    record_budget: Duration,
    job: impl FnOnce() -> T + Send,
) -> T {
    let done = std::sync::atomic::AtomicBool::new(false);
    let started = Instant::now();
    let (out, (mut records, mut searches)) = std::thread::scope(|scope| {
        let other = scope.spawn(|| {
            let (mut records, mut searches) = (Vec::new(), Vec::new());
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                let t = Instant::now();
                db.search(&query("kubectl")).unwrap();
                searches.push(t.elapsed());
                let t = Instant::now();
                db.insert(&[entry(None, 1, "echo meanwhile", "", Some(0))], NOW)
                    .unwrap();
                records.push(t.elapsed());
                #[expect(
                    clippy::disallowed_methods,
                    reason = "a plain thread, as the app's recording is"
                )]
                std::thread::sleep(Duration::from_millis(5));
            }
            (records, searches)
        });
        let out = job();
        done.store(true, std::sync::atomic::Ordering::Release);
        (out, other.join().unwrap())
    });
    records.sort();
    searches.sort();
    let slowest = |times: &[Duration]| times.last().copied().unwrap_or_default();
    let (record, search) = (slowest(&records), slowest(&searches));
    println!(
        "{name:32} {:>10.2?}; {} records, slowest {record:.2?}; {} searches, slowest {search:.2?}",
        started.elapsed(),
        records.len(),
        searches.len(),
    );
    assert!(search < BUDGET, "a search waited {search:?} during {name}");
    assert!(
        record < record_budget,
        "a record waited {record:?} during {name}"
    );
    out
}

/// Whether `word` is anywhere in the index's own pages.
fn indexed(db: &HistoryDb, word: &str) -> bool {
    let conn = db.lock();
    let mut statement = conn.prepare("SELECT block FROM entries_fts_data").unwrap();
    let blocks: Vec<Vec<u8>> = statement
        .query_map([], |r| r.get::<_, Option<Vec<u8>>>(0))
        .unwrap()
        .filter_map(|b| b.unwrap())
        .collect();
    blocks
        .iter()
        .any(|block| block.windows(word.len()).any(|w| w == word.as_bytes()))
}

#[test]
fn the_byte_total_follows_every_change() {
    let dir = Dir::new("stats");
    let db = HistoryDb::open(&dir.db(), &SecretKey::random().unwrap()).unwrap();
    let total = |db: &HistoryDb| {
        let conn = db.lock();
        let kept: i64 = conn
            .query_row("SELECT bytes FROM stats", [], |r| r.get(0))
            .unwrap();
        let summed: i64 = conn
            .query_row("SELECT coalesce(sum(size), 0) FROM entries", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kept, summed);
        kept
    };
    sample(&db);
    assert!(total(&db) > 0);
    db.clear(Some("web")).unwrap();
    total(&db);
    db.prune(
        Retention {
            max_bytes: 30,
            max_days: 0,
        },
        NOW,
    )
    .unwrap();
    assert!(total(&db) <= 30);
    db.clear(None).unwrap();
    assert_eq!(total(&db), 0);
}

#[test]
fn forgotten_and_pruned_words_leave_the_index_too() {
    let dir = Dir::new("purge");
    let db = HistoryDb::open(&dir.db(), &SecretKey::random().unwrap()).unwrap();
    sample(&db);
    db.insert(
        &[
            entry(Some("gone"), 5_000, "deploy zzhunter2zz", "", Some(0)),
            entry(None, 6_000, "echo zzoldsecretzz", "", Some(0)),
        ],
        NOW,
    )
    .unwrap();
    assert!(indexed(&db, "zzhunter2zz"));
    db.clear(Some("gone")).unwrap();
    assert!(
        !indexed(&db, "zzhunter2zz"),
        "a forgotten host's words remain"
    );
    let everything_but_newest = Retention {
        max_bytes: 20,
        max_days: 0,
    };
    db.prune(everything_but_newest, 0).unwrap();
    assert!(!indexed(&db, "nginx"), "pruned words remain");
    db.clear(None).unwrap();
    assert!(
        !indexed(&db, "zzoldsecretzz"),
        "words remain after forgetting all"
    );
    assert_eq!(db.count().unwrap(), 0);
}

#[test]
fn d_with_stroke_folds_like_other_diacritics() {
    let dir = Dir::new("fold");
    let db = HistoryDb::open(&dir.db(), &SecretKey::random().unwrap()).unwrap();
    db.insert(
        &[entry(None, 1, "echo đăng nhập Đà Nẵng", "", Some(0))],
        NOW,
    )
    .unwrap();
    for text in ["dang nhap", "đăng", "da nang", "ĐÀ"] {
        assert_eq!(db.search(&query(text)).unwrap().len(), 1, "{text}");
    }
}

#[test]
fn host_and_exit_filters_alone_use_the_index() {
    let dir = Dir::new("meta");
    let db = HistoryDb::open(&dir.db(), &SecretKey::random().unwrap()).unwrap();
    sample(&db);
    // A host id with characters the tokenizer splits on.
    db.insert(&[entry(Some("web-1.prod"), 9_000, "uptime", "", None)], NOW)
        .unwrap();
    let mut q = query("");
    q.host = Some("web-1.prod".into());
    assert_eq!(commands(&db.search(&q).unwrap()), ["uptime"]);
    q.host = Some("web".into());
    assert_eq!(db.search(&q).unwrap().len(), 2);
    let mut q = query("");
    q.to_ms = Some(500);
    assert_eq!(db.search(&q).unwrap().len(), 0, "nothing that early");
}

#[test]
fn reopening_turns_word_deletion_back_on() {
    let dir = Dir::new("secure");
    let key = SecretKey::random().unwrap();
    {
        let db = HistoryDb::open(&dir.db(), &key).unwrap();
        db.insert(&[entry(None, 1_000, "echo zzleftoverzz", "", Some(0))], NOW)
            .unwrap();
        // A quit while forgetting a host, between turning it off and on.
        db.lock()
            .execute_batch(
                "INSERT INTO entries_fts (entries_fts, rank) VALUES ('secure-delete', 0);",
            )
            .unwrap();
    }
    let db = HistoryDb::open(&dir.db(), &key).unwrap();
    db.lock().execute_batch("DELETE FROM entries").unwrap();
    assert!(
        !indexed(&db, "zzleftoverzz"),
        "a deleted row's words remain"
    );
}

#[test]
fn a_command_stored_after_the_clock_went_back_is_found_by_its_start() {
    let dir = Dir::new("clock");
    let db = HistoryDb::open(&dir.db(), &SecretKey::random().unwrap()).unwrap();
    db.insert(&[entry(None, 20_000, "echo late", "", Some(0))], 19_000)
        .unwrap();
    let hits = db
        .search(&HistoryQuery {
            from_ms: Some(19_500),
            ..query("")
        })
        .unwrap();
    assert_eq!(commands(&hits), ["echo late"]);
}

#[test]
fn an_insert_stores_nothing_once_the_vault_closed() {
    let dir = Dir::new("closed");
    let db = HistoryDb::open(&dir.db(), &SecretKey::random().unwrap()).unwrap();
    let stored = db
        .insert_while(
            &[entry(None, 1_000, "echo late", "", Some(0))],
            NOW,
            &|| false,
        )
        .unwrap();
    assert!(!stored);
    assert_eq!(db.count().unwrap(), 0);
}

#[test]
fn a_prune_stops_once_the_vault_locks() {
    let dir = Dir::new("prune-lock");
    let db = HistoryDb::open(&dir.db(), &SecretKey::random().unwrap()).unwrap();
    sample(&db);
    let before = db.count().unwrap();
    let nothing = Retention {
        max_bytes: 0,
        max_days: 0,
    };
    assert_eq!(db.prune_while(nothing, NOW, &|| false).unwrap(), 0);
    assert_eq!(db.count().unwrap(), before);
    assert_eq!(db.prune_while(nothing, NOW, &|| true).unwrap(), before);
}

#[test]
fn a_rekey_waits_for_the_file_to_be_free() {
    let dir = Dir::new("rekey-busy");
    let old = SecretKey::random().unwrap();
    let new = SecretKey::random().unwrap();
    let db = HistoryDb::open(&dir.db(), &old).unwrap();
    sample(&db);
    // Another connection is open: the file stays as it is.
    assert!(matches!(
        HistoryDb::rekey(&dir.db(), &old, &new),
        Err(HistoryError::Busy)
    ));
    assert_eq!(db.count().unwrap(), 4);
    drop(db);
    HistoryDb::rekey(&dir.db(), &old, &new).unwrap();
    assert!(matches!(
        HistoryDb::open(&dir.db(), &old),
        Err(HistoryError::WrongKey)
    ));
    let db = HistoryDb::open(&dir.db(), &new).unwrap();
    assert_eq!(db.count().unwrap(), 4);
    // Back on the write-ahead log.
    let mode: String = db
        .lock()
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
}
