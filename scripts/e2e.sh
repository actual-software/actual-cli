#!/usr/bin/env bash
# End-to-end test for actual CLI.
#
# Usage:
#   scripts/e2e.sh <path-to-actual-binary>
#
# Environment:
#   CLAUDE_CODE_OAUTH_TOKEN  - required: OAuth token for Claude Code auth
#   ACTUAL_E2E_API_URL       - optional: API URL (default: staging)
#
# Exit codes:
#   0 - all scenarios passed
#   1 - one or more scenarios failed

set -euo pipefail

# ── Config ───────────────────────────────────────────────────────────────────

ACTUAL_BIN="$(cd "$(dirname "${1:?Usage: $0 <path-to-actual-binary>}")" && pwd)/$(basename "$1")"
API_URL="${ACTUAL_E2E_API_URL:-https://api-service.api.staging.actual.ai}"
RESULTS_FILE="$(mktemp)"
trap 'rm -f "$RESULTS_FILE"' EXIT

# ── Helpers ──────────────────────────────────────────────────────────────────

log()  { echo "  $*"; }
ok()   { echo "  [PASS] $*"; echo "PASS" >> "$RESULTS_FILE"; }
fail() { echo "  [FAIL] $*"; echo "FAIL" >> "$RESULTS_FILE"; }

# Create a temp directory, run a test function inside it, clean up afterward.
# Usage: run_scenario <name> <setup_function>
run_scenario() {
    local name="$1"
    local setup_fn="$2"
    local dir
    dir="$(mktemp -d)"

    echo ""
    echo "Scenario: $name"

    local before_fail_count scenario_exit=0
    before_fail_count=$(grep -c '^FAIL$' "$RESULTS_FILE" 2>/dev/null || echo 0)

    (
        cd "$dir"
        "$setup_fn" "$dir"
    ) || scenario_exit=$?

    rm -rf "$dir"

    if [[ "$scenario_exit" -ne 0 ]]; then
        local after_fail_count
        after_fail_count=$(grep -c '^FAIL$' "$RESULTS_FILE" 2>/dev/null || echo 0)
        if [[ "$after_fail_count" -eq "$before_fail_count" ]]; then
            # Crash with no fail() call — record the failure
            echo "FAIL" >> "$RESULTS_FILE"
            echo "  [FAIL] $name: scenario aborted with exit $scenario_exit (no fail() recorded)"
        fi
    fi
}

# Initialize a bare git repo in the given directory.
git_init() {
    local dir="$1"
    git -C "$dir" init -q
    git -C "$dir" config user.email "test@example.com"
    git -C "$dir" config user.name "Test User"
    git -C "$dir" add .
    git -C "$dir" commit -qm "init"
}

# ── Project fixtures ──────────────────────────────────────────────────────────

# Minimal TypeScript/Express project - reliably matches ADRs on staging.
create_minimal_ts_project() {
    local dir="$1"
    cat > "$dir/package.json" <<'EOF'
{
  "name": "test-project",
  "version": "1.0.0",
  "dependencies": {
    "express": "^4.18.0",
    "typescript": "^5.0.0"
  }
}
EOF
    cat > "$dir/tsconfig.json" <<'EOF'
{
  "compilerOptions": {
    "target": "ES2020",
    "module": "commonjs",
    "strict": true,
    "outDir": "dist"
  }
}
EOF
    mkdir -p "$dir/src"
    cat > "$dir/src/index.ts" <<'EOF'
import express from 'express';

const app = express();
app.use(express.json());

app.get('/health', (_req, res) => {
  res.json({ ok: true });
});

app.listen(3000, () => {
  console.log('Server running on port 3000');
});
EOF
}

