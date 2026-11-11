//! Which existing decisions a new one likely duplicates: a port of the
//! word-overlap check Actual AI runs on a drafted decision.
//!
//! No model call. Each title or policy statement becomes its set of content
//! words (lowercased, stopwords and RFC 2119 keywords dropped, a plural `s`
//! trimmed), and two texts score the Dice similarity of their sets,
//! `2|A∩B| / (|A| + |B|)`. An existing decision overlaps the new one when its
//! title scores at least 0.5 against the new title, or any of its policies
//! scores at least 0.5 against any of the new policies. Titles are never
//! compared with policies.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use serde::Serialize;

/// Score at or above which two titles or two policies overlap.
const OVERLAP_THRESHOLD: f64 = 0.5;
/// Two texts sharing a single word are not an overlap, unless one of them has only that word.
const MIN_SHARED_WORDS: usize = 2;
/// Most overlapping decisions reported.
const MAX_OVERLAPS: usize = 5;
/// Most policy matches reported per decision.
const MAX_MATCHES_PER_DECISION: usize = 3;
/// Words this long or shorter are not content words.
const MAX_SKIPPED_WORD_LEN: usize = 2;

/// RFC 2119 keywords carry a policy's level, not its subject; the rest are
/// function words and verbs too common in policy statements to tell them apart.
const STOPWORDS: [&str; 63] = [
    "must",
    "shall",
    "should",
    "may",
    "not",
    "required",
    "recommended",
    "optional",
    "the",
    "and",
    "for",
    "with",
    "that",
    "this",
    "these",
    "those",
    "are",
    "was",
    "were",
    "been",
    "being",
    "has",
    "have",
    "had",
    "its",
    "into",
    "onto",
    "from",
    "than",
    "then",
    "when",
    "where",
    "which",
    "who",
    "whom",
    "what",
    "how",
    "all",
    "any",
    "each",
    "every",
    "per",
    "via",
    "our",
    "their",
    "they",
    "them",
    "can",
    "will",
    "would",
    "also",
    "only",
    "other",
    "such",
    "more",
    "most",
    "use",
    "uses",
    "used",
    "using",
    "new",
    "out",
    "off",
];

/// A decision to compare against: its id, title and policy statements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingDecision {
    /// The decision id.
    pub id: String,
    /// The decision title.
    pub title: String,
    /// The decision's policy statements.
    pub policies: Vec<String>,
}

/// One likely duplicate, mirroring the server check's result fields (id, title, score, and what matched).
///
/// Scores are rounded to three decimals the way Python's `round` does.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Overlap {
    /// The existing decision's id.
    pub adr_id: String,
    /// The existing decision's title.
    pub title: String,
    /// The higher of `title_score` and the best policy match.
    pub score: f64,
    /// Word-overlap similarity of the two titles.
    pub title_score: f64,
    /// Policy pairs at or above the threshold, closest first, at most three.
    pub matching_policies: Vec<PolicyMatch>,
}

/// A new policy and an existing decision's policy that say much the same thing.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PolicyMatch {
    /// Position of the new policy in the `policies` passed in, from 0.
    pub draft_policy_index: usize,
    /// The new policy statement.
    pub draft_policy: String,
    /// The existing decision's policy statement.
    pub existing_policy: String,
    /// Word-overlap similarity of the two statements.
    pub score: f64,
}

