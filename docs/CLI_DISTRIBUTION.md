# CLI Distribution

How the `actual` binary is built and published to Homebrew, npm, and GitHub Releases.

---

## Overview

A release is triggered by pushing a `v*` tag to `main`. Pre-release tags (suffixed with `-alpha`, `-beta`, `-rc`, or `-test`) are detected and skipped by all release workflows — only clean semver tags (e.g. `v0.2.0`) produce published artifacts.

The two release workflows are:

| Workflow | File | What it does |
|----------|------|-------------|
| Release | `.github/workflows/release.yml` | Builds binaries, creates GitHub Releases, updates Homebrew tap |
| npm Publish | `.github/workflows/npm-publish.yml` | Builds binaries, publishes to npm |

Both workflows build from the same tag but run independently.

---

## Cutting a release

First land a commit on `main` that bumps `version` in `Cargo.toml`, with the matching `Cargo.lock` change, and adds the release's `CHANGELOG.md` entry, the way `chore: release v0.8.0` did. `actual --version` reports the `Cargo.toml` version while npm and Homebrew take theirs from the tag, so a tag without the bump would publish npm 0.9.0 with a binary that reports 0.8.0.

Then tag that commit and push the tag:

```bash
# Ensure you're on main and up to date
git checkout main && git pull origin main

# Tag and push
git tag v0.2.0
git push origin v0.2.0
```

Both workflows fire automatically on tag push.

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
| `aarch64-apple-darwin` (macOS arm64) | `macos-latest` | Ad hoc only (see below) |
| `x86_64-apple-darwin` (macOS x64) | `macos-latest` | None (see below) |
| `x86_64-unknown-linux-gnu` (Linux x64) | `ubuntu-latest` | None |
| `aarch64-unknown-linux-gnu` (Linux arm64) | `ubuntu-latest` (cross) | None |

Linux arm64 is cross-compiled using `gcc-aarch64-linux-gnu`.

Both Linux binaries link against the runner's glibc, which on `ubuntu-latest` (Ubuntu 24.04) is 2.39, and they need glibc 2.39 or newer on the user's machine. The README's platform table and `docs/GETTING_STARTED.md` state that floor for users, so re-check it when `ubuntu-latest` moves to a newer Ubuntu (`readelf -V actual | grep GLIBC_` on a release binary lists the versions it requires). In v0.8.0 the only 2.39 symbols are `pidfd_spawnp` and `pidfd_getpid`, which Rust's standard library links weakly. The x64 binary marks that version requirement weak and the arm64 binary doesn't, so glibc 2.34 to 2.38 runs the x64 binary with a `weak version` warning and refuses to start the arm64 one.

All builds use `cargo build --release` (LTO, stripped, single codegen unit — see `[profile.release]` in `Cargo.toml`).

---

## macOS signing and notarization

Both workflows have steps that sign the macOS binaries with an Apple Developer ID certificate and notarize them with `xcrun notarytool`, which would let Gatekeeper accept them without a quarantine prompt. The steps have been in both workflows since v0.1.0, but they run only when `APPLE_CERTIFICATE_P12` is set in the repo secrets, and no release has had it. The runs for v0.2.0 through v0.8.0 show the import, sign, and notarize steps skipped in both macOS jobs of both workflows. GitHub no longer keeps step records for the v0.1.x runs, and their binaries carry no Developer ID signature either. Every published arm64 binary carries only the linker's ad hoc signature (`codesign -dv` reports `Signature=adhoc`), every x64 binary is unsigned (`codesign -dv` reports `code object is not signed at all`), and Gatekeeper rejects both (`spctl --assess --type execute` reports `rejected`).

Until the binaries are signed, each channel handles Gatekeeper its own way:

- **Manual download**: a browser marks the downloaded archive as quarantined and `tar` carries the mark onto the extracted binary, so the README's install steps include `xattr -dr com.apple.quarantine` for a browser download.
- **npm**: the macOS platform packages run a `postinstall` script that signs the binary ad hoc (`codesign --sign -`).
- **Homebrew**: the formula's caveat gives an `xattr` command for when Gatekeeper blocks the binary.

The same caveat says the Apple Developer application is under review. Once a certificate exists, these secrets turn the steps on:

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

The `release.yml` workflow creates two GitHub Releases from the same artifacts, and both are public:

1. On `actual-software/actual-cli` (this repo)
2. On `actual-software/actual-releases`

`actual-releases` is the download source for the Homebrew formula and for manual installs. It holds no CLI source, only the release assets and the npm package scaffolding.

**Artifact filenames** (each is a `.tar.gz` containing a single `actual` binary):

```
actual-darwin-arm64.tar.gz
actual-darwin-x64.tar.gz
actual-linux-x64.tar.gz
actual-linux-arm64.tar.gz
```

---

## Homebrew tap

After the GitHub Release is created, `release.yml` automatically updates the formula in `actual-software/homebrew-actual`.

The update script (`scripts/update-formula.py` in the tap repo) takes the new version and SHA256 hashes computed from the release tarballs and rewrites `Formula/actual.rb` from `template/actual.rb`.

The formula is committed directly to `main` on the tap repo. Homebrew picks it up immediately when users run `brew upgrade` or `brew install`.

**Homebrew install (user-facing):**
```bash
brew install actual-software/actual/actual
```

**Secret required:** `TAP_GITHUB_TOKEN` — a PAT with write access to `actual-software/homebrew-actual` and `actual-software/actual-releases`.

---

## npm packages

The `npm-publish.yml` workflow publishes five packages to the `@actualai` scope on the public npm registry:

| Package | Contents |
|---------|---------|
| `@actualai/actual` | Root package — selects the right platform package as an optional dependency |
| `@actualai/actual-darwin-arm64` | macOS arm64 binary |
| `@actualai/actual-darwin-x64` | macOS x64 binary |
| `@actualai/actual-linux-x64` | Linux x64 binary |
| `@actualai/actual-linux-arm64` | Linux arm64 binary |

The npm package scaffolding (package.json files, install scripts) lives in `actual-software/actual-releases` under `npm/`. Versions are synced from the git tag via `scripts/sync-npm-version.sh` before publishing.

**npm install (user-facing):**
```bash
npm install -g @actualai/actual
```

**Credentials:** the publish job authenticates through npm trusted publishing (GitHub OIDC, from the job's `id-token: write` permission, with npm 11.5.1 or later), so it uses no npm token, and npm records a provenance attestation for each package it publishes. Apart from the optional Apple signing secrets, the workflow needs only `TAP_GITHUB_TOKEN`, to check out `actual-software/actual-releases`.

---

## Decision log

| # | Decision | Rationale |
|---|----------|-----------|
| 1 | Separate `release.yml` and `npm-publish.yml` workflows | Keeps concerns isolated; a failed npm publish doesn't block a GitHub Release |
| 2 | Pre-release tag detection via suffix pattern | Allows staging releases (`-beta`, `-rc`) without publishing to users |
| 3 | Tarballs contain a single `actual` binary at root (no subdirectory) | Simplifies Homebrew formula and manual install — `tar xzf` drops the binary directly into the current directory |
| 4 | Release assets mirrored to `actual-software/actual-releases` | Gave Homebrew, manual installs, and the npm scaffolding a public home while this repo was private. This repo is public now and its releases carry the same assets, but the formula and the README still download from `actual-releases` |
| 5 | Signing conditional on `APPLE_CERTIFICATE_P12` presence | Forks and contributors can still build and test release artifacts without Apple credentials. No release run has had the secret, so no release has been Developer ID signed yet |
