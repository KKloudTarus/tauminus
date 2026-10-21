#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Tauminus contributors
# SPDX-License-Identifier: GPL-3.0-or-later

# Starts an OpenSSH server that checks passwords, for the password and
# keyboard-interactive tests in crates/tauminus-ssh/tests/password_sshd.rs.
# Checking a password needs root (PAM reads the shadow file), so this runs
# as root on a throwaway machine: a CI runner or a container. It creates a
# user with a random password and never touches the system sshd.
#
# Prints, and appends to $GITHUB_ENV when set:
#   TAUMINUS_PASSWORD_SSHD=<port> <user> <password>

set -euo pipefail

if [ "$(id -u)" -ne 0 ]; then
  echo "password-sshd.sh must run as root on a throwaway machine" >&2
  exit 1
fi

port=${PORT:-2299}
user=tauminus-pw
password=$(head -c 18 /dev/urandom | base64 | tr -d '/+=')
dir=$(mktemp -d)

if ! id "$user" > /dev/null 2>&1; then
  useradd --create-home --shell /bin/sh "$user"
fi
echo "$user:$password" | chpasswd

ssh-keygen -q -t ed25519 -N '' -f "$dir/host"
cat > "$dir/sshd_config" << EOF
Port $port
ListenAddress 127.0.0.1
HostKey $dir/host
PidFile $dir/sshd.pid
PasswordAuthentication yes
KbdInteractiveAuthentication yes
UsePAM yes
PubkeyAuthentication no
PermitRootLogin no
AllowUsers $user
EOF
# sshd refuses to start as root without its privilege separation directory.
mkdir -p /run/sshd
/usr/sbin/sshd -f "$dir/sshd_config" -E "$dir/sshd.log"

if [ -n "${GITHUB_ACTIONS:-}" ]; then
  echo "::add-mask::$password"
fi
line="TAUMINUS_PASSWORD_SSHD=$port $user $password"
if [ -n "${GITHUB_ENV:-}" ]; then
  echo "$line" >> "$GITHUB_ENV"
fi
echo "$line"
