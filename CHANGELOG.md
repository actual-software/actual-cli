# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `actual self-update` replaces a binary downloaded by hand with the latest release. It checks `SHA256SUMS.minisig` against the keys in `release-keys/`, including the tag in its signed comment, and the archive against `SHA256SUMS`, before renaming the new binary over the old one under a lock. A copy installed with Homebrew, npm or `cargo install` gets that tool's upgrade command instead. It won't replace a binary in a directory the user can't write or one another user owns, and won't run as root, under CI, or with update checks turned off. See `docs/SELF_UPDATE.md`

### Changed
- For a binary no package manager owns, the daily update notice and `actual version` name `actual self-update` instead of linking the release page. `actual version --json` is unchanged

## [0.9.0] - 2026-10-23

### Added
- Once-a-day update notice: after a successful command run at a terminal (stdout and stderr both), the CLI prints the upgrade command for the channel that installed the binary (Homebrew, npm, or a link to the latest release elsewhere). No command waits on the network; a detached background lookup refreshes a cached answer. Hook modes, `--json` output, CI and redirected output never show it, and a failed command does not spend the day's notice. `ACTUAL_NO_UPDATE_CHECK=1` or `update_check.enabled: false` stops every registry request, and a config that fails to load counts as off. See `docs/UPDATE_CHECK.md`
- `actual version` reports the version, install channel and upgrade command offline; `--check` adds one bounded registry lookup and `--json` prints the object documented in `docs/UPDATE_CHECK.md`
- ADR file header v2: rule files may open with a YAML frontmatter block, as the ADR generator now writes. The CLI uses `metadata.scope.{root,paths}` and `metadata.actual.{adr_id,status}`; `candidate_group_id` also groups files into decisions; `languages` and the other fields are parsed and carried but do not affect selection yet. **Requires CLI 0.9.0 or later**; an earlier CLI folds the block into the scope sentence and scopes the decision wrongly, and CLI 0.8.0 also ignores `status`, so it still enforces `draft`, `rejected` and the other retired decisions. Files without a header work exactly as before, and both formats can share a directory
- Declared `paths` replace the globs derived from `### Verify` and prose; `!` paths are excludes, and paths are relative to `root`. When a header declares include paths, a file outside the decision's root is never governed by it, and neither is a file matching its exclude, in selection, the brief, `plan-check` and `impl-check`
- Decisions with a retired status (`deleted`, `draft`, `archived-draft`, `deferred`, `rejected`, `suggested`, `superseded`, `deprecated`) are not indexed, so they are never selected, briefed, judged or counted; a missing or unknown status is active. `rules index` reports how many were left out
- Decisions are keyed on `candidate_group_id`, else `adr_id`, when a file has one, so files sharing an id are one decision and renaming a file does not re-brief it within a session
- Additive JSON fields: `adr_id`, `candidate_group_id`, `status` and `root` in `rules ls`, and `adr_id`, `candidate_group_id` and `root` in `rules select` and `rules select --by-adr` (`root` only when the header declares paths); `inactive_documents` and declared-versus-derived path counts in `rules index`. Fields are omitted for files without a header
- Warnings for a malformed or unrecognised header (`malformed_frontmatter`, `unknown_version`) and for two rule files that share a file stem (`duplicate_slug`)
- `rules brief --help` names the capability marker `adr-header-v2`, which the Actual Skill probes to learn that the CLI reads the v2 header

### Changed
- The `plan-check` folder pass is bounded: each folder is decided once, smallest first, searching the files the plan names first and then the rest in name order, at most 500 files per folder and nothing after 3 seconds. A folder either limit stops short of a governed file is undecided, and the session record notes it as neither covered nor a gap. `plan-check --json` reports the pass as `folders`
- The scope index cache format version is bumped and the brief memory and ledger move to version 2, so existing caches rebuild on first use and a session may be briefed once more on a decision after upgrading
- The reported decision key for a file with a v2 header is `group:<candidate_group_id>` when it has one, else `id:<adr_id>` (`rules select --by-adr` `key`); the brief heading is still the decision title

### Fixed
- When two rule files share a file stem, `rules select` and its `--explain` output no longer resolve both to the first one; lookups key on the file path

## [0.8.0] - 2026-10-19

