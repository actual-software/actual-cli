# CLI Distribution

How the `actual` binary is built and published to Homebrew, npm, and GitHub Releases.

---

## Overview

A release is triggered by merging a version bump to `main`: `.github/workflows/tag-release.yml` tags the merged commit `v<version>`, and the tag starts the release. Pre-release tags (suffixed with `-alpha`, `-beta`, `-rc`, or `-test`) are detected and skipped by the release workflow — only clean semver tags (e.g. `v0.2.0`) produce published artifacts.

One workflow, `.github/workflows/release.yml`, builds the binaries once per tag and feeds every channel from that build: the two GitHub Releases, the Homebrew tap and the five npm packages. It also writes `SHA256SUMS` for the four tarballs, which a member of the signing group signs after the release (see [RELEASE_SIGNING.md](RELEASE_SIGNING.md)).

A pull request that changes `release.yml`, the signing tool in `crates/release-sign` or the public keys in `release-keys/` runs the workflow as a dry run: the same build, `SHA256SUMS` and npm packages, all checked, with every publishing job skipped.

---

## Cutting a release

Open a pull request that bumps `version` in `Cargo.toml`, with the matching `Cargo.lock` change, and adds the release's `CHANGELOG.md` entry, the way `chore: release v0.8.0` did. [`.github/CODEOWNERS`](../.github/CODEOWNERS) owns `Cargo.toml` and the release pipeline, so the bump needs an approving review from a teammate; other pull requests can merge without one.

Merging it is the release. `Tag Release` (`.github/workflows/tag-release.yml`) runs on every push to `main` that changes `Cargo.toml`. When the `Cargo.toml` version has no tag yet, it creates `v<version>` on the merged commit, and the release workflow fires on that tag. A `Cargo.toml` change that leaves the version alone, such as a dependency update, finds its tag already there and does nothing. A version lower than the latest release fails `Tag Release` and publishes nothing. Once the release workflow has published both GitHub Releases, a member of the signing group signs the release's `SHA256SUMS` with `cargo run -p release-sign -- release <tag> --kms-key <key version>`, as [RELEASE_SIGNING.md](RELEASE_SIGNING.md) describes.

Nobody pushes `v*` tags by hand: the "Release Tags" ruleset lets only the release GitHub App create, move or delete them, and `Tag Release` creates the tag with that app's token (`RELEASE_APP_ID` and `RELEASE_APP_PRIVATE_KEY`). A tag created with the workflow's own `GITHUB_TOKEN` would not start the release workflow. The release workflow still stops before it builds anything unless the tag is on `main`, matches the `Cargo.toml` version and is newer than the latest release.

**Pre-release** (does not publish): set the `Cargo.toml` version to one with a pre-release suffix, such as `0.2.0-beta.1`. `Tag Release` tags it like any other version, and the release workflow skips it.

---

## Build matrix

The release workflow builds the four targets once per tag, in parallel:

| Target | Runner | Signing |
|--------|--------|---------|
| `aarch64-apple-darwin` (macOS arm64) | `macos-latest` | Ad hoc only (see below) |
| `x86_64-apple-darwin` (macOS x64) | `macos-latest` | None (see below) |
| `x86_64-unknown-linux-gnu` (Linux x64) | `ubuntu-latest` | None |
| `aarch64-unknown-linux-gnu` (Linux arm64) | `ubuntu-latest` (cross) | None |

Linux arm64 is cross-compiled using `gcc-aarch64-linux-gnu`.

Both Linux binaries link against the runner's glibc, which on `ubuntu-latest` (Ubuntu 24.04) is 2.39, and they need glibc 2.39 or newer on the user's machine. The README's platform table and `docs/GETTING_STARTED.md` state that floor for users, so re-check it when `ubuntu-latest` moves to a newer Ubuntu (`readelf -V actual | grep GLIBC_` on a release binary lists the versions it requires). In v0.8.0 the only 2.39 symbols are `pidfd_spawnp` and `pidfd_getpid`, which Rust's standard library links weakly. The x64 binary marks that version requirement weak and the arm64 binary doesn't, so glibc 2.34 to 2.38 runs the x64 binary with a `weak version` warning and refuses to start the arm64 one.

All builds use `cargo build --release` (LTO, stripped, single codegen unit — see `[profile.release]` in `Cargo.toml`).

Each of the four build steps sets `ACTUAL_RELEASE_BUILD=1`, which marks the binary as a release build. [`actual self-update`](SELF_UPDATE.md) replaces only a release build, and the [update notice](UPDATE_CHECK.md) points any other hand-downloaded build at the `cargo install` command, so a build step that loses the variable ships manual-download binaries that refuse to update themselves and tell their users to rebuild. Homebrew and npm installs of such a build still upgrade through their package manager. A test in `src/update_check.rs` reads this workflow and fails when one does.

---

## macOS signing and notarization

The macOS build jobs have steps that sign the binaries with an Apple Developer ID certificate and notarize them with `xcrun notarytool`, which would let Gatekeeper accept them without a quarantine prompt. The steps have been in the release builds since v0.1.0, but they run only when `APPLE_CERTIFICATE_P12` is set in the repo secrets, and no release has had it. The runs for v0.2.0 through v0.8.0 show the import, sign, and notarize steps skipped in both macOS jobs of both workflows. GitHub no longer keeps step records for the v0.1.x runs, and their binaries carry no Developer ID signature either. Every published arm64 binary carries only the linker's ad hoc signature (`codesign -dv` reports `Signature=adhoc`), every x64 binary is unsigned (`codesign -dv` reports `code object is not signed at all`), and Gatekeeper rejects both (`spctl --assess --type execute` reports `rejected`).

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

