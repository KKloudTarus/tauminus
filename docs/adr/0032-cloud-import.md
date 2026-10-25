# ADR 0032: Hosts from cloud accounts

- Status: accepted
- Date: 2026-10-25

## Context

The v0.5 roadmap imports servers from AWS (EC2 and Lightsail),
DigitalOcean, Azure and GCP. A panel of three (architecture, security,
and an advocate for the providers' own command-line tools) weighed how
the accounts are reached, what may come from them, and how a second
import updates the first.

Two ways were weighed. Native clients call each provider's API with a
key pasted into the vault: an access key for AWS, a token for
DigitalOcean, an OAuth client secret for Azure, a service-account key
signed with RS256 for GCP. The providers' CLIs (`aws`, `doctl`, `az`,
`gcloud`) are run instead, signed in as the user already is.

## Decisions

### The providers' CLIs, on the desktop

An import runs the provider's CLI the user has installed and signed in
with, and reads its JSON. Tauminus keeps no cloud credential: single
sign-on, MFA, short-lived tokens and profiles stay the CLI's, as the git
sync target leaves signing in to `git` (ADR 0030). Native clients would
put long-lived keys in the vault, and Azure and GCP would need an OAuth
client of Tauminus's own; they cost weeks and new crates. The CLIs add
none, page through results themselves, and come with documented JSON.
A native path, using a token the CLI mints, is left for the mobile apps
(v1.0), which have no CLIs.

The commands, each with a fixed argument list, no shell, no input, and
a pager turned off:

| Provider | Commands |
|---|---|
| EC2 | `aws ec2 describe-regions`, then `aws ec2 describe-instances --region R` per region |
| Lightsail | `aws lightsail get-regions`, then `aws lightsail get-instances --region R` |
| DigitalOcean | `doctl account get`, then `doctl compute droplet list` |
| Azure | `az vm list --show-details` |
| GCP | `gcloud compute instances list` |

The user may name an AWS profile, an Azure subscription or a GCP
project; such a name is checked against `[A-Za-z0-9_.:@-]`, at most 128
characters, so no character means anything to `cmd.exe` on Windows,
where `az` and `gcloud` are batch files. A program is found in the
absolute entries of `PATH` and the usual install places, never in a
relative one, as `mosh-client` is; the dialog shows which file runs.
`AWS_PAGER` is empty and AWS's prompts are off, `gcloud` runs `--quiet`,
`az` `--only-show-errors`.

Each command has 120 s, four run at once, and at most 64 MiB of output
is read; past that, or past 10 000 hosts, the import stops with an
error rather than a short list. A failed command fails the import, with
its exit status and the first line of its error output, control
characters out and cut to 256 bytes.

### What a cloud may set on a host

Anyone who can tag an instance in a shared account writes what comes
back. An instance may give a host:

- its label: the name or `Name` tag, else its id, cleaned as ADR 0029
  cleans labels, with ` (id)` added when the label is taken;
- its address: the public or the private one, as the user chose for the
  import. An address that is loopback, unspecified, multicast or
  link-local (so never `169.254.169.254`), or that starts with `-`, is
  refused, and the instance listed as skipped;
- tags: a plain tag, or a key and value as `key.value`, kept only when
  that is a valid tag already, at most 32;
- its group: provider, then account or project, then region;
- its user, only where the provider says it: Lightsail's `username`,
  Azure's `adminUsername`, DigitalOcean's `root`. EC2 and GCP leave it
  to the group, where the user sets it once.

Nothing else: no jump hosts, proxy command, forwards, startup snippets,
identity or key path, port, protocol or guard. A tag `env=prod` does
not mark a host `prod`; the user does.

### One host per instance, found again by its origin

A host gains an `origin`, its account first:
`aws-ec2:<account>:<region>:<instance id>`,
`aws-lightsail:<account>:<region>:<instance id>`,
`do:<account uuid>:<droplet id>`, `azure:<vm resource id>` (which starts
with its subscription), `gcp:<project>:<numeric id>`; ASCII, at most
256 bytes. It is saved and
synced with the host, so a second device that imports matches the same
hosts. The hosts file gains version 12, written only when a host has an
origin; an older build refuses it and asks to be updated, as with every
version before.

An import first shows what it would do: hosts to add, hosts whose
address moved, hosts gone from the account, and instances skipped with
the reason. The user accepts that list, or nothing is saved; this is the
review a host from elsewhere needs before it is used (ADR 0030). A host
found again keeps everything the user set; only its address follows the
cloud. A host gone from the account is tagged `cloud-gone`, never
deleted, and only when the account it is under listed whole: a region
that failed, or another account's hosts, are never called gone. Listed
again, it loses the tag.

## Consequences

- An import needs the provider's CLI installed and signed in; without
  it the dialog says which program is missing and how to install it.
- Per import, O(R) commands for AWS's R regions, one for the others,
  and O(n + h) to match n instances with h saved hosts.
- No cloud credential is stored, synced or logged by Tauminus. What the
  CLI does with its own configuration (a credential helper, a pager) is
  the user's setup, run as the user.
- Tests run fake CLIs that print recorded JSON, fail, or hang; no cloud
  account is needed in CI.
