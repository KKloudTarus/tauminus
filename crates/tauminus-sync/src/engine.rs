// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! The sync adapter: [`Remote`] merges the saved lists with the other
//! devices' through a [`Target`], and publishes this device's state.
//!
//! This device keeps a base (`sync-state.json`): its id, its last object
//! number, its clock, the newest object number seen from each device, and
//! the merged state of the last sync, stamped. A sync stamps what changed
//! since, reads each device's newest object it has not read, joins them,
//! and gives the result back as lists the domain checks and repairs. Each
//! list is adopted into the base once it is saved here; publishing then
//! writes one new object with the whole state, if this device added
//! stamps of its own, and removes its older ones. A sync cut short at any
//! point compares against what this device holds, and the next publishes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tauminus_app::ports::{
    BoxFuture, Merged, SyncClash, SyncData, SyncError, SyncPart, SyncRemote,
};
use tauminus_store::interchange;

use crate::doc::{Clash, Doc, clashes, stamp_with};
use crate::flat::{Flat, flatten, unflatten};
use crate::seal::SyncKey;
use crate::stamp::Clock;
use crate::target::{KEY, MAX_KEY_FILE, MAX_OBJECT, Target, object_name, parse_object};

/// The payload format this build writes and the newest it reads.
const SCHEMA: u32 = 1;
/// Largest object once inflated: about 7 MB at 10 000 hosts with ten
/// fields each; the rest is room for hosts at their list limits.
const MAX_PLAIN: u64 = 512 * 1024 * 1024;
/// How long a deleted record's tombstone is kept: a device offline longer
/// than this may bring a deleted record back.
const TOMBSTONE_MS: u64 = 90 * 24 * 60 * 60 * 1000;
/// How often a device writes its object though nothing changed: others
/// take an object older than [`TOMBSTONE_MS`] for a device gone.
const KEEPALIVE_MS: u64 = 30 * 24 * 60 * 60 * 1000;
/// Object numbers that failed, by device: when last, and how often.
type Failed = BTreeMap<String, BTreeMap<u64, (u64, u32)>>;

/// Whether `seq` from `device` failed lately, so is not tried yet: an
/// hour after its first failure, doubling to 32 hours. A failure stamped
/// ahead of this clock counts as long ago.
fn failed_lately(failed: &Failed, device: &str, seq: u64, now: u64) -> bool {
    failed
        .get(device)
        .and_then(|f| f.get(&seq))
        .is_some_and(|(at, tries)| {
            let wait = RETRY_MS << tries.saturating_sub(1).min(5);
            *at <= now && now - at < wait
        })
}

/// How long a base changed only by what other devices wrote waits before
/// this device writes it again: a burst of edits elsewhere costs one
/// object here, not one per edit.
const ECHO_DELAY_MS: u64 = 60 * 60 * 1000;
/// Objects that fail to read, per device and in all, before the rest wait
/// for the next sync: a target that plants many forged ones costs little.
const TRIES: usize = 3;
/// How long an object that failed waits before it is tried again: one a
/// sync tool was still copying reads whole later.
const RETRY_MS: u64 = 60 * 60 * 1000;
const TRIES_PER_SYNC: usize = 16;
/// Clashes one object carries for the others.
const MAX_CLASHES: usize = 256;
/// How far ahead of this device's clock another device's stamps may pull
/// it: a device whose clock runs a day fast does not pin every other.
const MAX_DRIFT_MS: u64 = 60 * 60 * 1000;
/// The shortest sync passphrase: it guards a key file anyone with the
/// target can try passphrases against.
pub const MIN_PASSPHRASE: usize = 12;

const HOST_KINDS: [(&str, &str); 2] = [("groups", "group"), ("hosts", "host")];
const SNIPPET_KINDS: [(&str, &str); 1] = [("snippets", "snippet")];

/// What one object holds, as written: borrowed, so the doc is not copied.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PayloadOut<'a> {
    schema: u32,
    /// The `hosts.json` and `snippets.json` versions its records follow.
    hosts_version: u32,
    snippets_version: u32,
    device: &'a str,
    seq: u64,
    /// When it was written, in wall-clock milliseconds.
    written: u64,
    doc: &'a Doc,
    clashes: &'a [Clash],
}

impl<'a> PayloadOut<'a> {
    fn of(state: &'a State, now: u64) -> Self {
        Self {
            schema: SCHEMA,
            hosts_version: interchange::HOSTS_VERSION,
            snippets_version: interchange::SNIPPETS_VERSION,
            device: &state.device,
            seq: state.seq,
            written: now,
            doc: &state.doc,
            clashes: &state.untold,
        }
    }
}

/// What one object holds, past its [`Header`].
#[derive(Deserialize)]
struct Payload {
    device: String,
    seq: u64,
    #[serde(default)]
    written: u64,
    doc: Doc,
    /// Clashes this device settled in the sync that wrote this object,
    /// so the device whose value lost is told too.
    #[serde(default)]
    clashes: Vec<Clash>,
}

