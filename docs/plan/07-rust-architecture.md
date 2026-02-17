# 07 - Rust Architecture

## Crate Structure

Single binary crate with module-based organization:

```
actual-cli/
├── Cargo.toml
├── src/
│   ├── main.rs                  # Entry point, CLI argument parsing
│   ├── cli/
│   │   ├── mod.rs               # CLI module root
│   │   ├── args.rs              # Argument parsing (clap derive)
│   │   ├── commands/
│   │   │   ├── mod.rs
│   │   │   ├── sync.rs          # `actual sync` command
│   │   │   ├── status.rs        # `actual status` command
│   │   │   ├── auth.rs          # `actual auth` command
│   │   │   └── config.rs        # `actual config` command
│   │   └── ui/
│   │       ├── mod.rs
│   │       ├── confirm.rs       # ADR confirmation prompts
│   │       ├── progress.rs      # Progress indicators/spinners
│   │       ├── table.rs         # Table rendering
│   │       └── diff.rs          # Diff display
│   ├── claude/
│   │   ├── mod.rs               # Claude Code subprocess interface
│   │   ├── subprocess.rs        # Process spawning and output capture
│   │   ├── auth.rs              # Auth status checking
│   │   ├── schemas.rs           # JSON schemas for structured output
│   │   └── prompts.rs           # Prompt templates
│   ├── analysis/
│   │   ├── mod.rs               # Repo analysis orchestration
│   │   ├── types.rs             # RepoAnalysis, Project, Framework types
│   │   └── confirm.rs            # Accept/reject/retry confirmation for detected projects
│   ├── api/
│   │   ├── mod.rs               # API client module
│   │   ├── client.rs            # HTTP client (reqwest)
│   │   ├── types.rs             # Request/response types
│   │   └── retry.rs             # Retry logic with backoff
│   ├── tailoring/
│   │   ├── mod.rs               # Combined tailoring + formatting orchestration
│   │   ├── types.rs             # TailoringOutput, FileOutput, SkippedAdr types
│   │   └── batch.rs             # Batching logic for large ADR sets
│   ├── generation/
│   │   ├── mod.rs               # CLAUDE.md file writing
│   │   ├── merge.rs             # Managed marker merge logic
│   │   ├── markers.rs           # Managed section marker wrapping
│   │   └── writer.rs            # Multi-file write orchestration
│   ├── config/
│   │   ├── mod.rs               # Config file management
│   │   ├── types.rs             # Config struct
│   │   ├── paths.rs             # Config file path resolution
│   │   └── dotpath.rs           # Dotpath set/get for `actual config set`
│   ├── telemetry/
│   │   ├── mod.rs               # Telemetry module
│   │   ├── metrics.rs           # Sync metric collection and reporting
│   │   └── identity.rs          # Repo hash generation (SHA-256; origin+HEAD, or dir path fallback)
│   ├── branding/
│   │   ├── mod.rs               # ASCII art banner and branded output
│   │   └── banner.rs            # Banner rendering
│   └── error.rs                 # Error types (thiserror)
├── tests/
│   ├── integration/
│   │   ├── sync_test.rs
│   │   ├── merge_test.rs
│   │   └── fixtures/
│   │       ├── sample_claude_md/
│   │       └── sample_api_responses/
│   └── unit/
│       ├── renderer_test.rs
│       └── merge_test.rs
└── docs/
    └── plan/
        └── (these planning docs)
```

## Dependencies

