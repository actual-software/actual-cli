//! The scope index: which rule documents apply to a plan, and why.
//!
//! # Design
//!
//! At plan time there is no diff, so path matching cannot be the only selector
//! and free text cannot be ignored. The index therefore scores every document
//! on **five independently weighted fields** and sums them:
//!
//! | field | source | why it is weighted where it is |
//! |---|---|---|
//! | `path` | verify-block operands | executable evidence the author had to get right |
//! | `scope` | the prose applicability sentence | states applicability outright, in words |
//! | `path_terms` | words inside those operands | reaches path evidence from a plan that names no path |
//! | `title` | the `#` heading | free, and states the subject |
//! | `slug` | the aspect segment of the filename | the status quo's *only* signal, kept as a tiebreak |
//!
//! Two properties fall out of that table and are the point of the design.
//!
//! **Every term is weighted by inverse document frequency.** The filename topic
//! prefix that motivated this work — `cross-cutting-`, on all 425 files of the
//! reference corpus — appears in every document, so its IDF is zero and it
//! contributes nothing. That needs no stopword entry, no threshold, and no
//! per-corpus tuning: a segment stops counting exactly when it stops
//! discriminating. [`ScopeIndex::ubiquitous_terms`] reports which terms fell out
//! this way, so the effect is inspectable rather than mysterious.
//!
//! **Path containment is directional both ways.** A plan naming
//! `apps/actual/lib/oauth/token.ts` matches a rule globbing
//! `apps/actual/lib/oauth/**`, and a plan naming `apps/actual/lib/` matches it
//! too. At plan time the author may be more or less specific than the rule
//! file; scoring by the number of agreeing leading segments keeps both useful
//! and still ranks the exact hit higher.
//!
//! Scoring is pure and deterministic — no LLM, no network, no clock. Ties break
//! on slug so a given index and query always produce the same order.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::rules::scope::signals::{self, DocumentSignals, PathGlob};
use crate::rules::{RuleDocument, RuleSetLoadReport};

/// Bump when the stored shape or the scoring inputs change, so a cached index
/// written by an older build is discarded rather than misread.
pub const INDEX_FORMAT_VERSION: u32 = 3;

/// Which signal a match came from. Ordered as the fields are documented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    Path,
    Scope,
    PathTerms,
    Title,
    Slug,
}

impl Field {
    pub const ALL: &'static [Field] = &[
        Field::Path,
        Field::Scope,
        Field::PathTerms,
        Field::Title,
        Field::Slug,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Field::Path => "path",
            Field::Scope => "scope",
            Field::PathTerms => "path-terms",
            Field::Title => "title",
            Field::Slug => "slug",
        }
    }
}

/// Per-field weights.
///
/// The defaults are set from the table in the module docs — evidence the rule
/// author had to make executable outranks prose, which outranks the filename —
/// not fitted to any golden set. They are a struct rather than constants so the
/// evaluation harness can measure the cost of each signal by zeroing it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Weights {
    pub path: f64,
    pub scope: f64,
    pub path_terms: f64,
    pub title: f64,
    pub slug: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            path: 3.0,
            scope: 2.0,
            path_terms: 1.0,
            title: 1.0,
            slug: 0.5,
        }
    }
}

impl Weights {
    pub fn get(&self, field: Field) -> f64 {
        match field {
            Field::Path => self.path,
            Field::Scope => self.scope,
            Field::PathTerms => self.path_terms,
            Field::Title => self.title,
            Field::Slug => self.slug,
        }
    }

    /// The same weights with `field` zeroed — an ablation, for measuring what
    /// one signal is worth.
    pub fn without(mut self, field: Field) -> Self {
        match field {
            Field::Path => self.path = 0.0,
            Field::Scope => self.scope = 0.0,
            Field::PathTerms => self.path_terms = 0.0,
            Field::Title => self.title = 0.0,
            Field::Slug => self.slug = 0.0,
        }
        self
    }
}

/// One indexed document: its identity and its extracted signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedDocument {
    /// The file stem, the document's stable identity.
    pub slug: String,
    /// Path relative to the scanned repository root, so an index built from the
    /// same rule set is identical on any machine.
    pub relative_path: String,
    pub title: Option<String>,
    pub scope: Option<String>,
    pub globs: Vec<String>,
    /// Terms per field with how often each occurs in that field.
    ///
    /// Frequencies, not a plain set, because inverse document frequency alone
    /// cannot tell a domain term from an ordinary English word: in a corpus of
    /// this size `value` is exactly as rare as `jwks` if each appears in one
    /// document. How often a document says a word is what separates them, so a
    /// saturating term-frequency factor rides alongside IDF in
    /// [`term_coverage`].
    pub field_terms: BTreeMap<Field, BTreeMap<String, u32>>,
}

impl IndexedDocument {
    fn terms(&self, field: Field) -> Option<&BTreeMap<String, u32>> {
        self.field_terms.get(&field)
    }

