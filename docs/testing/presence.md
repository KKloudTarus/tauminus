# Testing the presence check on real hardware

Touch ID and Windows Hello need a person and a sensor, which CI runners
lack. CI runs the OS calls and prints what each runner answers (the
"Presence check on this OS" step); these steps cover the prompt itself.
Run them on a Mac with Touch ID and on a Windows machine with Hello set
up, and record the results at the end.

## What CI answers

The "Presence check on this OS" step, on 2026-10-22 (run 39870978089):

| Runner | Answer |
|---|---|
| macOS (`macos-latest`) | `your Mac login password: available Ok(())`: no Touch ID, so the login password answers, and the name says so |
| Windows (`windows-latest`) | `Windows Hello: available Err("no Windows Hello device")` |
| Ubuntu (`ubuntu-22.04`) | `system authentication: available Err("the polkit action for Tauminus is not installed")`: polkitd answers, and the action ships only with the packages |

## 1. The adapter alone

```
cargo run -p tauminus-presence --example check
```

| Do | Expect |
|---|---|
| Run it, and touch the sensor (or look at the camera) | `available Ok(())`, a system prompt saying Tauminus wants to unlock its vault, then `confirm: Ok(())` |
| Run it, and press Cancel | `confirm: Err(Cancelled)` |
| Run it, and use a finger that is not enrolled until the prompt gives up | `confirm: Err(Denied)`, or the OS falls back to the login password or PIN |
| Run it, and leave the prompt for a minute | `confirm: Err(TimedOut) after 60…s`, and the prompt closes |
| macOS without Touch ID (or with the lid closed) | the name is `your Mac login password`, and the prompt asks for it |

On Windows, the prompt opens in front of the terminal window; in the app
it opens over the Tauminus window.

## 2. The app

Build and start it (`pnpm tauri dev` in `apps/desktop`), then:

1. Open the vault, create it, and in Settings turn on "Let the system
   keychain unlock the vault" with the master password.
2. Turn on "Ask for Touch ID before the keychain unlocks the vault" (or
   Windows Hello). Expect one prompt; answer it. The box stays checked.
3. Lock now. Expect the button "Unlock with Touch ID" (or Windows Hello).
4. Press it and answer the prompt. Expect the vault to open.
5. Lock, press it, and cancel. Expect the vault to stay locked, with "the
   system check was cancelled; the master password still unlocks the
   vault". The master password still unlocks.
6. Unlock, untick the presence box, and answer the prompt. Lock, and
   unlock with the keychain: no prompt.
7. Turn the check on again and quit. In `vault.json`, change
   `"keychainPresence"` to `"keychain"`. Start the app: unlocking with the
   keychain fails, and the master password still unlocks.

## Results

| Date | Machine and OS | Sensor | Step 1 | Step 2 | Notes |
|---|---|---|---|---|---|
| | | | | | |