### Added
- `rules brief` records the folder of every file it is asked about in the session record: covered when the search finds a decision, uncovered when it finds none. Docs, config, lockfiles and generated output never count. The brief also takes the repository root from a `--rules-dir` shaped `<root>/.actual/rules` when no `--repo` is given
- `plan-check --claude-hook` notes the folders a plan names, with no model call, so a planned folder no ADR covers counts as a gap. A folder a plan names is judged by the files directly in it
- `session summary` shows the areas with no ADR beside the ADR count, e.g. `Actual AI: 3 of 12 ADRs added to context · 2 areas with no ADR`, or `Actual AI: no ADR covers the 3 areas you worked in` when every area is a gap. `--json` adds `line`, `gaps` and a sorted `areas_without_adr`

### Changed
- The session summary line replaces "Actual AI: added 3 of 12 ADRs to context this session."

### Fixed
- `rules brief` decides whether a file is governed before `--limit` caps the search, so `--limit 0` no longer reports every governed read as `ungoverned` and records a gap for every folder
- The session summary keeps the count beside the gaps when the session also worked in a folder a decision covers

## [0.7.0] - 2026-10-14

### Added
- `rules brief`: the hook's view of what governs a file. Decisions are briefed once per session and reset on compaction (including a compacted subagent's own memory), the brief's total size is capped with a disclosed note of anything left out, and a path search falls back to text matching (two shared words minimum) only when the path finds nothing. It prints a stderr line saying why a brief stayed quiet
- `rules select` groups rule documents by decision and caps the selection in decisions rather than rules
- Rules score floor: a selection can now answer nothing. Settable persistently with `config` (`rules_min_score`), per repository, with validated inputs and the effective floor reported
- Session summary hook that reports the ADR count and keeps a session-wide record of the decisions briefed

### Changed
- `impl-check`'s Stop check now sees work committed during the turn; the turn start is recorded from the hook envelope's event and compared by checkout root rather than the working directory, and `--turn-start` stays silent when it cannot read its payload
- Claude subprocesses spawned by the CLI are marked with `ACTUAL_CLI_SUBPROCESS=1`
- README leads with the Actual.ai Skill

### Fixed
- `rules select` preserves colons in decision titles and accepts a blank plan alongside `--file`
- Brief hook: resolves symlinks and dot segments in paths, keeps brace lists whole and expands them before a glob answers, no longer splits a spaced path in two, and only answers documented event and tool pairs

## [0.6.2] - 2026-10-01

### Fixed
- npm publishing: the platform packages (`@actualai/actual-{darwin,linux}-{arm64,x64}`) now declare `repository` metadata, which OIDC trusted publishing requires to generate and validate build provenance. Without it the 0.6.1 npm publish was rejected (`E422`, provenance repository mismatch), so 0.6.1 never reached npm. No functional changes to the CLI itself since 0.6.0.

## [0.6.1] - 2026-10-01

