# npm Distribution Plan

This document describes how to publish the `actual` binary to npm so users can run it via `npx`.

## Overview

`actual` is a Rust binary. The industry-standard approach for distributing native binaries through npm — used by esbuild, Bun, Biome, SWC, and Turbo — is the **platform-specific optional packages pattern**. npm only downloads the package matching the user's current platform, so there is no postinstall download step and no network dependency at install time.

## Package Architecture

```
npm/
├── actual/                    ← root package users install
│   ├── package.json           ← declares optionalDependencies + bin
│   └── bin/
│       └── actual.js          ← JS shim that resolves + execs the correct binary
├── actual-darwin-arm64/       ← macOS Apple Silicon
│   ├── package.json
│   └── bin/actual
├── actual-darwin-x64/         ← macOS Intel
│   ├── package.json
│   └── bin/actual
├── actual-linux-x64/          ← Linux x86-64
│   ├── package.json
│   └── bin/actual
├── actual-linux-arm64/        ← Linux ARM64
│   ├── package.json
│   └── bin/actual
└── actual-win32-x64/          ← Windows x86-64
    ├── package.json
    └── bin/actual.exe
```

## Package Naming

Check npm availability before publishing:

| Option | `npx` invocation | Notes |
|--------|-----------------|-------|
| `actual` (unscoped) | `npx actual adr-bot` | Cleanest UX, but name may be taken |
| `@actualai/actual` (scoped) | `npx @actualai/actual adr-bot` | Guaranteed namespace, slightly verbose |

If `actual` is taken on npm, use `@actualai/actual` for the root package and `@actualai/actual-darwin-arm64` etc. for platform packages.

## File Contents

### Root `npm/actual/package.json`

```json
{
  "name": "actual",
  "version": "0.1.0",
  "description": "ADR-powered AI context file generator",
  "bin": { "actual": "./bin/actual.js" },
  "optionalDependencies": {
    "actual-darwin-arm64": "0.1.0",
    "actual-darwin-x64":   "0.1.0",
    "actual-linux-x64":    "0.1.0",
    "actual-linux-arm64":  "0.1.0",
    "actual-win32-x64":    "0.1.0"
  },
  "os": ["darwin", "linux", "win32"],
  "engines": { "node": ">=16" }
}
```

### `npm/actual/bin/actual.js` (JS shim)

```js
#!/usr/bin/env node
const { execFileSync } = require('child_process');
const os = require('os');

const PLATFORM_MAP = {
  'darwin-arm64': 'actual-darwin-arm64',
  'darwin-x64':   'actual-darwin-x64',
  'linux-x64':    'actual-linux-x64',
  'linux-arm64':  'actual-linux-arm64',
  'win32-x64':    'actual-win32-x64',
};

const key = `${os.platform()}-${os.arch()}`;
const pkg = PLATFORM_MAP[key];
if (!pkg) {
  console.error(`actual: unsupported platform: ${key}`);
  process.exit(1);
}

const ext = os.platform() === 'win32' ? '.exe' : '';
let bin;
try {
  bin = require.resolve(`${pkg}/bin/actual${ext}`);
} catch {
  console.error(
    `actual: platform package "${pkg}" not installed. Try reinstalling with npm install.`
  );
  process.exit(1);
}

try {
  execFileSync(bin, process.argv.slice(2), { stdio: 'inherit' });
} catch (e) {
  process.exit(e.status ?? 1);
}
```

### Per-platform `package.json` (example: `npm/actual-darwin-arm64/package.json`)

```json
{
  "name": "actual-darwin-arm64",
  "version": "0.1.0",
  "description": "actual binary for macOS ARM64",
  "os": ["darwin"],
  "cpu": ["arm64"],
  "files": ["bin/"]
}
```

The compiled binary sits at `npm/actual-darwin-arm64/bin/actual` and must be `chmod +x`. Repeat this structure for each platform, adjusting `name`, `os`, `cpu`, and the binary extension.

## Build Targets

