//! ADRs as files in a repository, written and read the way Actual AI writes
//! and reads them, for `actual adr`.
//!
//! * [`context_file`] reads and writes a decision's Context File, the
//!   markdown body of a decision doc and what a create sends.
//! * [`save_check`] is the check Actual AI runs before it saves a decision.
//! * [`overlap`] finds likely duplicates by word overlap, with no model call.
//! * [`names`], [`header`] and [`rule_body`] lay out the two files a
//!   decision has in a repository, as the repository sync names them.
//! * [`files`] finds, reads and renames those files on disk.
//! * [`workflow`] runs the web app's decision workflow around the decision
//!   routes: its lists, state rules and the route each change takes.

pub mod context_file;
pub mod files;
pub mod header;
pub mod names;
pub mod overlap;
pub mod rule_body;
pub mod save_check;
pub mod workflow;
