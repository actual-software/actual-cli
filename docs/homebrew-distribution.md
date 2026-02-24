# Homebrew Distribution Plan

This document describes how to distribute the `actual` binary via Homebrew so users can install it with `brew install actual-software/actual/actual`.

## Overview

Homebrew is the dominant package manager on macOS and is also supported on Linux via the same toolchain. For distributing a pre-built Rust binary (rather than building from source), the approach is:

1. **Create a Homebrew Tap** — a GitHub repo named `actual-software/homebrew-actual`
2. **Write a Formula** — a Ruby file that points to platform-specific release tarballs with their SHA256 checksums
3. **Upload release tarballs** to GitHub Releases (replacing the current raw binary upload)
4. **Automate formula updates** in CI so every tagged release bumps the formula automatically

## Homebrew Tap

By Homebrew convention, a tap is a GitHub repository named `homebrew-<name>`:

```
Repository: actual-software/homebrew-actual
```

Users add the tap and install in two commands:

```bash
brew tap actual-software/actual
brew install actual
```

Or as a single command:

```bash
brew install actual-software/actual/actual
```

The tap repo needs no special structure beyond a `Formula/` directory containing `actual.rb`.

## Formula (`Formula/actual.rb`)

Homebrew formulas for pre-built binaries use `on_macos`/`on_linux` + `on_arm`/`on_intel` blocks to select the correct tarball per platform. Each block declares a `url` and `sha256`.

```ruby
class Actual < Formula
  desc "ADR-powered AI context file generator"
  homepage "https://github.com/actual-software/actual-cli"
  version "0.1.0"

  on_macos do
    on_arm do
      url "https://github.com/actual-software/actual-cli/releases/download/v0.1.0/actual-darwin-arm64.tar.gz"
      sha256 "PLACEHOLDER_SHA256_DARWIN_ARM64"
    end
    on_intel do
      url "https://github.com/actual-software/actual-cli/releases/download/v0.1.0/actual-darwin-x64.tar.gz"
      sha256 "PLACEHOLDER_SHA256_DARWIN_X64"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/actual-software/actual-cli/releases/download/v0.1.0/actual-linux-arm64.tar.gz"
      sha256 "PLACEHOLDER_SHA256_LINUX_ARM64"
    end
    on_intel do
      url "https://github.com/actual-software/actual-cli/releases/download/v0.1.0/actual-linux-x64.tar.gz"
      sha256 "PLACEHOLDER_SHA256_LINUX_X64"
    end
  end

  def install
    bin.install "actual"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/actual --version")
  end
end
```

Homebrew validates `sha256` checksums at install time, so each tarball's hash must be exact.

## Release Tarballs

The current `release.yml` uploads a raw binary named `actual`. Homebrew requires downloadable archives. Change the upload to produce named tarballs per platform:

| Tarball filename | Rust target | Contents |
|---|---|---|
| `actual-darwin-arm64.tar.gz` | `aarch64-apple-darwin` | `actual` binary (signed + notarized) |
| `actual-darwin-x64.tar.gz` | `x86_64-apple-darwin` | `actual` binary |
| `actual-linux-x64.tar.gz` | `x86_64-unknown-linux-gnu` | `actual` binary |
| `actual-linux-arm64.tar.gz` | `aarch64-unknown-linux-gnu` | `actual` binary |

Each tarball contains a single file named `actual` (no subdirectory), so `bin.install "actual"` in the formula works without adjustment.

### Creating tarballs in CI

```bash
# After building each target:
tar -czf actual-darwin-arm64.tar.gz -C target/aarch64-apple-darwin/release actual
sha256sum actual-darwin-arm64.tar.gz  # capture for formula update
gh release upload "${{ github.ref_name }}" actual-darwin-arm64.tar.gz --clobber
```

On macOS, use `shasum -a 256` instead of `sha256sum`.

## Build Targets

The current release workflow only builds `aarch64-apple-darwin`. The following targets need to be added (Linux builds already exist in `build-and-test.yml` and just need promoting to the release job):

| Target | Status |
|---|---|
| `aarch64-apple-darwin` | Built, signed, notarized |
| `x86_64-apple-darwin` | New — add to release job |
| `x86_64-unknown-linux-gnu` | Built in CI, not yet released |
| `aarch64-unknown-linux-gnu` | Built in CI, not yet released |

## CI Changes (`release.yml`)

### Step 1 — Build all targets

Extend the existing `build-sign-macos` job to also cross-compile `x86_64-apple-darwin`, and add a parallel `build-linux` job that reuses the same cross-compilation setup already present in `build-and-test.yml`.

### Step 2 — Create and upload tarballs

After each binary is built and (on macOS) signed and notarized:

```yaml
- name: Package and upload tarballs
  env:
    GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
  run: |
    VERSION="${{ github.ref_name }}"

    # macOS arm64
    tar -czf actual-darwin-arm64.tar.gz -C target/aarch64-apple-darwin/release actual
    gh release upload "$VERSION" actual-darwin-arm64.tar.gz --clobber

    # macOS x64
    tar -czf actual-darwin-x64.tar.gz -C target/x86_64-apple-darwin/release actual
    gh release upload "$VERSION" actual-darwin-x64.tar.gz --clobber
```

### Step 3 — Update the formula

