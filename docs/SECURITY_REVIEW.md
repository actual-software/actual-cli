# Security Review — actual-cli

**Date:** 2026-02-17
**Reviewer:** Internal engineering audit
**Version audited:** 0.1.0 (pre-public-release)
**Scope:** Full source audit targeting binary distribution to the public

---

## Purpose

This document captures security findings from a pre-release audit of `actual-cli`, a proprietary Rust CLI binary that will be distributed publicly. The primary audience is the engineering team preparing for the initial public release. Findings are organized by severity with explicit code references and concrete remediation steps.

---

## Executive Summary

No critical vulnerabilities were found. The codebase is security-conscious in several areas: TLS is enforced via `rustls`, there are no hardcoded user credentials, config files are written with `0600` permissions on Unix, and the claude subprocess is sandboxed to read-only tools. The primary risks for a startup releasing a binary are **IP leakage through the binary itself**, **a path traversal vulnerability in LLM-driven file writes**, and **several medium-severity operational concerns**.

---

## Severity Definitions

| Level | Meaning |
|---|---|
| **Critical** | Exploitable remotely or by untrusted input with material impact |
| **High** | Exploitable locally or with significant impact on data/IP |
| **Medium** | Meaningful risk with a realistic attack scenario |
| **Low** | Defense-in-depth gap or elevated risk with limited realistic impact |
| **Info** | Positive finding or context worth documenting |

---

## Summary Table

| # | Severity | Title | File(s) |
|---|---|---|---|
| 1 | Medium | Path traversal in LLM-driven file writes | `src/generation/writer.rs:47`, `src/tailoring/invoke.rs:91` |
| 2 | Medium | Telemetry key embedded as plaintext in binary | `src/telemetry/reporter.rs:16` |
| 3 | Medium | Proprietary prompt strategy fully extractable from binary | `src/claude/prompts.rs` |
| 4 | Medium | JSON schema leaks internal output contract | `src/claude/schemas.rs` |
| 5 | Medium | No debug symbol stripping in release builds | `Cargo.toml` |
| 6 | Medium | `--allow-dangerously-skip-permissions` passed unconditionally | `src/claude/options.rs:65` |
| 7 | Low | `--api-url` allows HTTP downgrade; no scheme validation | `src/cli/args.rs`, `src/cli/commands/sync.rs:148` |
| 8 | Low | 5xx response body echoed verbatim to user | `src/api/client.rs:147` |
| 9 | Low | Error hint points to wrong config path | `src/error.rs:64` |
| 10 | Low | `check_auth` has no timeout when called from `actual auth` | `src/cli/commands/auth.rs:22` |
| 11 | Low | Email and org name printed to stdout; captured in CI logs | `src/cli/commands/auth.rs:96` |
| 12 | Low | Internal filesystem paths in warning messages | `src/analysis/static_analyzer/manifests.rs` |
| 13 | Low | Subprocess stderr surfaced to callers verbatim | `src/error.rs:12`, `src/claude/subprocess.rs:73` |
| 14 | Low | CI actions pinned by mutable tag, not commit SHA | `.github/workflows/build.yml:20`, `claude-code-review.yml:36` |
| 15 | Low | `serde_yaml` 0.9.x stack overflow on deeply nested YAML | `Cargo.toml:30`, `src/config/paths.rs:62` |
| 16 | Low | TOCTOU race in config file creation | `src/config/paths.rs:60` |
| 17 | Low | Tailoring cache stores full LLM output in plaintext on disk | `src/cli/commands/sync.rs:517` |
| 18 | Info | `unsafe` blocks are test-only and properly guarded | `src/testutil.rs` |
| 19 | Info | TLS correctly uses `rustls`, no OpenSSL | `Cargo.toml:25` |
| 20 | Info | Config file permissions correctly set to `0600` | `src/config/paths.rs:104` |

---

## Findings

### 1. Medium — Path Traversal in LLM-Driven File Writes

**Files:** `src/tailoring/invoke.rs:91-95`, `src/generation/writer.rs:47`

