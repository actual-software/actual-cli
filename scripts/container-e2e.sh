#!/usr/bin/env bash
# Container-based E2E test for actual CLI.
#
# Starts a lightweight Ubuntu container that:
#   1. Builds (or reuses) a Linux binary from the host repo
#   2. Maps the binary into the container
#   3. Starts a local auth-proxy (scripts/auth-proxy.py) that captures a fresh
#      OAuth token from the host's `claude` binary and forwards container API
#      calls to api.anthropic.com with that token
#   4. Fetches the actual skill from the standalone repo (actual-software/actual-skill)
#   5. Invokes actual adr-bot via the 'actual' skill through Claude Code
#   6. Streams all Claude Code output back to the host
#
# Usage:
#   scripts/container-e2e.sh [OPTIONS]
#
# Options:
#   --build             Force rebuild of Linux binary even if one already exists
#   --api-url URL       Override ADR API URL (default: staging)
#   --output-dir DIR    Host directory for output (default: .container-e2e-output/<timestamp>)
#   --no-tailor         Pass --no-tailor to actual adr-bot (skip AI tailoring step)
#   --platform PLAT     Docker platform (default: linux/amd64)
#   --proxy-port PORT   Port for the local auth proxy (default: 7477)
#   -h, --help          Show this help and exit
#
# Prerequisites:
#   - docker running
#   - `claude` CLI installed and logged in (run `claude auth login` if not)
#   - python3 (for auth-proxy.py)
#   - `actual_e2e_api_url` env var or --api-url flag for ADR endpoint
#
# Environment:
#   ACTUAL_E2E_API_URL  Optional: ADR API URL override (overrides --api-url)
#
# Output:
#   All container output is streamed live and also saved to --output-dir:
#     claude-run.log      Full Claude Code session output (stdout + stderr)
#     actual-auth.log     Output of 'actual auth' pre-flight check
#     actual-runners.log  Output of 'actual runners' pre-flight check
#     exit-code.txt       Container exit code
#     proxy.log           Auth proxy log (token capture + forwarding)
#
# Exit codes:
#   0  - Claude Code ran actual adr-bot successfully
#   1  - Test failed (see output logs)
#   2  - Prerequisite missing (docker, claude not logged in, etc.)

set -euo pipefail

# ── Constants ─────────────────────────────────────────────────────────────────

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT_NAME="$(basename "${BASH_SOURCE[0]}")"
CONTAINER_PLATFORM="${CONTAINER_PLATFORM:-linux/amd64}"
DEFAULT_API_URL="https://api-service.api.staging.actual.ai"
# musl target produces a fully-static binary: no GLIBC version dependency across distros
BINARY_TARGET="x86_64-unknown-linux-musl"   # matches linux/amd64, static binary

# ── Parse arguments ───────────────────────────────────────────────────────────

FORCE_BUILD=0
NO_TAILOR=0
API_URL="${ACTUAL_E2E_API_URL:-$DEFAULT_API_URL}"
TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
OUTPUT_DIR="${REPO_ROOT}/.container-e2e-output/${TIMESTAMP}"
PROXY_PORT=7477
PROXY_PID=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --build)        FORCE_BUILD=1 ;;
    --no-tailor)    NO_TAILOR=1 ;;
    --api-url)      API_URL="${2:?--api-url requires a value}"; shift ;;
    --output-dir)   OUTPUT_DIR="${2:?--output-dir requires a value}"; shift ;;
    --platform)     CONTAINER_PLATFORM="${2:?--platform requires a value}"; shift ;;
    --proxy-port)   PROXY_PORT="${2:?--proxy-port requires a value}"; shift ;;
    -h|--help)
      awk '/^# Usage:/,/^[^#]/' "${BASH_SOURCE[0]}" | grep '^#' | sed 's/^# //' | sed 's/^#//'
      exit 0
      ;;
    *)
      echo "ERROR: Unknown argument: $1" >&2
      echo "Run '$SCRIPT_NAME --help' for usage." >&2
      exit 2
      ;;
  esac
  shift
done

# Adjust binary target for ARM64 containers
if [[ "$CONTAINER_PLATFORM" == "linux/arm64" ]]; then
  BINARY_TARGET="aarch64-unknown-linux-musl"
