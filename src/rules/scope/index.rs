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
//! | `slug` | the aspect segment of the filename | the status quo's *only* signal, kept as the weakest field |
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
//! Scoring is pure and deterministic — no LLM, no network, no clock. Equal
//! scores are common, because sibling documents generated from one decision
//! share their verify paths, so ties break on specificity before falling back
//! to slug: first the rarest glob among those that reached the document's best
//! path coverage (a glob only a few documents claim says more than one many
//! share), then how few globs the document declares (a document scoped to one
//! tree is narrower than one spanning many). A given index and query always
//! produce the same order.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::rules::scope::signals::{self, DocumentSignals, PathGlob};
use crate::rules::{RuleDocument, RuleSetLoadReport};

/// Bump when the stored shape or the scoring inputs change, so a cached index
/// written by an older build is discarded rather than misread.
pub const INDEX_FORMAT_VERSION: u32 = 5;

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
    /// The decision this document is one aspect of, when its title names one.
    ///
    /// A generated rule set splits one decision across many documents whose
    /// titles share a prefix — `Adopt Pydantic Models…: Database Interactions`
    /// and `Adopt Pydantic Models…: Activity Inputs` are aspects of a single
    /// ADR. The prefix is the only machine-readable link between them the
    /// published files carry, so it is the key here, derived at build time so
    /// the parse happens once rather than per query.
    pub adr: Option<String>,
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
/// `min_score` is the floor a document must clear to be worth returning at
/// all. It rides on the query rather than on each search call so that every
/// path — `rules select`, the prefilter feeding stage 2, the grouped search,
/// and the evaluation harness — inherits it from the one place the request is
/// built, and none of them can forget to apply it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    pub text: String,
    pub paths: Vec<String>,
    /// Documents scoring below this are dropped. Zero, the default, keeps
    /// everything that scored at all.
    pub min_score: f64,
}

impl Query {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            paths: Vec::new(),
            min_score: 0.0,
        }
    }

    /// Drop documents scoring below `min_score`.
    ///
    /// Abstention is the point: a file no rule really governs should get an
    /// empty answer rather than the least bad few. Without a floor a selection
    /// always returns its cap whenever anything scored above zero, which for a
    /// hook means briefing an agent on rules that do not apply to the file it
    /// is editing.
    pub fn with_min_score(mut self, min_score: f64) -> Self {
        self.min_score = min_score;
        self
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
    /// The decision this document belongs to, when its title names one.
    pub adr: Option<String>,
    pub score: f64,
    pub contributions: Vec<FieldContribution>,
    /// Document globs that matched a path the query named, with the number of
    /// agreeing leading segments.
    pub matched_globs: Vec<GlobMatch>,
}