The validation in `validate_and_filter_output` checks that each path from Claude's output ends with `"CLAUDE.md"`:

```rust
// src/tailoring/invoke.rs:91
if !file.path.ends_with("CLAUDE.md") {
    return Err(ActualError::TailoringValidationError(...));
}
```

This check is insufficient. A path like `"../CLAUDE.md"` or `"../../../home/user/.ssh/CLAUDE.md"` satisfies `ends_with("CLAUDE.md")` and passes validation. The path is then joined with `root_dir` without canonicalization:

```rust
// src/generation/writer.rs:47
let full_path = root_dir.join(&file.path);
```

`root_dir.join("../CLAUDE.md")` resolves to the **parent directory** of the repo root. If Claude is compromised, returns a maliciously crafted response due to a prompt injection, or if the LLM hallucinates a dangerous path, this could write files anywhere the running user can write.

**Attack surface:** Claude's structured output is parsed and acted upon without root-directory confinement. The JSON schema (`src/claude/schemas.rs`) defines `path` as a free-form string.

**Remediation:** After joining, canonicalize the result and assert it has `root_dir` as a prefix:

```rust
let canonical_root = root_dir.canonicalize()?;
let full_path = root_dir.join(&file.path);
// Canonicalize requires the path to exist; for new files, canonicalize the parent.
let parent = full_path.parent().expect("always has parent");
std::fs::create_dir_all(parent)?;
let canonical_parent = parent.canonicalize()?;
if !canonical_parent.starts_with(&canonical_root) {
    return WriteResult { action: WriteAction::Failed, error: Some("path escapes root".into()), ... };
}
```

---

### 2. Medium — Telemetry Key Embedded as Plaintext in Binary

**File:** `src/telemetry/reporter.rs:16`

```rust
const SERVICE_KEY: &str = "ak_telemetry_prod_actual_cli";
```

This string is a static string literal. In any compiled Rust binary — including a stripped release build — string literals appear verbatim in the binary's data segment. Running `strings ./actual | grep ak_` reveals the key in under one second:

```
ak_telemetry_prod_actual_cli
```

The existing comment acknowledges this is intentional and compares it to a Segment write key. There is also a test (`test_service_key_not_in_log_output`) that verifies the key is never printed to stdout or stderr.

**Risk:** A competitor or bad actor downloading the binary immediately has your telemetry write key. They can:

1. Submit unlimited fake counter events, inflating metrics and potentially causing cost overruns on the telemetry service.
2. Observe the counter names and event shapes to understand your product's usage patterns (depending on what counters you collect).

**Remediation options (choose based on accepted risk):**

- **Accept:** The key is already write-only per design. Add server-side rate limiting per IP and per key to cap abuse potential. Document the accepted risk here.
- **Proxy:** Remove the key from the binary entirely. Route telemetry through a first-party endpoint (`/telemetry`) that your API server authenticates before forwarding to the telemetry service. Users never hold the key.
- **Obfuscate:** XOR the key at compile time and reconstruct it at runtime. Raises the bar slightly against `strings` but not against a motivated reverse engineer with a disassembler.

---

### 3. Medium — Proprietary Prompt Strategy Fully Extractable from Binary

**File:** `src/claude/prompts.rs:8-48`

The `tailoring_prompt` function constructs a prompt via a Rust `format!()` string template. The static portion — all text that is not `{projects_json}`, `{existing_claude_md_paths}`, or `{adr_json_array}` — is embedded verbatim as a string literal in the compiled binary.

Running `strings ./actual` on a release build reveals:

```
You are tailoring Architecture Decision Records (ADRs) for a specific codebase
and generating CLAUDE.md file content.
...
1. **Tailor each ADR**: Examine the repository to verify applicability.
...
Return your response as a JSON object matching the provided schema.
```

Any competitor who downloads your binary can extract the **exact prompt**, understand your ADR-tailoring strategy, and reproduce the approach without the years of iteration it took to develop.

