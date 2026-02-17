//! # Output Stream Policy
//!
//! - **stderr**: Banner, pipeline spinners, verbose output, error messages.
//!   Styled text for stderr must use `.for_stderr()` on `StyledObject`.
//! - **stdout**: Project summary, diff summary, write results, sync summary,
//!   status output. Goes through `TerminalIO.write_line()` in sync command,
//!   `println!()` in status/config commands.
//! - **Format functions** (`format_project_summary`, `format_diff_summary`, etc.)
//!   return `String` with embedded ANSI codes. They use `console::style()`
//!   which checks stdout color support. This is correct when the caller uses
//!   `term.write_line()` (stdout) but incorrect when the caller uses
//!   `eprintln!()`. Known limitation; tracked for future refactor.

pub mod confirm;
pub mod diff;
pub mod file_confirm;
pub mod progress;
pub mod real_terminal;
pub mod theme;
