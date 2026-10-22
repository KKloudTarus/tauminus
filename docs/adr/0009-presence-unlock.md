# ADR 0009: Touch ID, Windows Hello, and polkit before the keychain unlocks

- Status: accepted
- Date: 2026-10-22

## Context

The keychain slot (ADR 0007) opens the vault without the master password.
Whoever sits at an unlocked, unattended session can click "Unlock with the
system keychain". The roadmap asks for biometric unlock in v0.2, and for
keys bound to the Secure Enclave and Windows Hello later.

The keychain entry itself is readable by any program running as the user:
`CredRead` on Windows, any D-Bus client of an unlocked Secret Service on
Linux, and, on macOS, any program the user lets through the legacy
keychain's prompt. ADR 0007 puts same-user processes out of scope.

Release builds are not code-signed. On macOS a keychain item the OS itself
guards with biometrics (`SecAccessControl`) needs the data protection
keychain, which needs a keychain access group entitlement, so a signed app.

## Decision

A panel weighed three options:

- (A) a presence check in front of the existing slot;
- (B) slots the OS guards on every platform;
- (C) a Windows Hello slot whose key is derived from a Hello signature,
  with checks elsewhere.

B cannot ship on macOS unsigned. C's Windows half relies on Hello
signatures of a fixed challenge being the same every time, which
Microsoft does not document (it calls them RSA-PSS, which is randomized),
and this project has no Windows machine with Hello to test it on. It
belongs to the later roadmap item. v0.2 ships A.

### The check

With the setting on, unlocking with the keychain asks the OS to confirm
the user is present:

- macOS: `LocalAuthentication` with `LAPolicyDeviceOwnerAuthentication`,
  which is Touch ID, or the login password without a sensor; the settings
  name whichever this Mac has.
- Windows: `UserConsentVerifier`, which is Windows Hello's face,
  fingerprint, or PIN, opened over the app's window.
- Linux: polkit's `CheckAuthorization`, which the session's agent answers
  with the user's own password, or a fingerprint where PAM uses fprintd.
  Unlocking asks for the action `io.github.kkloudtarus.tauminus.unlock-vault`
  and changing the setting for `...change-unlock-check`, so each prompt
  says what it is for: only the policy file sets the message.

The backend runs the check. The view never sends a "confirmed" flag. Each
unlock asks afresh: macOS gets a new `LAContext` with no Touch ID reuse,
and the polkit action is `auth_self`, never `auth_self_keep`, and only at
the active local seat. A prompt waits at most 60 s, then is cancelled. One
prompt runs at a time.

A cancel, a timeout, or a failure leaves the vault locked and the slot
in place; the master password always unlocks. A system without the check
(no sensor, no Hello, no polkit, or no polkit action, as in an AppImage)
reports why, and the setting cannot be turned on there.

### Bound to the slot, not to a flag

The setting is the keychain slot's kind: `keychain` or
`keychainPresence`. The kind is part of the slot's additional data, so a
`vault.json` edited to turn a `keychainPresence` slot into a plain one
fails to unwrap, before the data key is ever in memory. The app reads the
kind, runs the check its kind asks for, then opens through a slot of that
kind only. Turning the setting on or off replaces the slot under a fresh
keychain key, and takes the check itself: turning it on shows it works,
and turning it off needs the same proof. Turning the keychain off removes
either kind of slot with no check, since the vault then opens with the
password only; turning it on again takes the master password and starts
without the check. That is also the way out when the check stops working
(a removed sensor, a missing polkit action), and the settings say so.

The polkit action ships in `/usr/share/polkit-1/actions` with the `.deb`
and `.rpm` packages.

## Consequences

- The check stops someone using Tauminus at an unattended session. It does
  not stop a program running as the user, which reads the keychain entry
  directly. The settings say so next to the switch.
- On Linux the check is a password prompt unless PAM is set up for a
  fingerprint, so the setting is called "system authentication".
- A build older than this one cannot read a vault with a
  `keychainPresence` slot. Turning the setting off makes the vault
  readable by it again.
- Keys the Secure Enclave or Windows Hello guard remain a later roadmap
  item. Signed macOS builds can move the slot into the data protection
  keychain with `SecAccessControl`. A Windows Hello slot must first show,
  on real hardware, that signing the same challenge twice gives the same
  bytes.
