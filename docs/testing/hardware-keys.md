# Testing keys held by hardware, through agents

CI signs in through agents with every key type such agents serve, and
with a public key as the identity file (`crates/tauminus-ssh/tests/openssh.rs`,
`windows_agents.rs`). These steps run the real agents on real hardware
(ADR 0028). FIDO2 keys have their own steps in `fido2.md`.

## macOS: Secure Enclave keys through Secretive

On a Mac with a Secure Enclave (Apple silicon, or an Intel Mac with a T2
chip), install Secretive and create a key in it. In `~/.ssh/config`, as
its setup screen shows:

```
Host test-host
  HostName <host>
  IdentityAgent ~/Library/Containers/com.maxgoedjen.Secretive.SecretAgent/Data/socket.ssh
  IdentityFile ~/Library/Containers/com.maxgoedjen.Secretive.SecretAgent/Data/PublicKeys/<id>.pub
  IdentitiesOnly yes
```

Add the public key to the host's `~/.ssh/authorized_keys`. Import
`~/.ssh/config` into Tauminus; the host's form shows Secretive's socket
under "Agent socket". Keep a second key in the agent `SSH_AUTH_SOCK`
names.

| Do | Expect |
|---|---|
| Connect to `test-host` | Secretive asks for Touch ID if the key wants it; the shell opens |
| Connect with Secretive's agent quit | The error says it had nothing to offer; no passphrase is asked for the `.pub` file |
| Connect, with the second key in the default agent | Only Secretive's key is offered: the host's log shows one public key tried |
| Set the host's agent socket to `none` | No agent is asked; the sign-in fails unless a key file signs |

## Windows: FIDO2 keys and Windows Hello through the OpenSSH agent

With OpenSSH for Windows 8.9 or later, `Start-Service ssh-agent`, then:

```
ssh-keygen -t ecdsa-sk -f $HOME\.ssh\id_ecdsa_sk
ssh-add $HOME\.ssh\id_ecdsa_sk
```

| Do | Expect |
|---|---|
| Run `ssh-keygen -t ecdsa-sk` with no USB key plugged in | Record what happens: whether Windows Hello offers itself as the authenticator. This decides whether Windows users get a Hello-bound key before v0.5 |
| Connect to a host holding the key's `.pub` | The agent's own prompt asks for the touch or Hello; the shell opens |

## Status

No run on real hardware has been recorded yet. Record the first here,
with the OS version, the agent and its version, and the Tauminus commit.