### Fixed
- Release pipeline: npm packages are now published correctly via OIDC trusted publishing. 0.6.0 shipped to GitHub Releases and Homebrew, but its npm publish failed (CI installed an npm version incompatible with the runner's Node), so 0.6.0 never reached npm. There are no functional changes to the CLI itself since 0.6.0.

## [0.6.0] - 2026-10-01

### Added
- Plan-governance and scope telemetry now carry a hashed identity + timestamp envelope so metrics can be sliced by user, org and repo without any raw identifier ever leaving the machine: `user_id_hash`/`org_id_hash`/`repo_id_hash` (unsalted, domain-separated SHA-256 of the logged-in ids), a synthetic repo-slug-derived `org_id` on anonymous runs, and `datetime_utc`. Ids are read locally (stored credentials + a config sticky-scope pin) with no network call on the hook path; the backend applies a secret pepper before anything reaches PostHog
- `rules select` / the signals scope workflow now emit a `plan_governance_scope_select` event (narrative 6) describing each local-ADR selection — rules scanned, stage-1 candidates, whether stage 2 (the LLM rank) ran and its status/degradation, selected count, cache hit/miss, rank latency and runner — plus a random `scope_run_id` correlation id. Only counts, enums, ids and hashes are sent, never plan or rule text
- `check-override` now emits a first-class `plan_governance_rule_override` event (narrative 4) instead of a plain completion, so overrides are cleanly separable and override rate has an unambiguous denominator. The override reason is captured privacy-preservingly as `reason_category`/`reason_len`/`reason_hash` — never the raw text
- `plan-check --claude-hook` / `impl-check --claude-hook` completion events now carry `round_index`/`round_total` (narrative 5) for measuring revision-loop convergence

### Changed
- `PRIVACY.md` documents every new telemetry field; all remain opt-out and fire-and-forget, and no raw identifiers, plan text, diff content or reason text are ever sent

### Fixed
- `rules select` no longer waits on the scope-telemetry network send, and no longer risks losing the event when the process exits: the event is written to a small durable local spool (owner-readable only, bounded, never plan/rule text) and delivered by a separate best-effort uploader process that outlives the command, retrying on a later run until the proxy confirms it. A slow or unreachable endpoint can never delay, fail, or block the selection result or the command's exit, and an opted-out run still spools and sends nothing

## [0.5.0] - 2026-09-23

### Added
- `actual rules select --file <path>` no longer requires a plan (AK-786): a hook selecting rules for the file an agent is about to touch can pass only a path. A path-only query skips the stage-2 rank rather than ranking against empty prose, reports why it skipped, and the panel drops its `Plan:` row; a call naming neither a plan nor a path is still a usage error

### Changed
- Scope-index ties are now broken by the rarest glob a document matched, then by how few distinct globs it declares, and only then by slug (AK-785), so sibling rule documents generated from one decision no longer rank alphabetically on path-only queries. Scores, the index format and the cache are unchanged

### Fixed
- `claude --print` subprocesses spawned by the CLI now run with hooks disabled, so `impl-check --claude-hook` no longer recurses through the user's own `Stop` hook (e.g. `impl-gate.sh`), times out, or leaves orphaned nested judge calls running
- Multi-line diffs, verdict reasons and quoted spans no longer break the box borders of the `plan-check` / `impl-check` panel

## [0.4.0] - 2026-09-22

### Added
- `actual impl-check` (AK-755): judges a `git diff` against the rule documents selected for it, sharing `plan-check`'s discovery/scoring/judging pipeline, revision-loop and telemetry machinery via a new shared `check_engine` module rather than duplicating it — same four outcomes, same exit-code contract, and the same `--claude-hook` JSON contract (driven by the separate plugin repo's `hooks/impl-gate.sh`); defaults to the working tree vs `HEAD` in the current repo (including untracked, non-ignored files, without mutating the user's index), with `--diff-file` or piped stdin to supply the diff explicitly. Its own revision loop is budgeted independently via `--max-rounds`/`ACTUAL_IMPL_CHECK_MAX_ROUNDS` (deny counts, rounds, and content-scoped clearances are per command inside the shared session file). A `check-override` recorded against the session still applies to both commands.

### Changed
- `plan-check-override` renamed to `check-override` (reflecting that the override mechanism is shared by both `plan-check` and `impl-check` denials), with `plan-check-override` kept as a backward-compatible alias for existing scripts
- `impl-check` panels, `--json` `not_checked` detail, and `--claude-hook` deny/round-limit copy now name a diff and working tree rather than a plan
- `impl-check` runs `git` with `--no-pager`, `color.ui=never` and `--no-ext-diff`, so a user's `color.ui=always` or `diff.external` no longer corrupts the diff the judge reads; an over-limit diff now stops and reaps the `git` child; the round-limit and partial-coverage notices say the `plan-check-overrides.log` audit log is shared with `plan-check`

## [0.3.0] - 2026-09-14

### Added
- Local scope resolution over `.actual/rules/`: an offline, deterministic index that ranks rule documents against a plan using path globs extracted from `### Verify` operands, the prose scope sentence, the aspect slug and the title
- `actual rules index` to build or refresh that index, `actual rules index --clear` to drop every cached index (including those left by other repositories) and rebuild this one, `actual rules select` to rank rule documents against a plan, and `actual rules eval` to score the index against the status-quo filename scan on a golden set (`--rebuild` forces a fresh index so the measurement is not silently cached)
- `actual rules select --explain`, which attributes every hit to the signal and terms that carried it and prints what the filename scan would have chosen instead
- Two-stage rule selection: `actual rules select` now ranks the deterministic prefilter's surplus with a configured runner when it returns more candidates than the cap allows, and returns a verdict and a reason for every rule it selects
- `--no-rank`, `--candidates`, `--runner` and `--model` on `actual rules select`, and `--rank` on `actual rules eval` to score the two-stage selector against the offline ones on the same golden set
- `StructuredRunner`, which lets all five runners answer a schema other than the tailoring one; stage-2 selection reuses each runner's existing structured-output plumbing rather than duplicating it
- A 90-second wall-clock deadline on the stage-2 rank, since a runner's own timeout is an inactivity timer and does not bound the call
- Design notes and measurements for scope resolution in `docs/SCOPE_INDEX.md`, and for two-stage selection in `docs/RULE_SELECTION.md`
- `actual advisor` command: ask the Advisor org-scoped architecture questions from the terminal and print the answer with any related ADRs (requires signing in with `actual login`)
- Repository scoping for `actual advisor`: `--repo <name|owner/name|uuid>` to target a connected repository, automatic detection from the working tree's `origin` remote, a scope remembered per repository, the `none` and `auto` keywords to pin org-level or reset, and `--show-scope` to print the active scope
- Documentation for the advisor command and repository scoping in the README and Getting Started guide
- `actual rules ls` to list the rule documents under `.actual/rules/` with a per-document level histogram, parse warnings and errors, and `--json` for the same report as structured data
- `actual plan-check` to judge an implementation plan against the rule documents selected for it, printing a panel (or `--json`) with one of four outcomes — conforming, conflicting, requires_decision, or not_checked — and exiting non-zero only for a real `conflicting` verdict; `--claude-hook` runs the same pipeline against a Claude Code `PreToolUse` hook envelope on stdin and emits that hook's own JSON contract instead — nothing on a clean pass, a `permissionDecision: "deny"` object on a real conflict, never an explicit "allow" — always exiting 0 and failing open on any infrastructure failure (no runner, no rules, a crashed judge call)
- The plan-check revision loop (AK-677): under `--claude-hook`, a rule the judge already cleared for the session is never re-asked, a `requires_decision` verdict blocks exactly like a conflict pending explicit human review, and a single rule stops blocking on its own after `--max-rounds` (`ACTUAL_PLAN_CHECK_MAX_ROUNDS`, default 3) denied rounds — every round, override, and partial-coverage disclosure is appended to a durable, append-only audit log
- `actual plan-check-override` for a human to explicitly clear one or more rules a plan-check session has denied, with a required `--reason` recorded to the audit log; refused outright unless run from an ordinary interactive terminal (never from a script, an agent's tool call, or Claude Code's own integrated terminal)

## [0.1.4] - 2026-03-31

### Fixed
- Semgrep-core download routed through TUI pipeline to prevent terminal corruption

## [0.1.3] - 2026-03-30

### Added
- PHP detection, including WordPress-specific detection fixes
- Swift language detection
- C/C++ project detection
- Java Vert.x and Kotlin Compose Multiplatform framework detection
- C# ASP.NET Core detection via `.csproj` parsing
- jQuery detection
- Signals pipeline: tree-sitter + semgrep embedded detectors wired into match requests
- V2 ADR schema support (MkII three-tier output model)
- Character budget enforcement on managed sections with TUI warnings when approaching limits
- Fast-fail on Claude Code hard rate-limit messages
- Semgrep installed in CI test and coverage jobs

### Changed
- Framework detection now prefers manifest-sourced frameworks over config-file-sourced ones in auto-selection
- Language and framework name matching with normalization improvements
- CLI homepage updated from `app.actual.ai` to `cli.actual.ai`
- HTTP retry logic extracted into shared `runner/http_retry.rs` module
- Runner utilities factored into `runner/util.rs`

### Fixed
- Tree-sitter query errors resolved
- Framework name matching fixes and normalization

## [0.1.2] - 2026-03-11

### Added
- Open-source readiness: MIT license, README, SECURITY.md, PRIVACY.md
- CONTRIBUTING.md and code of conduct
- CLI distribution guide (`docs/CLI_DISTRIBUTION.md`)
- Local development guide (`docs/LOCAL_DEVELOPMENT.md`)

### Changed
- Extracted symphony/factory module to `actual-software/factory`
- Moved publication scaffolding to distribution repos (`actual-software/actual-releases`, `actual-software/homebrew-actual`)
- Security contact email updated to `john@actual.ai`
- Removed hardcoded staging API URL

## [0.1.1] - 2026-03-10

### Added
- Initial public release of the `actual` CLI
- ADR-powered AI context file generator
- Multi-runner support: claude-cli, anthropic-api, openai-api, codex-cli, cursor-cli
- Output formats: `CLAUDE.md`, `AGENTS.md`, `.cursor/rules/actual-policies.mdc`
- Homebrew tap (`actual-software/actual/actual`)
- npm packages (`@actualai/actual`)

## [0.1.0] - 2026-02-27

### Added
- Initial release
