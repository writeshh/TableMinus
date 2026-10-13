#!/bin/sh
# Runs the SSH tunnel test (ssh::tests::tunnel_to_local_mysql) through a throwaway sshd on 127.0.0.1:2222.
# Needs MySQL on 127.0.0.1:3306 with a passwordless root. Uses a temporary HOME, so ~/.ssh is untouched.
set -eu
dir=$(mktemp -d)
trap 'kill "$(cat "$dir/sshd.pid" 2>/dev/null)" 2>/dev/null; rm -rf "$dir"' EXIT

mkdir -p "$dir/home/.ssh"
ssh-keygen -q -t ed25519 -N '' -f "$dir/hostkey"
ssh-keygen -q -t ed25519 -N '' -f "$dir/home/.ssh/id_ed25519"
ssh-keygen -q -t ed25519 -N '' -f "$dir/home/wrong_key"
cp "$dir/home/.ssh/id_ed25519.pub" "$dir/authorized_keys"
cat > "$dir/sshd_config" <<EOF
Port 2222
ListenAddress 127.0.0.1
HostKey $dir/hostkey
PidFile $dir/sshd.pid
AuthorizedKeysFile $dir/authorized_keys
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
AllowTcpForwarding yes
EOF
/usr/sbin/sshd -f "$dir/sshd_config"

cd "$(dirname "$0")/../src-tauri"
env -u SSH_AUTH_SOCK HOME="$dir/home" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" \
  cargo test tunnel_to_local_mysql -- --ignored
