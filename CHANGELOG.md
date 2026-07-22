# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `actual advisor` command: ask the Advisor org-scoped architecture questions from the terminal and print the answer with any related ADRs (requires signing in with `actual login`)
- Repository scoping for `actual advisor`: `--repo <name|owner/name|uuid>` to target a connected repository, automatic detection from the working tree's `origin` remote, a scope remembered per repository, the `none` and `auto` keywords to pin org-level or reset, and `--show-scope` to print the active scope
- Documentation for the advisor command and repository scoping in the README and Getting Started guide

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