After all tarballs are uploaded, a final job computes SHA256s, updates `Formula/actual.rb` in the tap repo, and commits:

```yaml
update-homebrew-tap:
  needs: [build-sign-macos, build-linux]
  runs-on: ubuntu-latest
  steps:
    - name: Checkout tap repo
      uses: actions/checkout@v4
      with:
        repository: actual-software/homebrew-actual
        token: ${{ secrets.TAP_GITHUB_TOKEN }}
        path: tap

    - name: Compute SHA256s
      env:
        GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        VERSION: ${{ github.ref_name }}
      run: |
        gh release download "$VERSION" --dir tarballs --repo actual-software/actual-cli
        echo "SHA_DARWIN_ARM64=$(shasum -a 256 tarballs/actual-darwin-arm64.tar.gz | awk '{print $1}')" >> $GITHUB_ENV
        echo "SHA_DARWIN_X64=$(shasum -a 256 tarballs/actual-darwin-x64.tar.gz | awk '{print $1}')" >> $GITHUB_ENV
        echo "SHA_LINUX_X64=$(shasum -a 256 tarballs/actual-linux-x64.tar.gz | awk '{print $1}')" >> $GITHUB_ENV
        echo "SHA_LINUX_ARM64=$(shasum -a 256 tarballs/actual-linux-arm64.tar.gz | awk '{print $1}')" >> $GITHUB_ENV

    - name: Update formula
      run: |
        VERSION="${{ github.ref_name }}"
        SEMVER="${VERSION#v}"
        sed -i "s|version \".*\"|version \"$SEMVER\"|" tap/Formula/actual.rb
        sed -i "s|actual-darwin-arm64.tar.gz\"|actual-darwin-arm64.tar.gz\"|" tap/Formula/actual.rb
        # Replace each URL's version and each SHA256 using the env vars set above
        python3 scripts/update-formula.py tap/Formula/actual.rb \
          --version "$SEMVER" \
          --sha-darwin-arm64 "$SHA_DARWIN_ARM64" \
          --sha-darwin-x64   "$SHA_DARWIN_X64" \
          --sha-linux-x64    "$SHA_LINUX_X64" \
          --sha-linux-arm64  "$SHA_LINUX_ARM64"

    - name: Commit and push
      run: |
        cd tap
        git config user.name "github-actions[bot]"
        git config user.email "github-actions[bot]@users.noreply.github.com"
        git add Formula/actual.rb
        git commit -m "actual ${{ github.ref_name }}"
        git push
```

A small `scripts/update-formula.py` helper does the precise line replacements, since `sed` multiline edits across blocks are fragile.

## Required Secrets and Tokens

| Secret | Location | Purpose |
|--------|----------|---------|
| `TAP_GITHUB_TOKEN` | `actual-software/actual-cli` repo secrets | Allows the release CI to push to `actual-software/homebrew-actual` |

The token needs `contents: write` scope on the tap repo. Create a fine-grained personal access token scoped to `actual-software/homebrew-actual`, or use a GitHub App.

## macOS Code Signing and Notarization

The macOS binary is already signed and notarized in the existing `release.yml`. This must be preserved — Homebrew on macOS enforces Gatekeeper, so unsigned binaries will be blocked at install time. The tarball should be created from the already-signed binary, not the raw build output.

Notarization applies to the binary itself; the tarball wrapping it does not need separate notarization.

## Apple Silicon vs. Intel on macOS

`on_arm` targets Apple Silicon Macs (`aarch64-apple-darwin`). `on_intel` targets Intel Macs (`x86_64-apple-darwin`). Homebrew automatically selects the correct block based on the running architecture. No user action required.

## Tap Repository Setup

Create `actual-software/homebrew-actual` on GitHub with this minimal structure:

```
homebrew-actual/
└── Formula/
    └── actual.rb
```

The `Formula/` directory must exist. Homebrew discovers formulas by convention — no additional config is needed.

## Local Testing

Before the first release, test the formula manually:

```bash
# Add the tap pointing to a local clone
brew tap actual-software/actual /path/to/homebrew-actual

# Install from the local formula
brew install actual

# Verify
actual --version
brew test actual
```

After a real release, test the full flow:

```bash
brew tap actual-software/actual
brew install actual
actual --version
```

To test an upgrade:

```bash
brew update
brew upgrade actual
```

## End-User Experience

```bash
# Install
brew install actual-software/actual/actual

# Or tap first, then install by short name
brew tap actual-software/actual
brew install actual

# Upgrade
brew upgrade actual

# Uninstall
brew uninstall actual
brew untap actual-software/actual
```

## Work Items

1. Create `actual-software/homebrew-actual` GitHub repository with `Formula/actual.rb`
2. Add `x86_64-apple-darwin` build target to `release.yml`
3. Promote Linux builds from `build-and-test.yml` into `release.yml`
4. Change release upload from raw binary to per-platform `.tar.gz` tarballs
5. Write `scripts/update-formula.py` for precise formula line replacement
6. Add `update-homebrew-tap` job to `release.yml`
7. Create `TAP_GITHUB_TOKEN` secret in `actual-software/actual-cli`
8. Test locally with `brew tap actual-software/actual /local/path` before first publish
9. Submit to Homebrew core (optional, future) once the tap is established and stable
