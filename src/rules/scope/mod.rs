//! Local scope resolution: which committed rule documents govern a plan.
//!
//! # Design
//!
//! The status quo asks a coding agent to pick rule files by reading their
//! names. That fails for a structural reason rather than a tuning one: in the
//! reference corpus every one of the 425 filenames carries the same
//! `cross-cutting-` topic prefix, so the segment meant to carry topic carries
//! nothing, and the selection is unverifiable besides — nothing records why a
//! file was chosen or what was passed over.
//!
//! Real applicability *is* present in every one of those files, in two places
//! the filename scan never opens: the prose sentence under the title, and the
//! `### Verify` block, whose grep and test operands name concrete paths. This
//! module reads both.
//!
//! Seven layers, each testable alone:
//!
//! * [`signals`] extracts globs and terms from one document's text. Pure.
//! * [`index`] builds the searchable index and scores a plan against it. Pure,
//!   deterministic, and the ranking half of the answer — no model, no network.
//! * [`cache`] keys a persisted index on a digest of the rule files' contents,
//!   so the build cost is paid once per rule-set change rather than once per
//!   command, and never reuses an index built from text that has changed.
//! * [`baseline`] mechanizes the filename scan, so the claim that this is
//!   better than the status quo is a measurement rather than an assertion.
//! * [`eval`] is the metric both selectors are scored with.
//! * [`rank`] is stage 2: the runner-backed judgement over a prefiltered
//!   candidate set, asked for only when stage 1 leaves more candidates than the
//!   caller may keep.
//! * [`select`] joins the two, and is what a caller wanting an answer should
//!   use. Every failure in stage 2 degrades to the stage-1 answer rather than
//!   to an error.
//!
//! [`resolve`] is the one entry point most callers want: load, reuse or build
//! the index, and hand back something searchable.

pub mod baseline;
pub mod cache;
pub mod eval;
pub mod index;
pub mod rank;
pub mod select;
pub mod signals;

use std::path::Path;

use crate::error::ActualError;
use crate::rules::{parse_rule_sources, read_rule_sources, rules_dir, RuleSetLoadReport};

pub use eval::{CaseResult, EvaluationReport, GoldenCase, Scores};
pub use index::{Field, GlobMatch, IndexedDocument, Match, Query, ScopeIndex, Weights};
pub use rank::{Candidate, RankedVerdict, Verdict};
pub use select::{
    prefilter, select, Prefiltered, SelectedRule, Selection, Stage, Stage2, DEFAULT_CANDIDATES,
};

/// Whether a resolved index was reused from cache or built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexSource {
    /// Read from the cache, fingerprint still valid.
    Cached,
    /// Built from the rule files in this call.
    Built,
    /// Built because the caller asked for a rebuild.
    Rebuilt,
}

impl IndexSource {
    pub fn label(self) -> &'static str {
        match self {
            IndexSource::Cached => "(cached)",
            IndexSource::Built => "(built)",
            IndexSource::Rebuilt => "(rebuilt)",
        }
    }
}

/// A resolved index and how it was obtained.
#[derive(Debug)]
pub struct ResolvedIndex {
    pub index: ScopeIndex,
    pub source: IndexSource,
    /// The load report, present only when the rule files were actually parsed.
    /// A cache hit reads them — that is how the digest is checked — but skips
    /// the parse and the index build, which is where the time goes.
    pub report: Option<RuleSetLoadReport>,
}

