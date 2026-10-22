// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The command history: an SQLite database with a full-text index,
//! encrypted page by page with SQLCipher under a key the vault holds.
//!
//! Every page, the write-ahead log included, is encrypted, so the file says
//! nothing without the key. Deleted rows are overwritten (`secure_delete`),
//! and so are their words in the index (FTS5 `secure-delete`). Searches use
//! FTS5 with diacritics folded, `đ` included, so "dang nhap" finds "đăng
//! nhập". A hidden index column holds a token per host and per exit status,
//! so those filters intersect posting lists instead of scanning. Each row
//! also keeps when it was stored, which never decreases with the rowid, so
//! a start time becomes a rowid bound.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use parking_lot::FairMutex;

use rusqlite::{Connection, OptionalExtension, params};
use tauminus_domain::{
    EXCERPT_FROM, ExitFilter, HistoryEntry, HistoryHit, HistoryQuery, Retention,
};
use zeroize::Zeroizing;

use crate::vault::SecretKey;

mod pending;

pub use pending::{HistoryKeyPair, Pending, PendingLog};

const SCHEMA_VERSION: i64 = 1;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    /// The key does not open the database, or the file is not one.
    #[error("the history key does not open the history file")]
    WrongKey,
    #[error("the history file: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("the history file: {0}")]
    Io(#[from] std::io::Error),
    #[error("the history file is damaged: {0}")]
    Corrupt(String),
}

/// The open history database. Calls block on SQLite: run them off the
/// async workers.
pub struct HistoryDb {
    /// Writes. Handed over in turn: a long prune or clear takes it one
    /// chunk at a time, and a waiting record gets it between chunks.
    conn: FairMutex<Connection>,
    /// Reads, on their own connection: with the write-ahead log, a search
    /// reads the last commit and never waits for a write.
    reader: FairMutex<Connection>,
}

impl std::fmt::Debug for HistoryDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryDb").finish_non_exhaustive()
    }
}

impl HistoryDb {
    /// Opens or creates the database at `path` with `key`.
    pub fn open(path: &Path, key: &SecretKey) -> Result<Self, HistoryError> {
        if let Some(dir) = path.parent() {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder.create(dir)?;
        }
        let conn = connect(path, key)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        match version {
            0 => create(&conn)?,
            // A clear cut short may have left word-by-word deletion off.
            SCHEMA_VERSION => conn.execute_batch(
                "INSERT INTO entries_fts (entries_fts, rank) VALUES ('secure-delete', 1);",
            )?,
            other => {
                return Err(HistoryError::Corrupt(format!(
                    "it is version {other}; update Tauminus"
                )));
            }
        }
        let reader = connect(path, key)?;
        reader.execute_batch("PRAGMA query_only = ON;")?;
        Ok(Self {
            conn: FairMutex::new(conn),
            reader: FairMutex::new(reader),
        })
    }

    /// Adds `entries` in one transaction, stored at `now_ms`. O(k) rows
    /// plus their index updates.
    pub fn insert(&self, entries: &[HistoryEntry], now_ms: i64) -> Result<(), HistoryError> {
        self.insert_while(entries, now_ms, &|| true).map(|_| ())
    }