    /// The globs, parsed back into the type that knows how to compare them.
    fn path_globs(&self) -> impl Iterator<Item = PathGlob> + '_ {
        self.globs.iter().map(|pattern| PathGlob {
            pattern: pattern.clone(),
        })
    }
}

/// A deterministic, offline index over a rule set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopeIndex {
    pub format_version: u32,
    /// Digest of the exact rule-file bytes this index was built from. A cached
    /// index whose digest no longer matches the files on disk is discarded.
    pub content_digest: String,
    pub documents: Vec<IndexedDocument>,
    /// term → number of documents containing it, in any field.
    pub document_frequency: BTreeMap<String, usize>,
}

/// A plan to be matched against the index.
///
/// `text` is the plan prose. `paths` are files or directories the plan already
/// names, when the caller has them; they are optional precisely because at plan
/// time there is usually no diff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub paths: Vec<String>,
}

impl Query {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            paths: Vec::new(),
        }
    }

    pub fn with_paths(mut self, paths: impl IntoIterator<Item = String>) -> Self {
        self.paths = paths.into_iter().collect();
        self
    }

    /// Paths the query names: those given explicitly, plus any path-shaped
    /// token found in the prose, so `touch apps/actual/lib/oauth/token.ts` in a
    /// plan sentence counts without the caller having to split it out.
    pub fn all_paths(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for path in self
            .paths
            .iter()
            .map(|p| p.trim_start_matches("./").trim_end_matches('/').to_string())
            .chain(
                signals::globs_from_prose(&self.text)
                    .into_iter()
                    .map(|g| g.pattern.trim_end_matches("/**").to_string()),
            )
        {
            if !path.is_empty() && !out.contains(&path) {
                out.push(path);
            }
        }
        out
    }
}

/// What one field contributed to a document's score, and on the strength of
/// which terms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldContribution {
    pub field: Field,
    /// The field's own 0..1 coverage of the query, before weighting.
    pub coverage: f64,
    /// `coverage * weight` — what actually entered the total.
    pub weighted: f64,
    /// The query terms this field matched, strongest first.
    pub matched: Vec<String>,
}

/// One ranked document, with the evidence for its rank.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Match {
    pub slug: String,
    pub relative_path: String,
    pub title: Option<String>,
    pub score: f64,
    pub contributions: Vec<FieldContribution>,
    /// Document globs that matched a path the query named, with the number of
    /// agreeing leading segments.
    pub matched_globs: Vec<GlobMatch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobMatch {
    pub glob: String,
    pub query_path: String,
    pub segments: usize,
    /// True when the glob matches the query path outright, rather than the two
    /// merely sharing a prefix.
    pub exact: bool,
}

/// Term-frequency half-saturation point. At `tf == TF_SATURATION` a term
/// contributes half its IDF weight; the curve is flat well before ten, so
/// repeating a word cannot carry a document on its own.
const TF_SATURATION: f64 = 1.5;

/// What partial path agreement is worth relative to an outright glob match.
const CONTAINMENT_DISCOUNT: f64 = 0.75;

/// Depth at which path agreement is considered total. Four segments is
/// `apps/actual/lib/oauth` — specific enough that more agreement adds nothing.
const PATH_SATURATION_SEGMENTS: usize = 4;

impl ScopeIndex {
    /// Build an index from a loaded rule set.
    ///
    /// `root` is the repository root the documents were discovered under; paths
    /// are stored relative to it so the index is machine-independent.
    /// `fingerprint` identifies the rule files the index was built from.
    pub fn build(
        report: &RuleSetLoadReport,
        root: &std::path::Path,
        content_digest: String,
    ) -> Self {
        let mut documents: Vec<IndexedDocument> = report
            .documents
            .iter()
            .map(|doc| index_document(doc, root))
            .collect();
        documents.sort_by(|a, b| a.slug.cmp(&b.slug));

        let mut document_frequency: BTreeMap<String, usize> = BTreeMap::new();
        for doc in &documents {
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            for field in Field::ALL {
                for term in doc.terms(*field).into_iter().flatten().map(|(t, _)| t) {
                    seen.insert(term.as_str());
                }
            }
            for term in seen {
                *document_frequency.entry(term.to_string()).or_insert(0) += 1;
            }
        }

        Self {
            format_version: INDEX_FORMAT_VERSION,
            content_digest,
            documents,
            document_frequency,
        }
    }

    pub fn len(&self) -> usize {
        self.documents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }

    /// Inverse document frequency, smoothed so a term present in every document
    /// scores exactly zero.
    ///
    /// This is the whole answer to the dead filename prefix: `cross-cutting`
    /// occurs in 425 of 425 documents, `ln(425/425) == 0`, and it drops out of
    /// every score without being named anywhere.
    pub fn idf(&self, term: &str) -> f64 {
        let n = self.documents.len() as f64;
        if n == 0.0 {
            return 0.0;
        }
        let df = *self.document_frequency.get(term).unwrap_or(&0) as f64;
        if df <= 0.0 {
            return 0.0;
        }
        (n / df).ln().max(0.0)
    }

