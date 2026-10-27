// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Vault identities shared through sync (ADR 0030). Which identities this
//! device shares is its own choice, kept on each one here. What comes from
//! the target may add an identity this device does not hold, change or
//! stop sharing one it shares, and nothing else: a record under the id of
//! an identity this device keeps to itself is refused, since hosts carry
//! those ids to every device. A key that arrives has its public half
//! derived here, never taken from the target.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::{
    Failed, Identity, IdentityInput, MAX_IDENTITIES, OpenVault, SecretRefs, Vault, VaultError,
    check_identity, save_into,
};
use crate::ports::{KeyDeriver, Secret, SyncedIdentity};
use crate::trust::{IdentityPrint, Prints, keyed_digest};

/// The identities `vault` shares, in id order.
fn shared(vault: &dyn OpenVault) -> Result<Vec<SyncedIdentity>, VaultError> {
    let mut out: Vec<SyncedIdentity> = vault
        .identities()?
        .into_iter()
        .filter(|identity| identity.sync)
        .map(|identity| SyncedIdentity {
            id: identity.id,
            name: identity.name,
            user: identity.user,
            password: identity.password,
            private_key: identity.private_key,
            passphrase: identity.passphrase,
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// The digest of what `identity` holds and where a password manager
/// holds the rest, keyed by `key`. O(n). An identity with no references
/// keeps the digest it had before they existed.
fn digest(key: &[u8; 32], identity: &SyncedIdentity, refs: &SecretRefs) -> [u8; 32] {
    fn text(value: Option<&str>) -> &[u8] {
        // Distinct from any text: absent is not empty.
        value.map_or(b"\xff", str::as_bytes)
    }
    let named = (!refs.is_empty()).then(|| format!("{refs:?}"));
    let mut parts: Vec<&[u8]> = vec![
        b"tauminus identity 1",
        identity.name.as_bytes(),
        text(identity.user.as_deref()),
        text(identity.password.as_deref().map(String::as_str)),
        text(identity.private_key.as_deref().map(String::as_str)),
        text(identity.passphrase.as_deref().map(String::as_str)),
    ];
    if let Some(named) = &named {
        parts.push(named.as_bytes());
    }
    keyed_digest(key, &parts)
}

/// What the review shows of `identity`, with `public_key` as this device
/// derived it.
fn lines(
    identity: &SyncedIdentity,
    public_key: Option<&str>,
    refs: &SecretRefs,
) -> Vec<(&'static str, String)> {
    let password = identity.password.is_some() || refs.password.is_some();
    let key = identity.private_key.is_some() || refs.private_key.is_some();
    let login = match (password, key) {
        (true, true) => "a password and a key",
        (true, false) => "a password",
        (false, true) => "a key",
        (false, false) => "the agent's keys only",
    };
    let mut out = vec![
        ("identity", identity.name.clone()),
        (
            "identity user",
            identity
                .user
                .clone()
                .unwrap_or_else(|| "the host's".to_owned()),
        ),
        ("identity login", login.to_owned()),
    ];
    if let Some(key) = public_key {
        out.push(("identity key", key.to_owned()));
    }
    let managers: Vec<&str> = [&refs.password, &refs.private_key, &refs.passphrase]
        .into_iter()
        .flatten()
        .map(tauminus_domain::SecretRef::manager)
        .collect();
    if !managers.is_empty() {
        out.push(("identity secrets from", managers.join(", ")));
    }
    out
}

fn print(
    key: &[u8; 32],
    identity: &SyncedIdentity,
    public_key: Option<&str>,
    refs: &SecretRefs,
) -> IdentityPrint {
    IdentityPrint {
        digest: digest(key, identity, refs),
        lines: lines(identity, public_key, refs),
    }
}

fn synced(identity: Identity) -> (SyncedIdentity, Option<String>, bool, SecretRefs) {
    (
        SyncedIdentity {
            id: identity.id,
            name: identity.name,
            user: identity.user,
            password: identity.password,
            private_key: identity.private_key,
            passphrase: identity.passphrase,
        },
        identity.public_key,
        identity.sync,
        identity.references,
    )
}

/// The review key, made and saved on first use.
fn review_key(vault: &mut dyn OpenVault) -> Result<zeroize::Zeroizing<[u8; 32]>, Failed> {
    let (key, made) = vault.review_key()?;
    if made {
        vault.save().map_err(|error| Failed {
            error,
            discard: true,
        })?;
    }
    Ok(key)
}

/// The prints of the identities `ids` names, or of every identity in
/// `vault`: shared or not, each counts in what a host logs in with.
/// O(k) decodes and digests.
fn prints_of(vault: &mut dyn OpenVault, ids: Option<&[String]>) -> Result<Prints, Failed> {
    let key = review_key(vault)?;
    let identities = match ids {
        Some(ids) => {
            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                out.extend(vault.identity(id)?);
            }
            out
        }
        None => vault.identities()?,
    };
    Ok(identities
        .into_iter()
        .map(|identity| {
            let (identity, public_key, _, refs) = synced(identity);
            let print = print(&key, &identity, public_key.as_deref(), &refs);
            (identity.id, print)
        })
        .collect())
}

/// Every identity's print now, and as taking `merged` would leave it: a
/// shared one `merged` holds as `merged` has it, the rest as they are.
/// O(n) decodes and digests.
fn merged_prints(
    vault: &mut dyn OpenVault,
    merged: &[SyncedIdentity],
) -> Result<(Prints, Prints), Failed> {
    let key = review_key(vault)?;
    let mut offered: BTreeMap<&str, &SyncedIdentity> =
        merged.iter().map(|i| (i.id.as_str(), i)).collect();
    let (mut before, mut after) = (Prints::new(), Prints::new());
    for identity in vault.identities()? {
        let (identity, public_key, sync, refs) = synced(identity);
        let now = print(&key, &identity, public_key.as_deref(), &refs);
        // One kept to itself is never taken; one this device shares
        // and the merge leaves out stays here as it is.
        let next = match offered.remove(identity.id.as_str()) {
            Some(theirs) if sync && *theirs != identity => {
                print(&key, theirs, None, &SecretRefs::default())
            }
            _ => now.clone(),
        };
        before.insert(identity.id.clone(), now);
        after.insert(identity.id, next);
    }
    for (id, theirs) in offered {
        after.insert(
            id.to_owned(),
            print(&key, theirs, None, &SecretRefs::default()),
        );
    }
    Ok((before, after))
}

/// As the view would have entered it, for the same checks.
fn as_input(identity: &SyncedIdentity) -> IdentityInput {
    IdentityInput {
        id: Some(identity.id.clone()),
        name: identity.name.clone(),
        user: identity.user.clone(),
        password: identity.password.clone(),
        private_key: identity.private_key.clone(),
        passphrase: identity.passphrase.clone(),
        public_key: None,
        sync: Some(true),
        references: None,
        references_seen: None,
    }
}

impl Vault {
    /// The identities this device shares, in id order. Needs the vault
    /// open, and is no use of it.
    pub async fn synced_identities(&self) -> Result<Vec<SyncedIdentity>, VaultError> {
        self.with_open_as(false, |vault| Ok(shared(vault)?)).await
    }

    /// The prints of the identities `ids` names, or of all of them:
    /// keyed by a key the vault makes on first use. O(k) in their size.
    pub async fn identity_prints(&self, ids: Option<Vec<String>>) -> Result<Prints, VaultError> {
        self.with_open_as(false, move |vault| prints_of(vault, ids.as_deref()))
            .await
    }

    /// The prints before and after taking `merged`, read with the check
    /// that the identities this device shares are still `expected`:
    /// `None` when they changed. O(n) in the vault's identities.
    pub async fn sync_prints(
        &self,
        expected: Vec<SyncedIdentity>,
        merged: Vec<SyncedIdentity>,
    ) -> Result<Option<(Prints, Prints)>, VaultError> {
        self.with_open_as(false, move |vault| {
            if shared(vault)? != expected {
                return Ok(None);
            }
            Ok(Some(merged_prints(vault, &merged)?))
        })
        .await
    }

    /// As [`Vault::save_identity`], with the print of the identity saved
    /// just before and just after, read with the save: a sync taken in
    /// between cannot pass for the user's edit.
    pub async fn save_identity_printed(
        &self,
        input: IdentityInput,
    ) -> Result<(String, Prints, Prints), VaultError> {
        check_identity(&input)?;
        let new_id = self.backend.new_id();
        self.with_open(move |vault| {
            let ids = [input.id.clone().unwrap_or_else(|| new_id.clone())];
            let before = prints_of(vault, Some(&ids))?;
            let id = save_into(vault, &input, new_id)?;
            let after = prints_of(vault, Some(std::slice::from_ref(&id)))?;
            Ok((id, before, after))
        })
        .await
    }

    /// As [`Vault::delete_identity`], with its print before; none after.
    pub async fn delete_identity_printed(
        &self,
        id: String,
    ) -> Result<(Prints, Prints), VaultError> {
        self.with_open(move |vault| {
            let before = prints_of(vault, Some(std::slice::from_ref(&id)))?;
            if !vault.remove_identity(&id) {
                return Err(VaultError::NoSuchIdentity(id).into());
            }
            vault.save().map_err(|error| Failed {
                error,
                discard: true,
            })?;
            Ok((before, Prints::new()))
        })
        .await
    }

    /// Takes `merged`, what a sync made of `expected`, the identities this
    /// device shared when it began, and saves the vault: `None`, saving
    /// nothing, when they changed meanwhile. An identity `expected` held
    /// that `merged` does not stops being shared here, and stays. Keys
    /// are derived first, outside the vault's lock. O(n log n) and a key
    /// derivation for each key that changed.
    ///
    /// # Errors
    /// The vault locked, or not saving.
    pub async fn apply_synced(
        &self,
        expected: Vec<SyncedIdentity>,
        merged: Vec<SyncedIdentity>,
        deriver: Arc<dyn KeyDeriver>,
    ) -> Result<Option<Applied>, VaultError> {
        // More than a vault may hold: nothing is derived for it.
        let keys = if merged.len() > MAX_IDENTITIES {
            BTreeMap::new()
        } else {
            derive_keys(&expected, &merged, deriver).await?
        };
        self.with_open_as(false, move |vault| {
            if shared(vault)? != expected {
                return Ok(None);
            }
            let before: BTreeMap<&str, &SyncedIdentity> =
                expected.iter().map(|i| (i.id.as_str(), i)).collect();
            let mut applied = Applied::default();
            let mut changed: Vec<Identity> = Vec::new();
            let mut taken = BTreeSet::new();
            for identity in &merged {
                taken.insert(identity.id.as_str());
                if before.get(identity.id.as_str()) == Some(&identity) {
                    continue;
                }
                match take(vault, identity, &keys, &mut applied.notices)? {
                    Some(next) => changed.push(next),
                    None => applied.refused.push(identity.id.clone()),
                }
            }
            for id in before.keys().filter(|id| !taken.contains(*id)) {
                if let Some(mut local) = vault.identity(id)? {
                    local.sync = false;
                    changed.push(local);
                }
            }
            if changed.is_empty() {
                return Ok(Some(applied));
            }
            // Over the limit, what is new waits; changes to what is here
            // go in.
            let (added, mut changed): (Vec<Identity>, Vec<Identity>) = changed
                .into_iter()
                .partition(|i| !before.contains_key(i.id.as_str()));
            let room = MAX_IDENTITIES.saturating_sub(vault.identity_count());
            if added.len() > room {
                applied.notices.push(format!(
                    "identities from other devices were not all taken: at most {MAX_IDENTITIES}"
                ));
            }
            let mut added = added.into_iter();
            changed.extend(added.by_ref().take(room));
            applied.refused.extend(added.map(|identity| identity.id));
            if changed.is_empty() {
                return Ok(Some(applied));
            }
            for identity in &changed {
                vault.put_identity(identity)?;
            }
            vault.save().map_err(|error| Failed {
                error,
                discard: true,
            })?;
            Ok(Some(applied))
        })
        .await
    }
}

/// What taking a sync's identities did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// What the user should know of what was not taken.
    pub notices: Vec<String>,
    /// The ids not taken: the base keeps what it held for them.
    pub refused: Vec<String>,
}

/// The public half of each key `merged` brings that `expected` did not
/// hold, by identity id, or why it does not derive. Off the async
/// workers, and off the vault's lock. O(k) derivations.
async fn derive_keys(
    expected: &[SyncedIdentity],
    merged: &[SyncedIdentity],
    deriver: Arc<dyn KeyDeriver>,
) -> Result<BTreeMap<String, Result<String, String>>, VaultError> {
    type Wanted = (String, Secret, Option<Secret>);
    let held: BTreeMap<&str, &SyncedIdentity> =
        expected.iter().map(|i| (i.id.as_str(), i)).collect();
    let wanted: Vec<Wanted> = merged
        .iter()
        .filter(|m| {
            held.get(m.id.as_str())
                .is_none_or(|h| (&h.private_key, &h.passphrase) != (&m.private_key, &m.passphrase))
        })
        .filter_map(|m| {
            m.private_key
                .clone()
                .map(|key| (m.id.clone(), key, m.passphrase.clone()))
        })
        .collect();
    if wanted.is_empty() {
        return Ok(BTreeMap::new());
    }
    tokio::task::spawn_blocking(move || {
        wanted
            .into_iter()
            .map(|(id, key, passphrase)| {
                let line = deriver.public_key(&key, passphrase.as_deref().map(String::as_str));
                (id, line)
            })
            .collect()
    })
    .await
    .map_err(|e| VaultError::Io(e.to_string()))
}

/// `identity` from the target as this vault would save it, or `None` with
/// a notice when it may not be taken.
fn take(
    vault: &dyn OpenVault,
    identity: &SyncedIdentity,
    derived: &BTreeMap<String, Result<String, String>>,
    notices: &mut Vec<String>,
) -> Result<Option<Identity>, VaultError> {
    // A name the checks refuse is not repeated to the user.
    let checked = check_identity(&as_input(identity));
    let name = if checked.is_ok() {
        identity.name.trim()
    } else {
        "one"
    };
    let saved = vault.identity(&identity.id)?;
    if saved.as_ref().is_some_and(|local| !local.sync) {
        notices.push(format!(
            "another device wrote an identity, {name}, under the id of one this device keeps to itself; it was not taken"
        ));
        return Ok(None);
    }
    if let Err(error) = checked {
        notices.push(format!(
            "an identity from another device, {name}, was not taken: {error}"
        ));
        return Ok(None);
    }
    let public_key = match (&identity.private_key, derived.get(&identity.id)) {
        (None, _) => None,
        (Some(_), Some(Ok(line))) => Some(line.clone()),
        (Some(_), Some(Err(error))) => {
            notices.push(format!(
                "an identity from another device, {name}, was not taken: its key: {error}"
            ));
            return Ok(None);
        }
        // The key this device holds already: its public half stands.
        (Some(key), None) => match saved {
            Some(saved)
                if saved.private_key.as_ref() == Some(key)
                    && saved.passphrase == identity.passphrase =>
            {
                saved.public_key
            }
            _ => {
                notices.push(format!(
                    "an identity from another device, {name}, was not taken: its key was not read"
                ));
                return Ok(None);
            }
        },
    };
    Ok(Some(Identity {
        id: identity.id.clone(),
        name: name.to_owned(),
        user: identity.user.clone(),
        password: identity.password.clone(),
        private_key: identity.private_key.clone(),
        passphrase: identity.passphrase.clone(),
        public_key,
        sync: true,
        references: SecretRefs::default(),
    }))
}

#[cfg(test)]
mod tests;
