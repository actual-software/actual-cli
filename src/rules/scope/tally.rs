//! What one run of a selector kept and dropped, and why.
//!
//! The brief and the gates each narrow the rule files in the scope index down
//! to the few an agent or the judge sees. A [`FilterTally`] counts every file
//! the index was built from into exactly one bucket: a reason it was dropped,
//! or kept. The buckets sum to [`FilterTally::index_size`], which is what makes
//! the count checkable.
//!
//! The unit is the rule file, the unit the index holds. Kept files are also
//! named by the logical ADR they belong to, the key `actual.adr.detected`
//! uses, so a selector's choice can be joined to what became of it.

use super::index::Match;

/// One selector run's account of the scope index, counted in rule files.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FilterTally {
    /// Rule files the index was built from, inactive ones included.
    pub index_size: usize,
    /// The score floor the run applied.
    pub min_score: f64,
    /// A retired status, or nothing left to use when the file was read.
    pub inactive: usize,
    /// The query's paths are all outside the file's root or excluded by it.
    pub excluded_path: usize,
    /// Scored nothing against the query.
    pub no_match: usize,
    /// Scored under [`Self::min_score`].
    pub below_floor: usize,
    /// Cut by the candidate cap or the caller's limit.
    pub capped: usize,
    /// Judged unrelated by stage 2's rank.
    pub unrelated: usize,
    /// Every rule already settled in this session.
    pub session: usize,
    /// No rule inside the judge's window this round.
    pub window: usize,
    /// Cut by the brief's own limits: its decision cap, the once-per-session
    /// memory, or its size budget.
    pub brief_cap: usize,
    /// The files kept, in rank order.
    pub kept: Vec<KeptFile>,
}

/// A rule file a selector kept.
#[derive(Debug, Clone, PartialEq)]
pub struct KeptFile {
    /// The logical ADR: the file's candidate group, else its `adr_id`. A file
    /// without a v2 header has neither.
    pub adr_ref: Option<String>,
    pub score: f64,
}

impl KeptFile {
    pub fn new(candidate_group_id: Option<&str>, adr_id: Option<&str>, score: f64) -> Self {
        let nonempty = |id: Option<&str>| id.filter(|id| !id.is_empty()).map(str::to_string);
        Self {
            adr_ref: nonempty(candidate_group_id).or_else(|| nonempty(adr_id)),
            score,
        }
    }

    pub fn of(hit: &Match) -> Self {
        Self::new(
            hit.candidate_group_id.as_deref(),
            hit.adr_id.as_deref(),
            hit.score,
        )
    }
}

impl FilterTally {
    /// Files counted into some bucket. Equal to [`Self::index_size`] for a
    /// complete account.
    pub fn accounted(&self) -> usize {
        self.inactive
            + self.excluded_path
            + self.no_match
            + self.below_floor
            + self.capped
            + self.unrelated
            + self.session
            + self.window
            + self.brief_cap
            + self.kept.len()
    }

    /// The logical ADRs among the kept files, each once at its best file's
    /// score, in rank order.
    pub fn kept_adrs(&self) -> Vec<(&str, f64)> {
        let mut adrs: Vec<(&str, f64)> = Vec::new();
        for file in &self.kept {
            let Some(adr_ref) = file.adr_ref.as_deref() else {
                continue;
            };
            if !adrs.iter().any(|(seen, _)| *seen == adr_ref) {
                adrs.push((adr_ref, file.score));
            }
        }
        adrs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(group: Option<&str>, adr_id: Option<&str>, score: f64) -> Match {
        Match {
            slug: "doc".to_string(),
            relative_path: "doc.md".to_string(),
            title: None,
            adr: None,
            adr_id: adr_id.map(str::to_string),
            candidate_group_id: group.map(str::to_string),
            root: None,
            score,
            contributions: Vec::new(),
            matched_globs: Vec::new(),
        }
    }

    #[test]
    fn test_a_kept_file_is_named_by_its_group_then_its_adr_id() {
        assert_eq!(
            KeptFile::of(&hit(Some("grp"), Some("adr"), 1.0))
                .adr_ref
                .as_deref(),
            Some("grp")
        );
        assert_eq!(
            KeptFile::of(&hit(Some(""), Some("adr"), 1.0))
                .adr_ref
                .as_deref(),
            Some("adr")
        );
        assert_eq!(KeptFile::of(&hit(None, Some(""), 1.0)).adr_ref, None);
    }

    #[test]
    fn test_kept_adrs_name_each_adr_once_at_its_best_score() {
        let tally = FilterTally {
            kept: vec![
                KeptFile::of(&hit(Some("grp"), Some("a"), 0.9)),
                KeptFile::of(&hit(None, None, 0.8)),
                KeptFile::of(&hit(None, Some("b"), 0.7)),
                KeptFile::of(&hit(Some("grp"), Some("c"), 0.6)),
            ],
            ..FilterTally::default()
        };
        assert_eq!(tally.kept_adrs(), vec![("grp", 0.9), ("b", 0.7)]);
    }

    #[test]
    fn test_accounted_sums_every_bucket() {
        let tally = FilterTally {
            index_size: 46,
            inactive: 1,
            excluded_path: 2,
            no_match: 3,
            below_floor: 4,
            capped: 5,
            unrelated: 6,
            session: 7,
            window: 8,
            brief_cap: 9,
            kept: vec![KeptFile::of(&hit(None, None, 1.0))],
            ..FilterTally::default()
        };
        assert_eq!(tally.accounted(), 46);
    }
}
