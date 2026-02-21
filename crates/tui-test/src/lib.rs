//! # tui-test
//!
//! E2E testing library for TUI applications. Provides a real PTY-based
//! session for spawning commands and interacting with them as a terminal
//! user would.
//!
//! ## Quick Start
//!
//! ```no_run
//! use tui_test::{TuiSession, Key};
//!
//! let mut session = TuiSession::new("my-tui-app")
//!     .size(120, 40)
//!     .timeout(std::time::Duration::from_secs(10))
//!     .spawn()
//!     .unwrap();
//!
//! session.type_text("hello").unwrap();
//! session.send_key(Key::Enter).unwrap();
//! ```

pub mod capture;
mod error;
mod keys;
mod pty;
pub mod render;
mod screen;
mod session;
pub mod snapshot;

pub use capture::{CaptureConfig, CaptureStore, EventType, Timeline, TimelineEntry};
pub use error::TuiTestError;
pub use keys::Key;
pub use render::{ColorScheme, ScreenRenderer};
pub use screen::ScreenSnapshot;
pub use session::{TuiSession, TuiSessionBuilder};
pub use vt100;

// Re-export insta when snapshot feature is enabled
#[cfg(feature = "snapshot")]
pub use insta;