```toml
[package]
name = "actual-cli"
version = "0.1.0"
edition = "2021"
default-run = "actual"

[[bin]]
name = "actual"
path = "src/main.rs"

[dependencies]
# CLI framework
clap = { version = "4", features = ["derive", "env"] }

# Async runtime
tokio = { version = "1", features = ["full"] }

# HTTP client
reqwest = { version = "0.12", features = ["json", "rustls-tls"], default-features = false }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"

# Error handling
thiserror = "2"
anyhow = "1"

# Terminal UI
dialoguer = "0.11"         # Interactive prompts
indicatif = "0.17"         # Progress bars/spinners
console = "0.15"           # Colors, styling
similar = "2"              # Diff generation

# File system
dirs = "5"                 # Platform-specific directories

# Crypto
sha2 = "0.10"              # SHA-256 for repo identity hashing

# Utilities
uuid = { version = "1", features = ["v4", "serde"] }
chrono = { version = "0.4", features = ["serde"] }
which = "7"                # Find `claude` binary

[dev-dependencies]
assert_cmd = "2"           # CLI integration tests
predicates = "3"           # Test assertions
mockito = "1"              # HTTP mocking
```

## Key Types

### Core Domain Types

```rust
// analysis/types.rs
#[derive(Debug, Serialize, Deserialize)]
pub struct RepoAnalysis {
    pub is_monorepo: bool,
    pub projects: Vec<Project>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Project {
    pub path: String,
    pub name: String,
    pub languages: Vec<Language>,
    pub frameworks: Vec<Framework>,
    pub package_manager: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    TypeScript, JavaScript, Python, Rust, Go, Java,
    Kotlin, Swift, Ruby, Php, C, Cpp, CSharp, Scala, Elixir, Other,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Framework {
    pub name: String,
    pub category: FrameworkCategory,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum FrameworkCategory {
    WebFrontend, WebBackend, Mobile, Desktop, Cli, Library, Data, Ml, Devops, Testing,
}
```

```rust
// api/types.rs
#[derive(Debug, Serialize, Deserialize)]
pub struct Adr {
    pub id: String,
    pub title: String,
    pub context: Option<String>,
    pub policies: Vec<String>,
    pub instructions: Option<Vec<String>>,
    pub category: AdrCategory,
    pub applies_to: AppliesTo,
    pub matched_projects: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AdrCategory {
    pub id: String,
    pub name: String,
    pub path: String,
}
```

```rust
// tailoring/types.rs

/// Output from the combined tailoring + formatting Claude Code invocation
#[derive(Debug, Serialize, Deserialize)]
pub struct TailoringOutput {
    pub files: Vec<FileOutput>,
    pub skipped_adrs: Vec<SkippedAdr>,
    pub summary: TailoringSummary,
}

/// A single CLAUDE.md file to write
#[derive(Debug, Serialize, Deserialize)]
pub struct FileOutput {
    /// File path relative to repo root (e.g., "CLAUDE.md" or "apps/web/CLAUDE.md")
    pub path: String,
    /// AI-generated markdown content (goes inside managed markers)
    pub content: String,
    /// Brief explanation of what this file contains
    pub reasoning: String,
    /// UUIDs of ADRs included in this file
    pub adr_ids: Vec<String>,
}

/// An ADR that was not applicable to the repo
#[derive(Debug, Serialize, Deserialize)]
pub struct SkippedAdr {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TailoringSummary {
    pub total_input: usize,
    pub applicable: usize,
    pub not_applicable: usize,
    pub files_generated: usize,
}
```

### Config Type

```rust
// config/types.rs
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    /// API endpoint URL
    pub api_url: Option<String>,

    /// Default model for Claude Code invocations
    pub model: Option<String>,

    /// Categories to always include
    pub include_categories: Option<Vec<String>>,

    /// Categories to always exclude
    pub exclude_categories: Option<Vec<String>>,

    /// Whether to include general (language-agnostic) ADRs
    pub include_general: Option<bool>,

    /// Maximum budget per tailoring invocation (USD) -- optional, no default
    pub max_budget_usd: Option<f64>,

    /// Batch size for ADR tailoring (default: 15)
    pub batch_size: Option<usize>,

    /// Max concurrent projects during tailoring (default: 3)
    pub concurrency: Option<usize>,

    /// Telemetry settings
    pub telemetry: Option<TelemetryConfig>,

    /// Rejected ADR IDs per repo (keyed by SHA-256 of origin URL)
    pub rejected_adrs: Option<HashMap<String, Vec<String>>>,

    /// Cached repo analysis (keyed to HEAD commit hash)
    pub cached_analysis: Option<CachedAnalysis>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TelemetryConfig {
    /// Whether telemetry is enabled (default: true, opt-out)
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CachedAnalysis {
    pub repo_path: String,
    pub head_commit: Option<String>,  // None if not a git repo
    pub analysis: RepoAnalysis,
    pub analyzed_at: chrono::DateTime<chrono::Utc>,
}
```