    /// As [`HistoryDb::insert`], when `open` still holds once the writer
    /// is taken, which may be seconds later during a forget: `false` when it
    /// does not, and nothing was stored.
    pub fn insert_while(
        &self,
        entries: &[HistoryEntry],
        now_ms: i64,
        open: &dyn Fn() -> bool,
    ) -> Result<bool, HistoryError> {
        let mut conn = self.lock();
        if !open() {
            return Ok(false);
        }
        let tx = conn.transaction()?;
        {
            // Never before the last row's, even if the clock went back, nor
            // before the command started: a stored time that only grows with
            // the rowid, and is never before `at`, bounds searches by start.
            let last: Option<i64> =
                tx.query_row("SELECT max(recorded) FROM entries", [], |r| r.get(0))?;
            let mut recorded = now_ms.max(last.unwrap_or(i64::MIN));
            let mut add = tx.prepare_cached(
                "INSERT INTO entries (host, label, at, exit, command, output, size, meta, recorded)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for entry in entries {
                let entry = entry.clone().bounded();
                if entry.command.is_empty() {
                    continue;
                }
                let host = entry.host.as_deref().unwrap_or("");
                recorded = recorded.max(entry.at_ms);
                add.execute(params![
                    host,
                    entry.label,
                    entry.at_ms,
                    entry.exit,
                    entry.command,
                    entry.output,
                    i64::try_from(entry.size()).unwrap_or(i64::MAX),
                    format!("{} {}", host_token(host), exit_token(entry.exit)),
                    recorded,
                ])?;
            }
        }
        tx.commit()?;
        Ok(true)
    }

