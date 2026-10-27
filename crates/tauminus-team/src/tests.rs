// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

fn device(user: &str) -> DeviceKeys {
    DeviceKeys::generate(UserId::new(user).unwrap()).unwrap()
}

fn user(id: &str) -> UserId {
    UserId::new(id).unwrap()
}

/// A vault as the Hub holds it: every entry and item, and the log an
/// admin's device verified.
struct Vault {
    entries: Vec<Entry>,
    items: Vec<SealedItem>,
    log: Log,
}

impl Vault {
    fn new(admin: &DeviceKeys) -> Self {
        let (log, first) = Log::create(VaultId::random().unwrap(), admin).unwrap();
        Self {
            entries: vec![first],
            items: Vec::new(),
            log,
        }
    }

    fn ring(&self, keys: &DeviceKeys) -> Keyring {
        Keyring::open(&self.log, keys).unwrap()
    }

    fn change(&mut self, entry: Entry) {
        self.log.extend([&entry]).unwrap();
        self.entries.push(entry);
    }

    fn add(&mut self, admin: &DeviceKeys, member: &DeviceKeys, role: Role, credentials: bool) {
        let ring = self.ring(admin);
        let entry = self
            .log
            .add_member(admin, &ring, member.card(), role, credentials)
            .unwrap();
        self.change(entry);
    }

    fn remove(&mut self, admin: &DeviceKeys, member: &str) {
        let ring = self.ring(admin);
        let entry = self
            .log
            .remove_member(admin, &ring, &user(member), &self.items)
            .unwrap();
        self.change(entry);
    }

    fn set_access(&mut self, admin: &DeviceKeys, member: &str, role: Role, credentials: bool) {
        let ring = self.ring(admin);
        let entry = self
            .log
            .set_access(
                admin,
                &ring,
                &user(member),
                (role, credentials),
                &self.items,
            )
            .unwrap();
        self.change(entry);
    }

    /// A member's device verifying the whole log the Hub serves.
    fn seen_by(&self, keys: &DeviceKeys) -> (Log, Keyring) {
        let log = Log::verify(&self.entries[0].hash(), &self.entries).unwrap();
        let ring = Keyring::open(&log, keys).unwrap();
        (log, ring)
    }

    fn write(&mut self, writer: &DeviceKeys, id: u8, kind: KeyKind, plain: &str) -> usize {
        let (log, ring) = self.seen_by(writer);
        let item = seal_item(
            &log,
            &ring,
            writer,
            [id; 16],
            kind,
            u64::from(id),
            plain.as_bytes(),
        )
        .unwrap();
        self.items.push(item);
        self.items.len() - 1
    }
}

fn read(log: &Log, ring: &Keyring, item: &SealedItem) -> Result<String, TeamError> {
    open_item(log, ring, item).map(|plain| String::from_utf8(plain.to_vec()).unwrap())
}

/// An item sealed with `ring`'s key and signed by `writer`, without
/// `seal_item`'s checks.
fn forge(writer: &DeviceKeys, ring: &Keyring, header: ItemHeader) -> SealedItem {
    let key = ring.key(header.kind, header.epoch).unwrap();
    let mut item = SealedItem {
        header,
        nonce: [0; 24],
        sealed: Vec::new(),
        signature: [0; 64],
    };
    let aad = item::header_bytes(&item.header);
    let (nonce, sealed) = seal::seal(key, &aad, b"ProxyCommand evil").unwrap();
    item.nonce = nonce;
    item.sealed = sealed;
    item.signature = writer.sign(&item::signed(&item));
    item
}

/// A header naming `log`'s newest entry.
fn header(log: &Log, writer: &DeviceKeys, kind: KeyKind, epoch: u32) -> ItemHeader {
    let (seq, head) = log.head();
    ItemHeader {
        vault: log.vault(),
        item: [7; 16],
        kind,
        epoch,
        seq,
        head,
        writer: writer.id(),
        counter: 99,
    }
}

fn resign(signer: &DeviceKeys, mut entry: Entry) -> Entry {
    let bytes = log::signed_for_tests(&entry);
    entry.signature = signer.sign(&bytes);
    entry
}

