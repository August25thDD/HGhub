#!/usr/bin/env bash
# Run as the account that will own this personal service. No root is required.
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
export XDG_RUNTIME_DIR="/run/user/$(id -u)"
export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
binary="$project_dir/web/target/release/hghub-web"
private_dir="$project_dir/.clawbench/private"
data_dir="$project_dir/.clawbench/web-data"
test -x "$binary"
test -f "$project_dir/dist/index.html"
umask 077
mkdir -p "$private_dir" "$data_dir" "$HOME/.config/systemd/user"
if [[ ! -f "$private_dir/web-password.hash" ]]; then
  if [[ ! -f "$private_dir/web-password.txt" ]]; then
    openssl rand -hex 24 > "$private_dir/web-password.txt"
  fi
  "$binary" --hash-password < "$private_dir/web-password.txt" > "$private_dir/web-password.hash"
fi
# Retain an existing origin / listener when updating a configured deployment.
if [[ ! -f "$private_dir/web.env" ]]; then
  cat > "$private_dir/web.env" <<EOF
HGHUB_LISTEN=127.0.0.1:8900
HGHUB_PUBLIC_ORIGIN=http://127.0.0.1:8900
HGHUB_PASSWORD_HASH_FILE=$private_dir/web-password.hash
HGHUB_DATA_DIR=$data_dir
HGHUB_DIST=$project_dir/dist
HGHUB_STRICT=1
EOF
fi
cat > "$HOME/.config/systemd/user/hghub-web.service" <<EOF
[Unit]
Description=HGhub password-gated Rust HTTP service
After=network.target

[Service]
Type=simple
WorkingDirectory=$project_dir
EnvironmentFile=$private_dir/web.env
ExecStart=$binary
Restart=on-failure
RestartSec=5
UMask=0077
NoNewPrivileges=true

[Install]
WantedBy=default.target
EOF
systemctl --user daemon-reload
systemctl --user enable --now hghub-web.service
systemctl --user is-enabled hghub-web.service
systemctl --user is-active hghub-web.service
loginctl show-user "$(id -un)" -p Linger
printf 'Private credentials: %s\n' "$private_dir/web-password.txt"
printf 'Listener: 127.0.0.1:8900 (unless an existing configuration was retained)\n'