fi

# ── Helpers ───────────────────────────────────────────────────────────────────

log()    { echo "[container-e2e] $*"; }
info()   { echo "[container-e2e] INFO: $*"; }
warn()   { echo "[container-e2e] WARN: $*" >&2; }
err()    { echo "[container-e2e] ERROR: $*" >&2; }
die()    { err "$*"; exit 2; }
hr()     { echo "────────────────────────────────────────────────────────"; }

# ── Preflight: host prerequisites ─────────────────────────────────────────────

hr
log "Checking host prerequisites..."

# Docker
if ! command -v docker &>/dev/null; then
  die "docker not found in PATH. Install Docker Desktop: https://www.docker.com/products/docker-desktop"
fi
if ! docker info &>/dev/null; then
  die "Docker daemon is not running. Start Docker Desktop and try again."
fi
info "Docker: $(docker --version | head -1)"

# claude CLI — must be installed and logged in
if ! command -v claude &>/dev/null; then
  die "claude CLI not found in PATH. Install from https://code.claude.com and run 'claude auth login'."
fi
CLAUDE_AUTH="$(claude auth status --json 2>/dev/null || echo '{}')"
if ! echo "$CLAUDE_AUTH" | python3 -c "import sys,json; d=json.load(sys.stdin); exit(0 if d.get('loggedIn') else 1)" 2>/dev/null; then
  cat >&2 <<'ERRMSG'
[container-e2e] ERROR: claude is not logged in.

The auth proxy uses your local `claude` binary to obtain a fresh OAuth token
and forward container API requests to api.anthropic.com.

Fix: run `claude auth login` and try again.
ERRMSG
  exit 2
fi
info "claude: logged in as $(echo "$CLAUDE_AUTH" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('email','?'))" 2>/dev/null)"

# python3 — required for auth-proxy.py
if ! command -v python3 &>/dev/null; then
  die "python3 not found in PATH (required for auth-proxy.py)"
fi

hr

# ── Build Linux binary ─────────────────────────────────────────────────────────

LINUX_BINARY="${REPO_ROOT}/target/${BINARY_TARGET}/release/actual"

if [[ -f "$LINUX_BINARY" && "$FORCE_BUILD" -eq 0 ]]; then
  info "Reusing existing Linux binary: $LINUX_BINARY"
  info "  (pass --build to force a rebuild)"
else
  log "Building Linux binary (target: $BINARY_TARGET)..."
  log "  This uses a Rust Docker image to cross-compile; may take several minutes on first run."

  # Install the target on the host's Rust toolchain if available; otherwise build inside Docker.
  if command -v cargo &>/dev/null && rustup target list --installed 2>/dev/null | grep -q "$BINARY_TARGET"; then
    info "Using host cargo with target $BINARY_TARGET"
    (
      cd "$REPO_ROOT"
      if [[ "$BINARY_TARGET" == "aarch64-unknown-linux-gnu" ]]; then
        CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
          cargo build --release --target "$BINARY_TARGET"
      else
        cargo build --release --target "$BINARY_TARGET"
      fi
    )
  else
    info "Using Docker (rust:slim) to build musl static binary — no host cross-compilation toolchain detected"
    docker run --rm \
      --platform "$CONTAINER_PLATFORM" \
      -v "${REPO_ROOT}:/src" \
      -w /src \
      rust:slim \
      bash -c "
        apt-get update -qq && apt-get install -y -qq musl-tools pkg-config > /dev/null 2>&1
        rustup target add ${BINARY_TARGET} 2>&1
        cargo build --release --target ${BINARY_TARGET} 2>&1
      "
    mkdir -p "$(dirname "$LINUX_BINARY")"
  fi

  if [[ ! -f "$LINUX_BINARY" ]]; then
    die "Build succeeded but binary not found at: $LINUX_BINARY"
  fi
  info "Binary built: $LINUX_BINARY ($(du -sh "$LINUX_BINARY" | cut -f1))"
fi

hr

# ── Create output directory ───────────────────────────────────────────────────

mkdir -p "$OUTPUT_DIR"
info "Output directory: $OUTPUT_DIR"

