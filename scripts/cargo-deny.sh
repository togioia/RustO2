#!/usr/bin/env bash
# Run cargo-deny against a throwaway copy of the workspace.
#
# Why a copy: RustO2's workspace crates are internal and are not published, but
# their manifests do not set `publish = false`. cargo-deny's wildcard check
# therefore classifies them as public and rejects the path-only dependency on
# `rusto2-common`. Adding `publish = false` to the checked-in manifests would
# fix that, but dependency manifests are intentionally out of scope for this
# supply-chain change. The temporary copy gets the marker injected, so
# `wildcards = "deny"` still applies to every registry dependency.
#
# Usage:
#   bash scripts/cargo-deny.sh
#   bash scripts/cargo-deny.sh advisories licenses sources
set -euo pipefail

cd "$(dirname "$0")/.."

for tool in cargo cargo-deny; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is not installed" >&2
    exit 127
  fi
done

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Copy tracked and untracked project files, excluding build output and VCS data.
tar --exclude=.git --exclude=target -cf - . | tar -xf - -C "$tmp"

for manifest in "$tmp"/crates/*/Cargo.toml; do
  [ -f "$manifest" ] || continue
  if ! grep -Eq '^[[:space:]]*publish[[:space:]]*=' "$manifest"; then
    # Insert the marker immediately after the [package] table header.
    # The explicit backup suffix keeps BSD/macOS sed happy too.
    sed -i.bak '/^\[package\]/a publish = false' "$manifest"
    rm -f "$manifest.bak"
  fi
done

cd "$tmp"
cargo deny check "$@"