/// This device's base, kept beside `hosts.json`.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct State {
    device: String,
    seq: u64,
    clock: Clock,
    /// The newest object number read from each other device: an older
    /// one offered later is a replay, and is not read.
    seen: BTreeMap<String, u64>,
    /// The merged state of the last sync, kept in a file of its own
    /// (`.doc` beside this one), read only when a sync merges.
    #[serde(skip)]
    doc: Doc,
    /// Whether the target has this device's part of `doc`: false between
    /// a sync that saved edits of its own here and the publish after it.
    #[serde(default = "yes")]
    published: bool,
    /// What the lists saved here hashed to when `doc` was saved: lists
    /// that still hash to it, with nothing new on the target, need no
    /// merge.
    #[serde(default)]
    fingerprint: String,
    /// The object numbers from each device that failed their checks: not
    /// tried again while the target lists them. Exact numbers, never a
    /// mark, so a forged one far ahead hides nothing behind it.
    #[serde(default)]
    failed: Failed,
    /// Whether a sync was saved whole here since this state began: until
    /// then each sync is a first one.
    #[serde(default = "yes")]
    joined: bool,
    /// When this device last wrote its object.
    #[serde(default)]
    written: u64,
    /// Whether a merge's state is kept (`.staged` beside this one), from
    /// before its lists are saved until its base is: values the lists then
    /// hold are the merge's, not edits.
    #[serde(default)]
    staged: bool,
    /// Clashes not yet written to the target for the others.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    untold: Vec<Clash>,
    /// When a base that only other devices changed is written again, so
    /// no object here keeps what they deleted or replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    echo_due: Option<u64>,
    /// Names in this device's folder that will not go (a planted folder it
    /// cannot empty): left alone, and numbered past.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    stuck: BTreeSet<String>,
}

const fn yes() -> bool {
    true
}

impl State {
    /// Whether this state needs nothing done: joined, published, nothing
    /// kept back, and written lately.
    fn idle(&self, now: u64) -> bool {
        self.joined
            && self.published
            && !self.staged
            && self.untold.is_empty()
            && self
                .echo_due
                .is_none_or(|due| now < due && due <= now.saturating_add(ECHO_DELAY_MS))
            && now.saturating_sub(self.written) < KEEPALIVE_MS
    }

    fn new() -> Result<Self, SyncError> {
        let mut id = [0u8; 16];
        rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut id)
            .map_err(|_| SyncError::Target("no randomness from the system".into()))?;
        let device = id.iter().fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        });
        Ok(Self {
            clock: Clock::new(device.clone()),
            device,
            seq: 0,
            seen: BTreeMap::new(),
            doc: Doc::default(),
            published: true,
            fingerprint: String::new(),
            failed: BTreeMap::new(),
            joined: false,
            written: 0,
            staged: false,
            untold: Vec::new(),
            echo_due: None,
            stuck: BTreeSet::new(),
        })
    }
}

/// What a merge left to adopt and publish.
struct Pending {
    /// The state as saved on disk, parts adopted into it as they are
    /// saved here.
    state: State,
    /// The merged state, as this device saves it.
    base: Doc,
    /// The clashes this sync settled, for the object it writes.
    clashes: Vec<Clash>,
    /// This device's newest object on the target.
    own_seq: u64,
    /// Whether the merge gave stamps of this device's: edits, repairs,
    /// or values a first sync brought. Only then do the others need a new
    /// object; what came from them they read from theirs.
    own: bool,
    /// The newest object read from each other device, and those that
    /// failed, kept once every part is adopted.
    seen: BTreeMap<String, u64>,
    failed: Failed,
    /// Names in this device's folder it did not write: removed at publish.
    forged: Vec<String>,
    /// The merged lists' fingerprint, kept likewise.
    fingerprint: String,
    /// The parts adopted so far.
    adopted: BTreeSet<&'static str>,
}

/// The other devices, through `target`, sealed with `key`.
#[derive(Debug)]
pub struct Remote {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Remote")
            .field("state", &self.state_path)
            .finish_non_exhaustive()
    }
}

struct Inner {
    target: Arc<dyn Target>,
    key: SyncKey,
    state_path: PathBuf,
    pending: Mutex<Option<Pending>>,
    /// Wall-clock milliseconds; a test moves it.
    now: fn() -> u64,
}

fn target_error(error: &std::io::Error) -> SyncError {
    SyncError::Target(error.to_string())
}

impl Remote {
    /// Syncs through `target` with `key`, keeping the base at
    /// `state_path`.
    #[must_use]
    pub fn new(target: Arc<dyn Target>, key: SyncKey, state_path: PathBuf) -> Self {
        Self {
            inner: Arc::new(Inner {
                target,
                key,
                state_path,
                pending: Mutex::new(None),
                now: now_ms,
            }),
        }
    }

    #[cfg(test)]
    fn with_clock(
        target: Arc<dyn Target>,
        key: SyncKey,
        state_path: PathBuf,
        now: fn() -> u64,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                target,
                key,
                state_path,
                pending: Mutex::new(None),
                now,
            }),
        }
    }
}

/// Starts sync on an empty `target`: a new sync key, wrapped under
/// `passphrase` in its key file. Runs Argon2id; blocking.
///
/// # Errors
/// The target already has a key file, or cannot be written.
pub fn create(
    target: &dyn Target,
    passphrase: &str,
    kdf: crate::seal::Kdf,
) -> Result<SyncKey, SyncError> {
    if passphrase.chars().count() < MIN_PASSPHRASE {
        return Err(SyncError::Sealed(format!(
            "the sync passphrase needs at least {MIN_PASSPHRASE} characters"
        )));
    }
    target.refresh().map_err(|e| target_error(&e))?;
    if target
        .list()
        .map_err(|e| target_error(&e))?
        .iter()
        .any(|n| n == KEY)
    {
        return Err(SyncError::Target(
            "it already holds a sync key; join it with its passphrase".into(),
        ));
    }
    let key = SyncKey::random().map_err(|e| SyncError::Sealed(e.to_string()))?;
    let file = key
        .wrap(passphrase, kdf)
        .map_err(|e| SyncError::Sealed(e.to_string()))?;
    target.put_new(KEY, &file).map_err(|e| target_error(&e))?;
    target.flush().map_err(|e| target_error(&e))?;
    Ok(key)
}