/// One decision a query matched, and the documents that carry it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdrGroup {
    /// What documents were grouped on: the decision name, or the slug of a
    /// document whose title names none.
    pub key: String,
    /// The decision's name, absent for a document that stands alone.
    pub title: Option<String>,
    /// The best score among this group's documents, which is what it ranks at.
    pub score: f64,
    /// Its matching documents, in document rank order.
    pub documents: Vec<Match>,
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
            // Applied here rather than inside the cap so it is a statement
            // about the match, not about the budget: a document below the
            // floor is dropped whether or not there is room for it.
            .filter(|hit| hit.score >= query.min_score)
            .collect();

        let glob_frequency = self.glob_document_frequency();
        let declared_globs: HashMap<&str, usize> = self
            .documents
            .iter()
            .map(|doc| (doc.slug.as_str(), distinct_globs(doc).len()))
            .collect();
        matches.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    rarest_matched_glob(a, &glob_frequency)
                        .cmp(&rarest_matched_glob(b, &glob_frequency))
                })
                .then_with(|| {
                    scope_breadth(a, &declared_globs).cmp(&scope_breadth(b, &declared_globs))
                })
                .then_with(|| a.slug.cmp(&b.slug))
        });
        matches.truncate(limit);
        matches
    }

    /// Glob pattern → number of documents declaring it.
    ///
    /// Computed per search rather than stored: it is a pass over a few hundred
    /// short strings, and keeping it out of the index leaves the cache format
    /// unchanged.
    fn glob_document_frequency(&self) -> HashMap<&str, usize> {
        let mut frequency: HashMap<&str, usize> = HashMap::new();
        for doc in &self.documents {
            for glob in distinct_globs(doc) {
                *frequency.entry(glob).or_insert(0) += 1;
            }
        }
        frequency
    }

    /// Rank the *decisions* a query matches, best first, keeping at most
    /// `limit` of them with every document each one matched.
    ///
    /// The unit matters as much as the ranking. A generated rule set splits one
    /// decision across many near-identical documents that share their verify
    /// paths, so a document-level top five can be five aspects of the same
    /// decision while the second relevant decision never appears. Ranking
    /// decisions and keeping their documents puts the caller's budget on
    /// distinct subjects, which is what a brief wants.
    ///
    /// Keeping *one* document per decision would be the other way to
    /// de-duplicate, and it measured worse: the documents that genuinely govern
    /// an edit are usually siblings, so dropping them loses real hits.
    ///
    /// A decision ranks where its best document ranked, and documents retain
    /// their relative rank within that decision. Making each group contiguous
    /// can change the flattened document order when decisions were interleaved.
    pub fn search_adrs(&self, query: &Query, limit: usize) -> Vec<AdrGroup> {
        self.search_adrs_weighted(query, limit, &Weights::default())
    }

    /// [`Self::search_adrs`] with explicit weights, for ablation measurements.
    pub fn search_adrs_weighted(
        &self,
        query: &Query,
        limit: usize,
        weights: &Weights,
    ) -> Vec<AdrGroup> {
        // Every scoring document, not the top `limit`: the cap counts
        // decisions here, and the documents of the second decision can sit
        // below any number of documents of the first.
        let matches = self.search_weighted(query, self.documents.len(), weights);

        group_matches(matches, limit)
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
            adr: doc.adr.clone(),
            score,
            contributions,
            matched_globs,
        })
    }
}

/// Partition ranked documents into contiguous decision groups.
///
/// Groups appear where their best document first appeared. Documents preserve
/// their relative order within a group, but flattening the groups can differ
/// from the original global document order: `A1, B1, A2` becomes
/// `A=[A1, A2], B=[B1]`.
fn group_matches(matches: Vec<Match>, limit: usize) -> Vec<AdrGroup> {
    let mut groups: Vec<AdrGroup> = Vec::new();
    for hit in matches {
        // A document whose title names no decision is its own group, keyed by
        // slug: pooling every such document under one heading would present
        // unrelated rules as one subject.
        let key = hit.adr.clone().unwrap_or_else(|| hit.slug.clone());
        match groups.iter_mut().find(|group| group.key == key) {
            Some(group) => group.documents.push(hit),
            None => groups.push(AdrGroup {
                key,
                // The first hit of a group is its best, because `matches` is
                // already in rank order.
                score: hit.score,
                title: hit.adr.clone(),
                documents: vec![hit],
            }),
        }
    }
    groups.truncate(limit);
    groups
}

/// First tie-break: how many documents declare the rarest glob that reached
/// this match's best path coverage. Fewer is more specific. A weaker agreeing
/// glob did not produce the score, so it does not count. A match with no
/// positive path evidence (it scored on words alone) has nothing to be
/// specific about and sorts last.
fn rarest_matched_glob(m: &Match, glob_frequency: &HashMap<&str, usize>) -> usize {
    let best = m
        .matched_globs
        .iter()
        .map(|g| glob_match_coverage(g.segments, g.exact))
        .fold(0.0, f64::max);
    if best <= 0.0 {
        return usize::MAX;
    }
    m.matched_globs
        .iter()
        .filter(|g| glob_match_coverage(g.segments, g.exact) == best)
        .filter_map(|g| glob_frequency.get(g.glob.as_str()).copied())
        .min()
        .unwrap_or(usize::MAX)
}

/// Distinct glob patterns a document declares. Scoring and both tie-breaks
/// count a repeated pattern once, so a hand-built fixture cannot buy breadth
/// or frequency by listing the same glob twice.
fn distinct_globs(doc: &IndexedDocument) -> BTreeSet<&str> {
    doc.globs.iter().map(String::as_str).collect()
}