## Error Handling Strategy

```rust
// error.rs
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ActualError {
    #[error("Claude Code not found. Install with: npm install -g @anthropic-ai/claude-code")]
    ClaudeNotFound,

    #[error("Claude Code not authenticated. Run: claude auth login")]
    ClaudeNotAuthenticated,

    // NotGitRepo is not an error -- it's handled as a warning in the environment check.
    // The CLI proceeds but with degraded caching (no HEAD-based cache key) and
    // telemetry (directory path hash fallback).

    #[error("Claude Code subprocess failed: {message}")]
    ClaudeSubprocessFailed { message: String, stderr: String },

    #[error("Failed to parse Claude Code output: {0}")]
    ClaudeOutputParse(#[from] serde_json::Error),

    #[error("API request failed: {0}")]
    ApiError(String),

    #[error("API returned error: {code}: {message}")]
    ApiResponseError { code: String, message: String },

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Config error: {0}")]
    ConfigError(String),

    #[error("User cancelled")]
    UserCancelled,
}

impl ActualError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::UserCancelled => 4,
            Self::ClaudeNotFound | Self::ClaudeNotAuthenticated => 2,
            Self::ApiError(_) | Self::ApiResponseError { .. } => 3,
            Self::IoError(_) => 5,
            _ => 1,
        }
    }
}
```

## Async Architecture

The CLI uses `tokio` for async operations:

- **Subprocess execution**: `tokio::process::Command` for Claude Code invocations
- **HTTP requests**: `reqwest` async client for API calls
- **Concurrent tailoring**: Multiple projects can be tailored in parallel using `tokio::join!` or `FuturesUnordered`

```rust
// Parallel tailoring for monorepos (with concurrency limit)
async fn tailor_all_projects(
    projects: &[ProjectAdrs],
    claude: &ClaudeClient,
    concurrency: usize,  // default: 3
) -> Result<Vec<TailoredProject>> {
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let futures: Vec<_> = projects
        .iter()
        .map(|p| {
            let sem = semaphore.clone();
            async move {
                let _permit = sem.acquire().await?;
                claude.tailor_adrs(p).await
            }
        })
        .collect();

    let results = futures::future::join_all(futures).await;
    // ... collect and handle errors
}
```

## Testing Strategy

1. **Unit tests**: Renderer, merge logic, config parsing -- pure functions, no I/O
2. **Integration tests**: Full CLI invocation with mocked HTTP (mockito) and mocked Claude Code subprocess
3. **Fixtures**: Sample API responses and CLAUDE.md files in `tests/integration/fixtures/`

### Mocking Claude Code

For tests, we mock the Claude Code subprocess by:
- Setting a `CLAUDE_BINARY` env var that points to a mock script
- The mock script reads the prompt from stdin/args and returns fixture JSON
- Or by injecting a trait-based `ClaudeRunner` that can be swapped for tests

```rust
#[cfg_attr(test, mockall::automock)]
#[async_trait::async_trait]
pub trait ClaudeRunner {
    async fn run(&self, prompt: &str, schema: &str, opts: &ClaudeOpts) -> Result<String>;
}
```

## Build & Distribution

- **Binary name**: `actual`
- **Cross-compilation**: Use `cross` or `cargo-zigbuild` for Linux/macOS/Windows
- **Release**: GitHub Releases with pre-built binaries
- **Install**: `cargo install actual-cli` or download from releases
- **CI**: GitHub Actions with matrix builds (linux-x86_64, linux-aarch64, macos-x86_64, macos-aarch64, windows-x86_64)