    /// What a query term is worth when ranking, which is inverse document
    /// frequency everywhere the corpus can support the question.
    ///
    /// [`Self::idf`] asks which document a term *discriminates towards*. A
    /// corpus of one document cannot answer that: every term it contains is a
    /// term in every document, so `ln(1/1)` is zero, every field's coverage is
    /// zero, and a freshly synced repository holding a single rule selects
    /// nothing at all for a plan that obviously matches it.
    ///
    /// So below two documents the question changes from "which rule matches
    /// best" to "does this rule match", and each term the corpus knows counts
    /// the same. A term the corpus has never seen still scores zero, so an
    /// unrelated plan still matches nothing.
    ///
    /// This deliberately does **not** smooth `idf` into never reaching zero.
    /// Zero for a term on every document is the property the whole index rests
    /// on — it is what retires the dead `cross-cutting` filename prefix across
    /// 425 of 425 documents with no stopword list — and a floor would resurrect
    /// it. The fallback fires only where ranking is impossible anyway.
    pub fn term_weight(&self, term: &str) -> f64 {
        if self.documents.len() > 1 {
            return self.idf(term);
        }
        if self.document_frequency.contains_key(term) {
            1.0
        } else {
            0.0
        }
    }

    /// Terms present in every indexed document, and therefore contributing
    /// nothing. Reported by `--explain` so a wrong selection is diagnosable.
    pub fn ubiquitous_terms(&self) -> Vec<&str> {
        let n = self.documents.len();
        if n == 0 {
            return Vec::new();
        }
        self.document_frequency
            .iter()
            .filter(|(_, df)| **df == n)
            .map(|(term, _)| term.as_str())
            .collect()
    }

    /// Rank every document against `query`, best first, keeping at most `limit`
    /// with a non-zero score.
    pub fn search(&self, query: &Query, limit: usize) -> Vec<Match> {
        self.search_weighted(query, limit, &Weights::default())
    }

    /// [`Self::search`] with explicit weights, for ablation measurements.
    pub fn search_weighted(&self, query: &Query, limit: usize, weights: &Weights) -> Vec<Match> {
        let query_terms: Vec<String> = dedup(signals::terms(&query.text));
        let query_paths = query.all_paths();

        // Normalizing by the query's own total IDF makes coverage a 0..1
        // fraction, so scores are comparable across queries of different
        // lengths and a weight means the same thing everywhere.
        let idf: HashMap<&str, f64> = query_terms
            .iter()
            .map(|t| (t.as_str(), self.term_weight(t)))
            .collect();
        // Summed over the sorted term list, never over the map's values.
        // `HashMap` seeds each instance from a thread-local counter, so its
        // iteration order differs between two maps holding identical keys in
        // one process; floating-point addition is not associative, so summing
        // in that order makes scores differ in their last bits from one call to
        // the next. Determinism is an acceptance criterion here, not a nicety.
        let total_idf: f64 = query_terms
            .iter()
            .map(|term| idf.get(term.as_str()).copied().unwrap_or(0.0))
            .sum();

        let mut matches: Vec<Match> = self
            .documents
            .iter()
            .filter_map(|doc| {
                self.score_document(doc, &query_terms, &idf, total_idf, &query_paths, weights)
            })
            .collect();

        matches.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.slug.cmp(&b.slug))
        });
        matches.truncate(limit);
        matches
    }

    fn score_document(
        &self,
        doc: &IndexedDocument,
        query_terms: &[String],
        idf: &HashMap<&str, f64>,
        total_idf: f64,
        query_paths: &[String],
        weights: &Weights,
    ) -> Option<Match> {
        let mut contributions: Vec<FieldContribution> = Vec::new();
        let mut score = 0.0;
        // Computed once and reused: it is the only field whose evidence is also
        // reported on the match itself.
        let (path_coverage_value, matched_globs) = path_coverage(doc, query_paths);

        for field in Field::ALL {
            let weight = weights.get(*field);
            if weight == 0.0 {
                continue;
            }
            let (coverage, matched) = if *field == Field::Path {
                (path_coverage_value, Vec::new())
            } else {
                term_coverage(doc.terms(*field), query_terms, idf, total_idf)
            };
            if coverage <= 0.0 {
                continue;
            }
            let weighted = coverage * weight;
            score += weighted;
            contributions.push(FieldContribution {
                field: *field,
                coverage,
                weighted,
                matched,
            });
        }

        if score <= 0.0 {
            return None;
        }
        contributions.sort_by(|a, b| {
            b.weighted
                .partial_cmp(&a.weighted)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.field.cmp(&b.field))
        });

        Some(Match {
            slug: doc.slug.clone(),
            relative_path: doc.relative_path.clone(),
            title: doc.title.clone(),
            score,
            contributions,
            matched_globs,
        })
    }
}

