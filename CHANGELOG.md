# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