/// Load the index for the repository at `root`, building it if the cache is
/// absent or stale.
///
/// `force_rebuild` skips the cache read but still writes the freshly built
/// index back, so a forced rebuild repairs the cache rather than bypassing it.
pub fn resolve(root: &Path, force_rebuild: bool) -> Result<ResolvedIndex, ActualError> {
    let dir = rules_dir(root);
    // One read per invocation. The digest covers exactly these bytes, so a hit
    // can never be keyed to text other than the text that was indexed.
    let sources = read_rule_sources(root)?;

    if !force_rebuild {
        if let Some(index) = cache::load(&dir, &sources.digest) {
            return Ok(ResolvedIndex {
                index,
                source: IndexSource::Cached,
                report: None,
            });
        }
    }

    let report = parse_rule_sources(sources);
    let index = ScopeIndex::build(&report, root, report.digest.clone());
    cache::store(&dir, &index);
    Ok(ResolvedIndex {
        index,
        source: if force_rebuild {
            IndexSource::Rebuilt
        } else {
            IndexSource::Built
        },
        report: Some(report),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::{tempdir, TempDir};

    use crate::testutil::{EnvGuard, ENV_MUTEX};

    const DOC: &str = "# Token Signing\n\nThese rules are ALWAYS ACTIVE for OAuth token signing in `services/auth/oauth/`.\n\n### Rules\n\n- **R-A-001** MUST: sign with RS256.\n";
    const OTHER: &str = "# Terraform\n\nThese rules are ALWAYS ACTIVE for Terraform configuration.\n\n### Rules\n\n- **R-B-001** MUST: pin providers.\n";

    /// Helper: a repository root whose `.actual/rules/` holds `files`.
    fn seed(files: &[(&str, &str)]) -> TempDir {
        let root = tempdir().unwrap();
        let dir = rules_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        for (name, contents) in files {
            std::fs::write(dir.join(name), contents).unwrap();
        }
        root
    }

    #[test]
    fn test_resolve_builds_then_reuses_the_cached_index() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _clear = EnvGuard::remove("ACTUAL_CONFIG");

        let root = seed(&[("a.md", DOC), ("b.md", OTHER)]);

        let first = resolve(root.path(), false).unwrap();
        assert_eq!(first.source, IndexSource::Built);
        assert_eq!(first.index.len(), 2);
        // The build read the rule files, so the load report is available.
        assert!(first.report.is_some());

        let second = resolve(root.path(), false).unwrap();
        assert_eq!(second.source, IndexSource::Cached);
        assert_eq!(second.index, first.index);
        // A cache hit does not read the rule files, which is the point of it.
        assert!(second.report.is_none());
    }

    #[test]
    fn test_resolve_rebuilds_after_a_rule_file_changes() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _clear = EnvGuard::remove("ACTUAL_CONFIG");

        let root = seed(&[("a.md", DOC)]);
        assert_eq!(resolve(root.path(), false).unwrap().index.len(), 1);

        std::fs::write(rules_dir(root.path()).join("b.md"), OTHER).unwrap();
        let after = resolve(root.path(), false).unwrap();
        assert_eq!(after.source, IndexSource::Built);
        assert_eq!(after.index.len(), 2);
    }

    /// A forced rebuild repairs the cache rather than bypassing it, so the next
    /// ordinary call is a hit.
    #[test]
    fn test_resolve_force_rebuild_rewrites_the_cache() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _clear = EnvGuard::remove("ACTUAL_CONFIG");

        let root = seed(&[("a.md", DOC)]);
        resolve(root.path(), false).unwrap();

        let forced = resolve(root.path(), true).unwrap();
        assert_eq!(forced.source, IndexSource::Rebuilt);
        assert_eq!(
            resolve(root.path(), false).unwrap().source,
            IndexSource::Cached
        );
    }

    #[test]
    fn test_resolve_on_a_repository_with_no_rules_directory() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _clear = EnvGuard::remove("ACTUAL_CONFIG");

        let root = tempdir().unwrap();
        let resolved = resolve(root.path(), false).unwrap();
        assert!(resolved.index.is_empty());
        assert!(resolved.index.search(&Query::new("anything"), 5).is_empty());
    }

    #[test]
    fn test_resolve_propagates_a_load_failure() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let _clear = EnvGuard::remove("ACTUAL_CONFIG");

        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".actual")).unwrap();
        std::fs::write(rules_dir(root.path()), "not a directory").unwrap();

        assert!(resolve(root.path(), false).is_err());
    }

    #[test]
    fn test_index_source_labels() {
        assert_eq!(IndexSource::Cached.label(), "(cached)");
        assert_eq!(IndexSource::Built.label(), "(built)");
        assert_eq!(IndexSource::Rebuilt.label(), "(rebuilt)");
        assert_eq!(IndexSource::Cached, IndexSource::Cached);
    }
}
