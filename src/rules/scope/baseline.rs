//! The status-quo selector, mechanized so it can be measured.
//!
//! # Design
//!
//! `CLAUDE.md` tells the agent to eyeball rule filenames, match aspect-slugs by
//! judgement, and read at most five. That is the bar the scope index has to
//! clear, so it is implemented here as an ordinary function: match query terms
//! against the filename's slug segments, rank by how many match, cap the
//! result.
//!
//! It lives in the library rather than in the test that uses it for one reason:
//! a baseline written inside its own comparison is a baseline nobody can check.
//! Here it is public, tested on its own, and callable from `--explain` so the
//! two selections can be put side by side on real input.
//!
//! It is deliberately *not* strengthened. Giving it path globs or prose would
//! make it a different algorithm, and the comparison would stop being about the
//! status quo.

use crate::rules::scope::index::ScopeIndex;
use crate::rules::scope::signals;

/// The cap `CLAUDE.md` places on how many rule files the agent may read.
pub const DEFAULT_LIMIT: usize = 5;

/// One filename-scan hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineMatch {
    pub slug: String,
    /// How many query terms the filename's slug segments matched.
    pub matched_terms: Vec<String>,
}

/// Select rule documents the way the status quo does: by filename alone.
///
/// Ranked by number of matching slug terms, ties broken by slug so the result
/// is deterministic, capped at `limit`.
pub fn select(index: &ScopeIndex, query_text: &str, limit: usize) -> Vec<BaselineMatch> {
    let query_terms: Vec<String> = signals::terms(query_text);
    let mut hits: Vec<BaselineMatch> = index
        .documents
        .iter()
        .filter_map(|doc| {
            let slug_terms = signals::slug_terms(&doc.slug);
            let mut matched: Vec<String> = query_terms
                .iter()
                .filter(|term| slug_terms.contains(term))
                .cloned()
                .collect();
            matched.sort();
            matched.dedup();
            (!matched.is_empty()).then(|| BaselineMatch {
                slug: doc.slug.clone(),
                matched_terms: matched,
            })
        })
        .collect();

    hits.sort_by(|a, b| {
        b.matched_terms
            .len()
            .cmp(&a.matched_terms.len())
            .then_with(|| a.slug.cmp(&b.slug))
    });
    hits.truncate(limit);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::{Path, PathBuf};

    use crate::rules::{parse_rule_document, RuleSetLoadReport};

    /// Helper: an index over documents named only by slug — the filename scan
    /// reads nothing else, so nothing else needs to vary.
    fn index_of(slugs: &[&str]) -> ScopeIndex {
        let documents = slugs
            .iter()
            .map(|slug| {
                parse_rule_document(
                    &PathBuf::from(format!("/repo/.actual/rules/{slug}.md")),
                    "# T\n\nScope.\n\n### Rules\n\n- **R-A-001** MUST: a.\n",
                )
                .unwrap()
            })
            .collect();
        ScopeIndex::build(
            &RuleSetLoadReport {
                rules_dir: PathBuf::from("/repo/.actual/rules"),
                documents,
                errors: Vec::new(),
                digest: String::new(),
            },
            Path::new("/repo"),
            "fp".to_string(),
        )
    }

    fn sample() -> ScopeIndex {
        index_of(&[
            "cross-cutting-access-tokens-include-e410",
            "cross-cutting-token-expiry-a1b2",
            "cross-cutting-dictionary-access-user-1b5c",
            "cross-cutting-provider-pinning-c3d4",
        ])
    }

    #[test]
    fn test_select_matches_on_filename_segments_only() {
        let hits = select(&sample(), "rotate the access token", 5);
        let slugs: Vec<&str> = hits.iter().map(|h| h.slug.as_str()).collect();
        assert_eq!(
            slugs,
            vec![
                "cross-cutting-access-tokens-include-e410",
                "cross-cutting-dictionary-access-user-1b5c",
                "cross-cutting-token-expiry-a1b2",
            ]
        );
    }

    /// The status quo's characteristic failure, kept intact so the comparison
    /// stays honest: a shared word drags in an unrelated rule, and the file
    /// that would settle it is never opened.
    #[test]
    fn test_select_ranks_by_how_many_segments_matched() {
        let hits = select(&sample(), "rotate the access token", 5);
        assert_eq!(hits[0].matched_terms, vec!["access", "token"]);
        assert_eq!(hits[1].matched_terms, vec!["access"]);
    }

    #[test]
    fn test_select_honours_the_cap() {
        assert_eq!(
            select(&sample(), "access token provider", DEFAULT_LIMIT).len(),
            4
        );
        assert_eq!(select(&sample(), "access token provider", 2).len(), 2);
        assert_eq!(select(&sample(), "access token", 0).len(), 0);
    }

    #[test]
    fn test_select_returns_nothing_when_no_segment_matches() {
        assert!(select(&sample(), "kubernetes ingress", 5).is_empty());
        assert!(select(&sample(), "", 5).is_empty());
    }

    /// The scan is blind to the dead topic prefix: matching on it would return
    /// the whole corpus. It does, which is exactly the status quo being
    /// measured — the prefix is on every filename, so it selects everything.
    #[test]
    fn test_select_is_dragged_by_the_ubiquitous_topic_prefix() {
        let hits = select(&sample(), "a cross-cutting change", DEFAULT_LIMIT);
        assert_eq!(hits.len(), 4, "the prefix matches every document");
    }

    #[test]
    fn test_select_is_deterministic() {
        let index = sample();
        let first = select(&index, "access token", 5);
        for _ in 0..3 {
            assert_eq!(select(&index, "access token", 5), first);
        }
        assert_eq!(first[0].clone(), first[0]);
        assert!(format!("{:?}", first[0]).contains("access"));
    }

    #[test]
    fn test_default_limit_matches_the_documented_cap() {
        assert_eq!(DEFAULT_LIMIT, 5);
    }
}
