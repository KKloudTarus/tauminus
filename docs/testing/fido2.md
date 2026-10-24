# Testing FIDO2 keys on real hardware

CI logs in with `sk-` keys through ssh-agent and through the security key
path, with OpenSSH's test provider (`sk-dummy.so`) and a stand-in that
signs as it does (`ci/sk-agent.sh`, `crates/tauminus-ssh/tests/sk_agent.rs`
and `sk_direct.rs`). No runner has a USB key, so the HID transport and the
key's own answers (touch, PIN, timeouts) are covered by these steps. Run
them with a YubiKey 5 (or any FIDO2 key) on Linux, macOS and Windows, and
record the results at the end. This is v0.4's exit criterion.

## Setup

On each machine, with OpenSSH 8.2 or later:

```
ssh-keygen -t ed25519-sk -f ~/.ssh/id_ed25519_sk -N ''
ssh-keygen -t ecdsa-sk -O verify-required -f ~/.ssh/id_ecdsa_sk_pin -N ''
```

Touch the key when asked. Append both `.pub` files to
`~/.ssh/authorized_keys` on a test host whose sshd is 8.2 or later. Save
the host in Tauminus with `IdentityFile` set to each key in turn, or leave
it empty to use the default `id_ed25519_sk`.

On Linux, the user needs access to `/dev/hidraw*`: systemd 244 and later
grant it through `uaccess` for keys its `fido_id` marks. Check with
`ls -l /dev/hidraw*` while the key is plugged in.

## 1. Direct signing (Linux and macOS), no agent

Stop any agent first: `unset SSH_AUTH_SOCK`, then start the app from that
shell (`pnpm tauri dev` in `apps/desktop`).

| Do | Expect |
|---|---|
| Connect with `id_ed25519_sk`, touch the key | "Touch your security key" over the pane while the key blinks; it goes away by itself, and the shell opens |
| Connect, press Cancel on the touch notice | The key stops blinking within a second; the error names "a FIDO2 key, whose touch was cancelled" |
| Connect again right after the cancel, touch the key | The shell opens: the cancelled request left the key free |
| Connect, wait without touching | After the key's own timeout (about 30 s), the error names "a FIDO2 key, which was not touched in time" |
| Unplug the key, connect | "a FIDO2 key, with no security key plugged in" |
| Connect with `id_ecdsa_sk_pin` | "PIN for the security key" first; type the PIN, then touch: the shell opens |
| Same, with a wrong PIN | "a FIDO2 key, whose PIN the security key refused"; the key's retry count goes down by one (`ykman fido info`) |
| Same, cancel the PIN form | "a FIDO2 key, whose PIN was not given"; the key never blinks |
| Turn on always-UV (`ykman fido config toggle-always-uv`), connect with `id_ed25519_sk` | The PIN is asked once, then the touch: the shell opens. Turn it off after |
| Plug in a second FIDO2 key that does not hold the credential, connect | Only the right key blinks; the shell opens after its touch. The other key's PIN retries do not change |
| Open two tabs to two hosts with the same key at once | Two touches, one notice after the other; both shells open |

## 2. Through the agent (all three)

Start an agent and add the key: `ssh-add ~/.ssh/id_ed25519_sk`. On
Windows, use the OpenSSH for Windows agent service (`Start-Service
ssh-agent`), whose OpenSSH 8.9 and later sign `sk-` keys through
`webauthn.dll`.

| Do | Expect |
|---|---|
| Connect | The agent asks for the touch (its own prompt on Windows); the shell opens |
| Remove the key from the agent (`ssh-add -D`), connect on Windows | The error names "a FIDO2 key, which only ssh-agent signs with" |

## Status

No run on real hardware has been recorded yet. Record each OS's first run
here, with the key model and firmware (`ykman info`), the OS version and
the Tauminus commit.
