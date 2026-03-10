#!/usr/bin/env bash
# build-skill-variants.sh - Generate distribution variants of the actual skill.
#
# The canonical skill lives at .claude/skills/actual/SKILL.md.
# This script produces variant copies with additional content injected at
# marker comments.  Currently the only variant is "openclaw".
#
# Usage:
#   bash scripts/build-skill-variants.sh [--check]
#
# --check  Verify that dist/ is up to date (for CI). Exits 1 if stale.
#
# Output:
#   dist/skills/openclaw/actual/SKILL.md        (enriched SKILL.md)
#   dist/skills/openclaw/actual/references/      (copied verbatim)
#   dist/skills/openclaw/actual/scripts/          (copied verbatim)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CANONICAL="$REPO_ROOT/.claude/skills/actual"
DIST="$REPO_ROOT/dist/skills/openclaw/actual"

CHECK_MODE=false
if [[ "${1:-}" == "--check" ]]; then
  CHECK_MODE=true
fi

# --- Build ---

build_openclaw() {
  mkdir -p "$DIST"

  local src="$CANONICAL/SKILL.md"
  local dst="$DIST/SKILL.md"
  local start_marker="<!-- variant:openclaw-start -->"
  local end_marker="<!-- variant:openclaw-end -->"

  # Verify markers exist
  if ! grep -q "$start_marker" "$src"; then
    echo "ERROR: Start marker not found in $src" >&2
    exit 1
  fi
  if ! grep -q "$end_marker" "$src"; then
    echo "ERROR: End marker not found in $src" >&2
    exit 1
  fi

  # Build the output: everything before the start marker, then the injected
  # section, then everything after the end marker.  Markers themselves are
  # stripped from the output.
  {
    # Lines before the start marker
    sed -n "/$start_marker/q;p" "$src"

    # Injected OpenClaw section
    cat <<'INJECT'
## ADR Pre-Check (OpenClaw)

Before creating a new skill, component, or feature, check whether the project
has ADR context available. This ensures new work aligns with existing
architectural decisions.

1. Check for managed section markers in CLAUDE.md or AGENTS.md:
   ```bash
   grep -l "managed:actual-start" CLAUDE.md AGENTS.md 2>/dev/null
   ```
2. **If markers found**: ADR context is already loaded into the output file.
   Proceed — the agent session already has this context.
3. **If no markers but `actual` CLI is available**:
   ```bash
   actual adr-bot --dry-run
   ```
   Review the output. If relevant ADRs exist, run `actual adr-bot` to sync
   them into CLAUDE.md / AGENTS.md before starting work.
4. **If no markers and no CLI**: proceed without. Do not block the user.

INJECT

    # Lines after the end marker
    sed -n "/$end_marker/,\$p" "$src" | tail -n +2
  } > "$dst"

  # Copy references and scripts verbatim
  rm -rf "$DIST/references" "$DIST/scripts"
  cp -R "$CANONICAL/references" "$DIST/references"
  cp -R "$CANONICAL/scripts" "$DIST/scripts"

  echo "Built openclaw variant at $DIST"
}

check_openclaw() {
  local tmpdir
  tmpdir=$(mktemp -d)
  trap 'rm -rf "${tmpdir:-}"' EXIT

  # Build to temp location
  local saved_dist="$DIST"
  DIST="$tmpdir/actual"
  build_openclaw >/dev/null 2>&1
  DIST="$saved_dist"

  # Compare SKILL.md (the only file that differs)
  if ! diff -q "$saved_dist/SKILL.md" "$tmpdir/actual/SKILL.md" >/dev/null 2>&1; then
    echo "STALE: dist/skills/openclaw/actual/SKILL.md is out of date." >&2
    echo "Run: bash scripts/build-skill-variants.sh" >&2
    exit 1
  fi

  echo "OK: dist/skills/openclaw/ is up to date."
}

if $CHECK_MODE; then
  check_openclaw
else
  build_openclaw
fi
