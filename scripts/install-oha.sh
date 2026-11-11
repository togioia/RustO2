#!/usr/bin/env bash
# Install oha (https://github.com/hatoo/oha) into ~/.local/bin without sudo.
set -euo pipefail

DEST="${DEST:-$HOME/.local/bin}"
mkdir -p "$DEST"
curl -fsSL -o "$DEST/oha" \
  https://github.com/hatoo/oha/releases/latest/download/oha-linux-amd64
chmod +x "$DEST/oha"
echo "oha installed at $DEST/oha"
echo "Make sure $DEST is in your PATH."