**Similarly exposed:** The JSON output schema (`src/claude/schemas.rs:7`) is a multi-line string constant. It reveals the exact structured output format your tool expects from Claude, including field names, types, and descriptions — all extractable with `strings`.

**Remediation options:**

- **Accept:** If the prompts are not core IP and the value is in your ADR content (server-side), this may be acceptable.
- **Serve at runtime:** Fetch the prompt template from your API server at runtime. Cache aggressively to avoid latency. The binary then contains only a URL, not the strategy.
- **Compile-time obfuscation:** Use a proc macro to XOR-encode the string at compile time and decode at runtime. Raises the bar against `strings` but not against dynamic analysis (an attacker can simply print the string after decoding).

---

### 4. Medium — JSON Schema Leaks Internal Output Contract

**File:** `src/claude/schemas.rs:7`

The `TAILORING_OUTPUT_SCHEMA` constant is a 60-line JSON schema string embedded verbatim in the binary. It documents:

- All field names and types in your internal `TailoringOutput` structure
- Descriptions of what each field means
- The fact that you use UUIDs for ADR IDs
- The internal vocabulary ("managed section markers", "ADRs", etc.)

This is exposed identically to the prompt text in Finding 3, and falls under the same remediation.

---

### 5. Medium — No Debug Symbol Stripping in Release Builds

**File:** `Cargo.toml` (missing configuration)

The `[profile.release]` section is absent from `Cargo.toml`. Rust release builds by default include:

- **Panic messages** with full file paths, e.g.: `src/tailoring/invoke.rs: called Result::unwrap() on an Err value`
- **Module path strings** embedded in panic handlers
- **Symbol names** (function names, type names) for Rust's demangle infrastructure

Running `strings ./actual | grep 'src/'` on the current binary reveals the internal source tree structure. A competitor or security researcher can map your module layout, identify which third-party crates you use, and locate panic-prone code paths.

**Remediation:** Add to `Cargo.toml`:

```toml
[profile.release]
strip = true        # Remove all symbols and debug info
lto = true          # Link-time optimization (also reduces binary size)
codegen-units = 1   # Better optimization, fewer symbol leaks
```

`strip = true` is the critical flag. It invokes the linker's strip step, removing symbol tables, debug info, and Rust's internal panic location strings from the final binary.

---

### 6. Medium — `--allow-dangerously-skip-permissions` Passed Unconditionally to Claude

**File:** `src/claude/options.rs:64-65`, `src/claude/options.rs:34-48`

The `InvocationOptions::for_tailoring` function sets `skip_permissions: true`, which causes `--allow-dangerously-skip-permissions` to be appended to every `claude --print` invocation:

```rust
// options.rs:34
pub fn for_tailoring(model_override: Option<&str>) -> Self {
    Self {
        ...
        skip_permissions: true,  // always true
    }
}

// options.rs:64
if self.skip_permissions {
    args.push("--allow-dangerously-skip-permissions".to_string());
}
```

The intent is correct: tailoring only uses `Read`, `Glob`, and `Grep`, so no permission prompts should be needed. However, passing `--allow-dangerously-skip-permissions` is a blanket suppression of ALL Claude Code permission checks. If a future developer adds a tool like `Bash` or `Write` to the `tools` list without removing this flag, Claude will execute shell commands or write files without ever prompting the user for permission.

**Risk:** Privilege escalation if tool list is carelessly expanded. The flag name itself ("dangerously") signals this intent.

**Remediation:** Assert at test time that if `skip_permissions` is `true`, only read-only tools are configured. Or alternatively, remove the flag and instead explicitly grant each tool via `--allowedTools` patterns, which Claude Code will honor without needing the skip flag.

---

### 7. Low — `--api-url` Allows HTTP Downgrade; No Scheme Validation

**Files:** `src/cli/args.rs`, `src/cli/commands/sync.rs:148-153`

The `--api-url` argument accepts any URL string, including `http://`:

```rust
// sync.rs:148
let api_url = args
    .api_url
    .as_deref()
    .or(config.api_url.as_deref())
    .unwrap_or(DEFAULT_API_URL);
```

No validation enforces HTTPS. A user tricked or misconfigured to use `--api-url http://...` sends all repository analysis data (project names, languages, framework names) in plaintext. The same configuration can be persisted in `~/.actualai/actual/config.yaml` as `api_url: http://...`.

Combined with Finding 8 below, a malicious `--api-url` server can also inject arbitrary text into error messages displayed to the user.

**Remediation:** Validate the scheme in `SyncArgs` parsing or at the point of `ActualApiClient::new`:

```rust
if !base_url.starts_with("https://") {
    return Err(ActualError::ConfigError(
        "api_url must use HTTPS".to_string()
    ));
}
```

---

### 8. Low — 5xx Response Body Echoed Verbatim to User

**File:** `src/api/client.rs:147-148`

```rust
let body = response.text().await.unwrap_or_default();
ActualError::ApiError(format!("HTTP {status}: {body}"))
```

For 5xx server errors, the full response body is included in the error message. This is combined with Finding 7: if a user points `--api-url` at a malicious server, that server can inject arbitrary text into error messages rendered in the user's terminal. In extreme cases, terminal escape sequences in the body could affect the terminal's display state.

**Remediation:** Truncate the body to a safe maximum (e.g., 256 bytes) and strip non-printable characters:

```rust
let body = response.text().await.unwrap_or_default();
let safe_body: String = body.chars()
    .filter(|c| c.is_ascii_graphic() || *c == ' ')
    .take(256)
    .collect();
ActualError::ApiError(format!("HTTP {status}: {safe_body}"))
```

---

### 9. Low — Error Hint Points to Wrong Config Path

**File:** `src/error.rs:64`

```rust
Self::ConfigError(_) => Some("Check ~/.config/actual/config.yaml"),
```

The actual config path is `~/.actualai/actual/config.yaml` (see `src/config/paths.rs:33`), but the error hint tells users to look in `~/.config/actual/config.yaml`. This is both incorrect and mildly misleading — a user following this hint will look in the wrong place and potentially create a stray unrelated config file.

**Remediation:** Correct the hint string:

```rust
Self::ConfigError(_) => Some("Check ~/.actualai/actual/config.yaml"),
```

---

### 10. Low — `check_auth` Has No Timeout When Called From `actual auth`

**File:** `src/cli/commands/auth.rs:22-43`

The `check_auth` function calls `std::process::Command::new(binary_path).output()`, which blocks indefinitely:

```rust
pub(crate) fn check_auth(binary_path: &Path) -> Result<ClaudeAuthStatus, ActualError> {
    let output = std::process::Command::new(binary_path)
        .args(["auth", "status", "--json"])
        .output()  // no timeout
        .map_err(...)?;
```

A bounded version, `check_auth_with_timeout`, exists and is used in `sync_wiring.rs`. However, `run_auth_with_binary` (the entry point for `actual auth`) calls the unbounded `check_auth` directly. If the `claude` binary hangs — due to a network issue, a bug, or a malicious replacement — `actual auth` will hang forever.

**Remediation:** Replace the `check_auth` call in `run_auth_with_binary` with `check_auth_with_timeout`:

```rust
fn run_auth_with_binary(binary_path: &Path) -> Result<(), ActualError> {
    let status = check_auth_with_timeout(binary_path, Duration::from_secs(10))?;
    ...
}
```

---

### 11. Low — Email and Org Name Printed to stdout; Captured in CI Logs

**File:** `src/cli/commands/auth.rs:95-99`

```rust
if let Some(ref email) = status.email {
    println!("  {:<14} {}", "Email:", email);
}
if let Some(ref org) = status.org_name {
    println!("  {:<14} {}", "Organization:", org);
}
```

When `actual auth` is run in a CI environment, the authenticated user's email address and organization name are printed to stdout and captured in CI logs. These logs are typically accessible to all repository contributors. Anthropic OAuth tokens are scoped to individual users, so a shared CI token's email appearing in logs could leak the service account identity.

