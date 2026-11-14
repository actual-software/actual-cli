//! The file names Actual AI's repository sync writes for a decision.
//!
//! The sync keeps two files per decision: the full decision document under
//! `docs/adr/` and the rule file under `.actual/rules/`. Both names are computed
//! from the decision id and its stored title alone, with no model involved, so
//! the CLI can name a file it writes exactly as the next sync will and the sync
//! updates that file instead of adding a second copy beside it.
//!
//! Every name goes through one scheme: only ASCII letters, digits and single
//! hyphens; at most 191 characters, and at most 200 for the repository-relative
//! path; a name that does not fit keeps its first whole words and ends in 16 hex
//! characters of the SHA-256 of the full name.

use sha2::{Digest, Sha256};

/// Directory the decision documents are written to.
const DOC_DIR: &str = "docs/adr";
/// Directory the rule files are written to.
const RULES_DIR: &str = ".actual/rules";
/// Extension of both files.
const EXTENSION: &str = ".md";
/// Longest generated file name.
const MAX_NAME_LENGTH: usize = 191;
/// Longest generated repository-relative path, directory included.
const MAX_PATH_LENGTH: usize = 200;
/// Hex characters of the hash a shortened name ends in.
const HASH_LENGTH: usize = 16;
/// Name cap for rule files, which an agent index lists.
const MAX_RULE_NAME_LENGTH: usize = 64;
/// Topic every decision created through the CLI is pinned to.
const RULE_TOPIC: &str = "cross-cutting";
/// Longest aspect slug kept in a rule file name.
const MAX_ASPECT_LENGTH: usize = 40;
/// Most title words kept in a rule file name.
const MAX_ASPECT_WORDS: usize = 5;
/// Title words left out of a rule file name.
const FILLER_WORDS: [&str; 11] = [
    "the", "a", "an", "for", "of", "in", "to", "and", "with", "or", "by",
];
/// Windows device names, which are always hashed.
const WINDOWS_RESERVED: [&str; 4] = ["con", "prn", "aux", "nul"];

/// `docs/adr/` file name for a decision: `<id>-<title slug>.md`, shortened with a hash past the sync's length cap.
pub fn decision_doc_file_name(adr_id: &str, title: &str) -> String {
    let slug = slugify(title);
    let readable = if slug.is_empty() {
        adr_id.to_string()
    } else {
        format!("{adr_id}-{slug}")
    };
    generated_file_name(DOC_DIR, &readable, MAX_NAME_LENGTH)
}

/// `.actual/rules/` file name for a decision: `cross-cutting-<aspect>-<hash4>.md`.
pub fn rule_file_name(adr_id: &str, title: &str) -> String {
    let aspect = slugify_aspect(title);
    // The aspect is ASCII, so any byte index is a char boundary.
    let capped = if aspect.len() > MAX_ASPECT_LENGTH {
        aspect[..MAX_ASPECT_LENGTH].trim_end_matches('-')
    } else {
        aspect.as_str()
    };
    let readable = format!("{RULE_TOPIC}-{capped}-{}", hash4(adr_id));
    generated_file_name(RULES_DIR, &readable, MAX_RULE_NAME_LENGTH)
}

/// The title lowercased, each run of characters outside `[a-z0-9]` turned into
/// one `-`, and one leading and one trailing `-` dropped.
///
/// Full Unicode lowercasing matters: it maps the Kelvin sign to `k`, as
/// JavaScript's `toLowerCase` does, so the same letters survive the filter.
fn slugify(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    for c in text.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    // Runs are collapsed above, so at most one hyphen sits at each end.
    let slug = slug.strip_prefix('-').unwrap_or(&slug);
    slug.strip_suffix('-').unwrap_or(slug).to_string()
}

/// The distinguishing part of a title: the text after its first `:` (the whole
/// title when it has none), as its first five `[a-z0-9]` words that are not
/// filler, joined by `-`. Falls back to the whole title's slug when no word is left.
fn slugify_aspect(title: &str) -> String {
    let aspect = title.split_once(':').map_or(title, |(_, rest)| rest);
    let lower = aspect.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .filter(|word| !word.is_empty() && !FILLER_WORDS.contains(word))
        .take(MAX_ASPECT_WORDS)
        .collect();
    if words.is_empty() {
        slugify(title)
    } else {
        words.join("-")
    }
}

/// The id's first four characters once its hyphens are dropped, lowercased.
///
/// The sync counts JavaScript (UTF-16) units; a character it would split in two
/// would become a hyphen and then be trimmed from the end of the name, so it is
/// left out here. For a UUID this is its first four hex digits.
fn hash4(adr_id: &str) -> String {
    let mut units = 0;
    adr_id
        .chars()
        .filter(|&c| c != '-')
        .take_while(|c| {
            units += c.len_utf16();
            units <= 4
        })
        .collect::<String>()
        .to_lowercase()
}