/// The likely duplicates of a decision with `title` and `policies` among `existing`, best first, at most five.
///
/// Decisions are ordered by score (highest first), then title, then id;
/// each decision's matches by score, then new-policy position, then the
/// existing statement.
pub fn find_overlaps(
    title: &str,
    policies: &[String],
    existing: &[ExistingDecision],
) -> Vec<Overlap> {
    let draft_title = content_words(title);
    let draft_policies: Vec<BTreeSet<String>> = policies
        .iter()
        .map(|policy| content_words(policy))
        .collect();

    let mut overlaps = Vec::new();
    for decision in existing {
        let title_score = similarity(&draft_title, &content_words(&decision.title));
        let mut matches = Vec::new();
        for policy in &decision.policies {
            let policy_words = content_words(policy);
            for (index, (statement, words)) in policies.iter().zip(&draft_policies).enumerate() {
                let score = similarity(words, &policy_words);
                if score >= OVERLAP_THRESHOLD {
                    matches.push(PolicyMatch {
                        draft_policy_index: index,
                        draft_policy: statement.clone(),
                        existing_policy: policy.clone(),
                        score: round3(score),
                    });
                }
            }
        }
        if title_score < OVERLAP_THRESHOLD && matches.is_empty() {
            continue;
        }
        matches.sort_by(|a, b| {
            descending(a.score, b.score)
                .then(a.draft_policy_index.cmp(&b.draft_policy_index))
                .then_with(|| a.existing_policy.cmp(&b.existing_policy))
        });
        let best = matches
            .iter()
            .fold(title_score, |best, matched| best.max(matched.score));
        matches.truncate(MAX_MATCHES_PER_DECISION);
        overlaps.push(Overlap {
            adr_id: decision.id.clone(),
            title: decision.title.clone(),
            score: round3(best),
            title_score: round3(title_score),
            matching_policies: matches,
        });
    }

    overlaps.sort_by(|a, b| {
        descending(a.score, b.score)
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.adr_id.cmp(&b.adr_id))
    });
    overlaps.truncate(MAX_OVERLAPS);
    overlaps
}

/// The text's content words, normalised for comparison.
fn content_words(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .filter(|word| word.len() > MAX_SKIPPED_WORD_LEN && !STOPWORDS.contains(word))
        .map(normalize)
        .collect()
}

/// A plural trimmed: `-ies` to `-y` past four letters, else a final `s` (not
/// `ss`) past three.
fn normalize(word: &str) -> String {
    if word.len() > 4 {
        if let Some(stem) = word.strip_suffix("ies") {
            return format!("{stem}y");
        }
    }
    if word.len() > 3 && !word.ends_with("ss") {
        if let Some(stem) = word.strip_suffix('s') {
            return stem.to_string();
        }
    }
    word.to_string()
}

/// Dice similarity of two word sets; 0.0 when they share too few words.
fn similarity(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let shared = left.intersection(right).count();
    if shared < MIN_SHARED_WORDS.min(left.len()).min(right.len()) {
        return 0.0;
    }
    (2 * shared) as f64 / (left.len() + right.len()) as f64
}

/// Python's `round(x, 3)`: the exact binary value rounded to three decimals,
/// ties to even. Rust's fixed-precision formatting rounds the same way.
fn round3(x: f64) -> f64 {
    format!("{x:.3}").parse().unwrap_or(x)
}