# ── Start auth proxy ──────────────────────────────────────────────────────────
# The proxy captures a fresh OAuth token from the local `claude` binary and
# forwards all container API calls to api.anthropic.com with that token.
# This avoids passing any credentials into the container.

PROXY_SCRIPT="${REPO_ROOT}/scripts/auth-proxy.py"
PROXY_LOG="${OUTPUT_DIR}/proxy.log"

# Kill any stale proxy on this port
lsof -ti ":${PROXY_PORT}" 2>/dev/null | xargs kill -9 2>/dev/null || true

log "Starting auth proxy on port ${PROXY_PORT}..."
python3 "$PROXY_SCRIPT" --port "$PROXY_PORT" >"$PROXY_LOG" 2>&1 &
PROXY_PID=$!

# Wait up to 30s for proxy to become ready
PROXY_READY=0
for i in $(seq 1 30); do
  if curl -s --max-time 1 "http://127.0.0.1:${PROXY_PORT}/" >/dev/null 2>&1; then
    PROXY_READY=1
    break
  fi
  # Also check if process is still alive
  if ! kill -0 "$PROXY_PID" 2>/dev/null; then
    break
  fi
  sleep 1
done

# Proxy doesn't need to respond to GET / — just check it's listening
if ! kill -0 "$PROXY_PID" 2>/dev/null; then
  err "Auth proxy failed to start. Check $PROXY_LOG"
  cat "$PROXY_LOG" >&2
  exit 2
fi
info "Auth proxy running (PID $PROXY_PID, port $PROXY_PORT)"

# Capture the fresh OAuth token from the local claude binary.
# This is passed to the container as CLAUDE_CODE_OAUTH_TOKEN so the Claude Code
# npm package considers itself "logged in" (required for auth status checks).
# The proxy still intercepts API calls and keeps the token fresh.
log "Capturing fresh OAuth token for container..."
FRESH_TOKEN="$(python3 - <<'PYEOF'
import http.server, json, threading, subprocess, os, socket, sys

captured = {}
done = threading.Event()

class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        auth = self.headers.get("Authorization", "")
        if auth.startswith("Bearer "):
            captured["token"] = auth[len("Bearer "):]
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps({"id":"msg_x","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"model":"claude-haiku-4-5","stop_reason":"end_turn","stop_sequence":None,"usage":{"input_tokens":1,"output_tokens":1}}).encode())
        done.set()
    def log_message(self, *a): pass

with socket.socket() as s:
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]

server = http.server.HTTPServer(("127.0.0.1", port), H)
t = threading.Thread(target=server.serve_forever, daemon=True)
t.start()

env = {**os.environ, "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{port}"}
subprocess.run(["claude","--print","--model","claude-haiku-4-5","--no-session-persistence"],
    input="hi", capture_output=True, text=True, timeout=25, env=env)
done.wait(timeout=8)
server.shutdown()
print(captured.get("token", ""), end="")
PYEOF
)"

if [[ -z "$FRESH_TOKEN" ]]; then
  warn "Could not capture fresh token — claude auth status may fail in container"
  FRESH_TOKEN="proxy-managed"
else
  info "Fresh token captured (${#FRESH_TOKEN} chars)"
fi