# Realistic Next.js project - matches more ADRs than the minimal fixture.
create_realistic_ts_project() {
    local dir="$1"
    cat > "$dir/package.json" <<'EOF'
{
  "name": "sample-nextjs-app",
  "version": "1.0.0",
  "dependencies": {
    "next": "^15.0.0",
    "react": "^18.0.0",
    "react-dom": "^18.0.0",
    "typescript": "^5.0.0",
    "zod": "^3.0.0"
  },
  "devDependencies": {
    "@types/node": "^20.0.0",
    "@types/react": "^18.0.0",
    "jest": "^29.0.0"
  }
}
EOF
    cat > "$dir/tsconfig.json" <<'EOF'
{
  "compilerOptions": {
    "target": "ES2020",
    "lib": ["dom", "dom.iterable", "esnext"],
    "module": "esnext",
    "moduleResolution": "bundler",
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "jsx": "preserve"
  }
}
EOF
    cat > "$dir/next.config.ts" <<'EOF'
import type { NextConfig } from 'next';
const config: NextConfig = { reactStrictMode: true };
export default config;
EOF
    mkdir -p "$dir/app/api/users" "$dir/lib" "$dir/tests"
    cat > "$dir/app/page.tsx" <<'EOF'
export default function HomePage() {
  return <main><h1>Hello World</h1></main>;
}
EOF
    cat > "$dir/app/layout.tsx" <<'EOF'
export default function RootLayout({ children }: { children: React.ReactNode }) {
  return <html lang="en"><body>{children}</body></html>;
}
EOF
    cat > "$dir/app/api/users/route.ts" <<'EOF'
import { NextResponse } from 'next/server';
import { z } from 'zod';

const UserSchema = z.object({ name: z.string().min(1), email: z.string().email() });

export async function POST(req: Request) {
  const body = await req.json();
  const result = UserSchema.safeParse(body);
  if (!result.success) return NextResponse.json({ errors: result.error.flatten() }, { status: 400 });
  return NextResponse.json({ user: result.data }, { status: 201 });
}
EOF
    cat > "$dir/lib/db.ts" <<'EOF'
export async function query<T>(sql: string, params: unknown[]): Promise<T[]> {
  void sql; void params; return [];
}
EOF
    cat > "$dir/tests/api.test.ts" <<'EOF'
describe('API routes', () => { it('works', () => expect(true).toBe(true)); });
EOF
    printf 'node_modules\n.next\ndist\n' > "$dir/.gitignore"
}

# ── Scenarios ─────────────────────────────────────────────────────────────────

scenario_auth_check() {
    local _dir="$1"
    log "Checking Claude Code authentication..."
    if "$ACTUAL_BIN" auth | grep -qi "uthenticat"; then
        ok "auth: actual auth reports authenticated"
    else
        fail "auth: actual auth did not report authenticated"
    fi
}

scenario_sync_no_tailor() {
    local dir="$1"
    log "Creating minimal TypeScript/Express project..."
    create_minimal_ts_project "$dir"
    git_init "$dir"

    log "Running: actual adr-bot --force --no-tailor"
    "$ACTUAL_BIN" adr-bot --force --no-tailor --api-url "$API_URL"

    if [[ ! -f "$dir/CLAUDE.md" ]]; then
        fail "sync_no_tailor: CLAUDE.md was not created"
        return
    fi
    ok "sync_no_tailor: CLAUDE.md created"

    local content
    content="$(cat "$dir/CLAUDE.md")"

    if echo "$content" | grep -q '<!-- managed:actual-start -->'; then
        ok "sync_no_tailor: managed-start marker present"
    else
        fail "sync_no_tailor: missing <!-- managed:actual-start -->"
    fi

    if echo "$content" | grep -q '<!-- managed:actual-end -->'; then
        ok "sync_no_tailor: managed-end marker present"
    else
        fail "sync_no_tailor: missing <!-- managed:actual-end -->"
    fi

    if echo "$content" | grep -q '<!-- version: 1 -->'; then
        ok "sync_no_tailor: version marker present"
    else
        fail "sync_no_tailor: missing <!-- version: 1 -->"
    fi

    if echo "$content" | grep -q '<!-- adr-ids:'; then
        ok "sync_no_tailor: adr-ids metadata present"
    else
        fail "sync_no_tailor: missing <!-- adr-ids: -->"
    fi

    if echo "$content" | grep -q '## '; then
        ok "sync_no_tailor: at least one ADR heading present"
    else
        fail "sync_no_tailor: no ADR headings found"
    fi
}

scenario_sync_realistic_project() {
    local dir="$1"
    log "Creating realistic Next.js/TypeScript project..."
    create_realistic_ts_project "$dir"
    git_init "$dir"

    log "Running: actual adr-bot --force --no-tailor"
    "$ACTUAL_BIN" adr-bot --force --no-tailor --api-url "$API_URL"

    if [[ ! -f "$dir/CLAUDE.md" ]]; then
        fail "sync_realistic: CLAUDE.md was not created"
        return
    fi
    ok "sync_realistic: CLAUDE.md created"

    local content
    content="$(cat "$dir/CLAUDE.md")"

    local heading_count
    heading_count="$(echo "$content" | grep -c '^## ' || true)"
    if [[ "$heading_count" -ge 3 ]]; then
        ok "sync_realistic: $heading_count ADR headings (>= 3 expected)"
    else
        fail "sync_realistic: only $heading_count ADR headings, expected >= 3"
    fi

    local adr_ids_line comma_count
    adr_ids_line="$(echo "$content" | grep '<!-- adr-ids:' || true)"
    comma_count="$(echo "$adr_ids_line" | tr -cd ',' | wc -c | tr -d ' ')"
    if [[ "$comma_count" -ge 2 ]]; then
        ok "sync_realistic: $((comma_count + 1)) ADR IDs in metadata (>= 3 expected)"
    else
        fail "sync_realistic: only $((comma_count + 1)) ADR ID(s), expected >= 3; line: $adr_ids_line"
    fi

    local managed_start managed_end managed_len
    managed_start="$(echo "$content" | grep -n '<!-- managed:actual-start -->' | cut -d: -f1 | head -1 || true)"
    managed_end="$(echo "$content" | grep -n '<!-- managed:actual-end -->' | cut -d: -f1 | head -1 || true)"
    if [[ -z "$managed_start" || -z "$managed_end" ]]; then
        fail "sync_realistic: managed-start or managed-end marker not found"
        return
    fi
    managed_len=$((managed_end - managed_start))
    if [[ "$managed_len" -gt 20 ]]; then
        ok "sync_realistic: managed section spans $managed_len lines (substantial)"
    else
        fail "sync_realistic: managed section only $managed_len lines, expected > 20"
    fi
}