/// How much of the query's weighted term mass this field covers, and which
/// terms it covered.
///
/// Each matched term contributes `idf(t) * tf/(tf + TF_SATURATION)`. The IDF
/// factor asks how unusual the term is in this corpus; the term-frequency
/// factor asks how much this document is actually *about* it. Both are needed:
/// IDF alone ranks a document that says `value` once level with one that says
/// `token` four times, which is the failure this factor exists to prevent. The
/// factor saturates rather than growing linearly, so a document cannot buy rank
/// by repetition.
fn term_coverage(
    field_terms: Option<&BTreeMap<String, u32>>,
    query_terms: &[String],
    idf: &HashMap<&str, f64>,
    total_idf: f64,
) -> (f64, Vec<String>) {
    let Some(field_terms) = field_terms else {
        return (0.0, Vec::new());
    };
    if total_idf <= 0.0 || field_terms.is_empty() {
        return (0.0, Vec::new());
    }
    let mut matched: Vec<(String, f64)> = Vec::new();
    let mut hit_mass = 0.0;
    for term in query_terms {
        let Some(frequency) = field_terms.get(term.as_str()) else {
            continue;
        };
        let weight = *idf.get(term.as_str()).unwrap_or(&0.0);
        if weight <= 0.0 {
            // A term in every document. Recorded nowhere, because counting it
            // would let a ubiquitous word inflate coverage.
            continue;
        }
        let frequency = *frequency as f64;
        let contribution = weight * (frequency / (frequency + TF_SATURATION));
        hit_mass += contribution;
        matched.push((term.clone(), contribution));
    }
    matched.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    (
        (hit_mass / total_idf).min(1.0),
        matched.into_iter().map(|(term, _)| term).collect(),
    )
}

/// Path agreement between a document's globs and the paths a query names.
///
/// Returns a 0..1 coverage and the glob matches behind it. Coverage is the best
/// single agreement rather than an average: a plan that touches one governed
/// directory and four ungoverned ones is still governed.
fn path_coverage(doc: &IndexedDocument, query_paths: &[String]) -> (f64, Vec<GlobMatch>) {
    if query_paths.is_empty() {
        return (0.0, Vec::new());
    }
    let mut best = 0.0f64;
    let mut matches: Vec<GlobMatch> = Vec::new();
    for glob in doc.path_globs() {
        let literal: Vec<&str> = glob.literal_segments();
        let compiled = glob::Pattern::new(&glob.pattern).ok();
        for query_path in query_paths {
            let query_segments: Vec<&str> = query_path
                .split('/')
                .filter(|s| !s.is_empty() && *s != ".")
                .collect();
            let agreeing = literal
                .iter()
                .zip(query_segments.iter())
                .take_while(|(a, b)| a == b)
                .count();
            let contained =
                agreeing > 0 && (agreeing == literal.len() || agreeing == query_segments.len());
            let exact = compiled
                .as_ref()
                .is_some_and(|pattern| pattern.matches(query_path));
            if !contained && !exact {
                continue;
            }
            // Both kinds of agreement are scored by *depth*, never by the mere
            // fact of matching. A `**` glob matches anything below it, so
            // `infra/terraform/**` matches a Lambda file just as outright as
            // `infra/terraform/lambda/**` does; only depth separates the rule
            // that governs the exact directory from the one that governs the
            // whole tree. Partial agreement is discounted on top, because a
            // plan naming an ancestor of the rule's directory may never touch
            // that subtree at all.
            let segments = if exact {
                agreeing.max(literal.len())
            } else {
                agreeing
            };
            let depth = (segments as f64 / PATH_SATURATION_SEGMENTS as f64).min(1.0);
            let coverage = if exact {
                depth
            } else {
                depth * CONTAINMENT_DISCOUNT
            };
            best = best.max(coverage);
            matches.push(GlobMatch {
                glob: glob.pattern.clone(),
                query_path: query_path.clone(),
                segments,
                exact,
            });
        }
    }
    matches.sort_by(|a, b| {
        b.segments
            .cmp(&a.segments)
            .then_with(|| b.exact.cmp(&a.exact))
            .then_with(|| a.glob.cmp(&b.glob))
    });
    matches.dedup();
    (best, matches)
}

fn index_document(doc: &RuleDocument, root: &std::path::Path) -> IndexedDocument {
    let extracted: DocumentSignals = signals::extract(doc);
    let mut field_terms: BTreeMap<Field, BTreeMap<String, u32>> = BTreeMap::new();
    field_terms.insert(Field::Scope, tally(extracted.scope_terms));
    field_terms.insert(Field::PathTerms, tally(extracted.path_terms));
    field_terms.insert(Field::Title, tally(extracted.title_terms));
    field_terms.insert(Field::Slug, tally(extracted.slug_terms));

    IndexedDocument {
        slug: doc.slug().unwrap_or("<unnamed>").to_string(),
        relative_path: doc
            .source_path
            .strip_prefix(root)
            .unwrap_or(&doc.source_path)
            .display()
            .to_string(),
        title: doc.title.clone(),
        scope: doc.scope.clone(),
        globs: extracted.globs.into_iter().map(|g| g.pattern).collect(),
        field_terms,
    }
}

/// Count occurrences into a sorted map — so a stored index is byte-stable for a
/// given rule set.
fn tally(terms: Vec<String>) -> BTreeMap<String, u32> {
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for term in terms {
        *counts.entry(term).or_insert(0) += 1;
    }
    counts
}

