//! Where a decision's rule file came from, recorded in the rule file's own
//! header so a later `actual adr discover` can tell a decision this CLI wrote
//! from one Actual AI's sync wrote, and see when its decision doc has changed
//! since.
//!
//! The record sits under `metadata.actual.source`:
//!
//! ```yaml
//! source:
//!   origin: local
//!   path: docs/adr/0007-use-postgres.md
//!   digest: sha256:…
//! ```
//!
//! The sync never writes it, so a rule file without one is the sync's. The
//! digest covers only what the rule file is built from (the title, the
//! status, the scope and the policies), so rewording a decision's context or
//! rationale doesn't make its rule file stale, and changing what it governs
//! does.

use sha2::{Digest, Sha256};

use crate::adr::context_file::ContextFile;
use crate::adr::header::normalize_scope_root;

/// The `origin` of a rule file this CLI wrote from a decision doc.
pub const ORIGIN_LOCAL: &str = "local";

/// The `metadata.actual.source` record of a rule file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Source {
    /// Who wrote the rule file: [`ORIGIN_LOCAL`] for this CLI.
    pub origin: String,
    /// The decision doc it was built from, relative to the repository root,
    /// with `/` separators.
    pub path: String,
    /// [`digest`] of that doc when the rule file was written.
    pub digest: String,
}

impl Source {
    /// A record for a rule file this CLI writes from the doc at `path`.
    pub fn local(path: &str, digest: String) -> Self {
        Source {
            origin: ORIGIN_LOCAL.to_string(),
            path: path.to_string(),
            digest,
        }
    }

    /// Whether this CLI wrote the rule file.
    pub fn is_local(&self) -> bool {
        self.origin == ORIGIN_LOCAL
    }
}

/// The fields a digest covers, in a fixed order.
#[derive(serde::Serialize)]
struct Governing<'a> {
    title: &'a str,
    status: String,
    root: String,
    paths: &'a [String],
    policies: Vec<(&'static str, &'a str)>,
}

/// `sha256:<hex>` over what a decision's rule file is built from: `doc`'s
/// title and policies, its header `status` (compared without case) and its
/// scope.
pub fn digest(doc: &ContextFile, status: &str, root: &str, paths: &[String]) -> String {
    let governing = Governing {
        title: doc.title.trim(),
        status: status.trim().to_ascii_lowercase(),
        root: normalize_scope_root(root),
        paths,
        policies: doc
            .policies
            .iter()
            .map(|policy| (policy.level.as_str(), policy.statement.trim()))
            .collect(),
    };
    // The struct holds only strings, so it always serializes.
    let canonical = serde_json::to_vec(&governing).unwrap_or_default();
    let hash = Sha256::digest(&canonical);
    let hex: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("sha256:{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adr::context_file::Policy;
    use crate::rules::types::RuleLevel;

    fn doc(title: &str, policies: &[(RuleLevel, &str)]) -> ContextFile {
        ContextFile {
            title: title.to_string(),
            policies: policies
                .iter()
                .map(|(level, statement)| Policy {
                    level: *level,
                    statement: statement.to_string(),
                })
                .collect(),
            ..ContextFile::default()
        }
    }

    #[test]
    fn digest_is_stable_and_prefixed() {
        let d = doc(
            "Use Postgres",
            &[(RuleLevel::Must, "Store data in Postgres.")],
        );
        let one = digest(&d, "accepted", ".", &[]);
        assert!(one.starts_with("sha256:"));
        assert_eq!(one.len(), "sha256:".len() + 64);
        assert_eq!(one, digest(&d, "accepted", ".", &[]));
    }

    #[test]
    fn digest_ignores_prose_and_case_but_not_what_governs() {
        let d = doc(
            "Use Postgres",
            &[(RuleLevel::Must, "Store data in Postgres.")],
        );
        let base = digest(&d, "accepted", ".", &[]);

        let mut reworded = d.clone();
        reworded.context = vec!["New context.".to_string()];
        reworded.rationale = vec!["New rationale.".to_string()];
        assert_eq!(digest(&reworded, " Accepted ", "./", &[]), base);

        let mut releveled = d.clone();
        releveled.policies[0].level = RuleLevel::Should;
        assert_ne!(digest(&releveled, "accepted", ".", &[]), base);
        assert_ne!(digest(&d, "draft", ".", &[]), base);
        assert_ne!(digest(&d, "accepted", "services", &[]), base);
        assert_ne!(digest(&d, "accepted", ".", &["src/**".to_string()]), base);
        assert_ne!(
            digest(
                &doc("Use MySQL", &[(RuleLevel::Must, "Store data in Postgres.")]),
                "accepted",
                ".",
                &[]
            ),
            base
        );
    }

    #[test]
    fn local_source_is_local() {
        let source = Source::local("docs/adr/a.md", "sha256:00".to_string());
        assert!(source.is_local());
        assert_eq!(source.path, "docs/adr/a.md");
        let other = Source {
            origin: "sync".to_string(),
            ..source
        };
        assert!(!other.is_local());
    }
}