**Remediation:** Mask the email in logs by default, or write auth-specific output to stderr (where CI systems often provide redaction controls):

```rust
// Option A: Write to stderr
eprintln!("  {:<14} {}", "Email:", email);

// Option B: Mask
println!("  {:<14} {}...{}", "Email:", &email[..3], &email[email.find('@').unwrap_or(0)..]);
```

---

### 12. Low — Internal Filesystem Paths in Warning Messages

**File:** `src/analysis/static_analyzer/manifests.rs` (multiple lines)

Warnings printed during analysis include full absolute paths:

```rust
eprintln!("Warning: failed to parse {}: {e}", path.display());
```

This pattern appears throughout the manifests analyzer. In automated pipelines, these warnings reveal the user's home directory structure (e.g., `/home/ci-user/workspace/project/package.json`) to anyone with access to the build log.

**Remediation:** Strip the `root_dir` prefix before printing, or use relative paths:

```rust
let relative = path.strip_prefix(root_dir).unwrap_or(path);
eprintln!("Warning: failed to parse {}: {e}", relative.display());
```

---

### 13. Low — Subprocess stderr Surfaced to Callers Verbatim

**File:** `src/error.rs:12`, `src/claude/subprocess.rs:73-83`

```rust
// error.rs:12
RunnerFailed { message: String, stderr: String },
```

When the `claude` subprocess fails, its full stderr is captured and stored in the error variant. This stderr is printed to the terminal by the error handler in `src/cli/commands/mod.rs`. If Claude Code emits sensitive information to stderr — partial tokens, session identifiers, internal API responses — those are surfaced to the user's terminal and potentially to log aggregation systems.

This is unlikely in practice given Claude Code's typical stderr output, but it is worth noting as a defense-in-depth gap.

**Remediation:** Truncate subprocess stderr to a reasonable maximum (e.g., 4KB) before storing:

```rust
let stderr_raw = String::from_utf8_lossy(&output.stderr).to_string();
let stderr = if stderr_raw.len() > 4096 {
    format!("{}... [truncated]", &stderr_raw[..4096])
} else {
    stderr_raw
};
```

---

### 14. Low — CI Actions Pinned by Mutable Tag, Not Commit SHA

**Files:** `.github/workflows/claude-code-review.yml:36`

```yaml
uses: anthropics/claude-code-action@v1  # mutable tag
```

This action uses a mutable version tag rather than an immutable commit SHA. If the upstream repository is compromised or if a tag is force-pushed, the next CI run executes attacker-controlled code with the permissions granted to the job.

Relevant permissions:
- `anthropics/claude-code-action`: runs with `id-token: write`, `pull-requests: write`, and `contents: read`

**Remediation:** Pin the action to a full commit SHA:

```yaml
uses: anthropics/claude-code-action@<full-commit-sha>
```

Use a tool like `Dependabot` or `pinact` to keep these up to date automatically.

---

### 15. Low — `serde_yaml` 0.9.x Stack Overflow on Deeply Nested YAML

**Files:** `Cargo.toml:30`, `src/config/paths.rs:62`

```toml
serde_yaml = "0.9"
```

```rust
// paths.rs:62
serde_yaml::from_str(&contents)
```

`serde_yaml` 0.9.x uses `yaml-rust` internally. Parsing deeply nested YAML (e.g., 10,000 levels of `{a: `) causes a stack overflow that cannot be caught by Rust's panic handler on all platforms. An attacker with write access to `~/.actualai/actual/config.yaml` — including through a symlink attack or in a shared environment — can crash the CLI with a crafted config file.

**Remediation:** Migrate to `serde_yml` (the community-maintained fork with an updated YAML parser) or add a pre-parse size/depth check. For `Cargo.toml`:

```toml
serde_yml = "0.0.12"  # drop-in replacement for serde_yaml 0.9
```

---

### 16. Low — TOCTOU Race in Config File Creation

