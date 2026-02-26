#!/usr/bin/env bash
set -euo pipefail
VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
echo "Syncing npm package versions to $VERSION..."
for pkg in npm/actual \
           npm/actual-darwin-arm64 npm/actual-darwin-x64 \
           npm/actual-linux-x64 npm/actual-linux-arm64; do
  tmp=$(mktemp)
  jq --arg v "$VERSION" \
    'if .optionalDependencies then .version = $v | .optionalDependencies |= with_entries(.value = $v) else .version = $v end' \
    "$pkg/package.json" > "$tmp"
  mv "$tmp" "$pkg/package.json"
  echo "  Updated $pkg/package.json"
done
echo "Done."
