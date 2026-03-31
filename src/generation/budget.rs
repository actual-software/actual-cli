use crate::generation::markers;
use crate::tailoring::types::AdrSection;

/// Claude Code's token limit for context files.
const TOKEN_LIMIT: usize = 40_000;

/// Conservative chars-per-token estimate.
const CHARS_PER_TOKEN: usize = 4;

/// Total character limit for the file (TOKEN_LIMIT * CHARS_PER_TOKEN).
pub const CHAR_LIMIT: usize = TOKEN_LIMIT * CHARS_PER_TOKEN; // 160,000

/// Maximum fraction of CHAR_LIMIT the managed section may occupy.
const BUDGET_RATIO: f64 = 0.75;

/// Hard cap on managed section characters (CHAR_LIMIT * BUDGET_RATIO).
pub const MAX_MANAGED_CHARS: usize = (CHAR_LIMIT as f64 * BUDGET_RATIO) as usize; // 120,000

/// Compute the character budget available for the managed section.
pub fn compute_managed_budget(existing_content: Option<&str>) -> usize {
    let non_managed_chars = existing_content
        .map(|content| {
            if let Some((start, end)) = markers::find_managed_section_bounds(content) {
                let managed_len = end + markers::END_MARKER.len() - start;
                content.len().saturating_sub(managed_len)
            } else {
                content.len()
            }
        })
        .unwrap_or(0);

    let dynamic_budget = CHAR_LIMIT.saturating_sub(non_managed_chars);
    MAX_MANAGED_CHARS.min(dynamic_budget)
}

/// Estimate the serialised character cost of one ADR section including its per-ADR markers.
pub fn estimate_section_chars(section: &AdrSection) -> usize {
    let id_len = section.adr_id.len();
    let marker_overhead = markers::ADR_SECTION_START_PREFIX.len()
        + id_len
        + markers::ADR_SECTION_START_SUFFIX.len()
        + 1
        + markers::ADR_SECTION_END_PREFIX.len()
        + id_len
        + markers::ADR_SECTION_END_SUFFIX.len()
        + 1;
    marker_overhead + section.content.len() + 1
}