# Ensure proxy is killed when the script exits
cleanup() {
  if [[ -n "$PROXY_PID" ]] && kill -0 "$PROXY_PID" 2>/dev/null; then
    log "Stopping auth proxy (PID $PROXY_PID)..."
    kill "$PROXY_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

hr

# ── Container inner script ────────────────────────────────────────────────────
# This heredoc is the script that runs *inside* the Ubuntu container.
# It is streamed via stdin to `docker run bash`.

read -r -d '' INNER_SCRIPT <<'INNER' || true
#!/usr/bin/env bash
set -euo pipefail

API_URL="$ACTUAL_E2E_API_URL"
NO_TAILOR_FLAG="$ACTUAL_NO_TAILOR"
ANTHROPIC_BASE_URL="$ACTUAL_ANTHROPIC_BASE_URL"
# CLAUDE_CODE_OAUTH_TOKEN is already in env (passed via docker -e)
OUTPUT="/output"

log()  { echo "[container] $*"; }
hr()   { echo "────────────────────────────────────────────────────────"; }
pass() { echo "  [PASS] $*"; echo "PASS" >> /tmp/results; }
fail() { echo "  [FAIL] $*"; echo "FAIL" >> /tmp/results; }
warn() { echo "  [WARN] $*"; }
ok()   { echo "  [OK]   $*"; }

# ── 1. System dependencies ────────────────────────────────────────────────────

hr
log "Installing system dependencies..."
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq curl git ca-certificates > /dev/null 2>&1

# Install Node.js 20 via NodeSource (Ubuntu 22.04 ships Node 12, too old for Claude Code)
log "  Installing Node.js 20 via NodeSource..."
curl -fsSL https://deb.nodesource.com/setup_20.x 2>/dev/null | bash - > /dev/null 2>&1
apt-get install -y -qq nodejs > /dev/null 2>&1

log "  node $(node --version), npm $(npm --version)"
hr

# ── 2. Install Claude Code ────────────────────────────────────────────────────

log "Installing Claude Code..."
npm install -g @anthropic-ai/claude-code --loglevel=error 2>&1 | tail -3
log "  claude installed: $(claude --version 2>/dev/null | head -1)"

# Create a non-root user to run claude (claude refuses --dangerously-skip-permissions as root)
useradd -m -s /bin/bash testuser 2>/dev/null || true
# Give testuser write access to the output dir (/usr/local/bin/actual is ro-mounted, already executable)
chmod 777 /output
# Fetch the actual skill from the standalone repo into testuser's claude config
mkdir -p /home/testuser/.claude/skills/actual
SKILL_REPO_URL="https://raw.githubusercontent.com/actual-software/actual-skill/main/skills/actual"
for f in SKILL.md references/config-reference.md references/error-catalog.md references/output-formats.md references/runner-guide.md references/sync-workflow.md scripts/diagnose.sh; do
  mkdir -p "/home/testuser/.claude/skills/actual/$(dirname "$f")"
  curl -sfL "$SKILL_REPO_URL/$f" -o "/home/testuser/.claude/skills/actual/$f" || log "  WARN: failed to fetch $f"
done
chmod +x /home/testuser/.claude/skills/actual/scripts/diagnose.sh 2>/dev/null || true
chown -R testuser:testuser /home/testuser/.claude
hr

# ── 3. Verify connectivity to auth proxy ─────────────────────────────────────

log "Verifying API connectivity via host auth proxy..."
log "  ANTHROPIC_BASE_URL=$ANTHROPIC_BASE_URL"

# Quick smoke test: call the proxy with a minimal request
SMOKE_JSON="$(curl -sf --max-time 10 \
  "${ANTHROPIC_BASE_URL}/v1/messages" \
  -H 'Content-Type: application/json' \
  -H 'anthropic-version: 2023-06-01' \
  -d '{"model":"claude-haiku-4-5","max_tokens":5,"messages":[{"role":"user","content":"hi"}]}' \
  2>/dev/null || echo '{}')"

if echo "$SMOKE_JSON" | grep -q '"content"'; then
  pass "Auth proxy responding — API connectivity verified"
else
  fail "Auth proxy smoke test failed: $SMOKE_JSON"
  exit 1
fi

# Export for su invocations below
export ANTHROPIC_BASE_URL CLAUDE_CODE_OAUTH_TOKEN
hr

# ── 4. Set up test project ────────────────────────────────────────────────────

log "Creating test project (Next.js/TypeScript)..."
mkdir -p /test-project/app/api/users /test-project/lib /test-project/tests

cat > /test-project/package.json <<'EOF'
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

cat > /test-project/tsconfig.json <<'EOF'
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

cat > /test-project/next.config.ts <<'EOF'
import type { NextConfig } from 'next';
const config: NextConfig = { reactStrictMode: true };
export default config;
EOF

cat > /test-project/app/page.tsx <<'EOF'
export default function HomePage() {
  return <main><h1>Hello World</h1></main>;
}
EOF

cat > /test-project/app/layout.tsx <<'EOF'
export default function RootLayout({ children }: { children: React.ReactNode }) {
  return <html lang="en"><body>{children}</body></html>;
}
EOF

cat > /test-project/app/api/users/route.ts <<'EOF'
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

cat > /test-project/lib/db.ts <<'EOF'
export async function query<T>(sql: string, params: unknown[]): Promise<T[]> {
  void sql; void params; return [];
}
EOF

cat > /test-project/tests/api.test.ts <<'EOF'
describe('API routes', () => { it('works', () => expect(true).toBe(true)); });
EOF

printf 'node_modules\n.next\ndist\n' > /test-project/.gitignore

# Initialize git repo (actual CLI requires a git repo)
git -C /test-project init -q
git -C /test-project config user.email "container-e2e@test.local"
git -C /test-project config user.name "Container E2E"
git -C /test-project add .
git -C /test-project commit -qm "init: test project"

log "  Test project ready at /test-project"
hr

# ── 5. Set up skill for Claude Code ──────────────────────────────────────────

log "Setting up actual skill for Claude Code..."
# Skill was fetched from GitHub and installed to /home/testuser/.claude/skills/actual/ in step 2.
# Write a CLAUDE.md for the test project so claude knows the actual binary is available
cat > /test-project/CLAUDE.md <<'EOF'
# actual CLI Test Project

This is a test project used to verify the actual CLI end-to-end.

The `actual` binary is installed at `/usr/local/bin/actual`.

When running sync, use:
- `--force` to skip interactive confirmation
- `--no-tailor` flag if specified in environment
- `--api-url` to point at the test API

Pre-flight before sync:
1. `actual runners` — check available runners
2. `actual auth`    — verify Claude Code is authenticated
3. `actual config show` — show current config

Then execute: `actual adr-bot --force [--no-tailor] --api-url <URL>`
EOF

log "  Skill fetched and installed at /home/testuser/.claude/skills/actual/"

# Also give testuser ownership of the test project
chown -R testuser:testuser /test-project
hr

# ── 6. First-run / out-of-the-box verification ───────────────────────────────
# Verify that a brand-new user (no prior config) gets a working setup automatically.
# No ACTUAL_CONFIG or ACTUAL_CONFIG_DIR is set — we use the default path.

log "Verifying first-run / out-of-the-box experience..."

CONFIG_PATH="/home/testuser/.actualai/actual/config.yaml"

# 6a. Confirm no config exists yet
if su -s /bin/bash testuser -c "test -f '$CONFIG_PATH'" 2>/dev/null; then
  fail "Config file already exists before first run — test is not hermetic: $CONFIG_PATH"
else
  pass "Config file does not exist before first run (clean slate)"
fi

# 6b. Run 'actual config show' — this triggers auto-creation of the config file
su -s /bin/bash testuser -c 'actual config show 2>&1' | tee /output/actual-config.log

# 6c. Verify config file was auto-created
if su -s /bin/bash testuser -c "test -f '$CONFIG_PATH'" 2>/dev/null; then
  pass "Config file auto-created on first run: $CONFIG_PATH"
else
  fail "Config file was NOT auto-created — first-run init is broken"
fi

# 6d. Verify config file permissions (should be 0600 — owner read/write only)
PERMS="$(su -s /bin/bash testuser -c "stat -c '%a' '$CONFIG_PATH'" 2>/dev/null || echo 'unknown')"
if [[ "$PERMS" == "600" ]]; then
  pass "Config file has correct permissions (0600)"
else
  fail "Config file has wrong permissions ($PERMS, expected 600)"
fi

# 6e. Verify default api_url is the prod endpoint (no manual config required)
DEFAULT_API_URL="https://api-service.api.prod.actual.ai"
if grep -q "api_url:.*${DEFAULT_API_URL}" /output/actual-config.log 2>/dev/null; then
  pass "Default api_url is set to prod endpoint (no manual config required)"
else
  warn "Default api_url not found or unexpected — check actual-config.log"
fi

hr

# ── 7. Runner pre-flight checks ───────────────────────────────────────────────

log "Running pre-flight checks via actual CLI (as testuser)..."

log "  actual runners:"
su -s /bin/bash testuser -c 'actual runners 2>&1' | tee /output/actual-runners.log || true

log "  actual auth:"
su -s /bin/bash testuser -c 'ANTHROPIC_BASE_URL="'"$ANTHROPIC_BASE_URL"'" actual auth 2>&1' | tee /output/actual-auth.log || true
hr

# ── 7. Invoke actual adr-bot via Claude Code + the 'actual' skill ────────────────

log "Invoking actual adr-bot via Claude Code using the 'actual' skill..."
log "  API URL: $API_URL"
log "  No-tailor: $NO_TAILOR_FLAG"

SYNC_FLAGS="--force --api-url $API_URL"
if [[ "$NO_TAILOR_FLAG" == "1" ]]; then
  SYNC_FLAGS="$SYNC_FLAGS --no-tailor"
fi

# Read the skill content to include as context in the prompt
SKILL_CONTENT="$(cat /home/testuser/.claude/skills/actual/SKILL.md)"

PROMPT="$(cat <<PROMPT
You are operating as an AI assistant with the 'actual' CLI skill loaded.

The 'actual' skill documentation is below — follow its operational workflows precisely.

--- SKILL DOCUMENTATION ---
${SKILL_CONTENT}
--- END SKILL DOCUMENTATION ---

## Your Task

Run actual adr-bot on the project at /test-project. 

Exact command to run (use these exact flags, do not modify):
  cd /test-project && actual adr-bot ${SYNC_FLAGS}

Steps to follow:
1. Run pre-flight checks as specified in the skill (actual runners, actual auth, actual config show)
2. Run actual adr-bot with the exact flags above
3. Report what happened — did it succeed? What was written? Were there any errors?

Important:
- The actual binary is at /usr/local/bin/actual and is already in PATH
- API calls are routed through the host auth proxy (ANTHROPIC_BASE_URL is set)
- Run all commands from /test-project
- If actual adr-bot exits non-zero, report the error and exit code
PROMPT
)"

