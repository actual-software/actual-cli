//! # Cursor as a Tailoring Backend — Investigation Findings
//!
//! ## Summary
//!
//! Cursor (https://cursor.sh) is an AI-powered IDE with no headless or CLI mode.
//! It cannot be used as a subprocess or HTTP API for tailoring invocations.
//!
//! ## Investigation
//!
//! ### CLI interface
//! Cursor does not expose a `--print` or non-interactive mode. The `cursor` binary
//! opens the Cursor IDE application — it has no exec or eval mode.
//!
//! ### API / IPC interface
//! Cursor does not expose a public REST API, gRPC endpoint, or IPC socket for
//! external process communication.
//!
//! ### MCP Server
//! Cursor can act as an MCP *client* (it can call MCP servers), but it does not
//! expose itself as an MCP server that external processes can query.
//!
//! ## Conclusion
//!
//! A `CursorRunner` that uses Cursor as an inference backend is not feasible.
//!
//! The correct way to support Cursor users is through **output format support**:
//! generating `.cursor/rules/actual-policies.mdc` files (already supported via
//! `OutputFormat::CursorRules`). This is an output concern, not a runner concern.
//!
//! No `CursorRunner` implementation is planned.

// This file intentionally contains no code — Cursor cannot be used as a runner.
// See the module doc comment above for the investigation findings.