The current release workflow (`release.yml`) only builds `aarch64-apple-darwin`. The following targets need to be added:

| Rust target | npm package | Runner | Status |
|---|---|---|---|
| `aarch64-apple-darwin` | `actual-darwin-arm64` | `macos-latest` | Built + signed |
| `x86_64-apple-darwin` | `actual-darwin-x64` | `macos-latest` | New |
| `x86_64-unknown-linux-gnu` | `actual-linux-x64` | `ubuntu-latest` | Built in CI (not released) |
| `aarch64-unknown-linux-gnu` | `actual-linux-arm64` | `ubuntu-latest` + `cross` | Built in CI (not released) |
| `x86_64-pc-windows-msvc` | `actual-win32-x64` | `windows-latest` | New |

Linux binaries are already cross-compiled in `build-and-test.yml` — they just need to be promoted to the release job.

## CI Changes (`release.yml`)

Add a matrix build step for all platforms, then publish after all binaries are assembled:

```yaml
- name: Copy binary into platform package
  run: |
    cp ./actual-aarch64-apple-darwin npm/actual-darwin-arm64/bin/actual
    chmod +x npm/actual-darwin-arm64/bin/actual
    # ... repeat for each platform ...

- name: Publish platform packages
  env:
    NODE_AUTH_TOKEN: ${{ secrets.NPM_TOKEN }}
  run: |
    for pkg in actual-darwin-arm64 actual-darwin-x64 actual-linux-x64 actual-linux-arm64 actual-win32-x64; do
      cd npm/$pkg && npm publish --access public && cd ../..
    done

- name: Publish root package
  env:
    NODE_AUTH_TOKEN: ${{ secrets.NPM_TOKEN }}
  run: cd npm/actual && npm publish --access public
```

Platform packages must be published before the root package so that `optionalDependencies` resolve correctly.

## Version Synchronization

The npm package version must stay in sync with the Cargo version in `Cargo.toml`. Add a script to `scripts/sync-npm-version.sh` that reads the version from `Cargo.toml` and writes it into all `package.json` files, then run it as part of the release process before publishing.

```bash
#!/usr/bin/env bash
VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
for pkg in npm/actual npm/actual-darwin-arm64 npm/actual-darwin-x64 \
           npm/actual-linux-x64 npm/actual-linux-arm64 npm/actual-win32-x64; do
  tmp=$(mktemp)
  jq --arg v "$VERSION" '.version = $v' "$pkg/package.json" > "$tmp"
  mv "$tmp" "$pkg/package.json"
done
echo "Set all npm package versions to $VERSION"
```

## Required GitHub Secrets

| Secret | Purpose |
|--------|---------|
| `NPM_TOKEN` | Authenticates `npm publish` in CI |

Create the token at npmjs.com under Account → Access Tokens → Generate New Token (Automation type). Add it to the repo at Settings → Secrets and variables → Actions.

## Local Testing

Before publishing, verify the shim works locally:

```bash
# Build for the current platform
cargo build --release

# Copy binary into the matching platform package
cp target/release/actual npm/actual-linux-x64/bin/actual  # adjust for your OS
chmod +x npm/actual-linux-x64/bin/actual

# Install the root package locally and test
cd npm/actual
npm install
npx --prefix . actual --help
```

## End-User Experience

After publishing:

```bash
# One-off via npx (no install needed)
npx actual adr-bot
npx actual status
npx actual auth

# Or install globally
npm install -g actual
actual adr-bot
```

## Work Items

1. Check if `actual` is available on npm; choose scoped vs. unscoped name
2. Create `npm/` directory with all six `package.json` files and the JS shim
3. Add `x86_64-apple-darwin` and `x86_64-pc-windows-msvc` build targets to `release.yml`
4. Promote Linux builds from `build-and-test.yml` into `release.yml`
5. Add npm publish steps to `release.yml`
6. Write `scripts/sync-npm-version.sh` and call it in the release workflow
7. Add `NPM_TOKEN` to GitHub repo secrets
8. Test locally before the first publish