scenario_resync_preserves_user_content() {
    local dir="$1"
    log "Creating minimal TypeScript/Express project..."
    create_minimal_ts_project "$dir"
    git_init "$dir"

    log "First sync..."
    "$ACTUAL_BIN" adr-bot --force --no-tailor --api-url "$API_URL"

    if [[ ! -f "$dir/CLAUDE.md" ]]; then
        fail "resync: first adr-bot run did not create CLAUDE.md"
        return
    fi

    log "Prepending user content..."
    local original
    original="$(cat "$dir/CLAUDE.md")"
    printf '# My Custom Notes\n\nUser content here.\n\n%s' "$original" > "$dir/CLAUDE.md"

    log "Second sync..."
    "$ACTUAL_BIN" adr-bot --force --no-tailor --api-url "$API_URL"

    local content
    content="$(cat "$dir/CLAUDE.md")"

    if echo "$content" | grep -q '# My Custom Notes'; then
        ok "resync: user heading preserved"
    else
        fail "resync: user heading lost"
    fi

    if echo "$content" | grep -q 'User content here\.'; then
        ok "resync: user content preserved"
    else
        fail "resync: user content lost"
    fi

    if echo "$content" | grep -q '<!-- version: 2 -->'; then
        ok "resync: version incremented to 2"
    else
        fail "resync: version not incremented to 2 (got: $(echo "$content" | grep '<!-- version:' || echo 'not found'))"
    fi
}

scenario_dry_run_no_files() {
    local dir="$1"
    log "Creating minimal TypeScript/Express project..."
    create_minimal_ts_project "$dir"
    git_init "$dir"

    log "Running: actual adr-bot --force --no-tailor --dry-run"
    "$ACTUAL_BIN" adr-bot --force --no-tailor --dry-run --api-url "$API_URL"

    if [[ ! -f "$dir/CLAUDE.md" ]]; then
        ok "dry_run: CLAUDE.md not created (correct)"
    else
        fail "dry_run: CLAUDE.md was created but should not be with --dry-run"
    fi
}

# ── Preflight ─────────────────────────────────────────────────────────────────

echo "actual CLI E2E"
echo "=============="
echo "Binary : $ACTUAL_BIN"
echo "API URL: $API_URL"

if [[ ! -x "$ACTUAL_BIN" ]]; then
    echo "ERROR: binary not found or not executable: $ACTUAL_BIN" >&2
    exit 1
fi

# Verify Claude Code is installed and authenticated
if ! claude auth status --json 2>/dev/null | grep -q '"loggedIn": *true'; then
    echo "ERROR: Claude Code not authenticated (CLAUDE_CODE_OAUTH_TOKEN set?)" >&2
    exit 1
fi
echo "Claude : authenticated"

# ── Run scenarios ─────────────────────────────────────────────────────────────

run_scenario "auth_check"                  scenario_auth_check
run_scenario "sync_no_tailor"              scenario_sync_no_tailor
run_scenario "sync_realistic_project"      scenario_sync_realistic_project
run_scenario "resync_preserves_user_content" scenario_resync_preserves_user_content
run_scenario "dry_run_no_files"            scenario_dry_run_no_files

# ── Summary ───────────────────────────────────────────────────────────────────

PASS=$(grep -c '^PASS$' "$RESULTS_FILE" || true)
FAIL=$(grep -c '^FAIL$' "$RESULTS_FILE" || true)

echo ""
echo "Results: $PASS passed, $FAIL failed"

if [[ "$FAIL" -gt 0 ]]; then
    exit 1
fi