# Run Claude Code non-interactively as testuser (claude refuses --dangerously-skip-permissions as root).
# Flags:
#   --print                         non-interactive mode, exit after response
#   --dangerously-skip-permissions  skip all tool permission prompts (safe in isolated container)
#   --output-format text            plain text output
#   --model sonnet                  Sonnet model (fast, capable)
#   --no-session-persistence        don't save session to disk
#   --max-budget-usd 5              cap spend at $5 (safety net)

# Write prompt to a temp file to avoid shell quoting/length issues
PROMPT_FILE="$(mktemp)"
cat > "$PROMPT_FILE" <<PROMPTEOF
${PROMPT}
PROMPTEOF
chown testuser:testuser "$PROMPT_FILE"

(
  su -s /bin/bash testuser -c "
    export ANTHROPIC_BASE_URL='${ANTHROPIC_BASE_URL}'
    export CLAUDE_CODE_OAUTH_TOKEN='${CLAUDE_CODE_OAUTH_TOKEN}'
    cd /test-project
    claude \
      --print \
      --dangerously-skip-permissions \
      --output-format text \
      --model sonnet \
      --no-session-persistence \
      --max-budget-usd 5 \
      \"\$(cat ${PROMPT_FILE})\" \
      2>&1
  "
) | tee /output/claude-run.log

