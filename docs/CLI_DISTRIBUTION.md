# CLI Distribution

How the `actual` binary is built, signed, and published to Homebrew, npm, and
GitHub Releases.

---

## Overview

A release is triggered by pushing a `v*` tag to `main`. Pre-release tags
(suffixed with `-alpha`, `-beta`, `-rc`, or `-test`) are detected and skipped
by all release workflows — only clean semver tags (e.g. `v0.2.0`) produce
published artifacts.

The two release workflows are:

| Workflow | File | What it does |
|----------|------|-------------|
| Release | `.github/workflows/release.yml` | Builds binaries, creates GitHub Releases, updates Homebrew tap |
| npm Publish | `.github/workflows/npm-publish.yml` | Builds binaries, publishes to npm |

Both workflows build from the same tag but run independently.

---

## Cutting a release

```bash
# Ensure you're on main and up to date
git checkout main && git pull origin main

# Tag and push
git tag v0.2.0
git push origin v0.2.0
```

That's it. Both workflows fire automatically on tag push.

**Pre-release tag** (does not publish):
```bash
git tag v0.2.0-beta.1
git push origin v0.2.0-beta.1
```

---

## Build matrix

Both workflows build the same four targets in parallel:

| Target | Runner | Signing |
|--------|--------|---------|
| `aarch64-apple-darwin` (macOS arm64) | `macos-latest` | Signed + notarized |
| `x86_64-apple-darwin` (macOS x64) | `macos-latest` | Signed + notarized |
| `x86_64-unknown-linux-gnu` (Linux x64) | `ubuntu-latest` | None |
| `aarch64-unknown-linux-gnu` (Linux arm64) | `ubuntu-latest` (cross) | None |

Linux arm64 is cross-compiled using `gcc-aarch64-linux-gnu`.

All builds use `cargo build --release` (LTO, stripped, single codegen unit — see
`[profile.release]` in `Cargo.toml`).

---

## macOS signing and notarization

macOS binaries are signed with an Apple Developer ID certificate and notarized
via `xcrun notarytool`. This allows Gatekeeper to pass without the quarantine
prompt users would otherwise see.

The signing steps run only when `APPLE_CERTIFICATE_P12` is set in the repo
secrets. Without it (e.g. in forks), builds succeed but produce unsigned
binaries.

**Secrets required:**

| Secret | Purpose |
|--------|---------|
| `APPLE_CERTIFICATE_P12` | Base64-encoded `.p12` certificate |
| `APPLE_CERTIFICATE_PASSWORD` | Password for the `.p12` |
| `APPLE_DEVELOPER_ID` | Developer ID string (e.g. `Developer ID Application: Acme Corp (TEAMID)`) |
| `APPLE_NOTARIZATION_APPLE_ID` | Apple ID email for notarytool |
| `APPLE_NOTARIZATION_TEAM_ID` | Apple Developer team ID |
| `APPLE_NOTARIZATION_PASSWORD` | App-specific password for notarytool |

---

## GitHub Releases

The `release.yml` workflow creates two GitHub Releases from the same artifacts:

1. **Private release** on `actual-software/actual-cli` (this repo)
2. **Public mirror** on `actual-software/actual-releases`

The public releases repo is the canonical download source for the Homebrew tap
and for users doing manual installs. It has no source code — only release
assets.

**Artifact filenames** (each is a `.tar.gz` containing a single `actual` binary):

```
actual-darwin-arm64.tar.gz
actual-darwin-x64.tar.gz
actual-linux-x64.tar.gz
actual-linux-arm64.tar.gz
```

---

## Homebrew tap

After the GitHub Release is created, `release.yml` automatically updates the
formula in `actual-software/homebrew-actual`.

The update script (`scripts/update-formula.py` in the tap repo) takes the new
version and SHA256 hashes computed from the release tarballs and rewrites
`Formula/actual.rb` from `template/actual.rb`.

The formula is committed directly to `main` on the tap repo. Homebrew picks it
up immediately when users run `brew upgrade` or `brew install`.

**Homebrew install (user-facing):**
```bash
brew install actual-software/actual/actual
```

**Secret required:** `TAP_GITHUB_TOKEN` — a PAT with write access to
`actual-software/homebrew-actual` and `actual-software/actual-releases`.

---

## npm packages

The `npm-publish.yml` workflow publishes five packages to the `@actualai` scope
on the public npm registry:

| Package | Contents |
|---------|---------|
| `@actualai/actual` | Root package — selects the right platform package as an optional dependency |
| `@actualai/actual-darwin-arm64` | macOS arm64 binary |
| `@actualai/actual-darwin-x64` | macOS x64 binary |
| `@actualai/actual-linux-x64` | Linux x64 binary |
| `@actualai/actual-linux-arm64` | Linux arm64 binary |

The npm package scaffolding (package.json files, install scripts) lives in
`actual-software/actual-releases` under `npm/`. Versions are synced from the
git tag via `scripts/sync-npm-version.sh` before publishing.

**npm install (user-facing):**
```bash
npm install -g @actualai/actual
```

**Secrets required:**
- `NPM_TOKEN` — npm publish token with write access to the `@actualai` scope
- `TAP_GITHUB_TOKEN` — to check out `actual-software/actual-releases`

---

## Decision log

| # | Decision | Rationale |
|---|----------|-----------|
| 1 | Separate `release.yml` and `npm-publish.yml` workflows | Keeps concerns isolated; a failed npm publish doesn't block a GitHub Release |
| 2 | Pre-release tag detection via suffix pattern | Allows staging releases (`-beta`, `-rc`) without publishing to users |
| 3 | Tarballs contain a single `actual` binary at root (no subdirectory) | Simplifies Homebrew formula and manual install — `tar xzf` drops the binary directly into the current directory |
| 4 | Public releases mirrored to `actual-software/actual-releases` | Homebrew and npm scaffolding need a public, stable download URL; keeping it separate from the private source repo gives more control over public asset availability |
| 5 | Signing conditional on `APPLE_CERTIFICATE_P12` presence | Forks and contributors can still build and test release artifacts without Apple credentials |
