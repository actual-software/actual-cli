// Note: A CursorRunner is not feasible — Cursor has no headless/CLI/API mode.
// Cursor users are supported via OutputFormat::CursorRules (output concern, not a runner concern).
// See the investigation in the removed cursor_runner.rs for full findings.
pub mod anthropic_api;
pub mod auth;
pub mod binary;
pub mod codex_cli;
pub mod obfuscation;
pub mod openai_api;
pub mod options;
pub mod prompts;
pub mod schemas;
pub mod subprocess;

pub use anthropic_api::AnthropicApiRunner;
pub use codex_cli::{find_codex_binary, CodexCliRunner};
pub use openai_api::OpenAiApiRunner;
pub use options::InvocationOptions;
pub use subprocess::{ClaudeRunner, CliClaudeRunner, TailoringRunner};