/// Select sections that fit within `budget` characters.
/// The `v2-governance` section is always included regardless of budget.
pub fn select_sections_within_budget(
    sections: &[AdrSection],
    budget: usize,
) -> (Vec<AdrSection>, usize) {
    let mut remaining = budget;
    let mut selected = Vec::new();
    let mut excluded = 0;

    for section in sections {
        let cost = estimate_section_chars(section);

        if section.adr_id == "v2-governance" {
            selected.push(section.clone());
            remaining = remaining.saturating_sub(cost);
            continue;
        }

        if cost <= remaining {
            selected.push(section.clone());
            remaining -= cost;
        } else {
            excluded += 1;
        }
    }

    (selected, excluded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_section(id: &str, content: &str) -> AdrSection {
        AdrSection {
            adr_id: id.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn test_estimate_section_chars_basic() {
        let section = make_section("adr-1", "hello");
        let chars = estimate_section_chars(&section);
        let expected =
            "<!-- adr:adr-1 start -->\n".len() + "hello\n".len() + "<!-- adr:adr-1 end -->\n".len();
        assert_eq!(chars, expected);
    }

    #[test]
    fn test_estimate_section_chars_empty_content() {
        let section = make_section("x", "");
        let chars = estimate_section_chars(&section);
        assert!(chars > 0);
    }

    #[test]
    fn test_budget_no_existing_content() {
        assert_eq!(compute_managed_budget(None), MAX_MANAGED_CHARS);
    }

    #[test]
    fn test_budget_small_non_managed_content() {
        let existing = format!(
            "{}\n\n{}\nold\n{}",
            "x".repeat(100),
            markers::START_MARKER,
            markers::END_MARKER,
        );
        let budget = compute_managed_budget(Some(&existing));
        assert_eq!(budget, MAX_MANAGED_CHARS);
    }

    #[test]
    fn test_budget_large_non_managed_content() {
        let user_content = "x".repeat(150_000);
        let existing = format!(
            "{user_content}\n{}\nold\n{}",
            markers::START_MARKER,
            markers::END_MARKER,
        );
        let budget = compute_managed_budget(Some(&existing));
        assert!(budget < MAX_MANAGED_CHARS);
        assert!(budget <= 10_100);
        assert!(budget >= 9_900);
    }

    #[test]
    fn test_budget_non_managed_exceeds_total_limit() {
        let user_content = "x".repeat(200_000);
        assert_eq!(compute_managed_budget(Some(&user_content)), 0);
    }

    #[test]
    fn test_budget_no_managed_markers_in_existing() {
        let existing = "x".repeat(50_000);
        let budget = compute_managed_budget(Some(&existing));
        assert_eq!(budget, CHAR_LIMIT - 50_000);
    }

    #[test]
    fn test_select_all_fit() {
        let sections = vec![
            make_section("adr-1", "short"),
            make_section("adr-2", "also short"),
        ];
        let (selected, excluded) = select_sections_within_budget(&sections, 10_000);
        assert_eq!(selected.len(), 2);
        assert_eq!(excluded, 0);
    }

    #[test]
    fn test_select_budget_exceeded() {
        let big = "x".repeat(5_000);
        let sections = vec![
            make_section("adr-1", &big),
            make_section("adr-2", &big),
            make_section("adr-3", &big),
        ];
        let (selected, excluded) = select_sections_within_budget(&sections, 11_000);
        assert_eq!(selected.len(), 2);
        assert_eq!(excluded, 1);
    }

    #[test]
    fn test_select_governance_always_included() {
        let sections = vec![
            make_section("v2-governance", "governance pointer"),
            make_section("adr-1", &"x".repeat(500)),
        ];
        let (selected, excluded) = select_sections_within_budget(&sections, 50);
        assert!(selected.iter().any(|s| s.adr_id == "v2-governance"));
        assert_eq!(excluded, 1);
    }

    #[test]
    fn test_select_empty_sections() {
        let (selected, excluded) = select_sections_within_budget(&[], 10_000);
        assert!(selected.is_empty());
        assert_eq!(excluded, 0);
    }

    #[test]
    fn test_select_zero_budget_governance_still_included() {
        let sections = vec![
            make_section("v2-governance", "gov"),
            make_section("adr-1", "content"),
        ];
        let (selected, excluded) = select_sections_within_budget(&sections, 0);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].adr_id, "v2-governance");
        assert_eq!(excluded, 1);
    }

    // ── Realistic scenario tests ──

    #[test]
    fn test_budget_with_governance_and_many_adrs() {
        // Simulate: governance pointer + 30 ADR sections, each ~4k chars
        let gov = make_section(
            "v2-governance",
            "<adr_governance source=\"docs/adr/\">\nADRs govern this project.\n</adr_governance>",
        );
        let mut sections = vec![gov];
        for i in 0..30 {
            sections.push(make_section(&format!("adr-{i:03}"), &"x".repeat(4_000)));
        }

        // With MAX_MANAGED_CHARS budget (~120k), about 28-29 sections fit
        let (selected, excluded) = select_sections_within_budget(&sections, MAX_MANAGED_CHARS);
        assert!(selected.len() > 1); // governance + some ADRs
        assert!(selected[0].adr_id == "v2-governance"); // governance always first
        assert!(excluded > 0); // some excluded
        assert_eq!(selected.len() + excluded, 31); // all accounted for

        // Total chars of selected sections should be within budget
        let total_chars: usize = selected.iter().map(estimate_section_chars).sum();
        assert!(total_chars <= MAX_MANAGED_CHARS);
    }

    #[test]
    fn test_budget_governance_only_when_severely_constrained() {
        let gov = make_section("v2-governance", "gov");
        let sections = vec![
            gov,
            make_section("adr-1", &"x".repeat(1_000)),
            make_section("adr-2", &"x".repeat(1_000)),
        ];
        // Budget of 100 chars — only governance fits (it's always included)
        let (selected, excluded) = select_sections_within_budget(&sections, 100);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].adr_id, "v2-governance");
        assert_eq!(excluded, 2);
    }

    #[test]
    fn test_budget_preserves_section_order() {
        let sections = vec![
            make_section("v2-governance", "gov"),
            make_section("adr-a", "content-a"),
            make_section("adr-b", "content-b"),
            make_section("adr-c", "content-c"),
        ];
        let (selected, _) = select_sections_within_budget(&sections, 100_000);
        let ids: Vec<&str> = selected.iter().map(|s| s.adr_id.as_str()).collect();
        assert_eq!(ids, &["v2-governance", "adr-a", "adr-b", "adr-c"]);
    }

    #[test]
    fn test_budget_compute_with_existing_file_no_markers() {
        // File exists but has no managed section — all content is user-written
        let existing = "# My Project\n\nHere are my notes.\n";
        let budget = compute_managed_budget(Some(existing));
        let expected = CHAR_LIMIT.saturating_sub(existing.len());
        assert_eq!(budget, expected.min(MAX_MANAGED_CHARS));
    }
}
