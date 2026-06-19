#!/bin/bash
set -euo pipefail

cd "$(dirname "$0")"

echo "Installing binaries to ~/.cargo/bin..."
cargo install --force --path .

echo "Installing service..."
install -Dm644 claude-architect.service ~/.config/systemd/user/claude-architect.service
systemctl --user daemon-reload

echo "Restarting daemon..."
systemctl --user restart claude-architect

echo "Verifying..."
sleep 0.5
systemctl --user is-active claude-architect
claude-architect-ctl ping