/// Order by `b` against `a`, so the higher score sorts first.
fn descending(a: f64, b: f64) -> Ordering {
    b.total_cmp(&a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn decision(id: &str, title: &str, policies: &[&str]) -> ExistingDecision {
        ExistingDecision {
            id: id.to_string(),
            title: title.to_string(),
            policies: policies.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn content_words_drop_levels_function_words_and_plural_s() {
        assert_eq!(
            content_words("Handlers MUST NOT use the user-scoped clients"),
            words(&["handler", "user", "scoped", "client"])
        );
        assert_eq!(content_words("Policies"), words(&["policy"]));
        assert_eq!(content_words("Access"), words(&["access"]));
    }

    #[test]
    fn content_words_edges() {
        // The stopword test runs before the plural trim.
        assert_eq!(content_words("uses users"), words(&["user"]));
        assert_eq!(content_words("ties abs gas"), words(&["tie", "abs", "gas"]));
        assert_eq!(content_words("go to db 42s"), words(&["42s"]));
        // Non-ASCII letters split words; the Kelvin sign lowercases to `k`.
        assert_eq!(
            content_words("Caf\u{e9} na\u{ef}ve \u{212a}eys \u{65e5}\u{672c}tokens"),
            words(&["caf", "key", "token"])
        );
        assert!(content_words("").is_empty());
    }

    #[test]
    fn similarity_is_dice_with_a_shared_word_floor() {
        assert_eq!(
            similarity(&words(&["a1x", "b2x", "c3x"]), &words(&["a1x", "b2x"])),
            0.8
        );
        // One shared word counts only when one side has nothing else.
        assert!((similarity(&words(&["a1x", "b2x"]), &words(&["a1x"])) - 2.0 / 3.0).abs() < 1e-12);
        assert_eq!(
            similarity(&words(&["a1x", "b2x"]), &words(&["a1x", "c3x"])),
            0.0
        );
        assert_eq!(similarity(&words(&[]), &words(&["a1x"])), 0.0);
        assert_eq!(similarity(&words(&["a1x"]), &words(&[])), 0.0);
    }

    #[test]
    fn find_overlaps_flags_a_near_duplicate_policy() {
        let existing = [
            decision(
                "adr-1",
                "Enforce RLS always",
                &[
                    "Handlers MUST access the database through the admin client",
                    "Tables MUST enable RLS",
                ],
            ),
            decision(
                "adr-2",
                "Frontend structure",
                &["Pages MUST NOT import server code"],
            ),
        ];
        let overlaps = find_overlaps(
            "Admin client for API handlers",
            &strings(&[
                "API handlers MUST read and write through the admin client",
                "Responses MUST be cached for one hour",
            ]),
            &existing,
        );
        assert_eq!(
            overlaps
                .iter()
                .map(|o| o.adr_id.as_str())
                .collect::<Vec<_>>(),
            ["adr-1"]
        );
        let overlap = &overlaps[0];
        assert_eq!(
            overlap
                .matching_policies
                .iter()
                .map(|m| m.draft_policy_index)
                .collect::<Vec<_>>(),
            [0]
        );
        let matched = &overlap.matching_policies[0];
        assert_eq!(
            matched.existing_policy,
            "Handlers MUST access the database through the admin client"
        );
        assert_eq!(
            matched.draft_policy,
            "API handlers MUST read and write through the admin client"
        );
        assert_eq!(overlap.score, matched.score);
        assert!(overlap.score >= 0.5);
        assert_eq!(overlap.score, 0.615);
        assert!(overlap.title_score < 0.5);
    }

    #[test]
    fn find_overlaps_flags_a_matching_title_without_policies() {
        let existing = [decision(
            "adr-1",
            "ts-rest contracts for API endpoints",
            &[],
        )];
        let overlaps = find_overlaps(
            "Use ts-rest contracts for API endpoints",
            &strings(&["Routes MUST declare a contract"]),
            &existing,
        );
        assert_eq!(overlaps.len(), 1);
        assert!(overlaps[0].matching_policies.is_empty());
        assert_eq!(overlaps[0].score, overlaps[0].title_score);
        assert!(overlaps[0].score >= 0.5);
    }

    #[test]
    fn find_overlaps_orders_closest_first_and_caps_the_list() {
        let existing: Vec<ExistingDecision> = (0..MAX_OVERLAPS + 3)
            .map(|i| {
                let policy = format!(
                    "Workers MUST retry failed jobs with backoff{}",
                    " capped".repeat(i)
                );
                decision(&format!("adr-{i}"), &format!("Decision {i}"), &[&policy])
            })
            .collect();
        let overlaps = find_overlaps(
            "Queue retries",
            &strings(&["Workers MUST retry failed jobs with exponential backoff"]),
            &existing,
        );
        assert_eq!(overlaps.len(), MAX_OVERLAPS);
        let scores: Vec<f64> = overlaps.iter().map(|o| o.score).collect();
        let mut sorted = scores.clone();
        sorted.sort_by(|a, b| descending(*a, *b));
        assert_eq!(scores, sorted);
        assert_eq!(overlaps[0].adr_id, "adr-0");
        // Equal scores fall back to the title.
        assert_eq!(
            overlaps
                .iter()
                .map(|o| o.title.as_str())
                .collect::<Vec<_>>(),
            [
                "Decision 0",
                "Decision 1",
                "Decision 2",
                "Decision 3",
                "Decision 4"
            ]
        );
        assert_eq!(overlaps[0].score, 0.909);
        assert_eq!(overlaps[1].score, 0.833);
    }

    #[test]
    fn find_overlaps_returns_nothing_for_unrelated_decisions() {
        let existing = [decision(
            "adr-1",
            "Database migrations",
            &["Migrations MUST be reversible"],
        )];
        assert!(find_overlaps(
            "Feature flags",
            &strings(&["Feature flags MUST be removed within two releases"]),
            &existing,
        )
        .is_empty());
        assert!(find_overlaps("", &[], &[]).is_empty());
    }

    #[test]
    fn matches_are_capped_and_ordered_by_score_position_and_text() {
        let existing = [
            decision(
                "adr-b",
                "Same title",
                &[
                    "cache tokens daily hourly",
                    "cache tokens daily",
                    "cache tokens",
                ],
            ),
            decision("adr-a", "Same title", &["cache tokens daily"]),
        ];
        let policies = strings(&["cache tokens daily", "Cache tokens daily."]);
        let overlaps = find_overlaps("Other words entirely", &policies, &existing);
        // Equal scores and titles fall back to the id.
        assert_eq!(
            overlaps
                .iter()
                .map(|o| o.adr_id.as_str())
                .collect::<Vec<_>>(),
            ["adr-a", "adr-b"]
        );
        let matched: Vec<(usize, &str, f64)> = overlaps[1]
            .matching_policies
            .iter()
            .map(|m| (m.draft_policy_index, m.existing_policy.as_str(), m.score))
            .collect();
        assert_eq!(
            matched,
            [
                (0, "cache tokens daily", 1.0),
                (1, "cache tokens daily", 1.0),
                (0, "cache tokens daily hourly", 0.857),
            ]
        );
        assert_eq!(overlaps[1].title_score, 0.0);
        assert_eq!(overlaps[1].score, 1.0);

        // Equal score and position fall back to the existing statement.
        let existing = [decision(
            "adr-z",
            "T",
            &["zeta cache tokens", "alpha cache tokens"],
        )];
        let overlaps = find_overlaps("x", &strings(&["cache tokens"]), &existing);
        let texts: Vec<&str> = overlaps[0]
            .matching_policies
            .iter()
            .map(|m| m.existing_policy.as_str())
            .collect();
        assert_eq!(texts, ["alpha cache tokens", "zeta cache tokens"]);
    }

    #[test]
    fn non_ascii_text_compares_its_ascii_words() {
        let existing = [decision(
            "adr-1",
            "R\u{e9}sum\u{e9} parsing pipeline \u{1F389}",
            &["Uploads MUST be scanned na\u{ef}vely before parsing"],
        )];
        let overlaps = find_overlaps(
            "\u{65e5}\u{672c} parsing pipeline",
            &strings(&["Uploaded files MUST be scanned before parsing"]),
            &existing,
        );
        assert_eq!(overlaps.len(), 1);
        assert_eq!(overlaps[0].title_score, 0.8);
        assert_eq!(overlaps[0].score, 0.8);
        assert_eq!(overlaps[0].matching_policies[0].score, 0.6);
    }

    #[test]
    fn rounding_matches_python() {
        // Python: round(0.0125, 3) == 0.013 (the double sits above the tie),
        // round(0.5625, 3) == 0.562 and round(0.1875, 3) == 0.188 (exact ties go to even).
        assert_eq!(round3(0.0125), 0.013);
        assert_eq!(round3(0.5625), 0.562);
        assert_eq!(round3(0.1875), 0.188);
        assert_eq!(round3(0.0625), 0.062);
        assert_eq!(round3(2.0 / 3.0), 0.667);
        assert_eq!(round3(8.0 / 13.0), 0.615);
        assert_eq!(round3(1.0), 1.0);
        assert_eq!(round3(0.0), 0.0);
    }

    #[test]
    fn overlap_serializes_with_the_server_field_names() {
        let overlap = Overlap {
            adr_id: "a".to_string(),
            title: "t".to_string(),
            score: 0.5,
            title_score: 0.5,
            matching_policies: vec![PolicyMatch {
                draft_policy_index: 0,
                draft_policy: "d".to_string(),
                existing_policy: "e".to_string(),
                score: 0.5,
            }],
        };
        assert_eq!(
            serde_json::to_string(&overlap).ok().as_deref(),
            Some(
                "{\"adr_id\":\"a\",\"title\":\"t\",\"score\":0.5,\"title_score\":0.5,\
                 \"matching_policies\":[{\"draft_policy_index\":0,\"draft_policy\":\"d\",\
                 \"existing_policy\":\"e\",\"score\":0.5}]}"
            )
        );
    }
}