    /// Newest first. Words, host, and exit status are one FTS5 match, read
    /// in rowid order so it stops at `limit`; a start time bounds the rowids
    /// from below. A query with none of them reads the newest rows.
    /// O(limit + log n) when matches are dense, and no worse than the
    /// matches stored since the start time when they are not.
    pub fn search(&self, query: &HistoryQuery) -> Result<Vec<HistoryHit>, HistoryError> {
        let conn = self.reader.lock();
        let Some(low) = first_rowid_since(&conn, query.from_ms)? else {
            return Ok(Vec::new());
        };
        let high = i64::MAX;
        let row = |r: &rusqlite::Row<'_>| {
            let host: String = r.get(1)?;
            let head: String = r.get(6)?;
            let hit = HistoryHit {
                id: r.get(0)?,
                host: (!host.is_empty()).then_some(host),
                label: r.get(2)?,
                at_ms: r.get(3)?,
                exit: r.get(4)?,
                command: r.get(5)?,
                // An excerpt of the output's start, found in Rust: FTS5's
                // snippet() reads the whole output, at a cost that grows
                // with the square of the words searched.
                snippet: query.excerpt(&head),
            };
            Ok(hit)
        };
        let limit = i64::from(query.limit);
        let times = (
            query.from_ms.unwrap_or(i64::MIN),
            query.to_ms.unwrap_or(i64::MAX),
        );
        let hits = if let Some(fts) = fts_match(query) {
            let mut statement = conn.prepare_cached(
                "SELECT e.id, e.host, e.label, e.at, e.exit, e.command,
                        substr(e.output, 1, ?7)
                 FROM entries_fts JOIN entries e ON e.id = entries_fts.rowid
                 WHERE entries_fts MATCH ?1
                   AND entries_fts.rowid BETWEEN ?2 AND ?3
                   AND e.at BETWEEN ?4 AND ?5
                 ORDER BY entries_fts.rowid DESC LIMIT ?6",
            )?;
            statement
                .query_map(
                    params![
                        fts,
                        low,
                        high,
                        times.0,
                        times.1,
                        limit,
                        i64::try_from(EXCERPT_FROM).unwrap_or(i64::MAX)
                    ],
                    row,
                )?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            let mut statement = conn.prepare_cached(
                "SELECT e.id, e.host, e.label, e.at, e.exit, e.command, ''
                 FROM entries e
                 WHERE e.id BETWEEN ?1 AND ?2 AND e.at BETWEEN ?3 AND ?4
                 ORDER BY e.id DESC LIMIT ?5",
            )?;
            statement
                .query_map(params![low, high, times.0, times.1, limit], row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(hits)
    }

    /// One command's full output, for showing a result.
    pub fn output(&self, id: i64) -> Result<Option<String>, HistoryError> {
        Ok(self
            .reader
            .lock()
            .query_row("SELECT output FROM entries WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Drops commands older than the retention, then the oldest until the
    /// text fits the size cap. Returns how many went. The byte total is kept
    /// by triggers, so a history under its cap costs O(1); dropping k rows
    /// costs O(k), in chunks that let recording and searching in between.
    pub fn prune(&self, retention: Retention, now_ms: i64) -> Result<u64, HistoryError> {
        let mut removed = 0;
        if retention.max_days > 0 {
            let cutoff = now_ms.saturating_sub(i64::from(retention.max_days) * DAY_MS);
            removed += self.delete_chunks(
                "DELETE FROM entries WHERE id IN (
                     SELECT id FROM entries INDEXED BY entries_at WHERE at < ?1 LIMIT ?2)",
                cutoff,
            )?;
        }
        let max = i64::try_from(retention.max_bytes).unwrap_or(i64::MAX);
        let mut chunk = Chunk::default();
        loop {
            let conn = self.lock();
            let started = Instant::now();
            let total: i64 = conn.query_row("SELECT bytes FROM stats", [], |r| r.get(0))?;
            if total <= max {
                break;
            }
            // The oldest rows that free enough, at most a chunk of them.
            let mut excess = total - max;
            let mut last = None;
            {
                let mut oldest =
                    conn.prepare_cached("SELECT id, size FROM entries ORDER BY id LIMIT ?1")?;
                let mut rows = oldest.query([chunk.rows])?;
                while let Some(row) = rows.next()? {
                    last = Some(row.get::<_, i64>(0)?);
                    excess -= row.get::<_, i64>(1)?;
                    if excess <= 0 {
                        break;
                    }
                }
            }
            let Some(last) = last else {
                break;
            };
            removed += conn.execute("DELETE FROM entries WHERE id <= ?1", [last])?;
            drop(conn);
            chunk.took(started.elapsed());
        }
        Ok(removed as u64)
    }

    /// Runs a deletion of at most `?2` rows until it deletes nothing, taking
    /// the connection for one chunk at a time. With secure-delete on, each
    /// row's words leave the index too, which costs 1 to 30 ms a row on a
    /// large index, so chunks are sized by the time they take.
    fn delete_chunks(&self, sql: &str, param: impl rusqlite::ToSql) -> Result<usize, HistoryError> {
        let mut removed = 0;
        let mut chunk = Chunk::default();
        loop {
            let conn = self.lock();
            let started = Instant::now();
            let n = conn
                .prepare_cached(sql)?
                .execute(rusqlite::params![&param, chunk.rows])?;
            drop(conn);
            chunk.took(started.elapsed());
            removed += n;
            if n == 0 {
                return Ok(removed);
            }
        }
    }

    /// Forgets one saved host's commands (`""` for local and ad hoc
    /// sessions), or every command, from the rows and from the index. One
    /// host's rows go in chunks, so recording and searching go on meanwhile,
    /// and then the index is merged, which drops their words; everything
    /// goes at once, with one reset of the index.
    pub fn clear(&self, host: Option<&str>) -> Result<u64, HistoryError> {
        let removed = if let Some(host) = host {
            // Row by row, secure-delete rewrites index pages for each row;
            // in bulk it is cheaper to leave tombstones and then merge the
            // whole index below, which drops the deleted words with them.
            self.lock().execute_batch(
                "INSERT INTO entries_fts (entries_fts, rank) VALUES ('secure-delete', 0);",
            )?;
            let removed = self.delete_chunks(
                "DELETE FROM entries WHERE id IN (
                     SELECT id FROM entries INDEXED BY entries_host WHERE host = ?1 LIMIT ?2)",
                host,
            );
            // Whatever happened, word-by-word deletion comes back on and the
            // index is merged, which drops the deleted rows' words. The
            // merge holds the writer for O(index size), seconds on a large
            // history; searches read meanwhile, and records wait.
            let restored = self.lock().execute_batch(
                "INSERT INTO entries_fts (entries_fts, rank) VALUES ('secure-delete', 1);
                 INSERT INTO entries_fts (entries_fts) VALUES ('optimize');",
            );
            let removed = removed?;
            restored?;
            removed
        } else {
            let mut conn = self.lock();
            let tx = conn.transaction()?;
            tx.execute_batch(
                "DROP TRIGGER entries_removed;
                 INSERT INTO entries_fts (entries_fts) VALUES ('delete-all');",
            )?;
            let removed = tx.execute("DELETE FROM entries", [])?;
            tx.execute_batch(REMOVED_TRIGGER)?;
            tx.commit()?;
            removed
        };
        // Hand the freed, zeroed pages back.
        self.lock()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(removed as u64)
    }

    /// How many commands are kept.
    pub fn count(&self) -> Result<u64, HistoryError> {
        let n: i64 = self
            .reader
            .lock()
            .query_row("SELECT count(*) FROM entries", [], |r| r.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    fn lock(&self) -> parking_lot::FairMutexGuard<'_, Connection> {
        self.conn.lock()
    }
}

/// A connection to the database at `path`, keyed and set up. Only "not a
/// database" means the wrong key.
fn connect(path: &Path, key: &SecretKey) -> Result<Connection, HistoryError> {
    let conn = Connection::open(path)?;
    // The raw key, so SQLCipher runs no KDF of its own: the vault's
    // Argon2id already guards it. The statement lives in a wiped
    // buffer, and no error from it is passed on, since SQLite quotes
    // the statement in some errors.
    let mut statement = Zeroizing::new(String::with_capacity(80));
    statement.push_str("PRAGMA key = \"x'");
    for b in key.as_bytes() {
        let _ = write!(statement, "{b:02x}");
    }
    statement.push_str("'\";");
    if conn.execute_batch(&statement).is_err() {
        return Err(HistoryError::WrongKey);
    }
    // Only "not a database" means the wrong key; a busy or failing
    // disk is reported as itself.
    match conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    }) {
        Ok(_) => {}
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::NotADatabase =>
        {
            return Err(HistoryError::WrongKey);
        }
        Err(e) => return Err(e.into()),
    }
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA secure_delete = ON;
         PRAGMA temp_store = MEMORY;
         PRAGMA journal_size_limit = 67108864;",
    )?;
    Ok(conn)
}

