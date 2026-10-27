// SPDX-FileCopyrightText: 2026 Tauminus contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Team vaults (ADR 0034): what a device checks so that the Hub, which
//! stores and forwards every byte here, can neither read a vault nor
//! change who is in it.
//!
//! - [`DeviceKeys`]: a device's signing and sealing keys, and its
//!   [`DeviceCard`] that others see.
//! - [`Log`]: the vault's membership log, verified from its first entry.
//!   Every change of members, devices or roles is an [`Entry`] an admin's
//!   device signed; removing anyone starts a new key epoch.
//! - [`Keyring`]: the epoch keys this device can open, from the wraps
//!   sealed to it.
//! - [`SealedItem`]: a host, snippet or credential, sealed under an epoch
//!   key and signed by the device that wrote it.
//!
//! Nothing here does I/O; the Hub client moves these values.

mod encode;
mod item;
mod keys;
mod log;
mod seal;

pub use item::{ItemHeader, SealedItem, open_item, seal_item};
pub use keys::{DeviceCard, DeviceId, DeviceKeys, UserId, VaultId};
pub use log::{Entry, Invite, Keyring, Log, Op, Role, Rotation};
pub use seal::{KeyKind, SealedKey, Wrap};

/// A SHA-256 digest: a log entry's hash.
pub type Hash = [u8; 32];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TeamError {
    #[error("a signature does not check")]
    BadSignature,
    #[error("the log differs from the one this device holds at entry {0}")]
    Fork(u64),
    #[error("the log entry is not valid: {0}")]
    BadEntry(&'static str),
    #[error("not allowed: {0}")]
    NotAllowed(&'static str),
    #[error("this device holds no key for epoch {0}")]
    NoKey(u32),
    #[error("sealed data does not open")]
    Sealed,
    #[error("a user id must be 1 to 256 bytes with no control characters")]
    BadUser,
    #[error("no random source")]
    Random,
}

#[cfg(test)]
mod tests;