/// Second tie-break: how many distinct globs the document declares. Fewer is
/// narrower. A document declaring none is unscoped, not narrow, so it sorts
/// last.
fn scope_breadth(m: &Match, declared_globs: &HashMap<&str, usize>) -> usize {
    match declared_globs.get(m.slug.as_str()).copied() {
        Some(0) | None => usize::MAX,
        Some(count) => count,
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

/// Coverage of one glob agreement. Depth saturates, and partial agreement is
/// discounted. Scoring and the rarity tie-break both use this, so a glob that
/// did not produce the score cannot win a tie.
fn glob_match_coverage(segments: usize, exact: bool) -> f64 {
    let depth = (segments as f64 / PATH_SATURATION_SEGMENTS as f64).min(1.0);
    if exact {
        depth
    } else {
        depth * CONTAINMENT_DISCOUNT
    }
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
            let coverage = glob_match_coverage(segments, exact);
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
        adr: adr_key(doc.title.as_deref()),
        scope: doc.scope.clone(),
        globs: extracted.globs.into_iter().map(|g| g.pattern).collect(),
        field_terms,
    }
}

/// The decision a document's title names, which is everything before the last
/// colon.
///
/// The aspect suffix is the last segment, so an earlier colon can remain part
/// of the decision's own name. A title with no colon names no decision — the
/// document stands alone. [`ScopeIndex::search_adrs`] groups it under its own
/// slug rather than inventing a shared key that would pool unrelated documents.
fn adr_key(title: Option<&str>) -> Option<String> {
    let (decision, _) = title?.rsplit_once(':')?;
    let decision = decision.trim();
    (!decision.is_empty()).then(|| decision.to_string())
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

    // ── the score floor ──────────────────────────────────────────────────

    /// Helper: the scores a path-only query produces against the sample
    /// corpus, so a floor can be set relative to real numbers rather than
    /// guessed.
    fn oauth_path_query() -> Query {
        Query::new("").with_paths(["services/auth/oauth/token.ts".to_string()])
    }

    /// A floor above everything returns nothing: abstention, not the least
    /// bad few.
    #[test]
    fn test_min_score_above_every_score_selects_nothing() {
        let index = sample_index();
        let top = index.search(&oauth_path_query(), 5)[0].score;

        let hits = index.search(&oauth_path_query().with_min_score(top + 0.01), 5);

        assert!(hits.is_empty());
    }

    /// The floor is inclusive, so a document scoring exactly the floor is
    /// kept. A floor set from an observed score would otherwise drop the very
    /// document it was read from.
    #[test]
    fn test_min_score_keeps_a_document_scoring_exactly_the_floor() {
        let index = sample_index();
        let top = index.search(&oauth_path_query(), 5)[0].score;

        let hits = index.search(&oauth_path_query().with_min_score(top), 5);

        assert!(!hits.is_empty());
        assert!(hits.iter().all(|hit| hit.score >= top));
    }

    /// A floor between the best and worst score keeps the better documents
    /// and drops the rest.
    #[test]
    fn test_min_score_drops_only_what_scores_below_it() {
        let index = sample_index();
        let query = Query::new("OAuth token signing and terraform providers");
        let all = index.search(&query, index.len());
        assert!(all.len() > 1, "fixture must spread scores");
        let floor = all[all.len() - 1].score + 0.001;

        let hits = index.search(&query.clone().with_min_score(floor), index.len());

        assert_eq!(hits.len(), all.len() - 1);
        assert!(hits.iter().all(|hit| hit.score >= floor));
    }

    /// Zero, the default, is not a floor: everything that scored at all is
    /// still returned.
    #[test]
    fn test_min_score_of_zero_keeps_every_scoring_document() {
        let index = sample_index();
        let query = Query::new("OAuth token signing");

        assert_eq!(
            index.search(&query, 5),
            index.search(&query.clone().with_min_score(0.0), 5)
        );
    }

    /// The floor applies before grouping too, so a decision whose documents
    /// all fall below it does not appear.
    #[test]
    fn test_min_score_applies_to_grouped_selection() {
        let index = sample_index();
        let top = index.search(&oauth_path_query(), 5)[0].score;

        assert!(!index.search_adrs(&oauth_path_query(), 5).is_empty());
        assert!(index
            .search_adrs(&oauth_path_query().with_min_score(top + 0.01), 5)
            .is_empty());
    }

    // ── decisions ────────────────────────────────────────────────────────

    /// A title's decision name is everything before the last colon; the
    /// aspect suffix after it is what distinguishes siblings.
    #[test]
    fn test_adr_key_is_the_title_prefix() {
        assert_eq!(
            adr_key(Some("Adopt RS256: Token Signing")).as_deref(),
            Some("Adopt RS256")
        );
    }

    /// A colon inside the decision title is part of its identity, not the
    /// aspect separator. Splitting at the first colon would merge distinct
    /// decisions such as `Cache: Use Redis` and `Cache: Use Memcached`.
    #[test]
    fn test_adr_key_preserves_colons_in_the_decision_title() {
        assert_eq!(
            adr_key(Some("Cache: Use Redis: Reads")).as_deref(),
            Some("Cache: Use Redis")
        );
        assert_eq!(
            adr_key(Some("Cache: Use Memcached: Reads")).as_deref(),
            Some("Cache: Use Memcached")
        );
    }

    /// A title naming no decision, or none at all, yields no key — the
    /// document stands alone rather than joining a pool of unrelated rules.
    #[test]
    fn test_adr_key_is_absent_without_a_named_decision() {
        assert_eq!(adr_key(Some("Pin Terraform Providers")), None);
        assert_eq!(adr_key(Some(":  aspect only")), None);
        assert_eq!(adr_key(None), None);
    }

    /// Building an index derives the key once per document, so a query never
    /// re-parses titles.
    #[test]
    fn test_build_derives_the_decision_key() {
        let index = sample_index();
        let keyed: Vec<(&str, Option<&str>)> = index
            .documents
            .iter()
            .map(|d| (d.slug.as_str(), d.adr.as_deref()))
            .collect();
        assert_eq!(
            keyed,
            vec![
                // `Pin Terraform Providers` names no decision: no colon.
                ("cross-cutting-provider-pinning-c3d4", None),
                ("cross-cutting-token-expiry-a1b2", Some("Adopt RS256")),
                ("cross-cutting-token-signing-e410", Some("Adopt RS256")),
            ]
        );
    }

    /// Sibling documents of one decision collapse into a single group, ranked
    /// at their best document's score, and the group keeps every one of them.
    #[test]
    fn test_search_adrs_groups_siblings_of_one_decision() {
        let index = sample_index();
        let groups = index.search_adrs(&Query::new("OAuth token signing"), 5);

        assert_eq!(groups[0].title.as_deref(), Some("Adopt RS256"));
        assert_eq!(groups[0].documents.len(), 2);
        assert_eq!(groups[0].score, groups[0].documents[0].score);
        assert!(groups[0].documents[0].score >= groups[0].documents[1].score);
    }

    /// The cap counts decisions, not documents: one decision's siblings can
    /// fill any number of document slots without crowding out the next
    /// decision, which is the whole point of grouping.
    #[test]
    fn test_search_adrs_limit_counts_decisions() {
        let index = sample_index();
        // A path both RS256 siblings glob, so one decision holds two documents.
        let query = Query::new("").with_paths(["services/auth/oauth/token.ts".to_string()]);
        let groups = index.search_adrs(&query, 1);

        assert_eq!(groups.len(), 1);
        // The one decision keeps both its siblings: the cap did not spend
        // itself on documents.
        assert_eq!(groups[0].documents.len(), 2);
    }

    /// A document whose title names no decision is its own group, keyed by
    /// slug, rather than pooled with every other unnamed document.
    #[test]
    fn test_search_adrs_gives_an_unnamed_document_its_own_group() {
        let index = index_of(vec![
            doc(
                "cross-cutting-standalone-a1b2",
                "Pin Terraform Providers",
                "These rules are ALWAYS ACTIVE for Terraform configuration in infra/terraform/.",
                "find infra/terraform -name '*.tf'",
            ),
            doc(
                "cross-cutting-other-c3d4",
                "Lock Terraform State",
                "These rules are ALWAYS ACTIVE for Terraform state in infra/terraform/.",
                "find infra/terraform -name '*.tfstate'",
            ),
        ]);
        // Matched on the path both documents glob: `terraform` is in every
        // title here, so its IDF is zero and a word query would match nothing.
        let query = Query::new("").with_paths(["infra/terraform/main.tf".to_string()]);
        let groups = index.search_adrs(&query, 5);

        assert_eq!(groups.len(), 2);
        for group in &groups {
            assert_eq!(group.title, None);
            assert_eq!(group.documents.len(), 1);
            assert_eq!(group.key, group.documents[0].slug);
        }
    }

    /// Decisions appear where their best document ranked, while later
    /// documents preserve their relative order inside that decision. Making
    /// groups contiguous deliberately changes a flattened interleaved order.
    #[test]
    fn test_group_matches_preserves_group_and_within_group_rank_order() {
        let hit = |slug: &str, adr: &str, score: f64| Match {
            slug: slug.to_string(),
            relative_path: format!(".actual/rules/{slug}.md"),
            title: Some(format!("{adr}: aspect")),
            adr: Some(adr.to_string()),
            score,
            contributions: Vec::new(),
            matched_globs: Vec::new(),
        };
        let matches = vec![
            hit("a-first", "Decision A", 3.0),
            hit("b-first", "Decision B", 2.0),
            hit("a-second", "Decision A", 1.0),
        ];

        let groups = group_matches(matches.clone(), 5);

        assert_eq!(
            groups
                .iter()
                .map(|group| group.key.as_str())
                .collect::<Vec<_>>(),
            vec!["Decision A", "Decision B"]
        );
        assert_eq!(
            groups[0]
                .documents
                .iter()
                .map(|document| document.slug.as_str())
                .collect::<Vec<_>>(),
            vec!["a-first", "a-second"]
        );
        assert_eq!(groups[0].score, 3.0);
        assert_eq!(groups[1].documents[0].slug, "b-first");
        assert_eq!(groups, group_matches(matches, 5));
    }

    /// Nothing matching means no groups, not an empty group.
    #[test]
    fn test_search_adrs_returns_nothing_when_no_document_matches() {
        let index = sample_index();
        assert!(index
            .search_adrs(&Query::new("kubernetes ingress"), 5)
            .is_empty());
    }

    // ── tie-breaking ─────────────────────────────────────────────────────

    /// Helper: a document declaring exactly `globs` and no terms, so a
    /// path-only query scores it on path agreement alone and ties are exact.
    fn globbed(slug: &str, globs: &[&str]) -> IndexedDocument {
        IndexedDocument {
            slug: slug.to_string(),
            relative_path: format!(".actual/rules/{slug}.md"),
            title: None,
            adr: None,
            scope: None,
            globs: globs.iter().map(|g| g.to_string()).collect(),
            field_terms: BTreeMap::new(),
        }
    }

    /// Helper: an index over hand-built documents.
    fn index_with(documents: Vec<IndexedDocument>) -> ScopeIndex {
        ScopeIndex {
            format_version: INDEX_FORMAT_VERSION,
            content_digest: "fp".to_string(),
            documents,
            document_frequency: BTreeMap::new(),
        }
    }

    /// Helper: a path-only query for one file under `services/api/`.
    fn api_file() -> Query {
        Query::new("").with_paths(["services/api/handlers/orders.ts".to_string()])
    }

    /// Among equal scores, a document whose matched glob few documents declare
    /// outranks one whose glob many share — even when its slug sorts last.
    #[test]
    fn test_ties_prefer_the_rarest_matched_glob() {
        let index = index_with(vec![
            globbed("a-shared-one", &["services/api/**"]),
            globbed("b-shared-two", &["services/api/**"]),
            globbed("z-rare", &["services/api/*/*.ts"]),
        ]);
        let hits = index.search(&api_file(), 3);
        assert!(
            hits.iter().all(|h| h.score == hits[0].score),
            "fixture must tie"
        );
        assert_eq!(slugs(&hits), ["z-rare", "a-shared-one", "b-shared-two"]);
    }

    /// When two globs tie for the coverage that produced the score, the rarer
    /// one still wins the tie-break.
    #[test]
    fn test_rarity_counts_every_glob_that_tied_for_best_coverage() {
        let index = index_with(vec![
            globbed("a-shared", &["services/api/**"]),
            globbed("b-shared", &["services/api/**"]),
            globbed("z-both", &["services/api/**", "services/api/*/*.ts"]),
        ]);
        let hits = index.search(&api_file(), 3);
        assert!(
            hits.iter().all(|h| h.score == hits[0].score),
            "fixture must tie"
        );
        assert_eq!(slugs(&hits)[0], "z-both");
    }

    /// A rarer glob that agrees more weakly than the glob behind the score does
    /// not win the tie. Breadth then demotes the document that declared it.
    #[test]
    fn test_rarity_ignores_a_weaker_matching_glob() {
        let index = index_with(vec![
            globbed("a-shared", &["services/api/handlers/**"]),
            globbed("b-shared", &["services/api/handlers/**"]),
            globbed(
                "z-shared-plus-parent",
                &["services/api/handlers/**", "services/**"],
            ),
        ]);
        let hits = index.search(&api_file(), 3);
        assert!(
            hits.iter().all(|h| h.score == hits[0].score),
            "fixture must tie"
        );
        assert_eq!(
            slugs(&hits),
            ["a-shared", "b-shared", "z-shared-plus-parent"]
        );
    }

    /// Among equal scores and equally rare globs, the document declaring fewer
    /// globs is narrower and ranks first.
    #[test]
    fn test_ties_prefer_the_narrower_document() {
        let index = index_with(vec![
            globbed("a-broad", &["services/api/**", "infra/**", "web/**"]),
            globbed("z-narrow", &["services/api/**"]),
        ]);
        let hits = index.search(&api_file(), 2);
        assert_eq!(hits[0].score, hits[1].score, "fixture must tie");
        assert_eq!(slugs(&hits), ["z-narrow", "a-broad"]);
    }

    /// Rarity is the first tie-break and breadth the second: a rare glob on a
    /// broad document still beats a common glob on a narrow one.
    #[test]
    fn test_glob_rarity_outranks_document_breadth() {
        let index = index_with(vec![
            globbed("a-common-narrow", &["services/api/**"]),
            globbed("b-common-narrow", &["services/api/**"]),
            globbed(
                "z-rare-broad",
                &["services/api/*/*.ts", "infra/**", "web/**"],
            ),
        ]);
        let hits = index.search(&api_file(), 3);
        assert_eq!(slugs(&hits)[0], "z-rare-broad");
    }

    /// Tie-breaks never override score: a deeper, better-agreeing glob wins
    /// regardless of how common it is.
    #[test]
    fn test_tie_breaks_never_override_a_higher_score() {
        let index = index_with(vec![
            globbed("a-deep-common", &["services/api/handlers/**"]),
            globbed("b-deep-common", &["services/api/handlers/**"]),
            globbed("z-shallow-rare", &["services/**"]),
        ]);
        let hits = index.search(&api_file(), 3);
        assert!(hits[0].score > hits[2].score);
        assert_eq!(
            slugs(&hits),
            ["a-deep-common", "b-deep-common", "z-shallow-rare"]
        );
    }

    /// When every tie-break is equal, slug still decides, so order stays
    /// deterministic.
    #[test]
    fn test_full_ties_fall_back_to_slug() {
        let index = index_with(vec![
            globbed("c-third", &["services/api/**"]),
            globbed("a-first", &["services/api/**"]),
            globbed("b-second", &["services/api/**"]),
        ]);
        let hits = index.search(&api_file(), 3);
        assert_eq!(slugs(&hits), ["a-first", "b-second", "c-third"]);
        assert_eq!(index.search(&api_file(), 3), hits);
    }

    /// Path evidence on an equal word score outranks a match that agreed with
    /// no query path, even when slug would put the words-only document first.
    /// Path is zeroed so the extra glob cannot change the score.
    #[test]
    fn test_path_evidence_outranks_a_words_only_tie() {
        let index = index_of(vec![
            doc(
                "a-words-only-aaaa",
                "Token",
                "These rules are ALWAYS ACTIVE for token handling.",
                "grep -r x web/app/",
            ),
            doc(
                "z-with-path-bbbb",
                "Token",
                "These rules are ALWAYS ACTIVE for token handling.",
                "grep -r x services/api/handlers/",
            ),
            doc(
                "decoy-cccc",
                "Pin Terraform Providers",
                "These rules are ALWAYS ACTIVE for Terraform configuration.",
                "find infra/terraform -name '*.tf'",
            ),
        ]);
        let query = Query::new("token").with_paths(["services/api/handlers/orders.ts".to_string()]);
        let hits = index.search_weighted(&query, 2, &Weights::default().without(Field::Path));
        assert_eq!(hits[0].score, hits[1].score, "fixture must tie");
        assert_eq!(slugs(&hits), ["z-with-path-bbbb", "a-words-only-aaaa"]);
    }

    /// A document that declares a glob outranks an unscoped sibling on an
    /// equal word score, even when neither matched a query path.
    #[test]
    fn test_declared_globs_outrank_an_unscoped_tie() {
        let index = index_of(vec![
            doc(
                "a-unscoped-aaaa",
                "Token",
                "These rules are ALWAYS ACTIVE for token handling.",
                "echo ok",
            ),
            doc(
                "z-scoped-bbbb",
                "Token",
                "These rules are ALWAYS ACTIVE for token handling.",
                "grep -r x services/api/",
            ),
            doc(
                "decoy-cccc",
                "Pin Terraform Providers",
                "These rules are ALWAYS ACTIVE for Terraform configuration.",
                "find infra/terraform -name '*.tf'",
            ),
        ]);
        let hits = index.search(&Query::new("token"), 2);
        assert_eq!(hits[0].score, hits[1].score, "fixture must tie");
        assert_eq!(slugs(&hits), ["z-scoped-bbbb", "a-unscoped-aaaa"]);
    }

    /// A match with no path evidence, and a document declaring no globs, have
    /// nothing to be specific about: both sort after anything that does.
    #[test]
    fn test_unscoped_matches_sort_last_on_tie_breaks() {
        let frequency: HashMap<&str, usize> = HashMap::from([("services/api/**", 2)]);
        let declared: HashMap<&str, usize> = HashMap::from([("scoped", 1), ("unscoped", 0)]);
        let with_path = Match {
            slug: "scoped".to_string(),
            relative_path: String::new(),
            title: None,
            adr: None,
            score: 1.0,
            contributions: Vec::new(),
            matched_globs: vec![GlobMatch {
                glob: "services/api/**".to_string(),
                query_path: "services/api/x.ts".to_string(),
                segments: 2,
                exact: true,
            }],
        };
        let words_only = Match {
            slug: "unscoped".to_string(),
            matched_globs: Vec::new(),
            ..with_path.clone()
        };
        assert_eq!(rarest_matched_glob(&with_path, &frequency), 2);
        assert_eq!(rarest_matched_glob(&words_only, &frequency), usize::MAX);
        assert_eq!(scope_breadth(&with_path, &declared), 1);
        assert_eq!(scope_breadth(&words_only, &declared), usize::MAX);
    }

    /// Glob frequency and breadth both count distinct patterns: a document
    /// repeating a glob counts once, so it ties a sibling that listed it once
    /// and falls through to slug.
    #[test]
    fn test_glob_document_frequency_counts_each_document_once() {
        let index = index_with(vec![
            globbed("z-duped", &["services/api/**", "services/api/**"]),
            globbed("a-once", &["services/api/**"]),
        ]);
        let frequency = index.glob_document_frequency();
        assert_eq!(frequency.get("services/api/**"), Some(&2));
        let hits = index.search(&api_file(), 2);
        assert_eq!(hits[0].score, hits[1].score, "fixture must tie");
        assert_eq!(slugs(&hits), ["a-once", "z-duped"]);
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