CLAUDE_EXIT="${PIPESTATUS[0]}"
rm -f "$PROMPT_FILE"
hr

# ── 8. Validate output ────────────────────────────────────────────────────────

log "Validating sync output..."
touch /tmp/results

# Save the final CLAUDE.md for host inspection regardless of outcome
[[ -f /test-project/CLAUDE.md ]] && cp /test-project/CLAUDE.md /output/generated-CLAUDE.md

# Check that actual adr-bot appended a managed section to CLAUDE.md
if grep -q 'managed:actual-start' /test-project/CLAUDE.md 2>/dev/null; then
  pass "CLAUDE.md contains managed:actual-start marker (actual adr-bot wrote ADR content)"
else
  fail "CLAUDE.md is missing managed:actual-start — actual adr-bot may not have run or produced output"
fi

if grep -q 'managed:actual-end' /test-project/CLAUDE.md 2>/dev/null; then
  pass "CLAUDE.md contains managed:actual-end marker (managed section is closed)"
else
  fail "CLAUDE.md is missing managed:actual-end — managed section may be truncated"
fi

if grep -q '<!-- version:' /test-project/CLAUDE.md 2>/dev/null; then
  pass "CLAUDE.md contains version metadata"
else
  warn "CLAUDE.md missing version metadata (may be OK depending on runner)"