#[test]
fn a_revoked_member_cannot_open_what_is_written_after_the_revoke() {
    let admin = device("admin@example.com");
    let carol = device("carol@example.com");
    let mut vault = Vault::new(&admin);
    vault.add(&admin, &carol, Role::Edit, true);
    let before = vault.write(&admin, 1, KeyKind::Data, "db-1.internal");
    let secret_before = vault.write(&admin, 2, KeyKind::Secrets, "hunter2");
    let carol_wrote = vault.write(&carol, 5, KeyKind::Data, "db-carol.internal");

    // Every key carol held before the revoke.
    let (_, carol_ring_before) = vault.seen_by(&carol);

    vault.remove(&admin, "carol@example.com");
    let after = vault.write(&admin, 3, KeyKind::Data, "db-2.internal");
    let secret_after = vault.write(&admin, 4, KeyKind::Secrets, "correct horse");

    // Carol with everything the Hub holds: the whole log and every item.
    let (log, carol_ring) = vault.seen_by(&carol);
    assert_eq!(
        read(&log, &carol_ring, &vault.items[before]).unwrap(),
        "db-1.internal"
    );
    assert_eq!(
        read(&log, &carol_ring, &vault.items[secret_before]).unwrap(),
        "hunter2"
    );
    for (i, ring) in [&carol_ring, &carol_ring_before].into_iter().enumerate() {
        for item in [after, secret_after] {
            assert_eq!(
                read(&log, ring, &vault.items[item]),
                Err(TeamError::NoKey(1)),
                "ring {i}"
            );
        }
    }
    // And each key she ever held, tried on the new ciphertexts directly
    // with their own associated data. The same call opens what she could
    // read, so the check can fail.
    for kind in [KeyKind::Data, KeyKind::Secrets] {
        let key = carol_ring.key(kind, 0).unwrap();
        for item in [&vault.items[after], &vault.items[secret_after]] {
            let aad = item::header_bytes(&item.header);
            assert!(seal::open(key, &aad, &item.nonce, &item.sealed).is_err());
        }
    }
    let old = &vault.items[before];
    let aad = item::header_bytes(&old.header);
    let data = carol_ring.key(KeyKind::Data, 0).unwrap();
    assert!(seal::open(data, &aad, &old.nonce, &old.sealed).is_ok());
    // No key of the new epoch is sealed to her device.
    assert!(
        vault
            .entries
            .iter()
            .flat_map(|e| e.op.wraps())
            .all(|w| w.device != carol.id() || w.epoch == 0)
    );
    // The admin still reads all five, carol's own host among them.
    let (log, ring) = vault.seen_by(&admin);
    for item in &vault.items {
        assert!(read(&log, &ring, item).is_ok());
    }
    assert_eq!(
        read(&log, &ring, &vault.items[carol_wrote]).unwrap(),
        "db-carol.internal"
    );
}

#[test]
fn a_removed_or_demoted_writer_cannot_backdate_a_write() {
    let admin = device("admin@example.com");
    let bob = device("bob@example.com");
    let carol = device("carol@example.com");
    let mut vault = Vault::new(&admin);
    vault.add(&admin, &bob, Role::Edit, true);
    vault.add(&admin, &carol, Role::Edit, true);
    // The log and keys each held while still allowed to write.
    let (then, bob_ring) = vault.seen_by(&bob);
    let (_, carol_ring) = vault.seen_by(&carol);
    let honest = vault.write(&bob, 1, KeyKind::Data, "db-1");

    vault.remove(&admin, "bob@example.com");
    vault.set_access(&admin, "carol@example.com", Role::View, true);
    // Bob's version is kept by his removal, and not listed again after.
    let kept = |entry: &Entry| match &entry.op {
        Op::RemoveMember { kept, .. } | Op::SetAccess { kept, .. } => kept.clone(),
        _ => Vec::new(),
    };
    let n = vault.entries.len();
    assert_eq!(kept(&vault.entries[n - 2]), [vault.items[honest].digest()]);
    assert_eq!(kept(&vault.entries[n - 1]), Vec::<Hash>::new());

    let (log, ring) = vault.seen_by(&admin);
    for (writer, ring_then) in [(&bob, &bob_ring), (&carol, &carol_ring)] {
        for kind in [KeyKind::Data, KeyKind::Secrets] {
            let backdated = forge(writer, ring_then, header(&then, writer, kind, 0));
            assert_eq!(
                read(&log, &ring, &backdated),
                Err(TeamError::NotAllowed(
                    "the writer may no longer write, and no admin kept this version"
                ))
            );
        }
    }
    // What bob wrote while he could, the admin kept: it still opens, for
    // the admin and for a member who joins later.
    assert_eq!(read(&log, &ring, &vault.items[honest]).unwrap(), "db-1");
    let dave = device("dave@example.com");
    vault.add(&admin, &dave, Role::View, false);
    let (log, dave_ring) = vault.seen_by(&dave);
    assert_eq!(
        read(&log, &dave_ring, &vault.items[honest]).unwrap(),
        "db-1"
    );
}