/// How many rows a deletion takes per hold of the connection: doubled
/// while a chunk takes under half the target, halved while over it, so a
/// waiting record waits about [`Chunk::TARGET`] at most.
struct Chunk {
    rows: i64,
}

impl Chunk {
    // Measured at 100,000 commands: a target of 150 ms made pruning 500
    // rows slower (2.8 s against 1.5 s) and a record wait 500 ms, as a
    // chunk that doubles can run into the index's own merging.
    const TARGET: Duration = Duration::from_millis(40);
    const MAX_ROWS: i64 = 1000;
}

impl Default for Chunk {
    fn default() -> Self {
        Self { rows: 32 }
    }
}

impl Chunk {
    fn took(&mut self, elapsed: Duration) {
        if elapsed > Self::TARGET {
            self.rows = (self.rows / 2).max(1);
        } else if elapsed < Self::TARGET / 2 {
            self.rows = (self.rows * 2).min(Self::MAX_ROWS);
        }
    }
}

/// The lowest rowid a command that started at or after `from` can have.
/// A command is stored after it starts, and stored times never decrease
/// with the rowid, so every such command is at or after the first row
/// stored since `from`. `None` when nothing was stored since. O(log n).
fn first_rowid_since(conn: &Connection, from: Option<i64>) -> Result<Option<i64>, HistoryError> {
    let Some(from) = from else {
        return Ok(Some(i64::MIN));
    };
    Ok(conn
        .query_row(
            "SELECT id FROM entries INDEXED BY entries_recorded
             WHERE recorded >= ?1 ORDER BY recorded, id LIMIT 1",
            [from],
            |r| r.get(0),
        )
        .optional()?)
}