fi

if grep -q '<!-- adr-ids:' /test-project/CLAUDE.md 2>/dev/null; then
  pass "CLAUDE.md contains ADR ID metadata"
else
  warn "CLAUDE.md missing ADR ID metadata (may be OK if no matching ADRs)"
fi

if [[ "$CLAUDE_EXIT" -eq 0 ]]; then
  pass "Claude Code exited successfully (exit 0)"
else
  fail "Claude Code exited with code $CLAUDE_EXIT"
fi

echo "$CLAUDE_EXIT" > /output/exit-code.txt

# ── 9. Summary ────────────────────────────────────────────────────────────────

hr
touch /tmp/results
TOTAL=$(grep -c '^' /tmp/results 2>/dev/null | tr -d ' ') || TOTAL=0
FAIL=$(grep -c 'FAIL' /tmp/results 2>/dev/null | tr -d ' ') || FAIL=0
PASS=$(( TOTAL - FAIL ))

log "Container test complete."
log "  Passed: $PASS  Failed: $FAIL"
log "  Output files are in /output (mapped to host)"

if [[ "$FAIL" -gt 0 ]]; then
  exit 1
fi
INNER

# ── Run the container ─────────────────────────────────────────────────────────

hr
log "Starting Ubuntu container..."
log "  Platform : $CONTAINER_PLATFORM"
log "  Binary   : $LINUX_BINARY"
log "  Repo     : $REPO_ROOT (→ /repo)"
log "  Output   : $OUTPUT_DIR (→ /output)"
log "  API URL  : $API_URL"
log "  Proxy    : http://host.docker.internal:${PROXY_PORT}"
hr

EXTRA_SYNC_FLAGS=0
if [[ "$NO_TAILOR" -eq 1 ]]; then
  EXTRA_SYNC_FLAGS=1
fi

docker run \
  --rm \
  --platform "$CONTAINER_PLATFORM" \
  --name "actual-cli-container-e2e-$$" \
  --add-host "host.docker.internal:host-gateway" \
  \
  `# Mount the compiled Linux actual binary (read-only)` \
  -v "${LINUX_BINARY}:/usr/local/bin/actual:ro" \
  \
  `# Mount the full repo read-only — provides source code and scripts` \
  -v "${REPO_ROOT}:/repo:ro" \
  \
  `# Mount host output directory — all logs are written here` \
  -v "${OUTPUT_DIR}:/output" \
  \
  `# Route all Claude Code API calls through the host auth proxy` \
  -e "ACTUAL_ANTHROPIC_BASE_URL=http://host.docker.internal:${PROXY_PORT}" \
  \
  `# Fresh OAuth token so Claude Code npm package considers itself logged in` \
  -e "CLAUDE_CODE_OAUTH_TOKEN=${FRESH_TOKEN}" \
  \
  `# Runtime config` \
  -e "ACTUAL_E2E_API_URL=${API_URL}" \
  -e "ACTUAL_NO_TAILOR=${EXTRA_SYNC_FLAGS}" \
  -e "DEBIAN_FRONTEND=noninteractive" \
  \
  ubuntu:22.04 \
  bash -c "$INNER_SCRIPT"

CONTAINER_EXIT=$?

# ── Host-side summary ─────────────────────────────────────────────────────────

hr
log "Container finished (exit $CONTAINER_EXIT)."
log ""
log "Output files:"
for f in "$OUTPUT_DIR"/*; do
  [[ -f "$f" ]] || continue
  SIZE="$(du -sh "$f" | cut -f1)"
  log "  $SIZE  $(basename "$f")"
done
log ""

if [[ "$CONTAINER_EXIT" -eq 0 ]]; then
  log "RESULT: PASS"
else
  log "RESULT: FAIL (container exit $CONTAINER_EXIT)"
  log ""
  log "Tail of Claude Code output:"
  hr
  tail -40 "${OUTPUT_DIR}/claude-run.log" 2>/dev/null || true
  hr
fi

exit "$CONTAINER_EXIT"
