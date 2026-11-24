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
//! * [`pair`] writes both files' text, and [`provenance`] the record a rule
//!   file this CLI wrote keeps of the doc it came from.
//! * [`discovery`] sorts every file under `docs/adr/` and `.actual/rules/`
//!   into a state: synced, managed, stale, unmanaged and the rest.
//! * [`normalize`] turns a decision doc written in another format into a
//!   Context File, without a model where its layout allows.
//! * [`refresh`] keeps each decision's rule file in step with its decision,
//!   from `docs/adr/` or from Actual AI, and lists the decision files it
//!   can't read.
//! * [`workflow`] runs the web app's decision workflow around the decision
//!   routes: its lists, state rules and the route each change takes.

pub mod context_file;
pub mod discovery;
pub mod files;
pub mod header;
pub mod names;
pub mod normalize;
pub mod overlap;
pub mod pair;
pub mod provenance;
pub mod refresh;
pub mod rule_body;
pub mod save_check;
pub mod workflow;