/// The FTS5 expression for a query, or `None` when it has no words and no
/// host or exit filter. Words match command and output; filters match the
/// hidden `meta` column.
fn fts_match(query: &HistoryQuery) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(words) = query.fts_query() {
        parts.push(format!("{{command output}} : ({})", fold(&words)));
    }
    if let Some(host) = &query.host {
        parts.push(format!("meta : {}", host_token(host)));
    }
    match query.exit {
        ExitFilter::Any => {}
        ExitFilter::Success => parts.push("meta : xok".into()),
        ExitFilter::Failure => parts.push("meta : xfail".into()),
    }
    (!parts.is_empty()).then(|| parts.join(" AND "))
}

/// A host as one token: hex keeps `-` and `.` from splitting it.
fn host_token(host: &str) -> String {
    let mut token = String::with_capacity(1 + host.len() * 2);
    token.push('h');
    for b in host.bytes() {
        let _ = write!(token, "{b:02x}");
    }
    token
}

fn exit_token(exit: Option<i32>) -> &'static str {
    match exit {
        None => "xnone",
        Some(0) => "xok",
        Some(_) => "xfail",
    }
}

/// `đ` and `Đ` have no decomposition, so the tokenizer keeps them; the
/// index and queries fold them the same way.
fn fold(text: &str) -> String {
    text.replace('đ', "d").replace('Đ', "D")
}

const REMOVED_TRIGGER: &str = "CREATE TRIGGER entries_removed AFTER DELETE ON entries BEGIN
     INSERT INTO entries_fts (entries_fts, rowid, command, output, meta)
     VALUES ('delete', old.id,
             replace(replace(old.command, 'đ', 'd'), 'Đ', 'D'),
             replace(replace(old.output, 'đ', 'd'), 'Đ', 'D'),
             old.meta);
 END;";

fn create(conn: &Connection) -> Result<(), HistoryError> {
    conn.execute_batch(&format!(
        "BEGIN;
         CREATE TABLE entries (
             id INTEGER PRIMARY KEY,
             host TEXT NOT NULL,
             label TEXT NOT NULL,
             at INTEGER NOT NULL,
             exit INTEGER,
             command TEXT NOT NULL,
             output TEXT NOT NULL,
             size INTEGER NOT NULL,
             meta TEXT NOT NULL,
             recorded INTEGER NOT NULL
         );
         CREATE INDEX entries_at ON entries (at);
         CREATE INDEX entries_recorded ON entries (recorded);
         CREATE INDEX entries_host ON entries (host, at);
         -- Prefix indexes make the first keystrokes of a search cheap.
         CREATE VIRTUAL TABLE entries_fts USING fts5(
             command, output, meta,
             content = 'entries', content_rowid = 'id',
             tokenize = 'unicode61 remove_diacritics 2',
             prefix = '2 3 4 5 6 7 8'
         );
         -- The text's byte total, so pruning knows without a scan.
         CREATE TABLE stats (bytes INTEGER NOT NULL);
         INSERT INTO stats (bytes) VALUES (0);
         CREATE TRIGGER entries_counted_in AFTER INSERT ON entries BEGIN
             UPDATE stats SET bytes = bytes + new.size;
         END;
         CREATE TRIGGER entries_counted_out AFTER DELETE ON entries BEGIN
             UPDATE stats SET bytes = bytes - old.size;
         END;
         INSERT INTO entries_fts (entries_fts, rank) VALUES ('secure-delete', 1);
         -- The index takes text with đ folded; snippets read the stored text.
         CREATE TRIGGER entries_added AFTER INSERT ON entries BEGIN
             INSERT INTO entries_fts (rowid, command, output, meta)
             VALUES (new.id,
                     replace(replace(new.command, 'đ', 'd'), 'Đ', 'D'),
                     replace(replace(new.output, 'đ', 'd'), 'Đ', 'D'),
                     new.meta);
         END;
         {REMOVED_TRIGGER}
         PRAGMA user_version = {SCHEMA_VERSION};
         COMMIT;"
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests;
