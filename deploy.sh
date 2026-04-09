#!/bin/bash
set -euo pipefail

cd "$(dirname "$0")"

echo "Building..."
cargo build --release

echo "Installing binaries..."
install -m755 target/release/claude-architect ~/.local/bin/
install -m755 target/release/claude-architect-hook ~/.local/bin/
install -m755 target/release/claude-architect-mcp ~/.local/bin/
install -m755 target/release/claude-architect-ctl ~/.local/bin/

echo "Installing service..."
install -Dm644 claude-architect.service ~/.config/systemd/user/claude-architect.service
systemctl --user daemon-reload

echo "Restarting daemon..."
systemctl --user restart claude-architect

echo "Verifying..."
sleep 0.5
systemctl --user is-active claude-architect
claude-architect-ctl ping