/// Joins sync on `target`: its sync key, unwrapped with `passphrase`.
/// Runs Argon2id; blocking.
///
/// # Errors
/// No key file, a wrong passphrase, or the target cannot be read.
pub fn join(target: &dyn Target, passphrase: &str) -> Result<SyncKey, SyncError> {
    target.refresh().map_err(|e| target_error(&e))?;
    let file = target
        .get(KEY, MAX_KEY_FILE)
        .map_err(|e| target_error(&e))?;
    SyncKey::unwrap(&file, passphrase).map_err(|e| SyncError::Sealed(e.to_string()))
}

impl SyncRemote for Remote {
    fn merge(&self, local: SyncData) -> BoxFuture<'_, Result<Merged, SyncError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || inner.merge(local))
                .await
                .map_err(|e| SyncError::Target(e.to_string()))?
        })
    }

    fn adopt(&self, part: SyncPart) -> BoxFuture<'_, Result<(), SyncError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || inner.adopt(part))
                .await
                .map_err(|e| SyncError::Target(e.to_string()))?
        })
    }

    fn publish(&self) -> BoxFuture<'_, Result<(), SyncError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || inner.publish())
                .await
                .map_err(|e| SyncError::Target(e.to_string()))?
        })
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The lists as their files hold them.
struct Encoded {
    hosts: Vec<u8>,
    snippets: Vec<u8>,
}

impl Encoded {
    /// O(H + S).
    fn of(data: &SyncData) -> Result<Self, SyncError> {
        let invalid = |e: String| SyncError::Invalid(e);
        Ok(Self {
            hosts: interchange::hosts_to_json(&data.hosts).map_err(invalid)?,
            snippets: interchange::snippets_to_json(&data.snippets).map_err(invalid)?,
        })
    }

    /// Their hash: the same lists hash alike. O(H + S).
    fn print(&self) -> String {
        use sha2::Digest as _;
        let mut hash = sha2::Sha256::new();
        hash.update(&self.hosts);
        hash.update([0]);
        hash.update(&self.snippets);
        hash.finalize()
            .iter()
            .fold(String::with_capacity(64), |mut hex, b| {
                let _ = write!(hex, "{b:02x}");
                hex
            })
    }

    /// Their flat records. O(H + S).
    fn flat(&self) -> Result<Flat, SyncError> {
        let parse = |bytes: &[u8]| {
            serde_json::from_slice::<Value>(bytes).map_err(|e| SyncError::Invalid(e.to_string()))
        };
        let mut flat = flatten(&parse(&self.hosts)?, &HOST_KINDS);
        flat.extend(flatten(&parse(&self.snippets)?, &SNIPPET_KINDS));
        Ok(flat)
    }
}

/// The lists a doc holds, repaired where the merge broke them.
/// Cuts each group's and host's lists a merge grew past what one may
/// hold, keeping the first by key (tags, variables) or by place (rules),
/// as every device does: the union of two devices' tags can pass 32.
fn cut_to_limits(lists: &mut Map<String, Value>) -> Vec<String> {
    let mut cut = Vec::new();
    for list in ["groups", "hosts"] {
        for record in lists
            .get_mut(list)
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            let name = record
                .get("label")
                .or_else(|| record.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let mut trim = |value: Option<&mut Value>, max: usize, what: &str| {
                if let Some(items) = value.and_then(Value::as_array_mut)
                    && items.len() > max
                {
                    items.truncate(max);
                    cut.push(format!(
                        "{name}: kept the first {max} {what} of both devices'"
                    ));
                }
            };
            trim(record.get_mut("tags"), tauminus_domain::MAX_TAGS, "tags");
            trim(
                record.get_mut("forwards"),
                tauminus_domain::MAX_FORWARDS,
                "forwarding rules",
            );
            trim(
                record.get_mut("settings").and_then(|s| s.get_mut("env")),
                tauminus_domain::MAX_ENV,
                "variables",
            );
        }
    }
    cut
}

fn lists_of(doc: &Doc) -> Result<(SyncData, Vec<tauminus_domain::Repair>, Vec<String>), SyncError> {
    let flat: Flat = doc
        .records
        .iter()
        .filter(|(_, r)| r.alive())
        .map(|(k, r)| (k.clone(), r.values()))
        .collect();
    let file = |version: u32, lists: Map<String, Value>| {
        let mut file = lists;
        file.insert("version".into(), Value::from(version));
        serde_json::to_vec(&Value::Object(file)).map_err(|e| SyncError::Invalid(e.to_string()))
    };
    let mut host_lists = unflatten(&flat, &HOST_KINDS);
    let cut = cut_to_limits(&mut host_lists);
    let hosts = file(interchange::HOSTS_VERSION, host_lists)?;
    let snippets = file(
        interchange::SNIPPETS_VERSION,
        unflatten(&flat, &SNIPPET_KINDS),
    )?;
    let (hosts, repairs) = interchange::hosts_from_json(&hosts).map_err(SyncError::Invalid)?;
    let snippets = interchange::snippets_from_json(&snippets).map_err(SyncError::Invalid)?;
    Ok((SyncData { hosts, snippets }, repairs, cut))
}