**Artifact filenames** (each `.tar.gz` contains a single `actual` binary):

```
actual-darwin-arm64.tar.gz
actual-darwin-x64.tar.gz
actual-linux-x64.tar.gz
actual-linux-arm64.tar.gz
SHA256SUMS
SHA256SUMS.minisig
```

`SHA256SUMS` lists the four tarballs in `sha256sum` format and ships with the release. `SHA256SUMS.minisig` appears once a member of the signing group has signed the release; [RELEASE_SIGNING.md](RELEASE_SIGNING.md) covers checking a download against it.

---

## Homebrew tap

After the GitHub Release is created, `release.yml` automatically updates the formula in `actual-software/homebrew-actual`.

The update script (`scripts/update-formula.py` in the tap repo) takes the new version and the tarballs' SHA256 hashes, read from the release's `SHA256SUMS`, and rewrites `Formula/actual.rb` from `template/actual.rb`.

The formula is committed directly to `main` on the tap repo. Homebrew picks it up immediately when users run `brew upgrade` or `brew install`.

**Homebrew install (user-facing):**
```bash
brew install actual-software/actual/actual
```

**Secret required:** `TAP_GITHUB_TOKEN` — a PAT with write access to `actual-software/homebrew-actual` and `actual-software/actual-releases`.

---

## npm packages

The release workflow publishes five packages to the `@actualai` scope on the public npm registry:

| Package | Contents |
|---------|---------|
| `@actualai/actual` | Root package — selects the right platform package as an optional dependency |
| `@actualai/actual-darwin-arm64` | macOS arm64 binary |
| `@actualai/actual-darwin-x64` | macOS x64 binary |
| `@actualai/actual-linux-x64` | Linux x64 binary |
| `@actualai/actual-linux-arm64` | Linux arm64 binary |

The npm package scaffolding (package.json files, install scripts) lives in `actual-software/actual-releases` under `npm/`. Versions are synced from the git tag via `scripts/sync-npm-version.sh`, and each platform package takes its binary from the release tarball. Before anything is published, the `npm Packages` job packs all five and checks that each platform package carries its tarball's binary, byte for byte. The `Publish to npm` job then publishes exactly those packed packages.

**npm install (user-facing):**
```bash
npm install -g @actualai/actual
```

**Credentials:** the publish job authenticates through npm trusted publishing (GitHub OIDC, from the job's `id-token: write` permission, with npm 11.5.1 or later), so it uses no npm token, and npm records a provenance attestation for each package it publishes. npm checks the workflow's file name, so `release.yml` has to be a trusted publisher on all five packages, with direct `npm publish` allowed. Only the publish job holds `id-token: write`: npm exchanges the OIDC token even for `npm publish --dry-run`, so the job that packs and checks the packages, which also runs on pull requests, goes without it. The scaffolding checkout uses the workflow's own token, since `actual-software/actual-releases` is public.

---

## Raising the supported minimum

The npm dist-tag `minimum` names the oldest release still supported. A release build from 0.18.0 on that is older refuses every command except `version`, `help`, `self-update` and `plugin update`, with exit 78, including in CI and in every plugin hook, and has no override. Releases up to 0.17 ignore the tag. See [UPDATE_CHECK.md](UPDATE_CHECK.md#supported-versions).

Before raising it, make sure every channel has the release users will upgrade to: the npm packages, the GitHub release with its signed `SHA256SUMS` (a hand-downloaded `self-update` checks the signature), and the Homebrew tap formula (Homebrew's `self-update` exits 1 while the tap lags). The minimum must not be newer than `latest`; the CLI ignores one that is.

```bash
npm dist-tag add @actualai/actual@0.19.0 minimum   # raise it
npm dist-tag ls @actualai/actual                   # check latest and minimum
npm dist-tag rm @actualai/actual minimum           # undo: no minimum
```

To undo a bad minimum, remove the tag or point it at an older release. A refused machine asks again at most once an hour, so it runs again within about an hour; one that can't reach the registry stays refused from its cache until it can. CI pipelines pinned below the minimum stop until their pin is raised, and `actual self-update` refuses under `CI`.

## Decision log

| # | Decision | Rationale |
|---|----------|-----------|
| 1 | One release workflow and one build per tag (through v0.9.0, `npm-publish.yml` built its own binaries) | Every channel ships the same binaries. The npm and GitHub Release jobs don't depend on each other, so a failed npm publish still doesn't block a GitHub Release |
| 2 | Pre-release tag detection via suffix pattern | Allows staging releases (`-beta`, `-rc`) without publishing to users |
| 3 | Tarballs contain a single `actual` binary at root (no subdirectory) | Simplifies Homebrew formula and manual install — `tar xzf` drops the binary directly into the current directory |
| 4 | Release assets mirrored to `actual-software/actual-releases` | Gave Homebrew, manual installs, and the npm scaffolding a public home while this repo was private. This repo is public now and its releases carry the same assets, but the formula and the README still download from `actual-releases` |
| 5 | Signing conditional on `APPLE_CERTIFICATE_P12` presence | Forks and contributors can still build and test release artifacts without Apple credentials. No release run has had the secret, so no release has been Developer ID signed yet |
| 6 | `SHA256SUMS` signed with minisign, by an org key in Google Cloud KMS, by hand after each release | Anyone can check a download with stock `minisign -V`, the key can't be exported, and nothing in GitHub can make a signature. See [RELEASE_SIGNING.md](RELEASE_SIGNING.md) |
