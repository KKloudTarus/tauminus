# ADR 0002: Self-hosted Hub for team features

- Status: accepted
- Date: 2026-10-21

## Context

Tauminus aims for parity with every Termius tier. Termius Team, Business, and
Enterprise features need a central authority: team vaults with roles, access
revocation, SSO, SCIM, shared session logs, audit logs, and live terminal
sharing. Tauminus is local-first and asks for no account.

Personal sync works without a server. An encrypted blob in a git repo, an S3
bucket, or a synced folder is enough for one person on several devices.

Team features do not work that way:

- Revocation needs an authority that stops serving new keys to a removed member
  and rotates vault keys.
- SSO and SCIM need a server that the identity provider calls.
- Live sharing needs a relay both peers can reach.
- An audit log needs a writer that members cannot edit.

## Decision

Build Tauminus Hub: a self-hosted server, GPL-3.0-or-later, written in Rust,
shipped as one binary plus Postgres and as a Docker image. The Tauminus
project does not run a hosted Hub in the first releases.

## Constraints

- The desktop app works fully without a Hub. Hub features are additive.
- Vault contents stay end-to-end encrypted. The Hub stores ciphertext and
  wrapped keys and cannot read secrets.
- Live sharing goes through the Hub relay end-to-end encrypted between peers.
- The client speaks OIDC. SAML terminates on the Hub.

## Consequences

- Teams need to operate a service. Small teams can use git or S3 sync with
  shared keys and skip the Hub, without revocation.
- The Hub is a second product with its own threat model, tests, and security
  review before v0.6 ships.
- A hosted Hub can come later without changing the protocol.
