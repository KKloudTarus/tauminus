# ADR 0033: Updates from the project's releases

- Status: accepted
- Date: 2026-10-26

## Context

v0.5 lists auto-update with `tauri-plugin-updater`, its signing key in
CI secrets. An SSH client holds keys and passwords, so whoever can make
it install a build owns every server it reaches. Releases are built by
`.github/workflows/release.yml` as drafts the maintainer publishes.

## Decision

`tauri-plugin-updater` 2.14.0 reads `latest.json` from the latest
GitHub release, over HTTPS only. A bundle installs only when its
minisign signature checks against the public key in `tauri.conf.json`
and names the version `latest.json` announces (`requireSignedVersion`),
so an edited `latest.json` cannot pair a new version number with an
older bundle. The private key and its password live in the
`TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
secrets of the `release` environment and nowhere in the repository.

The key never meets the build. The release builds and bundles without
it; a `sign` job then takes the bundles, builds only `cargo xtask`, and
with the key in reach runs `cargo xtask updater-sign`, which signs each
bundle as `tauri signer` does with `version:` in the trusted comment,
checks every signature with `minisign-verify` (what the app uses),
checks the key against the public key in `tauri.conf.json`, writes
`latest.json` with one entry per target the plugin looks up, and
attaches them to the draft release. No package install and no app build
runs in that job.

- Only a `v*` tag may use the `release` environment, the maintainer
  approves each run of it, and a ruleset lets only repository admins
  create, move or delete `v*` tags. A manual run signs with a key made
  for that run, to check the files, and attaches nothing.
- The release notes in `latest.json` are the tag's message
  (`git tag -a`).

- The plugin's `zip` feature is off. It only reads the formats of
  Tauri 1 (zipped Windows installers, gzipped AppImages), and the `zip`
  crate it brings had three open advisories (RUSTSEC-2026-0367 to
  0369). Windows gets the `.msi` or `.exe` itself, Linux the AppImage
  itself, macOS the `.app.tar.gz`, whose `tar` the plugin needs anyway.
  Its system proxy feature is off too, as for every other client here.
- The webview has no updater permission. It calls two app commands:
  `update_check`, which keeps the update it found, and `update_install`,
  which installs only that one and restarts. Release notes reach the
  view with control and bidi characters removed, cut to 4000
  characters, and are shown as text.
- A check gives up after 30 s. A download reports each whole percent
  and is given up after 60 s without a byte, however long it takes in
  all. Release builds take a bundle only from `https://github.com`.
  Installing runs off the async workers, as a `.deb` or `.rpm` waits on
  a password dialog.
- The app looks when it starts, at most once a day, and the user can
  turn that off from the command palette. A look at start says nothing
  unless it finds a version; a check the user asks for always answers.
  Nothing installs until the user presses "Install and restart".
- Releases are no longer marked prerelease, as `releases/latest`
  skips prereleases; the draft the maintainer publishes is the last
  review.
- `.deb` and `.rpm` installs update through `dpkg -i` or `rpm -U`, as
  the plugin does, run through `pkexec`, else a password dialog for
  `sudo`, else `sudo` itself.

## Consequences

- The updater key cannot be rotated without a release that carries the
  new public key and is signed with the old one. Losing the private key
  or its password strands every installed copy on its version.
- Until a release is published with `latest.json`, a check finds
  nothing to read (404): quietly at start, as an error when asked.
- The `sign` job still builds `xtask`'s dependencies on the runner
  before the key arrives; a build script that left a process behind
  could read the next step's environment. That is a far smaller set than
  the app's crates and npm packages, and is the risk left.
- The plugin holds the whole bundle in memory and checks its signature
  on an async worker (about 1.5 ms per MB); a download cannot be capped
  in size from outside the plugin.
- Checked live with a debug build pointed at a local server, and
  bundles `cargo xtask updater-sign` signed with the real key: a
  signature for another file was refused, a `latest.json` announcing
  0.0.2 for a bundle signed as 0.0.1 was refused, a signature with no
  version was refused, and a correct one installed in place of the app
  file and restarted into it.
- `a_bundle_installs_only_as_it_was_signed_for_its_version` repeats
  those refusals on every CI run: `update_check` and the download
  `update_install` uses, with the settings in `tauri.conf.json`, against
  a local server and bundles signed by a test key. The stall abort
  has no test: it needs `STALL` and `WATCH` injected to run in time.
