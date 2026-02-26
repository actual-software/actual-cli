# 08 - Authentication Strategy

## Overview

`actual` delegates all AI authentication to Claude Code. The CLI itself never handles API keys, OAuth tokens, or credentials directly. This is a deliberate design choice that simplifies our auth story and leverages Claude Code's existing infrastructure.

## Auth Flow

```
actual adr-bot
  |
  |-- 1. Check Claude Code is installed
  |     |-- `which claude` -> found or error
  |
  |-- 2. Check Claude Code auth status
  |     |-- `claude auth status` -> parse JSON output
  |         |
  |         |-- loggedIn: true
  |         |   |-- authMethod: "claude.ai" (Pro/Max subscription)
  |         |   |-- authMethod: "api-key" (ANTHROPIC_API_KEY)
  |         |   |-- Proceed with sync
  |         |
  |         |-- loggedIn: false
  |             |-- Error: "Claude Code not authenticated"
  |                 Suggestion: "Run: claude auth login"
  |
  |-- 3. Proceed with Claude Code subprocess calls
        |-- Claude Code uses its own auth for all API calls
```

## Supported Auth Methods

### 1. ANTHROPIC_API_KEY (Environment Variable)

Users with direct API access set `ANTHROPIC_API_KEY` in their environment. Claude Code picks this up automatically.

**How we detect it**: `claude auth status` returns `"apiKeySource": "ANTHROPIC_API_KEY"`.

**Considerations**:
- Usage is billed to the user's Anthropic account
- All models available depending on account tier
- `--max-budget-usd` flag works with API key auth

### 2. Claude Pro/Max Subscription (OAuth)

Users with Claude Pro or Max subscriptions authenticate via `claude auth login`, which performs an OAuth flow with claude.ai.

**How we detect it**: `claude auth status` returns `"authMethod": "claude.ai"`.

**Considerations**:
- Usage counts against subscription limits
- Model availability depends on subscription tier
- `--max-budget-usd` may not apply (subscription-based, not pay-per-use)

### 3. Claude Code Setup Token

For CI/automation environments, users can run `claude setup-token` to configure a long-lived token.

**How we detect it**: Same as OAuth -- appears as `"authMethod": "claude.ai"` in auth status.

## Auth Status Parsing

```rust
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAuthStatus {
    pub logged_in: bool,
    pub auth_method: Option<String>,
    pub api_provider: Option<String>,
    pub api_key_source: Option<String>,
    pub email: Option<String>,
    pub org_id: Option<String>,
    pub org_name: Option<String>,
    pub subscription_type: Option<String>,
}

impl ClaudeAuthStatus {
    pub fn is_usable(&self) -> bool {
        self.logged_in
    }

    pub fn display_method(&self) -> &str {
        match self.auth_method.as_deref() {
            Some("claude.ai") => "Claude subscription (OAuth)",
            _ if self.api_key_source.is_some() => "API key (ANTHROPIC_API_KEY)",
            _ => "Unknown",
        }
    }
}
```

## CLI Auth Command

`actual auth` is a thin wrapper around Claude Code's auth:

```
$ actual auth
Claude Code Authentication Status:
  Status: Authenticated
  Method: Claude subscription (OAuth)
  Email: user@example.com

To change authentication:
  claude auth login    # Sign in with Claude subscription
  claude auth logout   # Sign out
```

This delegates to `claude auth status` but formats the output consistently with our CLI's style.

## Telemetry Auth (Service Key)

The CLI embeds a service key for authenticating with the sprintreview api-service's `POST /counter/record` endpoint (at the base URL, no `/v1/` prefix). This is used exclusively for telemetry -- no user data or source code is transmitted.

**How it works**:
- A service key is compiled into the binary (or loaded from an env var `ACTUAL_SERVICE_KEY` for development)
- Telemetry requests include `Authorization: ServiceKey <key>` header
- The sprintreview api-service validates the key against `SUPABASE_SERVICE_ROLE_KEY`
- Metrics are tagged with `source: "actual-cli"` and `service: "actual-cli"` by the auth wrapper

**Opt-out**: Users can disable telemetry via:
- `actual config set telemetry.enabled false`
- Setting `ACTUAL_NO_TELEMETRY=1` environment variable

## Backend API Auth (Future)

The ADR Bank API endpoints (`/adrs/match`, `/taxonomy/*`) are unauthenticated in v1. When auth is added, it will likely be one of:

1. **Separate API key**: Users provide an `ACTUAL_API_KEY` env var or store it in config
2. **Anthropic key forwarding**: The backend validates the user's Anthropic key (without storing it)
3. **OAuth with actualai.dev**: Separate OAuth flow

The config file (`~/.actualai/actual/config.yaml`) has a reserved field for this:

```yaml
# Future: API authentication
# api_key: "ak_..."
# auth_method: "api-key" | "oauth"
```

## Security Considerations

1. **No key storage**: `actual` never reads, stores, or transmits the `ANTHROPIC_API_KEY`. Claude Code handles that.
2. **Embedded service key**: The telemetry service key is compiled into the binary. It has write-only access to the metrics table and cannot read any user data.
3. **No credential logging**: The `--verbose` flag must not log auth status details beyond what `claude auth status` outputs, and must not log the embedded service key.
4. **Config file permissions**: `~/.actualai/actual/config.yaml` should be created with `0600` permissions (user read/write only) in case future API keys are stored there.
5. **Subprocess isolation**: Claude Code subprocess calls inherit the user's environment. We don't inject or modify auth-related env vars.
6. **Telemetry privacy**: Only hashed repo identity (SHA-256 of origin URL + HEAD commit) and aggregate counts are transmitted. No source code, file paths, or repo URLs are included.