/// ASCII letters and digits kept, every other run turned into one `-` (never
/// first), and the trailing `-` dropped.
fn portable(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// The first whole words of `text` (ASCII, from [`portable`]) within `keep`
/// characters; mid-word only when no word ends in time.
fn whole_words(text: &str, keep: usize) -> &str {
    if text.len() <= keep {
        return text;
    }
    let head = &text[..keep];
    if text.as_bytes()[keep] == b'-' {
        return head;
    }
    match head.rfind('-') {
        Some(boundary) if boundary > 0 => head[..boundary].trim_end_matches('-'),
        _ => head.trim_end_matches('-'),
    }
}

/// True for a name Windows reserves for a device, whatever its case.
fn is_windows_reserved(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let numbered = |prefix: &str| {
        lower
            .strip_prefix(prefix)
            .is_some_and(|n| matches!(n.as_bytes(), [b'1'..=b'9']))
    };
    WINDOWS_RESERVED.contains(&lower.as_str()) || numbered("com") || numbered("lpt")
}

/// The sync's one naming scheme for a generated file in `directory`.
fn generated_file_name(directory: &str, readable: &str, max_name_length: usize) -> String {
    let readable = portable(readable);
    let full_name = format!("{readable}{EXTENSION}");
    let path_budget = MAX_PATH_LENGTH - (directory.trim_end_matches('/').len() + 1);
    let limit = max_name_length.min(MAX_NAME_LENGTH).min(path_budget);
    let must_hash = readable.is_empty() || is_windows_reserved(&readable);
    if !must_hash && full_name.len() <= limit {
        return full_name;
    }
    let keep = limit - EXTENSION.len() - 1 - HASH_LENGTH;
    let hash = hex_prefix(&Sha256::digest(full_name.as_bytes()), HASH_LENGTH);
    let head = whole_words(&readable, keep);
    if head.is_empty() {
        format!("{hash}{EXTENSION}")
    } else {
        format!("{head}-{hash}{EXTENSION}")
    }
}

/// The first `len` lowercase hex characters of `bytes`.
fn hex_prefix(bytes: &[u8], len: usize) -> String {
    let mut hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    hex.truncate(len);
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG_ID: &str = "c357c7f2-86cb-461d-85d7-5dc849bac736";
    const LONG_TITLE: &str =
        "Adoption of @aws-sdk/client-bedrock-runtime for Foundation Model and \
        Embedding Operations: Foundation Model Requests Dispatched Through InvokeModelCommand Are \
        Serialized, Validated and Retried With Backoff";

    fn sha256_hex(text: &str) -> String {
        hex_prefix(&Sha256::digest(text.as_bytes()), 64)
    }

    #[test]
    fn ordinary_titles() {
        let id = "0b6f2a1e-1111-2222-3333-444455556666";
        assert_eq!(
            decision_doc_file_name(id, "Use Postgres for storage"),
            "0b6f2a1e-1111-2222-3333-444455556666-use-postgres-for-storage.md"
        );
        assert_eq!(
            rule_file_name(id, "Use Postgres for storage"),
            "cross-cutting-use-postgres-storage-0b6f.md"
        );
        assert_eq!(
            decision_doc_file_name("ADR-0002", "Adopt PostgreSQL CTEs"),
            "ADR-0002-adopt-postgresql-ctes.md"
        );
        assert_eq!(
            rule_file_name("ADR-0002", "Adopt PostgreSQL CTEs"),
            "cross-cutting-adopt-postgresql-ctes-adr0.md"
        );
        assert_eq!(
            decision_doc_file_name("ADR-0004", "Some -- Title --- Here"),
            "ADR-0004-some-title-here.md"
        );
        assert_eq!(
            decision_doc_file_name("ADR-0005", "  Spaces Around  "),
            "ADR-0005-spaces-around.md"
        );
        assert_eq!(
            rule_file_name("ADR-0005", "  Spaces Around  "),
            "cross-cutting-spaces-around-adr0.md"
        );
        // Upper case in the id survives the doc name; hash4 is lowercased.
        let upper = "9A1C0000-AAAA-BBBB-CCCC-DDDDDDDDDDDD";
        assert_eq!(
            decision_doc_file_name(upper, "Upper id"),
            "9A1C0000-AAAA-BBBB-CCCC-DDDDDDDDDDDD-upper-id.md"
        );
        assert_eq!(
            rule_file_name(upper, "Upper id"),
            "cross-cutting-upper-id-9a1c.md"
        );
    }

    #[test]
    fn colon_titles_name_the_rule_file_after_the_colon() {
        let id = "fdbbcb01-e293-46dd-a7af-ca1f67f66c5b";
        let title = "@temporalio/common Library Adoption: Activities Returning Validation \
            Errors Pass Structured";
        assert_eq!(
            decision_doc_file_name(id, title),
            "fdbbcb01-e293-46dd-a7af-ca1f67f66c5b-temporalio-common-library-adoption-\
             activities-returning-validation-errors-pass-structured.md"
        );
        // Five words, then cut to 40 characters.
        assert_eq!(
            rule_file_name(id, title),
            "cross-cutting-activities-returning-validation-errors-p-fdbb.md"
        );
        let other = "9a1c0000-aaaa-bbbb-cccc-dddddddddddd";
        // Only the first colon splits.
        assert_eq!(
            rule_file_name(other, "API: v2: Routes"),
            "cross-cutting-v2-routes-9a1c.md"
        );
        assert_eq!(
            decision_doc_file_name(other, "API: v2: Routes"),
            "9a1c0000-aaaa-bbbb-cccc-dddddddddddd-api-v2-routes.md"
        );
        // Nothing after the colon: the whole title's slug.
        assert_eq!(
            rule_file_name(other, "Trailing colon:"),
            "cross-cutting-trailing-colon-9a1c.md"
        );
        // Only filler after the colon: the whole title's slug, filler included.
        assert_eq!(
            rule_file_name(other, "The and of: for the a"),
            "cross-cutting-the-and-of-for-the-a-9a1c.md"
        );
    }

    #[test]
    fn titles_with_no_ascii_words_fall_back_to_the_id() {
        for title in ["\u{1F389}", "\u{65e5}\u{672c}\u{8a9e}", "###", "   ", ""] {
            assert_eq!(decision_doc_file_name("ADR-0006", title), "ADR-0006.md");
            assert_eq!(rule_file_name("ADR-0006", title), "cross-cutting-adr0.md");
        }
        let id = "9a1c0000-aaaa-bbbb-cccc-dddddddddddd";
        assert_eq!(
            decision_doc_file_name(id, "\u{65e5}\u{672c}: \u{8a9e}"),
            "9a1c0000-aaaa-bbbb-cccc-dddddddddddd.md"
        );
        assert_eq!(
            rule_file_name(id, "\u{65e5}\u{672c}: \u{8a9e}"),
            "cross-cutting-9a1c.md"
        );
    }

    #[test]
    fn non_ascii_titles_keep_only_ascii_letters() {
        let id = "9a1c0000-aaaa-bbbb-cccc-dddddddddddd";
        let title = "Caf\u{e9} cr\u{e8}me: \u{dc}ber na\u{ef}ve r\u{e9}sum\u{e9}s";
        assert_eq!(
            decision_doc_file_name(id, title),
            "9a1c0000-aaaa-bbbb-cccc-dddddddddddd-caf-cr-me-ber-na-ve-r-sum-s.md"
        );
        assert_eq!(
            rule_file_name(id, title),
            "cross-cutting-ber-na-ve-r-sum-9a1c.md"
        );
        // Full lowercasing: the Kelvin sign becomes `k`, a dotted capital I
        // becomes `i` and a combining dot, as in JavaScript.
        let title = "\u{212a}elvin \u{130}stanbul";
        assert_eq!(
            decision_doc_file_name(id, title),
            "9a1c0000-aaaa-bbbb-cccc-dddddddddddd-kelvin-i-stanbul.md"
        );
        assert_eq!(
            rule_file_name(id, title),
            "cross-cutting-kelvin-i-stanbul-9a1c.md"
        );
    }

    #[test]
    fn long_aspect_is_cut_to_forty_characters() {
        let id = "9a1c0000-aaaa-bbbb-cccc-dddddddddddd";
        let title = "Supercalifragilisticexpialidocious antidisestablishmentarianism words";
        assert_eq!(
            rule_file_name(id, title),
            "cross-cutting-supercalifragilisticexpialidocious-antid-9a1c.md"
        );
        assert_eq!(
            rule_file_name(LONG_ID, &"x".repeat(300)),
            "cross-cutting-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx-c357.md"
        );
    }

    #[test]
    fn long_title_is_shortened_with_a_hash() {
        // The server test's vector: the first 171 characters end on a hyphen,
        // which is dropped, so 170 + "-" + 16 + ".md".
        let full = format!(
            "{LONG_ID}-adoption-of-aws-sdk-client-bedrock-runtime-for-foundation-model-and-\
             embedding-operations-foundation-model-requests-dispatched-through-invokemodelcommand-\
             are-serialized-validated-and-retried-with-backoff.md"
        );
        let name = decision_doc_file_name(LONG_ID, LONG_TITLE);
        assert_eq!(
            name,
            format!("{}-{}.md", &full[..170], &sha256_hex(&full)[..16])
        );
        assert_eq!(
            name,
            "c357c7f2-86cb-461d-85d7-5dc849bac736-adoption-of-aws-sdk-client-bedrock-runtime-for-\
             foundation-model-and-embedding-operations-foundation-model-requests-dispatched-\
             through-d3ad72f8eb383f98.md"
        );
        assert!(format!("docs/adr/{name}").len() <= 200);
        assert_eq!(
            rule_file_name(LONG_ID, LONG_TITLE),
            "cross-cutting-foundation-model-requests-dispatched-thr-c357.md"
        );
    }

    #[test]
    fn name_of_191_characters_or_fewer_is_unchanged() {
        let id = "1dc81e6b-7e7c-41f1-9c98-350a25f02946";
        let title = "Adoption of @aws-sdk/client-bedrock-runtime for Foundation Model and \
            Embedding Operations: Instances BedrockRuntimeClient Initialized Per Target Region";
        assert_eq!(
            decision_doc_file_name(id, title),
            "1dc81e6b-7e7c-41f1-9c98-350a25f02946-adoption-of-aws-sdk-client-bedrock-runtime-for-\
             foundation-model-and-embedding-operations-instances-bedrockruntimeclient-initialized-\
             per-target-region.md"
        );
        assert_eq!(
            rule_file_name(id, title),
            "cross-cutting-instances-bedrockruntimeclient-initializ-1dc8.md"
        );
    }

    #[test]
    fn long_word_with_no_boundary_keeps_only_the_id() {
        // Character 171 is mid-word and the last hyphen before it ends the id.
        assert_eq!(
            decision_doc_file_name(LONG_ID, &"x".repeat(300)),
            "c357c7f2-86cb-461d-85d7-5dc849bac736-a94537e07ff1823c.md"
        );
        // Non-ASCII letters become hyphens before the words are counted.
        let title = format!("{} {}", "\u{e9}".repeat(50), "word ".repeat(60));
        assert_eq!(
            decision_doc_file_name(LONG_ID, &title),
            format!("{LONG_ID}-{}673b8ade201d20fc.md", "word-".repeat(27))
        );
    }

    #[test]
    fn whole_words_cuts_mid_word_only_without_a_boundary() {
        assert_eq!(whole_words("abc", 5), "abc");
        assert_eq!(whole_words("ab-cd", 2), "ab");
        assert_eq!(whole_words("abcdef", 3), "abc");
        assert_eq!(whole_words("-abcdef", 3), "-ab");
        assert_eq!(whole_words("ab--cdef", 5), "ab");
    }

    #[test]
    fn reserved_and_empty_names_are_always_hashed() {
        for reserved in ["CON", "nul", "Com1", "lpt9"] {
            let full = format!("{reserved}.md");
            assert_eq!(
                decision_doc_file_name(reserved, ""),
                format!("{reserved}-{}.md", &sha256_hex(&full)[..16])
            );
        }
        for not_reserved in ["com0", "com10", "lpt", "console"] {
            assert_eq!(
                decision_doc_file_name(not_reserved, ""),
                format!("{not_reserved}.md")
            );
        }
        assert_eq!(
            decision_doc_file_name("", ""),
            format!("{}.md", &sha256_hex(".md")[..16])
        );
        assert_eq!(decision_doc_file_name("", "Title"), "title.md");
    }

    #[test]
    fn hash4_counts_utf16_units_and_skips_hyphens() {
        assert_eq!(hash4("0b6f2a1e-1111"), "0b6f");
        assert_eq!(hash4("ab-cd-ef"), "abcd");
        assert_eq!(hash4("AB"), "ab");
        // An astral character takes two units; one that would straddle the
        // fourth unit is left out.
        assert_eq!(hash4("abc\u{1F389}d"), "abc");
        assert_eq!(hash4("ab\u{1F389}d"), "ab\u{1F389}");
        assert_eq!(
            rule_file_name("abc\u{1F389}d", "T"),
            "cross-cutting-t-abc.md"
        );
        assert_eq!(rule_file_name("\u{c9}tat", "T"), "cross-cutting-t-tat.md");
    }

    #[test]
    fn portable_collapses_and_trims() {
        assert_eq!(portable("--a__b--"), "a-b");
        assert_eq!(portable("\u{e9}a\u{e9}"), "a");
        assert_eq!(portable(""), "");
    }
}