/// Deduplicate while preserving nothing but membership, for query terms.
fn dedup(terms: Vec<String>) -> Vec<String> {
    let set: BTreeSet<String> = terms.into_iter().collect();
    set.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A field absent from a document's term map scores zero rather than
    /// panicking. An index deserialized from an older cache entry can be
    /// missing a field the current build knows about.
    #[test]
    fn test_term_coverage_of_an_absent_field_is_zero() {
        let idf: HashMap<&str, f64> = HashMap::from([("token", 1.0)]);
        let (coverage, matched) = term_coverage(None, &["token".to_string()], &idf, 1.0);
        assert_eq!(coverage, 0.0);
        assert!(matched.is_empty());
    }

    use std::path::{Path, PathBuf};

    use crate::rules::{parse_rule_document, RuleSetLoadReport};

    /// Helper: a rule document with the given title, scope sentence and verify
    /// body, named `<slug>.md` under a fixed root.
    fn doc(slug: &str, title: &str, scope: &str, verify: &str) -> RuleDocument {
        let text = format!(
            "# {title}\n\n{scope}\n\n### Rules\n\n- **R-A-001** MUST: a rule.\n\n### Verify\n\n```bash\n{verify}\n```\n"
        );
        parse_rule_document(
            &PathBuf::from(format!("/repo/.actual/rules/{slug}.md")),
            &text,
        )
        .expect("fixture parses")
    }

    /// Helper: an index over the given documents, rooted at `/repo`.
    fn index_of(documents: Vec<RuleDocument>) -> ScopeIndex {
        let report = RuleSetLoadReport {
            rules_dir: PathBuf::from("/repo/.actual/rules"),
            documents,
            errors: Vec::new(),
            digest: String::new(),
        };
        ScopeIndex::build(&report, Path::new("/repo"), "fp".to_string())
    }

    /// Helper: a three-document corpus with one clear OAuth cluster, one
    /// Terraform document, and a shared `cross-cutting` prefix on every name.
    fn sample_index() -> ScopeIndex {
        index_of(vec![
            doc(
                "cross-cutting-token-signing-e410",
                "Adopt RS256: Token Signing",
                "These rules are ALWAYS ACTIVE for OAuth token issuance, token signing and token verification.",
                "grep -r \"jwt.sign\" services/auth/oauth/ --include=\"*.ts\"",
            ),
            doc(
                "cross-cutting-token-expiry-a1b2",
                "Adopt RS256: Token Expiry",
                "These rules are ALWAYS ACTIVE for OAuth token lifetime configuration.",
                "grep -r \"expiresIn\" services/auth/oauth/",
            ),
            doc(
                "cross-cutting-provider-pinning-c3d4",
                "Pin Terraform Providers",
                "These rules are ALWAYS ACTIVE for Terraform configuration in infra/terraform/.",
                "find infra/terraform -name '*.tf'",
            ),
        ])
    }

    /// Helper: the slugs of a search, in rank order.
    fn slugs(matches: &[Match]) -> Vec<&str> {
        matches.iter().map(|m| m.slug.as_str()).collect()
    }

    // ── building ─────────────────────────────────────────────────────────

    #[test]
    fn test_build_indexes_every_document_sorted_by_slug() {
        let index = sample_index();
        assert_eq!(index.len(), 3);
        assert!(!index.is_empty());
        assert_eq!(index.format_version, INDEX_FORMAT_VERSION);
        assert_eq!(index.content_digest, "fp");
        assert_eq!(
            index
                .documents
                .iter()
                .map(|d| d.slug.as_str())
                .collect::<Vec<_>>(),
            vec![
                "cross-cutting-provider-pinning-c3d4",
                "cross-cutting-token-expiry-a1b2",
                "cross-cutting-token-signing-e410",
            ]
        );
    }

    #[test]
    fn test_build_stores_paths_relative_to_the_scanned_root() {
        let index = sample_index();
        assert!(index
            .documents
            .iter()
            .all(|doc| doc.relative_path.starts_with(".actual/rules/")));
    }

    #[test]
    fn test_build_extracts_globs_and_extensions_onto_the_document() {
        let index = sample_index();
        let signing = index
            .documents
            .iter()
            .find(|d| d.slug.ends_with("e410"))
            .unwrap();
        assert_eq!(signing.globs, vec!["services/auth/oauth/**"]);
    }

    #[test]
    fn test_build_on_an_empty_rule_set() {
        let index = index_of(Vec::new());
        assert!(index.is_empty());
        assert_eq!(index.idf("anything"), 0.0);
        assert!(index.ubiquitous_terms().is_empty());
        assert!(index.search(&Query::new("a plan"), 5).is_empty());
    }

    // ── inverse document frequency ───────────────────────────────────────

    /// The whole answer to the dead filename prefix: a term on every document
    /// scores exactly zero, with no stopword entry naming it.
    #[test]
    fn test_idf_is_zero_for_a_term_in_every_document() {
        let index = sample_index();
        assert_eq!(index.idf("cross"), 0.0);
        assert_eq!(index.idf("cutting"), 0.0);
        assert!(index.idf("token") > 0.0);
    }

    #[test]
    fn test_idf_is_zero_for_an_absent_term() {
        assert_eq!(sample_index().idf("kubernetes"), 0.0);
    }

    #[test]
    fn test_idf_rises_as_a_term_gets_rarer() {
        let index = sample_index();
        // `terraform` is on one document, `token` on two.
        assert!(index.idf("terraform") > index.idf("token"));
    }

    // ── term_weight: IDF, and what replaces it when IDF cannot speak ────

    /// Above one document `term_weight` is inverse document frequency and
    /// nothing else, so none of the measured behaviour changes.
    #[test]
    fn test_term_weight_is_idf_on_a_corpus_that_can_discriminate() {
        let index = sample_index();
        for term in ["token", "terraform", "cross", "kubernetes"] {
            assert_eq!(index.term_weight(term), index.idf(term), "{term}");
        }
    }

    /// A one-document corpus scores `ln(1/1) == 0` for every term it holds, so
    /// IDF alone makes a freshly synced repository unsearchable. Each known
    /// term counts the same instead.
    #[test]
    fn test_term_weight_falls_back_to_uniform_on_a_single_document() {
        let index = index_of(vec![doc(
            "cross-cutting-token-signing-e410",
            "Adopt RS256: Token Signing",
            "These rules are ALWAYS ACTIVE for OAuth token signing.",
            "grep -r \"jwt.sign\" services/auth/oauth/",
        )]);
        assert_eq!(index.idf("token"), 0.0, "IDF is structurally zero here");
        assert_eq!(index.term_weight("token"), 1.0);
        // A term the corpus has never seen is still worth nothing, so an
        // unrelated plan cannot match by default.
        assert_eq!(index.term_weight("kubernetes"), 0.0);
    }

    /// The behaviour the fallback exists for: one rule, a plan that plainly
    /// matches it, and no path named. Before the fallback this returned
    /// nothing.
    #[test]
    fn test_a_single_document_corpus_is_searchable() {
        let index = index_of(vec![doc(
            "cross-cutting-token-signing-e410",
            "Adopt RS256: Token Signing",
            "These rules are ALWAYS ACTIVE for OAuth token signing.",
            "grep -r \"jwt.sign\" services/auth/oauth/",
        )]);

        let hits = index.search(&Query::new("rotate the OAuth token signing keypair"), 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].slug, "cross-cutting-token-signing-e410");
        assert!(hits[0].score > 0.0);

        // An unrelated plan still selects nothing, so the fallback bought
        // recall without giving up the ability to say "no rule applies".
        assert!(index
            .search(&Query::new("kubernetes ingress controller"), 5)
            .is_empty());
    }

    /// An empty corpus has no terms, so nothing is worth anything and the
    /// single-document branch cannot divide by a corpus that is not there.
    #[test]
    fn test_term_weight_on_an_empty_corpus() {
        let index = index_of(Vec::new());
        assert_eq!(index.term_weight("token"), 0.0);
        assert!(index.search(&Query::new("token"), 5).is_empty());
    }

    /// The dead-prefix property is the reason IDF was not simply smoothed, so
    /// it is asserted next to the fallback that could have broken it.
    #[test]
    fn test_the_fallback_does_not_resurrect_a_ubiquitous_term() {
        let index = sample_index();
        assert_eq!(index.term_weight("cross"), 0.0);
        assert_eq!(index.term_weight("cutting"), 0.0);
    }

    /// Extensions are extracted so they never become path globs, and then
    /// dropped because nothing scores them. The diversion is the point, so it
    /// is asserted here now that no field records it.
    #[test]
    fn test_an_extension_filter_never_becomes_a_path_glob() {
        let index = index_of(vec![doc(
            "cross-cutting-token-signing-e410",
            "Adopt RS256: Token Signing",
            "These rules are ALWAYS ACTIVE for OAuth token signing.",
            "grep -r \"jwt.sign\" services/auth/oauth/ --include=\"*.ts\"",
        )]);
        let globs = &index.documents[0].globs;
        assert!(
            globs.iter().all(|g| !g.contains("*.ts")),
            "an extension filter leaked into the path globs: {globs:?}"
        );
        assert!(globs.iter().any(|g| g.starts_with("services/auth/oauth")));
    }

    #[test]
    fn test_ubiquitous_terms_lists_the_dead_prefix() {
        let index = sample_index();
        let ubiquitous = index.ubiquitous_terms();
        assert!(ubiquitous.contains(&"cross"));
        assert!(ubiquitous.contains(&"cutting"));
        assert!(!ubiquitous.contains(&"token"));
    }

    // ── searching ────────────────────────────────────────────────────────

    #[test]
    fn test_search_ranks_the_matching_cluster_first() {
        let index = sample_index();
        let hits = index.search(&Query::new("rotate the OAuth signing key"), 3);
        assert_eq!(hits[0].slug, "cross-cutting-token-signing-e410");
        assert!(hits[0].score > 0.0);
    }

    #[test]
    fn test_search_excludes_documents_that_match_nothing() {
        let index = sample_index();
        let hits = index.search(&Query::new("terraform provider version"), 10);
        assert_eq!(slugs(&hits), vec!["cross-cutting-provider-pinning-c3d4"]);
    }

    #[test]
    fn test_search_respects_the_limit() {
        let index = sample_index();
        assert_eq!(index.search(&Query::new("token"), 1).len(), 1);
        assert_eq!(index.search(&Query::new("token"), 0).len(), 0);
    }

    /// A query made only of terms every document carries must select nothing,
    /// rather than returning the whole corpus in filename order.
    #[test]
    fn test_search_on_ubiquitous_terms_alone_selects_nothing() {
        let index = sample_index();
        assert!(index
            .search(&Query::new("cross cutting rules"), 10)
            .is_empty());
    }

    #[test]
    fn test_search_is_deterministic_and_breaks_ties_on_slug() {
        let index = sample_index();
        let query = Query::new("token");
        let first = index.search(&query, 10);
        for _ in 0..5 {
            assert_eq!(index.search(&query, 10), first);
        }
        // Equal-scoring documents come back in slug order.
        let scores: Vec<f64> = first.iter().map(|m| m.score).collect();
        for pair in scores.windows(2) {
            assert!(pair[0] >= pair[1], "not sorted by descending score");
        }
    }

    /// Term frequency, not just rarity: a document that says `token` three
    /// times outranks one that says it once, all else equal.
    #[test]
    fn test_search_prefers_the_document_that_says_the_term_more_often() {
        let index = index_of(vec![
            doc(
                "a-often-1111",
                "Tokens",
                "These rules are ALWAYS ACTIVE for token issuance, token signing and token expiry.",
                "echo x",
            ),
            doc(
                "b-once-2222",
                "Tokens",
                "These rules are ALWAYS ACTIVE for token issuance.",
                "echo x",
            ),
            // A third document that never says `token`, so the term is rare
            // rather than ubiquitous and carries a non-zero weight.
            doc(
                "c-other-3333",
                "Terraform",
                "These rules are ALWAYS ACTIVE for Terraform configuration.",
                "echo x",
            ),
        ]);
        let hits = index.search(&Query::new("token"), 3);
        assert_eq!(hits[0].slug, "a-often-1111");
        assert!(hits[0].score > hits[1].score);
    }

    // ── explanation ──────────────────────────────────────────────────────

    #[test]
    fn test_match_reports_contributions_strongest_first() {
        let index = sample_index();
        let hits = index.search(&Query::new("OAuth token signing"), 1);
        let contributions = &hits[0].contributions;
        assert!(!contributions.is_empty());
        for pair in contributions.windows(2) {
            assert!(pair[0].weighted >= pair[1].weighted);
        }
        let scope = contributions
            .iter()
            .find(|c| c.field == Field::Scope)
            .unwrap();
        assert!(scope.matched.contains(&"token".to_string()));
        assert!(scope.coverage > 0.0 && scope.coverage <= 1.0);
    }

    /// A ubiquitous term is never listed as evidence, because counting it would
    /// let a word that discriminates nothing appear to justify a rank.
    #[test]
    fn test_match_never_credits_a_ubiquitous_term() {
        let index = sample_index();
        let hits = index.search(&Query::new("cross cutting token"), 3);
        for hit in &hits {
            for contribution in &hit.contributions {
                assert!(!contribution.matched.contains(&"cross".to_string()));
            }
        }
    }

    // ── path matching ────────────────────────────────────────────────────

    #[test]
    fn test_search_scores_a_path_the_query_names() {
        let index = sample_index();
        let query = Query::new("change signing")
            .with_paths(vec!["services/auth/oauth/token.ts".to_string()]);
        let hits = index.search(&query, 3);
        assert_eq!(hits[0].slug, "cross-cutting-token-signing-e410");
        let glob = &hits[0].matched_globs[0];
        assert_eq!(glob.glob, "services/auth/oauth/**");
        assert!(glob.exact);
    }

    /// Both directions matter: at plan time the author may be broader or
    /// narrower than the rule file.
    #[test]
    fn test_path_coverage_matches_an_ancestor_and_a_descendant() {
        let index = sample_index();
        let signing = index
            .documents
            .iter()
            .find(|d| d.slug.ends_with("e410"))
            .unwrap();

        // Descendant of the rule's directory: an outright glob match, scored
        // at the glob's own depth of three segments.
        let (deep, deep_globs) =
            path_coverage(signing, &["services/auth/oauth/jwks/keys.ts".to_string()]);
        assert!(deep_globs[0].exact);
        assert_eq!(deep_globs[0].segments, 3);
        assert_eq!(deep, 3.0 / PATH_SATURATION_SEGMENTS as f64);

        // Ancestor of it: partial agreement, discounted.
        let (shallow, shallow_globs) = path_coverage(signing, &["services/auth".to_string()]);
        assert!(!shallow_globs[0].exact);
        assert!(shallow > 0.0 && shallow < deep);
    }

    #[test]
    fn test_path_coverage_is_zero_without_query_paths_or_agreement() {
        let index = sample_index();
        let signing = index
            .documents
            .iter()
            .find(|d| d.slug.ends_with("e410"))
            .unwrap();
        assert_eq!(path_coverage(signing, &[]).0, 0.0);
        assert_eq!(
            path_coverage(signing, &["web/app/page.tsx".to_string()]).0,
            0.0
        );
    }

    /// An exact glob match is complete evidence and beats deeper-but-partial
    /// agreement, which is what keeps a rule scoped to the exact directory
    /// above one scoped to the whole tree.
    #[test]
    fn test_exact_match_outscores_containment() {
        let index = index_of(vec![
            doc(
                "narrow-1111",
                "Narrow",
                "Scope.",
                "grep -r x infra/terraform/lambda/",
            ),
            doc(
                "broad-2222",
                "Broad",
                "Scope.",
                "grep -r x infra/terraform/",
            ),
        ]);
        let query = Query::new("a change")
            .with_paths(vec!["infra/terraform/lambda/reports.tf".to_string()]);
        let hits = index.search(&query, 2);
        assert_eq!(hits[0].slug, "narrow-1111");
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn test_query_reads_paths_out_of_the_plan_prose() {
        let query = Query::new("update services/auth/oauth/token.ts to rotate keys");
        assert_eq!(query.all_paths(), vec!["services/auth/oauth/token.ts"]);
    }

    #[test]
    fn test_query_merges_explicit_paths_and_prose_paths_without_duplicates() {
        let query = Query::new("touch backend/db/ again")
            .with_paths(vec!["backend/db/".to_string(), "web/app".to_string()]);
        assert_eq!(query.all_paths(), vec!["backend/db", "web/app"]);
    }

    #[test]
    fn test_query_without_paths_is_empty() {
        assert!(Query::new("no paths here").all_paths().is_empty());
        assert_eq!(Query::default().text, "");
    }

    // ── weights ──────────────────────────────────────────────────────────

    #[test]
    fn test_default_weights_rank_evidence_above_prose_above_filename() {
        let weights = Weights::default();
        assert!(weights.get(Field::Path) > weights.get(Field::Scope));
        assert!(weights.get(Field::Scope) > weights.get(Field::Title));
        assert!(weights.get(Field::Title) > weights.get(Field::Slug));
    }

    #[test]
    fn test_without_zeroes_exactly_one_field() {
        for field in Field::ALL {
            let weights = Weights::default().without(*field);
            assert_eq!(weights.get(*field), 0.0, "{} not zeroed", field.as_str());
            for other in Field::ALL.iter().filter(|f| *f != field) {
                assert_eq!(weights.get(*other), Weights::default().get(*other));
            }
        }
    }

    #[test]
    fn test_search_weighted_drops_a_disabled_field_from_the_explanation() {
        let index = sample_index();
        let query = Query::new("OAuth token signing");
        let hits = index.search_weighted(&query, 3, &Weights::default().without(Field::Scope));
        assert!(hits
            .iter()
            .all(|hit| hit.contributions.iter().all(|c| c.field != Field::Scope)));
    }

    #[test]
    fn test_search_with_every_field_disabled_selects_nothing() {
        let index = sample_index();
        let mut weights = Weights::default();
        for field in Field::ALL {
            weights = weights.without(*field);
        }
        assert!(index
            .search_weighted(&Query::new("token"), 5, &weights)
            .is_empty());
    }

    #[test]
    fn test_field_names_are_stable() {
        let names: Vec<&str> = Field::ALL.iter().map(|f| f.as_str()).collect();
        assert_eq!(names, vec!["path", "scope", "path-terms", "title", "slug"]);
        assert_eq!(
            serde_json::to_string(&Field::PathTerms).unwrap(),
            "\"path_terms\""
        );
    }

    // ── serialization ────────────────────────────────────────────────────

    /// The index is cached as JSON, so a round trip must be lossless — a lossy
    /// one would silently degrade every cache hit.
    #[test]
    fn test_index_serde_round_trip_is_lossless() {
        let index = sample_index();
        let json = serde_json::to_string(&index).unwrap();
        let back: ScopeIndex = serde_json::from_str(&json).unwrap();
        assert_eq!(back, index);
        assert_eq!(
            back.search(&Query::new("token signing"), 5),
            index.search(&Query::new("token signing"), 5)
        );
    }

    /// Two builds of the same rule set must serialize byte-identically, or the
    /// cache churns for no reason.
    #[test]
    fn test_index_serialization_is_byte_stable() {
        let a = serde_json::to_string(&sample_index()).unwrap();
        let b = serde_json::to_string(&sample_index()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn test_match_is_serializable() {
        let index = sample_index();
        let hits = index.search(&Query::new("OAuth token"), 1);
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&hits).unwrap()).unwrap();
        assert!(value[0]["slug"].is_string());
        assert!(value[0]["score"].is_number());
        assert!(value[0]["contributions"][0]["field"].is_string());
    }
}