**File:** `src/config/paths.rs:60-70`

```rust
match std::fs::read_to_string(path) {     // read (check)
    Ok(contents) => ...
    Err(e) if e.kind() == NotFound => {
        let default = Config::default();
        save_to(&default, path)?;          // create (use)
```

There is a time-of-check/time-of-use (TOCTOU) window between the `read_to_string` returning `NotFound` and `save_to` writing the file. In a shared environment where an attacker can write to `~/.actualai/actual/`, they could create a symlink at the config path during this window, causing `save_to` to write the default config to the symlink target instead.

**Remediation:** Use `OpenOptions` with `create_new` for the first-creation case to make the create atomic:

```rust
Err(e) if e.kind() == NotFound => {
    let default = Config::default();
    // create_new fails atomically if the file was created by another process
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    save_to(&default, path)?;
    Ok(default)
}
```

---

### 17. Low — Tailoring Cache Stores Full LLM Output in Plaintext on Disk

**File:** `src/cli/commands/sync.rs:516-527`

```rust
let tailoring_value =
    serde_yaml::to_value(output).expect("TailoringOutput serialization is infallible");
config.cached_tailoring = Some(CachedTailoring {
    cache_key: key.to_string(),
    repo_path: root_dir.to_string_lossy().to_string(),
    tailoring: tailoring_value,
    ...
});
let _ = save_to(config, cfg_path);
```

The full tailoring output — which includes the generated CLAUDE.md content for the entire repository, ADR IDs, reasoning strings from Claude, and the repo's absolute path — is serialized and stored in `~/.actualai/actual/config.yaml`. This file is protected by `0600` permissions on Unix, but:

1. On Windows, no equivalent permission restriction is applied (`set_config_permissions` is a no-op on non-Unix).
2. Any process running as the same user can read the cached tailoring data.
3. The cache is not invalidated when the user rotates API keys or changes subscriptions.
4. The absolute `repo_path` is stored, leaking workspace layout even after the user deletes the repository.

**Remediation:**

- On Windows: Apply an ACL to restrict the config file to the current user (use `windows-acl` crate or similar).
- Clear `repo_path` from cache entries when the repo is deleted or when computing a cache miss.
- Consider encrypting cache entries with a key derived from a machine-specific secret (e.g., OS keychain).

---

### 18. Info — `unsafe` Blocks Are Test-Only and Properly Guarded

**File:** `src/testutil.rs:31, 49, 65, 72`

All `unsafe` code in the project is confined to the test utility module and is not compiled into the production binary:

```rust
// testutil.rs:31
unsafe { std::env::set_var(key, val) }
```

These calls are made `unsafe` in Rust 1.80+ due to potential races with multithreaded code reading the environment. The codebase correctly addresses this with `ENV_MUTEX: Mutex<()>` and requires callers to hold the lock. The `// SAFETY:` comments are present and accurate.

No `unsafe` blocks exist in any production source file. This is a positive finding.

---

### 19. Info — TLS Correctly Uses `rustls`, No OpenSSL Dependency

**File:** `Cargo.toml:25`

```toml
reqwest = { version = "0.12", features = ["json", "rustls-tls"], default-features = false }
```

The explicit use of `rustls-tls` (and `default-features = false` to prevent the OpenSSL fallback) eliminates an entire class of native library vulnerabilities. Certificate validation uses rustls defaults, which enforce TLS 1.2 minimum and do not disable certificate verification.

---

### 20. Info — Config File Permissions Correctly Set to 0600

**File:** `src/config/paths.rs:101-107`

```rust
#[cfg(unix)]
fn set_config_permissions(path: &Path) -> Result<(), ActualError> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms)...
}
```

Config files are correctly written with owner-only permissions on Unix. This prevents other users on a shared system from reading cached tailoring results or configuration.

The corresponding `#[cfg(not(unix))]` no-op means Windows users do not benefit from this protection (see Finding 17).

---

## Additional Reverse Engineering Surface