/// A record's name for the user: a host's label, else its name, else its
/// id.
fn record_name(doc: &Doc, key: &str) -> String {
    let values = doc.records.get(key).map(crate::doc::Record::values);
    values
        .as_ref()
        .and_then(|v| v.get("label").or_else(|| v.get("name")))
        .and_then(Value::as_str)
        .map_or_else(|| key.to_owned(), str::to_owned)
}

/// A device id as the user sees it: its first eight digits.
fn short(device: &str) -> &str {
    device.get(..8).unwrap_or(device)
}

fn shown(value: &Value) -> String {
    match value {
        Value::Null => "(none)".into(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

impl Inner {
    fn doc_path(&self) -> PathBuf {
        self.state_path.with_extension("doc")
    }

    fn staged_path(&self) -> PathBuf {
        self.state_path.with_extension("staged")
    }

    /// The state without its doc, and whether it was on disk.
    fn load(&self) -> Result<(State, bool), SyncError> {
        match std::fs::read(&self.state_path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(|state| (state, true))
                .map_err(|e| SyncError::Invalid(format!("the sync state: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((State::new()?, false)),
            Err(e) => Err(SyncError::Target(format!("the sync state: {e}"))),
        }
    }

    fn load_doc(path: &Path) -> Result<Option<Doc>, SyncError> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| SyncError::Invalid(format!("the sync state: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(SyncError::Target(format!("the sync state: {e}"))),
        }
    }

    /// This device's newest object on the target, and the names in its
    /// folder it did not write. A number past the state's is one a sync
    /// cut short wrote, or a state restored from a backup knows nothing
    /// of: it counts if it opens under the key and names this device.
    /// Anything else the target planted. O(n), plus a read for each name
    /// past the state's.
    fn own_objects(
        &self,
        state: &State,
        names: &[String],
    ) -> Result<(u64, Vec<String>), SyncError> {
        let mut own_seq = 0;
        let mut past: Vec<(u64, &String)> = Vec::new();
        for (name, (device, seq)) in names.iter().filter_map(|n| Some((n, parse_object(n)?))) {
            if device != state.device {
                continue;
            }
            // Left alone, but numbered past: a state restored from a backup
            // may hold a name since taken by this device's own object.
            if state.stuck.contains(name) {
                if seq > state.seq && seq - state.seq <= 1 << 32 {
                    own_seq = own_seq.max(seq);
                }
                continue;
            }
            if seq <= state.seq {
                own_seq = own_seq.max(seq);
            } else {
                past.push((seq, name));
            }
        }
        // Newest first: the first that opens is this device's last, and
        // every other past the state goes. At most so many are read: a
        // sync cut short leaves one or two; many more were planted.
        past.sort_unstable_by_key(|(seq, _)| std::cmp::Reverse(*seq));
        let mut forged = Vec::new();
        let mut found = false;
        for (n, (seq, name)) in past.into_iter().enumerate() {
            if found {
                forged.push(name.clone());
                continue;
            }
            // Unread, it may be this device's: numbering starts past it,
            // if it is within the numbers a device could have used.
            if n >= TRIES_PER_SYNC {
                if seq - state.seq <= 1 << 32 {
                    own_seq = own_seq.max(seq);
                }
                forged.push(name.clone());
                continue;
            }
            match self.read_object(&state.device, seq, name) {
                Ok(_) => {
                    own_seq = own_seq.max(seq);
                    found = true;
                }
                Err(error @ SyncError::Target(_)) => return Err(error),
                Err(_) => forged.push(name.clone()),
            }
        }
        Ok((own_seq, forged))
    }

    fn merge(&self, local: SyncData) -> Result<Merged, SyncError> {
        let (mut state, on_disk) = self.load()?;
        let now = (self.now)();
        self.target.refresh().map_err(|e| target_error(&e))?;
        let names = self.target.list().map_err(|e| target_error(&e))?;
        state.stuck.retain(|n| names.contains(n));
        let (own_seq, forged) = self.own_objects(&state, &names)?;
        let encoded = Encoded::of(&local)?;
        // Nothing edited here since the last sync, nothing new from the
        // others, this device's state on the target and fresh: the lists
        // stand, and the doc is not read.
        if forged.is_empty()
            && own_seq == state.seq
            && state.idle(now)
            && encoded.print() == state.fingerprint
            && nothing_new(&state, &names, now)
        {
            *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = None;
            return Ok(Merged {
                data: local,
                repairs: Vec::new(),
                clashes: Vec::new(),
                notices: Vec::new(),
            });
        }
        // A state that was not on disk has no files beside it either: any
        // there are another state's.
        let mut lost_base = false;
        if on_disk {
            match Self::load_doc(&self.doc_path())? {
                Some(doc) => state.doc = doc,
                // The base went (a backup that kept one file): this device
                // joins again, as a first sync, taking the target's values.
                None if state.joined => {
                    state.joined = false;
                    lost_base = true;
                }
                None => {}
            }
        }
        let staged = if on_disk && state.staged {
            Self::load_doc(&self.staged_path())?
        } else {
            None
        };
        let mut read = self.read_others(&state, &names, now)?;
        let mut notices = std::mem::take(&mut read.notices);
        if lost_base {
            notices.push(
                "this device's sync state was incomplete; the sync target's values were kept"
                    .into(),
            );
        }
        let (merged, found, edited) = join_docs(
            &mut state,
            &read,
            &encoded,
            staged.as_ref(),
            now,
            &mut notices,
        )?;
        drop(staged);
        let (data, repairs, cut) = lists_of(&merged)?;
        notices.extend(cut);
        let saved = Encoded::of(&data)?;
        // The base is the merged state as saved here: a value the checks
        // normalized, or a repair, is a change of this device's, stamped
        // so the others take it too.
        let (base, normalized) =
            stamp_with(merged, None, &saved.flat()?, &mut state.clock, now, true);
        let changed = base != state.doc;
        // An edit of this device's goes out now. A base only the others
        // changed goes out too, a little later: an object holding what was
        // deleted or replaced since would keep it on the target.
        let own = edited || !normalized.is_empty() || !found.is_empty();
        if !own && changed && state.echo_due.is_none() {
            state.echo_due = Some(now.saturating_add(ECHO_DELAY_MS));
        }
        let told = with_untold(&state.untold, found);
        let mut shown_clashes: Vec<SyncClash> = told.iter().map(|c| sync_clash(&base, c)).collect();
        shown_clashes.extend(read.told.iter().map(|c| sync_clash(&base, c)));
        // Kept ahead of the lists: a sync stopped after saving them reads
        // their values as this merge's. A base unchanged needs none.
        if changed {
            save_json(&self.staged_path(), &base)?;
            state.staged = true;
        }
        // Owed from now: a base saved without the header after it still
        // publishes.
        if own {
            state.published = false;
        }
        save_json(&self.state_path, &state)?;
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(Pending {
            state,
            base,
            clashes: told,
            own_seq,
            own,
            seen: read.seen,
            failed: read.failed,
            forged,
            fingerprint: saved.print(),
            adopted: BTreeSet::new(),
        });
        Ok(Merged {
            data,
            repairs,
            clashes: shown_clashes,
            notices,
        })
    }

    /// Each other device's newest readable object, joined; what they told
    /// this device it lost; and what the user should know.
    fn read_others(&self, state: &State, names: &[String], now: u64) -> Result<Read, SyncError> {
        let mut objects = others_objects(&state.device, names);
        let mut read = Read {
            seen: state.seen.clone(),
            failed: still_listed(&state.failed, &objects),
            ..Read::default()
        };
        // Told once, then forgotten: its objects, should they return, are
        // read as new.
        for device in state.seen.keys() {
            if !objects.contains_key(device) {
                read.notices.push(format!(
                    "device {} has no objects on the sync target any more",
                    short(device)
                ));
                read.seen.remove(device);
            }
        }
        let mut failures: Vec<(String, usize, SyncError)> = Vec::new();
        // Devices read before are outside the budget: ids a target
        // invents cannot crowd them out.
        let (mut budget, mut untried) = (TRIES_PER_SYNC, 0);
        for (device, list) in &mut objects {
            list.sort_unstable_by_key(|(seq, _)| std::cmp::Reverse(*seq));
            let seen = state.seen.get(device).copied();
            let known = seen.is_some();
            let (mut failed, mut last_error) = (0, None);
            for (seq, name) in list.iter() {
                // Read before: what it held is in the base already.
                if seen == Some(*seq) {
                    break;
                }
                // An older object than one read before is a rollback.
                if seen.is_some_and(|seen| *seq < seen) {
                    read.notices.push(format!(
                        "device {}'s newest object is older than one read before",
                        short(device)
                    ));
                    break;
                }
                if failed_lately(&read.failed, device, *seq, now) {
                    continue;
                }
                if !known && budget == 0 {
                    untried += 1;
                    break;
                }
                if failed == TRIES {
                    break;
                }
                match self.read_object(device, *seq, name) {
                    // An object not written for longer than a tombstone
                    // lives is a device gone: it may hold what was
                    // deleted since, and its older ones more so.
                    Ok(payload) if now.saturating_sub(payload.written) >= TOMBSTONE_MS => {
                        read.notices.push(format!(
                            "device {} has not synced for 90 days; its object is not read",
                            short(device)
                        ));
                        break;
                    }
                    Ok(payload) => {
                        read.theirs.merge(&payload.doc);
                        read.told.extend(
                            payload
                                .clashes
                                .into_iter()
                                .filter(|c| c.loser == state.device)
                                .take(MAX_CLASHES),
                        );
                        read.seen.insert(device.clone(), *seq);
                        break;
                    }
                    Err(newer @ SyncError::Newer(_)) => return Err(newer),
                    // The target could not be read: tried again next time.
                    Err(error @ SyncError::Target(_)) => {
                        last_error = Some(error);
                        break;
                    }
                    // A damaged or forged object: not tried again, and the
                    // device's next older one is read instead.
                    // Said the first time only.
                    Err(error) => {
                        let entry = read.failed.entry(device.clone()).or_default();
                        let tries = entry.get(seq).map_or(0, |(_, t)| *t);
                        entry.insert(*seq, (now, tries.saturating_add(1)));
                        failed += 1;
                        if !known {
                            budget -= 1;
                        }
                        if tries > 0 {
                            continue;
                        }
                        last_error = Some(error);
                    }
                }
            }
            if let Some(error) = last_error {
                failures.push((device.clone(), failed, error));
            }
        }
        read.notices.extend(failure_notices(failures));
        if untried > 0 {
            read.notices.push(format!(
                "{untried} device(s) new to this one were not read this time"
            ));
        }
        Ok(read)
    }

    /// The payload one object holds, checked: sealed with this key under
    /// its name, of a version this build reads, naming itself.
    fn read_object(&self, device: &str, seq: u64, name: &str) -> Result<Payload, SyncError> {
        // A file too large, or not a plain file, is a damaged object; the
        // target failing to send one is not.
        let sealed = self.target.get(name, MAX_OBJECT).map_err(|e| {
            if matches!(
                e.kind(),
                std::io::ErrorKind::InvalidData | std::io::ErrorKind::PermissionDenied
            ) {
                SyncError::Invalid(format!("{name}: {e}"))
            } else {
                target_error(&e)
            }
        })?;
        let packed = self
            .key
            .open(name, &sealed)
            .map_err(|e| SyncError::Sealed(e.to_string()))?;
        let plain = unpack(&packed, MAX_PLAIN)?;
        let invalid = |e: serde_json::Error| {
            SyncError::Invalid(format!("an object from another device: {e}"))
        };
        // The versions first: a newer format's doc may not parse here.
        let header: Header = serde_json::from_slice(&plain).map_err(invalid)?;
        if header.schema > SCHEMA
            || header.hosts_version > interchange::HOSTS_VERSION
            || header.snippets_version > interchange::SNIPPETS_VERSION
        {
            return Err(SyncError::Newer(format!(
                "format {}, hosts {}",
                header.schema, header.hosts_version
            )));
        }
        let payload: Payload = serde_json::from_slice(&plain).map_err(invalid)?;
        if payload.device != device || payload.seq != seq {
            return Err(SyncError::Sealed(format!("{name} names another object")));
        }
        Ok(payload)
    }

    /// Takes `part` of the merged state into the base on disk: it is what
    /// this device now holds.
    fn adopt(&self, part: SyncPart) -> Result<(), SyncError> {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(pending) = pending.as_mut() else {
            return Ok(());
        };
        let (name, kinds): (&'static str, &[&str]) = match part {
            SyncPart::Hosts => ("hosts", &["group:", "host:"]),
            SyncPart::Snippets => ("snippets", &["snippet:"]),
        };
        let of_part = |key: &String| kinds.iter().any(|k| key.starts_with(k));
        pending.adopted.insert(name);
        let whole = pending.adopted.len() == 2;
        // Compared in place, O(R), no copy.
        let mut doc_changed = !pending
            .state
            .doc
            .records
            .iter()
            .filter(|(k, _)| of_part(k))
            .eq(pending.base.records.iter().filter(|(k, _)| of_part(k)));
        if whole {
            // The other part is in already: the base, whole, is the doc.
            doc_changed = doc_changed || pending.state.doc != pending.base;
            if doc_changed {
                pending.state.doc = std::mem::take(&mut pending.base);
            }
        } else if doc_changed {
            pending.state.doc.records.retain(|k, _| !of_part(k));
            let records: Vec<_> = pending
                .base
                .records
                .iter()
                .filter(|(k, _)| of_part(k))
                .map(|(k, r)| (k.clone(), r.clone()))
                .collect();
            pending.state.doc.records.extend(records);
        }
        if pending.own {
            pending.state.published = false;
        }
        pending.state.untold.clone_from(&pending.clashes);
        // What was read counts as read once all of it is saved here.
        if whole {
            pending.state.seen.clone_from(&pending.seen);
            pending.state.failed.clone_from(&pending.failed);
            pending.state.fingerprint.clone_from(&pending.fingerprint);
            pending.state.joined = true;
            pending.state.staged = false;
        }
        if doc_changed {
            save_json(&self.doc_path(), &pending.state.doc)?;
        }
        save_json(&self.state_path, &pending.state)?;
        if whole {
            let _ = std::fs::remove_file(self.staged_path());
        }
        Ok(())
    }

    fn publish(&self) -> Result<(), SyncError> {
        let Some(Pending {
            mut state,
            own_seq,
            adopted,
            forged,
            ..
        }) = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return Ok(());
        };
        // Only a sync saved whole publishes.
        if adopted.len() < 2 {
            return Ok(());
        }
        let now = (self.now)();
        // A sync with nothing new to write, this device's state on the
        // target already and fresh, writes nothing.
        if state.published
            && own_seq == state.seq
            && state.untold.is_empty()
            && forged.is_empty()
            && state
                .echo_due
                .is_none_or(|due| now < due && due <= now.saturating_add(ECHO_DELAY_MS))
            && now.saturating_sub(state.written) < KEEPALIVE_MS
        {
            return Ok(());
        }
        // What the target planted goes first: it may hold the next name.
        // One that will not go (a folder it cannot empty) is stepped past.
        for name in &forged {
            if let Err(error) = self.target.remove(name) {
                tracing::warn!(%error, "a file planted on the sync target stays");
                if state.stuck.len() < TRIES_PER_SYNC {
                    state.stuck.insert(name.clone());
                }
            }
        }
        let forged: BTreeSet<String> = forged.into_iter().collect();
        // Past any object a sync cut short left behind.
        let next = |seq: u64| {
            seq.checked_add(1)
                .ok_or_else(|| SyncError::Invalid("this device's object number ran out".into()))
        };
        state.seq = next(state.seq.max(own_seq))?;
        // Past numbers the target held forged ones at: another device may
        // have found one failed, and would not read it again soon.
        while {
            let name = object_name(&state.device, state.seq);
            forged.contains(&name) || state.stuck.contains(&name)
        } {
            state.seq = next(state.seq)?;
        }
        // Only those ahead need keeping: numbering passes the rest anyway.
        let seq = state.seq;
        state
            .stuck
            .retain(|n| parse_object(n).is_some_and(|(_, s)| s > seq));
        // A name taken by something no listing shows (a folder planted
        // there) is skipped, a few times.
        let mut taken = 0;
        loop {
            let name = object_name(&state.device, state.seq);
            let plain = zeroize::Zeroizing::new(
                serde_json::to_vec(&PayloadOut::of(&state, now))
                    .map_err(|e| SyncError::Invalid(e.to_string()))?,
            );
            let sealed = self
                .key
                .seal(&name, &pack(&plain)?)
                .map_err(|e| SyncError::Sealed(e.to_string()))?;
            match self.target.put_new(&name, &sealed) {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && taken < 3 => {
                    taken += 1;
                    state.seq = next(state.seq)?;
                }
                Err(e) => return Err(target_error(&e)),
            }
        }
        let older: BTreeSet<String> = self
            .target
            .list()
            .map_err(|e| target_error(&e))?
            .into_iter()
            .filter(|n| parse_object(n).is_some_and(|(d, s)| d == state.device && s != state.seq))
            .collect();
        // The new object is out; one older that will not go is left.
        for old in &older {
            if let Err(error) = self.target.remove(old) {
                tracing::warn!(%error, "an older object stays on the sync target");
            }
        }
        self.target.flush().map_err(|e| target_error(&e))?;
        state.published = true;
        state.written = now;
        state.untold.clear();
        state.echo_due = None;
        save_json(&self.state_path, &state)
    }
}

/// The versions an object says it holds, read ahead of the rest.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Header {
    schema: u32,
    hosts_version: u32,
    snippets_version: u32,
}

/// An object's payload, deflated: JSON of hosts compresses about 20 to 1,
/// and the target sees the size either way.
pub(crate) fn pack(plain: &[u8]) -> Result<zeroize::Zeroizing<Vec<u8>>, SyncError> {
    use std::io::Write as _;
    // Room up front, so the buffer seldom moves and leaves copies behind.
    let mut encoder = flate2::write::DeflateEncoder::new(
        Vec::with_capacity(plain.len() / 4),
        flate2::Compression::fast(),
    );
    encoder
        .write_all(plain)
        .and_then(|()| encoder.finish())
        .map(zeroize::Zeroizing::new)
        .map_err(|e| SyncError::Invalid(e.to_string()))
}

/// Inflates a payload, refusing one past `limit` bytes. O(limit).
fn unpack(packed: &[u8], limit: u64) -> Result<zeroize::Zeroizing<Vec<u8>>, SyncError> {
    use std::io::Read as _;
    let mut plain = zeroize::Zeroizing::new(Vec::new());
    flate2::read::DeflateDecoder::new(packed)
        .take(limit + 1)
        .read_to_end(&mut plain)
        .map_err(|e| SyncError::Invalid(format!("an object from another device: {e}")))?;
    if plain.len() as u64 > limit {
        return Err(SyncError::Invalid(
            "an object from another device is too large".into(),
        ));
    }
    Ok(plain)
}

/// A first sync's records: one the target holds already keeps the
/// target's values, and each of this device's that lost is a clash; what
/// only this device holds stays a change of its own. O(c log n).
fn take_theirs(
    theirs: &Doc,
    mine: &mut Doc,
    changed: &mut BTreeSet<(String, String)>,
    me: &str,
    found: &mut Vec<Clash>,
) {
    for (key, field) in std::mem::take(changed) {
        let cell = theirs.records.get(&key).and_then(|r| r.fields.get(&field));
        match (cell, mine.records.get_mut(&key)) {
            (Some(cell), Some(record)) => {
                if let Some(lost) = record.fields.insert(field.clone(), cell.clone()) {
                    found.push(Clash {
                        record: key.clone(),
                        field,
                        kept: cell.value.clone(),
                        lost: lost.value,
                        loser: me.to_owned(),
                    });
                }
            }
            _ => {
                changed.insert((key, field));
            }
        }
    }
}

/// What reading the other devices' objects gave.
#[derive(Default)]
struct Read {
    theirs: Doc,
    told: Vec<Clash>,
    notices: Vec<String>,
    seen: BTreeMap<String, u64>,
    failed: Failed,
}

/// This device's lists stamped against its base, joined with what the
/// others hold: the merged doc, and the clashes this device's changes
/// met. A first sync joins what the target holds: a value both have keeps
/// the target's stamp, and a record only the target has is not taken for
/// one deleted here. O(n log n) for n fields.
fn join_docs(
    state: &mut State,
    read: &Read,
    encoded: &Encoded,
    staged: Option<&Doc>,
    now: u64,
    notices: &mut Vec<String>,
) -> Result<(Doc, Vec<Clash>, bool), SyncError> {
    let first = !state.joined;
    let base = if first {
        read.theirs.clone()
    } else {
        state.doc.clone()
    };
    let (mut mine, mut changed) = stamp_with(
        base,
        staged,
        &encoded.flat()?,
        &mut state.clock,
        now,
        !first,
    );
    let mut found = Vec::new();
    if first {
        take_theirs(
            &read.theirs,
            &mut mine,
            &mut changed,
            &state.device,
            &mut found,
        );
    }
    // What clashes need of this device's side: the records it changed.
    // Each record once: `changed` holds it once per field. O(c + r·s).
    let mut keys: Vec<&String> = changed.iter().map(|(k, _)| k).collect();
    keys.dedup();
    let local_changed = Doc {
        records: keys
            .into_iter()
            .filter_map(|k| Some((k.clone(), mine.records.get(k)?.clone())))
            .collect(),
    };
    let mut merged = mine;
    merged.merge(&read.theirs);
    if first {
        // What a first sync cut short already took in.
        merged.merge(&state.doc);
    }
    // What was deleted leaves the target in time.
    merged.expire(now, TOMBSTONE_MS);
    if let Some(latest) = merged.latest() {
        if latest.ms > now.saturating_add(MAX_DRIFT_MS) {
            notices.push(format!(
                "device {}'s clock runs ahead of this one's",
                short(&latest.device)
            ));
        } else {
            state.clock.observe(latest);
        }
    }
    found.extend(clashes(
        &state.device,
        &state.doc,
        &local_changed,
        &read.theirs,
        &merged,
        &changed,
    ));
    drop(local_changed);
    Ok((merged, found, !changed.is_empty()))
}

/// Clashes from a sync that never got out, with this one's after them,
/// each once, at most [`MAX_CLASHES`]. O(c²) for c ≤ 256.
fn with_untold(untold: &[Clash], found: Vec<Clash>) -> Vec<Clash> {
    let mut told = untold.to_vec();
    for clash in found {
        if !told.contains(&clash) {
            told.push(clash);
        }
    }
    told.truncate(MAX_CLASHES);
    told
}

/// The other devices' objects on the target, by device. O(n log n).
fn others_objects(me: &str, names: &[String]) -> BTreeMap<String, Vec<(u64, String)>> {
    let mut objects: BTreeMap<String, Vec<(u64, String)>> = BTreeMap::new();
    for name in names {
        if let Some((device, seq)) = parse_object(name)
            && device != me
        {
            objects
                .entry(device.to_owned())
                .or_default()
                .push((seq, name.clone()));
        }
    }
    objects
}

/// The failures the target still lists; the rest are forgotten.
fn still_listed(failed: &Failed, objects: &BTreeMap<String, Vec<(u64, String)>>) -> Failed {
    failed
        .iter()
        .filter_map(|(device, seqs)| {
            let list: BTreeSet<u64> = objects.get(device)?.iter().map(|(s, _)| *s).collect();
            let listed: BTreeMap<u64, (u64, u32)> = seqs
                .iter()
                .filter(|(seq, _)| list.contains(seq))
                .map(|(seq, at)| (*seq, *at))
                .collect();
            (!listed.is_empty()).then(|| (device.clone(), listed))
        })
        .collect()
}

/// One line each for a few devices whose objects were not read; one line
/// for many.
fn failure_notices(failures: Vec<(String, usize, SyncError)>) -> Vec<String> {
    if failures.len() > 4 {
        return vec![format!(
            "objects from {} devices were not read; the sync target may hold files it was not given",
            failures.len()
        )];
    }
    failures
        .into_iter()
        .map(|(device, failed, error)| {
            format!(
                "{} object(s) from device {} were not read: {error}",
                failed.max(1),
                short(&device)
            )
        })
        .collect()
}

/// Whether the target lists nothing this device has not read or found
/// failed, and still lists every device it read. O(n log d).
fn nothing_new(state: &State, names: &[String], now: u64) -> bool {
    let mut listed = BTreeSet::new();
    for (device, seq) in names.iter().filter_map(|n| parse_object(n)) {
        if device == state.device {
            continue;
        }
        listed.insert(device);
        let known = state.seen.get(device).is_some_and(|seen| seq <= *seen)
            || failed_lately(&state.failed, device, seq, now);
        if !known {
            return false;
        }
    }
    state.seen.keys().all(|d| listed.contains(d.as_str()))
}

fn sync_clash(doc: &Doc, clash: &Clash) -> SyncClash {
    SyncClash {
        record: record_name(doc, &clash.record),
        field: clash.field.clone(),
        kept: shown(&clash.kept),
        lost: shown(&clash.lost),
    }
}

/// Writes the base beside the other files, readable by this user only:
/// under a fresh name, created new, then moved into place.
fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<(), SyncError> {
    use std::io::Write as _;
    let bytes = zeroize::Zeroizing::new(
        serde_json::to_vec(value).map_err(|e| SyncError::Invalid(e.to_string()))?,
    );
    let failed = |e: std::io::Error| SyncError::Target(format!("the sync state: {e}"));
    let mut suffix = [0u8; 8];
    rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut suffix)
        .map_err(|_| SyncError::Target("no randomness from the system".into()))?;
    let partial = path.with_extension(format!("partial-{}", u64::from_le_bytes(suffix)));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let written = options.open(&partial).and_then(|mut file| {
        file.write_all(&bytes)?;
        file.sync_all()
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&partial);
        return Err(failed(e));
    }
    std::fs::rename(&partial, path).map_err(|e| {
        let _ = std::fs::remove_file(&partial);
        failed(e)
    })
}

#[cfg(test)]
mod tests;
