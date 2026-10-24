#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Tauminus contributors
# SPDX-License-Identifier: GPL-3.0-or-later

# Sets up FIDO2 (`sk-`) keys in an ssh-agent and an OpenSSH server that
# takes them, for crates/tauminus-ssh/tests/sk_agent.rs. With no security
# key at hand, OpenSSH's own test provider `sk-dummy.so` stands in: it
# signs as a key would, without a touch. It is built from the source of
# the OpenSSH installed (the provider API changes between releases),
# pinned by hash: each tarball's signature was checked against the
# OpenSSH release key, 7168B983815A5EEF59A4ADFD2A3F414E736060BA, before
# its hash went here. Run after ci/password-sshd.sh, which made the
# user; the sshd part runs as root through sudo. CI or a container only:
# sk-dummy's key handle is the private key itself, and the agent and the
# server keep running.
#
# Prints, and appends to $GITHUB_ENV when set:
#   TAUMINUS_SK_SSHD=<port> <user> <agent socket> <key dir>

set -euo pipefail

port=${SK_PORT:-2298}
user=tauminus-pw
installed=$(ssh -V 2>&1 | sed -n 's/^OpenSSH_\([0-9.]*p[0-9]*\).*/\1/p')
case "$installed" in
  8.9p1) sha256=fd497654b7ab1686dac672fb83dfb4ba4096e8b5ffcdaccd262380ae58bec5e7 ;;
  9.6p1) sha256=910211c07255a8c5ad654391b40ee59800710dd8119dd5362de09385aa7a777c ;;
  *)
    echo "sk-agent.sh: no pinned source for OpenSSH $installed" >&2
    exit 1
    ;;
esac
version=openssh-$installed

work=$(mktemp -d)
keys="$work/keys"
mkdir -m 700 "$keys"

curl -fsSL --proto '=https' --tlsv1.2 --retry 3 --connect-timeout 10 --max-time 120 \
  -o "$work/$version.tar.gz" \
  "https://cdn.openbsd.org/pub/OpenBSD/OpenSSH/portable/$version.tar.gz"
echo "$sha256  $work/$version.tar.gz" | sha256sum -c - > /dev/null
tar -xzf "$work/$version.tar.gz" -C "$work"
(
  cd "$work/$version"
  # Warnings (OpenSSL 3 deprecations) would fill the log; errors still
  # show.
  ./configure --quiet CFLAGS=-w > /dev/null
  make -s -j"$(nproc)" libssh.a openbsd-compat/libopenbsd-compat.a > /dev/null
  make -s regress/misc/sk-dummy/sk-dummy.so > /dev/null
)
cp "$work/$version/regress/misc/sk-dummy/sk-dummy.so" "$keys/"

for type in ed25519-sk ecdsa-sk; do
  ssh-keygen -q -t "$type" -w "$keys/sk-dummy.so" -N '' -f "$keys/id_$type" < /dev/null
done

# The agent loads the provider only from where -P allows.
ssh-agent -a "$keys/agent.sock" -P "$keys/*" > /dev/null
for type in ed25519-sk ecdsa-sk; do
  SSH_AUTH_SOCK="$keys/agent.sock" SSH_SK_PROVIDER="$keys/sk-dummy.so" \
    ssh-add -q "$keys/id_$type"
done

# A server that takes public keys only, for the same user.
sudo bash -s "$port" "$user" "$keys" << 'ROOT'
set -euo pipefail
port=$1 user=$2 keys=$3
home=$(getent passwd "$user" | cut -d: -f6)
# Written as the user, so no link in that home leads root elsewhere.
cat "$keys"/id_*-sk.pub | sudo -u "$user" sh -c \
  'umask 077 && mkdir -p ~/.ssh && cat > ~/.ssh/authorized_keys' 
dir=$(mktemp -d)
ssh-keygen -q -t ed25519 -N '' -f "$dir/host"
cat > "$dir/sshd_config" << EOF
Port $port
ListenAddress 127.0.0.1
HostKey $dir/host
PidFile $dir/sshd.pid
PubkeyAuthentication yes
AuthenticationMethods publickey
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
PermitRootLogin no
AllowUsers $user
EOF
mkdir -p /run/sshd
/usr/sbin/sshd -f "$dir/sshd_config" -E "$dir/sshd.log"
echo "sk-agent.sh: sshd log in $dir/sshd.log" >&2
ROOT

line="TAUMINUS_SK_SSHD=$port $user $keys/agent.sock $keys"
if [ -n "${GITHUB_ENV:-}" ]; then
  echo "$line" >> "$GITHUB_ENV"
fi
echo "$line"
