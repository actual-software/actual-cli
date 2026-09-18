# actual

Guide AI coding agents with tailored software development best practices.

[![CI](https://github.com/actual-software/actual-cli/actions/workflows/build-and-test.yml/badge.svg)](https://github.com/actual-software/actual-cli/actions/workflows/build-and-test.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

## What it does

Actual CLI establishes guardrails for AI coding agents by delivering
Architecture Decision Records (ADRs) — short documents that capture
software development best practices and design guidance — tailored to your
specific codebase using an LLM. This catches edge cases and guides agents
toward correct decisions. The output is written to context files
(`CLAUDE.md`, `AGENTS.md`, or Cursor rules) that agents read automatically.

The Actual API is **free and requires no account or API key**. You only need
a configured AI runner (Claude Code, Anthropic API, OpenAI API, etc.) for
the tailoring step.

## Quick start

1. **Install**

   ```bash
   brew install actual-software/actual/actual
   ```

2. **Configure a runner** (Claude Code CLI is the default)

   ```bash
   claude auth login
   ```

3. **Run it** from inside any git repo

   ```bash
   actual adr-bot
   ```

This analyzes your repo, fetches relevant ADRs, tailors them to your
codebase, and writes the result to `CLAUDE.md`.

Preview before writing anything:

```bash
actual adr-bot --dry-run
```

## Installation

### Homebrew (recommended)

```bash
brew install actual-software/actual/actual
```

### npm

```bash
npm install -g @actualai/actual
```

### Manual download

Download the binary for your platform from the
[releases repository](https://github.com/actual-software/actual-releases/releases)
(binaries are published separately from source), then:

```bash
chmod +x ./actual
sudo mv ./actual /usr/local/bin/actual
```

**macOS only:** remove quarantine before running:

```bash
xattr -dr com.apple.quarantine /usr/local/bin/actual
```

### Build from source

Requires a C compiler (for tree-sitter native dependencies):

```bash
# macOS: Xcode command-line tools (usually already installed)
xcode-select --install

# Debian/Ubuntu:
sudo apt-get install build-essential

# Then:
cargo install --git https://github.com/actual-software/actual-cli.git
```

## Supported platforms

| Platform | Architecture | Install method |
|----------|-------------|----------------|
| macOS | Apple Silicon (arm64) | Homebrew, npm, manual download |
| macOS | Intel (x64) | Homebrew, npm, manual download |
| Linux | x64 | npm, manual download |
| Linux | arm64 | npm, manual download |

Windows is not currently supported.

## Output formats

By default, `actual adr-bot` writes to `CLAUDE.md`. Content is wrapped in
managed markers so future runs update cleanly without touching anything
you've written yourself.

| Format | Flag | Output file |
|--------|------|-------------|
| Claude Code (default) | `--output-format claude-md` | `CLAUDE.md` |
| Agents | `--output-format agents-md` | `AGENTS.md` |
| Cursor Rules | `--output-format cursor-rules` | `.cursor/rules/actual-policies.mdc` |

## Ask the Advisor

Beyond generating context files, `actual` can answer architecture questions
directly. `actual advisor` sends your question to the Advisor and prints a
tailored answer, plus any related ADRs, in the terminal.

The Advisor works against your Actual AI organization, so sign in first:

```bash
actual login
actual advisor "Should new services talk over gRPC or REST?"
```

By default the answer is scoped to the repository you're standing in:
`actual` reads the working tree's `origin` remote and, if a connected
repository matches, scopes the question to it. If nothing matches, the
question runs at the organization level.

Set the scope explicitly with `--repo`:

```bash
actual advisor --repo actual-cli "..."       # scope to a repo by name
actual advisor --repo owner/actual-cli "..." # disambiguate a shared name
actual advisor --repo none "..."             # ask at the organization level
actual advisor --show-scope                  # print the active scope and exit
```

The scope you choose is remembered per repository for later calls from the
same working tree; `--repo auto` forgets the pin and returns to
auto-detection. See
[Getting Started](docs/GETTING_STARTED.md#ask-the-advisor) for the full flag
reference.

## Check plans against your rules

`actual` can also govern a plan against the rule documents your codebase has
committed under `.actual/rules/` (the same documents `actual rules ls`
inspects).

List and rank rule documents directly:

```bash
actual rules ls                          # list rule documents, level counts, warnings
actual rules index --rebuild             # (re)build the local scope index
actual rules select "Add a caching layer in front of the user repository."
```

`actual rules select` supports `--file <PATH>` (repeatable) to also weigh
files the plan touches, `--explain` to show why each document was picked, and
`--no-rank` to skip the runner-backed second stage and return the
deterministic prefilter alone.

`actual plan-check` judges a plan against the documents selected for it and
reports one of four outcomes: `conforming`, `conflicting`, `requires_decision`
(the judge thinks the plan deliberately supersedes a rule — surfaced for
human review, not auto-approved), or `not_checked` (no rules directory, no
runner, or the judge call itself failed). Only a real `conflicting` verdict
exits non-zero; every other outcome, including `requires_decision`, exits 0 —
a CI job that needs to tell "checked and clean" apart from "could not check"
should read `--json`'s `status` field instead of the exit code alone:

```bash
actual plan-check "Add a caching layer in front of the user repository."
actual plan-check --plan-file plan.md --json
```

Piped with `--claude-hook`, the same pipeline instead reads a Claude Code
`PreToolUse` hook envelope from stdin and prints that hook's own JSON
contract on stdout: nothing at all on a clean pass, or a single-line
`permissionDecision: "deny"` object naming the offending rule and plan
span. A `requires_decision` verdict denies there exactly like a conflict,
pending explicit human review; the exit-0 behavior above is direct mode
only. Every infrastructure failure fails open rather than blocking — this is
an advisory gate, not an enforcement boundary. The `hooks/plan-gate.sh`
script that drives this mode, and the Claude Code settings that install it,
live in the separate `actual-skill` plugin repository, not here.

`actual impl-check` runs the same pipeline against a `git diff` instead of
plan text — same four outcomes, same exit-code contract, same
`--claude-hook` JSON contract (driven by the separate plugin repo's
`hooks/impl-gate.sh`), and the same revision-loop/session machinery as
`plan-check` (a `--claude-hook` session is keyed by `session_id`, independent
of whether a `plan-check` session for that id ever existed). By default it
diffs the current repo's working tree against `HEAD`, including untracked
files that gitignore would not hide (the user's index is not touched);
`--diff-file` or piped stdin can supply the diff explicitly instead:

```bash
actual impl-check                     # working tree vs HEAD, including untracked files
actual impl-check --diff-file out.diff --json
git diff HEAD | actual impl-check
```

Under `--claude-hook` (either command), a rule already cleared for the
session is never re-judged, and a single rule stops blocking on its own
after `--max-rounds` denied rounds (default 3, or
`ACTUAL_PLAN_CHECK_MAX_ROUNDS` / `ACTUAL_IMPL_CHECK_MAX_ROUNDS` respectively —
each command's revision loop is budgeted independently) so a persistently
unresolved rule can't get the hook disabled outright. A human can also clear
a denied rule explicitly, for either command:

```bash
actual check-override --session <id> --rule <doc-slug>::<rule-id> --reason "..."
```

(`plan-check-override` still works as an alias for `check-override`, for
anyone with it already scripted or memorized.) `--reason` is required, and
the command refuses to run anywhere but an ordinary interactive terminal —
never from a script, an agent's tool call, or Claude Code's own integrated
terminal — since an override records a human decision, not the agent's. An
override recorded against a session clears that rule for both `plan-check`
and `impl-check` checks of that session — one override, not two independent
mechanisms. Every round, override, and partial-coverage disclosure is
appended to a durable audit log at
`~/.actualai/actual/plan-check-overrides.log` (kept under its original
filename for history continuity, even though the command that appends to it
is now named `check-override`).

## Commands

```
actual adr-bot        # analyze repo & write AI context files
actual advisor        # ask the Advisor an architecture question
actual status         # check output file state (managed markers, staleness)
actual auth           # verify authentication
actual auth create-token  # mint a scoped token for CI / agents (prototype)
actual mint-token     # mint a token from a service-account key (no browser)
actual config show    # view current configuration
actual config set     # set a config value
actual config path    # print config file location
actual runners        # list available AI backend runners
actual models         # list known model names grouped by runner
actual cache clear    # clear local analysis and tailoring caches
actual rules ls       # list the rule documents under .actual/rules/
actual rules index    # build or refresh the local rule scope index
actual rules select   # select the rule documents that govern a plan
actual plan-check     # check a plan against the rules selected for it
actual impl-check     # check a git diff against the rules selected for it
actual check-override # human override for a rule plan-check/impl-check denied
```

For non-interactive (CI / agent) authentication, see
[Agent authentication](docs/AGENT_AUTH.md). It covers both headless paths: the
scoped access tokens `auth create-token` mints from an existing login session,
and the service-account keys `mint-token` signs with when there is no human to
log in at all. It also covers the dedicated-token-per-agent and never-in-prompt
rules agents must follow.

## Configuration

Config lives at `~/.actualai/actual/config.yaml` and is created automatically
on first run. See [Getting Started](docs/GETTING_STARTED.md) for the full
reference including all flags, runner configuration, and environment variable
overrides.

## Privacy & Telemetry

Actual CLI collects minimal, anonymous telemetry (aggregate counters only —
no PII, source code, or file paths). Telemetry can be disabled via
environment variable (`ACTUAL_NO_TELEMETRY=1`), config file, or compile-time
feature flag. See [PRIVACY.md](PRIVACY.md) for full details.

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for build
instructions, coding standards, and PR guidelines.

## Security

To report a vulnerability, **do not open a public issue**. See
[SECURITY.md](SECURITY.md) for responsible disclosure instructions.

## License

[MIT](LICENSE)