Beyond the specific findings above, the following are visible in a non-stripped release binary and are worth considering before public release:

| Artifact | Extractable With | Content |
|---|---|---|
| Telemetry key | `strings ./actual` | `ak_telemetry_prod_actual_cli` |
| Production API hostname | `strings ./actual` | `https://api-service.api.prod.actual.ai` |
| Full tailoring prompt | `strings ./actual` | All static prompt text |
| JSON output schema | `strings ./actual` | All field names, types, descriptions |
| Internal module paths | `strings ./actual` | `src/tailoring/invoke.rs`, etc. |
| Crate versions | `strings ./actual` | `actual-cli/0.1.0` in User-Agent |
| Default model name | `strings ./actual` | `sonnet` |
| Managed section markers | `strings ./actual` | The exact marker strings used in CLAUDE.md |
| Cargo crate names | `nm ./actual` (unstripped) | All third-party dependency names |

The **most sensitive** of these are the API hostname and the prompt text. The hostname tells attackers where to direct probes, and the prompt reveals your proprietary strategy. Adding `strip = true` (Finding 5) removes the Rust-specific symbol information but does **not** remove string literals — all of the above except crate names remain visible after stripping.

To protect string literals, the only effective mitigations are fetching them at runtime from your server (recommended for the prompt) or compile-time obfuscation (for strings that cannot be fetched, like the telemetry key).

---

## Recommended Pre-Release Action Plan

Items are ordered by risk-adjusted effort: highest impact, lowest effort first.

### Immediate (before any public binary release)

1. **Add `strip = true` to `[profile.release]`** — 5 minutes, eliminates symbol/module leakage. Also add `lto = true` and `codegen-units = 1` for additional binary hardening.

2. **Fix the path traversal in `writer.rs`** — Canonicalize the output path and assert it has `root_dir` as a prefix before writing. This is a genuine security vulnerability. (Finding 1)

3. **Fix the wrong config path hint** — One-line change. Users hitting config errors get bad guidance. (Finding 9)

### Short-Term (before wide distribution)

4. **Add server-side rate limiting for the telemetry key** — The key will be public the moment the binary ships. Add IP-level and per-key rate limits server-side to prevent abuse. (Finding 2)

5. **Validate HTTPS scheme for `--api-url`** — Reject `http://` URLs to prevent accidental plaintext transmission of analysis data. (Finding 7)

6. **Truncate and sanitize 5xx response bodies** — Prevent terminal injection and limit information leakage from error responses. (Finding 8)

7. **Fix `actual auth` to use `check_auth_with_timeout`** — One-line change to use the already-implemented timeout variant. (Finding 10)

8. **Pin CI actions to commit SHAs** — Supply chain hygiene. (Finding 14)

### Medium-Term (before scale)

9. **Evaluate prompt protection strategy** — Decide whether to serve the prompt at runtime or accept the extraction risk. This decision should be made consciously with legal/product input. (Finding 3)

10. **Upgrade `serde_yaml` to `serde_yml`** — Drop-in replacement that closes the stack overflow risk. (Finding 15)

11. **Add Windows config file ACL** — Parity with Unix `0600` protection. (Finding 17)

12. **Strip absolute paths from warning output** — Use relative paths in all `eprintln!` warnings in the analysis layer. (Finding 12)

---

## Deferred / Accepted Risks

The following findings are documented as accepted risks with rationale:

| Finding | Rationale for Deferral |
|---|---|
| API hostname in binary (#table) | Standard practice for a CLI tool; hostname is not secret |
| Managed section markers in binary (#table) | These are by design public-facing; users interact with them directly |
| TOCTOU in config creation (16) | Requires attacker to have write access to `~/.actualai/`; low realistic impact for a developer tool |
| Subprocess stderr capture (13) | Claude Code's stderr does not contain sensitive data in practice |
| Email in stdout (11) | Only printed when user explicitly runs `actual auth`; acceptable for interactive use |

---

*This document should be reviewed and updated before each major release.*