#[test]
fn the_hub_holds_only_ciphertext() {
    let admin = device("admin@example.com");
    let bob = device("bob@example.com");
    let mut vault = Vault::new(&admin);
    vault.add(&admin, &bob, Role::Edit, true);
    let canaries = [
        "canary-host.prod.example.net",
        "canary-p4ssw0rd-7f3a",
        "-----BEGIN OPENSSH PRIVATE KEY-----canary",
        "canary-snippet systemctl restart",
    ];
    vault.write(&admin, 1, KeyKind::Data, canaries[0]);
    vault.write(&admin, 2, KeyKind::Secrets, canaries[1]);
    vault.write(&bob, 3, KeyKind::Secrets, canaries[2]);
    vault.write(&bob, 4, KeyKind::Data, canaries[3]);
    vault.remove(&admin, "bob@example.com");

    let mut stored: Vec<u8> = Vec::new();
    for entry in &vault.entries {
        stored.extend(entry.to_bytes());
    }
    for item in &vault.items {
        stored.extend(item.to_bytes());
    }
    let ring = vault.ring(&admin);
    let mut needles: Vec<Vec<u8>> = Vec::new();
    for canary in canaries {
        needles.push(canary.as_bytes().to_vec());
        needles.push(hex(canary.as_bytes()).into_bytes());
        needles.push(canary.encode_utf16().flat_map(u16::to_le_bytes).collect());
    }
    for kind in [KeyKind::Data, KeyKind::Secrets] {
        for epoch in 0..=1 {
            needles.push(ring.key(kind, epoch).unwrap().to_vec());
        }
    }
    for needle in needles {
        assert!(
            !stored.windows(needle.len()).any(|w| w == needle.as_slice()),
            "the stored bytes hold {}",
            String::from_utf8_lossy(&needle)
        );
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[test]
fn a_key_the_hub_adds_or_swaps_is_refused() {
    let admin = device("admin@example.com");
    let bob = device("bob@example.com");
    let hub = device("bob@example.com");
    let mut vault = Vault::new(&admin);
    let ring = vault.ring(&admin);
    let add = vault
        .log
        .add_member(&admin, &ring, bob.card(), Role::Edit, false)
        .unwrap();

    // The Hub swaps bob's card for its own key in the admin's entry.
    let mut swapped = add.clone();
    if let Op::AddMember { card, .. } = &mut swapped.op {
        *card = hub.card().clone();
    }
    assert_eq!(
        vault.log.clone().extend([&swapped]),
        Err(TeamError::BadSignature)
    );

    // The Hub signs an entry of its own: its device is in no vault.
    let forged = vault
        .log
        .add_member(&hub, &ring, hub.card(), Role::Admin, true)
        .unwrap();
    assert!(matches!(
        vault.log.clone().extend([&forged]),
        Err(TeamError::NotAllowed(_))
    ));

    // A member who is no admin cannot add a key either.
    vault.change(add);
    let bob_ring = vault.ring(&bob);
    let by_bob = vault.log.add_device(&bob, &bob_ring, hub.card()).unwrap();
    assert!(matches!(
        vault.log.clone().extend([&by_bob]),
        Err(TeamError::NotAllowed(_))
    ));
}

#[test]
fn a_log_the_hub_forks_or_rewrites_is_refused() {
    let admin = device("admin@example.com");
    let bob = device("bob@example.com");
    let eve = device("eve@example.com");
    let mut vault = Vault::new(&admin);
    let ring = vault.ring(&admin);
    let add_bob = vault
        .log
        .add_member(&admin, &ring, bob.card(), Role::Edit, false)
        .unwrap();
    let add_eve = vault
        .log
        .add_member(&admin, &ring, eve.card(), Role::Edit, false)
        .unwrap();
    vault.change(add_bob);

    // Two valid entries at the same position: the Hub shows bob's to one
    // device and eve's to another. Each refuses the other's entry, and
    // items written against the other branch.
    let other_entries = [vault.entries[0].clone(), add_eve.clone()];
    let other = Log::verify(&vault.entries[0].hash(), &other_entries).unwrap();
    let eve_ring = Keyring::open(&other, &eve).unwrap();
    let on_other = seal_item(&other, &eve_ring, &eve, [9; 16], KeyKind::Data, 1, b"x").unwrap();
    let (log, ring) = vault.seen_by(&admin);
    assert_eq!(read(&log, &ring, &on_other), Err(TeamError::Fork(1)));
    assert_eq!(
        vault.log.clone().extend([&add_eve]),
        Err(TeamError::Fork(1))
    );

    // A log that starts elsewhere does not match the pin.
    let (_, elsewhere) = Log::create(vault.log.vault(), &admin).unwrap();
    assert_eq!(
        Log::verify(&vault.entries[0].hash(), [&elsewhere]).unwrap_err(),
        TeamError::Fork(0)
    );
    // An entry changed after it was signed.
    let mut changed = vault.entries.clone();
    if let Op::AddMember { role, .. } = &mut changed[1].op {
        *role = Role::Admin;
    }
    assert!(Log::verify(&vault.entries[0].hash(), &changed).is_err());

    // Entries the device already holds, sent again, change nothing; one
    // past a gap is refused as such.
    let head = vault.log.head();
    vault.log.extend(&vault.entries).unwrap();
    assert_eq!(vault.log.head(), head);
    let ring = vault.ring(&admin);
    let first = vault
        .log
        .set_access(
            &admin,
            &ring,
            &user("bob@example.com"),
            (Role::View, false),
            &[],
        )
        .unwrap();
    let mut ahead = vault.log.clone();
    ahead.extend([&first]).unwrap();
    let ring = Keyring::open(&ahead, &admin).unwrap();
    let second = ahead
        .remove_member(&admin, &ring, &user("bob@example.com"), &[])
        .unwrap();
    assert_eq!(
        vault.log.clone().extend([&second]),
        Err(TeamError::BadEntry("an entry is missing before this one"))
    );
}

#[test]
fn a_removed_admin_cannot_hand_a_joiner_another_log() {
    let admin = device("admin@example.com");
    let eve = device("eve@example.com");
    let hub = device("hub@example.com");
    let mut vault = Vault::new(&admin);
    vault.add(&admin, &eve, Role::Admin, true);
    let before_removal = vault.entries.clone();
    let eve_log = vault.log.clone();
    vault.remove(&admin, "eve@example.com");

    // Eve signs her own next entry off the log as she last held it,
    // adding a member of the Hub's choosing.
    let eve_ring = Keyring::open(&eve_log, &eve).unwrap();
    let branch = eve_log
        .add_member(&eve, &eve_ring, hub.card(), Role::Admin, true)
        .unwrap();
    let forked: Vec<Entry> = before_removal.into_iter().chain([branch]).collect();

    // Pinned to the first entry alone, a joiner would take it; with the
    // newest entry the invite carries, it refuses.
    assert!(Log::verify(&forked[0].hash(), &forked).is_ok());
    let invite = vault.log.invite();
    assert_eq!(
        Log::join(&invite, &forked).unwrap_err(),
        TeamError::Fork(invite.seq)
    );
    assert!(Log::join(&invite, &vault.entries).is_ok());
}

#[test]
fn a_view_member_cannot_plant_a_host() {
    let admin = device("admin@example.com");
    let viewer = device("viewer@example.com");
    let mut vault = Vault::new(&admin);
    vault.add(&admin, &viewer, Role::View, false);
    let (log, viewer_ring) = vault.seen_by(&viewer);
    let planted = seal_item(
        &log,
        &viewer_ring,
        &viewer,
        [1; 16],
        KeyKind::Data,
        1,
        b"ProxyCommand evil",
    );
    assert_eq!(
        planted.unwrap_err(),
        TeamError::NotAllowed("the writer could only view")
    );

    // Built by hand with the data key the viewer holds, and its own
    // signature: readers still refuse it.
    let forged = forge(
        &viewer,
        &viewer_ring,
        header(&log, &viewer, KeyKind::Data, 0),
    );
    let (log, ring) = vault.seen_by(&admin);
    assert_eq!(
        read(&log, &ring, &forged),
        Err(TeamError::NotAllowed("the writer could only view"))
    );
}

#[test]
fn an_editor_without_credentials_cannot_write_a_secret() {
    let admin = device("admin@example.com");
    let bob = device("bob@example.com");
    let mut vault = Vault::new(&admin);
    vault.add(&admin, &bob, Role::Edit, false);
    let (log, bob_ring) = vault.seen_by(&bob);
    assert_eq!(
        seal_item(&log, &bob_ring, &bob, [1; 16], KeyKind::Secrets, 1, b"x").unwrap_err(),
        TeamError::NotAllowed("the writer held no credentials")
    );

    // Even with the secrets key from elsewhere, readers refuse his secret.
    let admin_ring = vault.ring(&admin);
    let forged = forge(&bob, &admin_ring, header(&log, &bob, KeyKind::Secrets, 0));
    assert_eq!(
        read(&log, &admin_ring, &forged),
        Err(TeamError::NotAllowed("the writer held no credentials"))
    );
}

#[test]
fn an_item_moved_changed_or_sealed_under_an_old_epoch_is_refused() {
    let admin = device("admin@example.com");
    let bob = device("bob@example.com");
    let laptop = device("admin@example.com");
    let mut vault = Vault::new(&admin);
    vault.add(&admin, &bob, Role::Edit, false);
    let i = vault.write(&admin, 1, KeyKind::Data, "db-1");
    let (log, ring) = vault.seen_by(&admin);

    let mut moved = vault.items[i].clone();
    moved.header.item = [2; 16];
    assert_eq!(read(&log, &ring, &moved), Err(TeamError::BadSignature));
    let mut flipped = vault.items[i].clone();
    flipped.sealed[0] ^= 1;
    assert_eq!(read(&log, &ring, &flipped), Err(TeamError::BadSignature));

    // After bob's removal, an item that names the new log position but
    // was sealed under the old epoch, as a writer kept in the dark would.
    vault.remove(&admin, "bob@example.com");
    let (log, ring) = vault.seen_by(&admin);
    let stale = forge(&admin, &ring, header(&log, &admin, KeyKind::Data, 0));
    assert_eq!(
        read(&log, &ring, &stale),
        Err(TeamError::NotAllowed(
            "the item is not sealed under the epoch of its log entry"
        ))
    );

    // A device's write at the very entry that removed it is refused.
    let ring = vault.ring(&admin);
    let add = vault.log.add_device(&admin, &ring, laptop.card()).unwrap();
    vault.change(add);
    let ring = vault.ring(&admin);
    let remove = vault
        .log
        .remove_device(&admin, &ring, laptop.id(), &vault.items)
        .unwrap();
    vault.change(remove);
    let (log, ring) = vault.seen_by(&admin);
    let at_removal = forge(&laptop, &ring, header(&log, &laptop, KeyKind::Data, 2));
    assert_eq!(
        read(&log, &ring, &at_removal),
        Err(TeamError::NotAllowed(
            "the writer's device was not in the vault"
        ))
    );
}

#[test]
fn a_new_member_reads_the_history_and_credentials_follow_the_grant() {
    let admin = device("admin@example.com");
    let bob = device("bob@example.com");
    let carol = device("carol@example.com");
    let dave = device("dave@example.com");
    let mut vault = Vault::new(&admin);
    vault.add(&admin, &bob, Role::Edit, true);
    let host = vault.write(&admin, 1, KeyKind::Data, "db-1");
    let password = vault.write(&bob, 2, KeyKind::Secrets, "hunter2");
    // Two epochs later, dave joins without credentials.
    vault.remove(&admin, "bob@example.com");
    vault.add(&admin, &carol, Role::Edit, true);
    vault.remove(&admin, "carol@example.com");
    vault.add(&admin, &dave, Role::View, false);
    assert_eq!(vault.log.epoch(), 2);

    let (log, dave_ring) = vault.seen_by(&dave);
    assert_eq!(read(&log, &dave_ring, &vault.items[host]).unwrap(), "db-1");
    assert_eq!(
        read(&log, &dave_ring, &vault.items[password]),
        Err(TeamError::NoKey(0))
    );

    // Granted credentials, dave reads the password written two epochs ago.
    vault.set_access(&admin, "dave@example.com", Role::View, true);
    let (log, dave_ring) = vault.seen_by(&dave);
    assert_eq!(
        read(&log, &dave_ring, &vault.items[password]).unwrap(),
        "hunter2"
    );

    // Taken away again: a new epoch, whose secrets key dave never holds.
    vault.set_access(&admin, "dave@example.com", Role::View, false);
    let later = vault.write(&admin, 3, KeyKind::Secrets, "new password");
    let (log, dave_ring) = vault.seen_by(&dave);
    assert_eq!(
        read(&log, &dave_ring, &vault.items[later]),
        Err(TeamError::NoKey(3))
    );
}

#[test]
fn the_vault_keeps_an_admin_and_entries_carry_exactly_their_keys() {
    let admin = device("admin@example.com");
    let bob = device("bob@example.com");
    let stranger = device("stranger@example.com");
    let laptop = device("admin@example.com");
    let mut vault = Vault::new(&admin);
    let ring = vault.ring(&admin);
    let me = user("admin@example.com");

    // The builder refuses to leave the vault without an admin, and so does
    // every reader when an admin signs such an entry anyway.
    let remove_self = vault.log.remove_member(&admin, &ring, &me, &[]);
    let demote = vault
        .log
        .set_access(&admin, &ring, &me, (Role::Edit, true), &[]);
    for refused in [remove_self, demote] {
        assert_eq!(
            refused.unwrap_err(),
            TeamError::NotAllowed("the vault would have no admin device")
        );
    }
    let remaining = vault.log.add_device(&admin, &ring, laptop.card()).unwrap();
    let mut log = vault.log.clone();
    log.extend([&remaining]).unwrap();
    let both = Keyring::open(&log, &admin).unwrap();
    let mut demote = log
        .set_access(&admin, &both, &me, (Role::Admin, true), &[])
        .unwrap();
    if let Op::SetAccess { role, .. } = &mut demote.op {
        *role = Role::Edit;
    }
    assert_eq!(
        log.clone().extend([&resign(&admin, demote)]),
        Err(TeamError::NotAllowed(
            "the vault would have no admin device"
        ))
    );
    assert_eq!(
        vault
            .log
            .add_member(&admin, &ring, bob.card(), Role::Admin, false)
            .unwrap_err(),
        TeamError::BadEntry("an admin holds credentials")
    );

    // An entry whose wraps leave out a device, or seal to one too many, is
    // refused even when an admin signed it.
    let entry = vault
        .log
        .add_member(&admin, &ring, bob.card(), Role::Edit, true)
        .unwrap();
    let mut fewer = entry.clone();
    if let Op::AddMember { wraps, .. } = &mut fewer.op {
        wraps.retain(|w| w.kind == KeyKind::Data);
    }
    let mut more = entry.clone();
    if let Op::AddMember { wraps, .. } = &mut more.op {
        let mut extra = wraps[0].clone();
        extra.device = stranger.id();
        wraps.push(extra);
    }
    for bad in [fewer, more] {
        assert!(matches!(
            vault.log.clone().extend([&resign(&admin, bad)]),
            Err(TeamError::BadEntry(_))
        ));
    }
    vault.change(entry);
}

#[test]
fn a_rotation_is_checked_and_a_failed_batch_changes_nothing() {
    let admin = device("admin@example.com");
    let bob = device("bob@example.com");
    let laptop = device("admin@example.com");
    let mut vault = Vault::new(&admin);
    vault.add(&admin, &bob, Role::Edit, true);

    // Taking credentials without a new epoch, or a rotation that names
    // the wrong epoch, is refused.
    let ring = vault.ring(&admin);
    let take = vault
        .log
        .set_access(
            &admin,
            &ring,
            &user("bob@example.com"),
            (Role::Edit, false),
            &[],
        )
        .unwrap();
    let mut no_rotation = take.clone();
    if let Op::SetAccess { rotation, .. } = &mut no_rotation.op {
        *rotation = None;
    }
    let mut wrong_epoch = take;
    if let Op::SetAccess {
        rotation: Some(rotation),
        ..
    } = &mut wrong_epoch.op
    {
        rotation.epoch = 5;
    }
    for bad in [no_rotation, wrong_epoch] {
        assert!(matches!(
            vault.log.clone().extend([&resign(&admin, bad)]),
            Err(TeamError::BadEntry(_))
        ));
    }

    // A batch that fails leaves the log as it was.
    let good = vault.log.add_device(&admin, &ring, laptop.card()).unwrap();
    let mut bad = good.clone();
    bad.signature[0] ^= 1;
    let before = (vault.log.head(), vault.log.epoch());
    let mut bad_next = resign(&admin, bad.clone());
    bad_next.seq += 1;
    assert!(vault.log.extend([&good, &bad_next]).is_err());
    assert_eq!((vault.log.head(), vault.log.epoch()), before);

    // A second device of the admin's gets both keys, and the history.
    vault.change(good);
    let first = vault.write(&admin, 1, KeyKind::Secrets, "hunter2");
    let ring = vault.ring(&admin);
    let remove = vault
        .log
        .remove_device(&admin, &ring, admin.id(), &vault.items)
        .unwrap();
    vault.change(remove);
    let (log, laptop_ring) = vault.seen_by(&laptop);
    assert_eq!(
        read(&log, &laptop_ring, &vault.items[first]).unwrap(),
        "hunter2"
    );
    assert_eq!(laptop.card().fingerprint().len(), 49);
    // A removed device is never added back.
    let ring = vault.ring(&laptop);
    assert_eq!(
        vault
            .log
            .add_device(&laptop, &ring, admin.card())
            .unwrap_err(),
        TeamError::BadEntry("the device was in the vault before")
    );
}

#[test]
fn a_badly_sealed_history_key_leaves_the_newer_keys_open() {
    let admin = device("admin@example.com");
    let bob = device("bob@example.com");
    let mut vault = Vault::new(&admin);
    vault.add(&admin, &bob, Role::Edit, true);
    let ring = vault.ring(&admin);
    let mut remove = vault
        .log
        .remove_member(&admin, &ring, &user("bob@example.com"), &[])
        .unwrap();
    if let Op::RemoveMember { rotation, .. } = &mut remove.op {
        rotation.previous_data.sealed[0] ^= 1;
    }
    vault.change(resign(&admin, remove));
    let ring = vault.ring(&admin);
    assert!(ring.key(KeyKind::Data, 1).is_ok());
    assert_eq!(ring.key(KeyKind::Data, 0).unwrap_err(), TeamError::NoKey(0));
    assert!(ring.key(KeyKind::Secrets, 0).is_ok());
}

#[test]
fn a_user_id_is_bounded_and_printable() {
    assert!(UserId::new("").is_err());
    assert!(UserId::new("a\u{7}b").is_err());
    assert!(UserId::new("x".repeat(257)).is_err());
    assert!(UserId::new("x".repeat(256)).is_ok());
}

/// The next entry `make` builds on `vault`'s log, changed by `edit` and
/// signed again by `admin`, applied to a copy of the log.
fn signed_wrong(
    vault: &Vault,
    admin: &DeviceKeys,
    make: impl FnOnce(&Log, &Keyring) -> Result<Entry, TeamError>,
    edit: impl FnOnce(&mut Entry),
) -> Result<(), TeamError> {
    let mut entry = make(&vault.log, &vault.ring(admin)).unwrap();
    edit(&mut entry);
    vault.log.clone().extend([&resign(admin, entry)])
}

#[test]
fn readers_refuse_what_an_admin_signs_wrongly() {
    let admin = device("admin@example.com");
    let bob = device("bob@example.com");
    let carol = device("carol@example.com");
    let laptop = device("bob@example.com");
    let phone = device("bob@example.com");
    let mut vault = Vault::new(&admin);
    vault.add(&admin, &bob, Role::Edit, true);
    vault.add(&admin, &carol, Role::Edit, false);
    let ring = vault.ring(&admin);
    let add = vault.log.add_device(&admin, &ring, laptop.card()).unwrap();
    vault.change(add);
    let ring = vault.ring(&admin);
    let remove = vault
        .log
        .remove_device(&admin, &ring, laptop.id(), &[])
        .unwrap();
    vault.change(remove);
    let bob_id = user("bob@example.com");

    // A rotation that also seals the new keys to the member it removes.
    let refused = signed_wrong(
        &vault,
        &admin,
        |log, ring| log.remove_member(&admin, ring, &user("carol@example.com"), &[]),
        |entry| {
            if let Op::RemoveMember { rotation, .. } = &mut entry.op {
                let mut extra = rotation.wraps[0].clone();
                extra.device = carol.id();
                rotation.wraps.push(extra);
            }
        },
    );
    assert!(matches!(refused, Err(TeamError::BadEntry(_))));
    // The right position, after another entry than the one held there.
    let refused = signed_wrong(
        &vault,
        &admin,
        |log, ring| log.add_device(&admin, ring, phone.card()),
        |entry| entry.prev = [1; 32],
    );
    assert_eq!(refused, Err(TeamError::Fork(vault.log.head().0 + 1)));
    // A device of a member with credentials, without the secrets key.
    let refused = signed_wrong(
        &vault,
        &admin,
        |log, ring| log.add_device(&admin, ring, phone.card()),
        |entry| {
            if let Op::AddDevice { wraps, .. } = &mut entry.op {
                wraps.retain(|w| w.kind == KeyKind::Data);
            }
        },
    );
    assert!(matches!(refused, Err(TeamError::BadEntry(_))));
    // A device removed before, added back.
    let refused = signed_wrong(
        &vault,
        &admin,
        |log, ring| log.add_device(&admin, ring, phone.card()),
        |entry| {
            if let Op::AddDevice { card, .. } = &mut entry.op {
                *card = laptop.card().clone();
            }
        },
    );
    assert_eq!(
        refused,
        Err(TeamError::BadEntry("the device was in the vault before"))
    );
    // A member added a second time.
    let refused = signed_wrong(
        &vault,
        &admin,
        |log, ring| log.add_member(&admin, ring, phone.card(), Role::View, false),
        |_| {},
    );
    assert_eq!(
        refused,
        Err(TeamError::BadEntry("the member is already in the vault"))
    );

    // Removed and invited again with a new device, bob writes from that
    // one; his old device stays out.
    vault.remove(&admin, "bob@example.com");
    let ring = vault.ring(&admin);
    let again = vault
        .log
        .add_member(&admin, &ring, phone.card(), Role::Edit, false)
        .unwrap();
    vault.change(again);
    assert!(vault.log.role_at(&bob_id, vault.log.head().0).is_some());
    let fresh = vault.write(&phone, 1, KeyKind::Data, "db-1");
    let (log, ring) = vault.seen_by(&admin);
    assert_eq!(read(&log, &ring, &vault.items[fresh]).unwrap(), "db-1");
    let old = forge(&bob, &ring, header(&log, &bob, KeyKind::Data, log.epoch()));
    assert_eq!(
        read(&log, &ring, &old),
        Err(TeamError::NotAllowed(
            "the writer's device was not in the vault"
        ))
    );
}

#[test]
fn a_demotion_keeps_honest_writes_and_never_a_forgery() {
    let admin = device("admin@example.com");
    let carol = device("carol@example.com");
    let mut vault = Vault::new(&admin);
    vault.add(&admin, &carol, Role::Edit, false);
    let (then, carol_ring) = vault.seen_by(&carol);
    let honest = vault.write(&carol, 1, KeyKind::Data, "db-1");

    vault.set_access(&admin, "carol@example.com", Role::View, false);
    let (log, ring) = vault.seen_by(&admin);
    assert_eq!(read(&log, &ring, &vault.items[honest]).unwrap(), "db-1");

    // Demoted, carol backdates a host; the Hub hands it to the admin, who
    // then removes her. The removal keeps none of it.
    let forged = forge(&carol, &carol_ring, header(&then, &carol, KeyKind::Data, 0));
    vault.items.push(forged);
    vault.remove(&admin, "carol@example.com");
    let (log, ring) = vault.seen_by(&admin);
    assert_eq!(read(&log, &ring, &vault.items[honest]).unwrap(), "db-1");
    assert_eq!(
        read(&log, &ring, vault.items.last().unwrap()),
        Err(TeamError::NotAllowed(
            "the writer may no longer write, and no admin kept this version"
        ))
    );
}

#[test]
fn a_badly_sealed_wrap_costs_only_its_own_kind() {
    let admin = device("admin@example.com");
    let bob = device("bob@example.com");
    let mut vault = Vault::new(&admin);
    vault.add(&admin, &bob, Role::Edit, false);
    let ring = vault.ring(&admin);
    let mut grant = vault
        .log
        .set_access(
            &admin,
            &ring,
            &user("bob@example.com"),
            (Role::Edit, true),
            &[],
        )
        .unwrap();
    if let Op::SetAccess { wraps, .. } = &mut grant.op {
        wraps[0].sealed[0] ^= 1;
    }
    vault.change(resign(&admin, grant));
    let ring = vault.ring(&bob);
    assert!(ring.key(KeyKind::Data, 0).is_ok());
    assert_eq!(
        ring.key(KeyKind::Secrets, 0).unwrap_err(),
        TeamError::NoKey(0)
    );
}
