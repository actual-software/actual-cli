# Token Limit Enforcement for CLAUDE.md/AGENTS.md

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent CLAUDE.md/AGENTS.md from exceeding Claude Code's ~40k token limit by routing all ADRs through per-file output and enforcing a character budget on the managed section.

**Architecture:** All ADRs (V1 and V2) are written to `docs/adr/<slug>.md` + `.claude/rules/<slug>.md` via the existing V2 output pipeline. The CLAUDE.md/AGENTS.md managed section contains a governance pointer plus budget-limited ADR sections. A new `budget` module computes character budgets and selects which sections fit. Auto-migration from inline-only format is detected and logged in the TUI.

**Tech Stack:** Rust, existing `generation` and `tailoring` crates, `chrono` (already depended on)

**Linear Issue:** ACTCLI-116

---

### Task 1: Create character budget module

**Files:**
- Create: `src/generation/budget.rs`
- Modify: `src/generation/mod.rs`

- [ ] **Step 1: Write failing tests for budget computation**

Add tests to a new file `src/generation/budget.rs`. These test the public API before implementing it.

```rust
// src/generation/budget.rs

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
///
/// If `existing_content` contains non-managed text that already eats into the
/// total file budget, the managed budget is reduced accordingly. The hard cap
/// of [`MAX_MANAGED_CHARS`] always applies.
pub fn compute_managed_budget(existing_content: Option<&str>) -> usize {
    todo!()
}

/// Estimate the serialised character cost of one ADR section including its
/// per-ADR markers (`<!-- adr:ID start/end -->`).
pub fn estimate_section_chars(section: &AdrSection) -> usize {
    todo!()
}

/// Select sections that fit within `budget` characters.
///
/// Returns `(selected, excluded_count)`. Sections are included in order until
/// the budget is exhausted. The `v2-governance` section is **always** included
/// regardless of budget. Only complete sections are included — no mid-section
/// cuts.
pub fn select_sections_within_budget(
    sections: &[AdrSection],
    budget: usize,
) -> (Vec<AdrSection>, usize) {
    todo!()
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

    // ── estimate_section_chars ──

    #[test]
    fn test_estimate_section_chars_basic() {
        let section = make_section("adr-1", "hello");
        let chars = estimate_section_chars(&section);
        // <!-- adr:adr-1 start -->\nhello\n<!-- adr:adr-1 end -->\n
        let expected = "<!-- adr:adr-1 start -->\n".len()
            + "hello\n".len()
            + "<!-- adr:adr-1 end -->\n".len();
        assert_eq!(chars, expected);
    }

    #[test]
    fn test_estimate_section_chars_empty_content() {
        let section = make_section("x", "");
        let chars = estimate_section_chars(&section);
        // Markers + newlines only
        assert!(chars > 0);
    }

    // ── compute_managed_budget ──

    #[test]
    fn test_budget_no_existing_content() {
        assert_eq!(compute_managed_budget(None), MAX_MANAGED_CHARS);
    }

    #[test]
    fn test_budget_small_non_managed_content() {
        // 100 chars of user content + managed block → budget stays at MAX
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
        // 150k chars of user content → budget = 160k - 150k = 10k (below MAX)
        let user_content = "x".repeat(150_000);
        let existing = format!(
            "{user_content}\n{}\nold\n{}",
            markers::START_MARKER,
            markers::END_MARKER,
        );
        let budget = compute_managed_budget(Some(&existing));
        // Non-managed ≈ 150k, total limit = 160k, so ~10k budget
        assert!(budget < MAX_MANAGED_CHARS);
        assert!(budget <= 10_100); // allow small marker overhead variance
        assert!(budget >= 9_900);
    }

    #[test]
    fn test_budget_non_managed_exceeds_total_limit() {
        let user_content = "x".repeat(200_000);
        assert_eq!(compute_managed_budget(Some(&user_content)), 0);
    }

    #[test]
    fn test_budget_no_managed_markers_in_existing() {
        // Existing file without managed block — all content is "non-managed"
        let existing = "x".repeat(50_000);
        let budget = compute_managed_budget(Some(&existing));
        // 160k - 50k = 110k, capped at MAX_MANAGED_CHARS (120k) → 110k
        assert_eq!(budget, CHAR_LIMIT - 50_000);
    }

    // ── select_sections_within_budget ──

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
        // Budget fits ~2 sections (each ~5k + marker overhead)
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
        // Tiny budget — only governance should fit
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
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /Users/poile/repos/actualai/actual-cli/.worktrees/actcli-116-token-limit && cargo test --lib generation::budget -- --nocapture 2>&1 | head -40`

Expected: Compilation error — `todo!()` stubs not implemented.

- [ ] **Step 3: Implement the budget functions**

Replace the three `todo!()` stubs with real implementations:

```rust
pub fn compute_managed_budget(existing_content: Option<&str>) -> usize {
    let non_managed_chars = existing_content
        .map(|content| {
            if let Some((start, end)) = markers::find_managed_section_bounds(content) {
                let managed_len = end + markers::END_MARKER.len() - start;
                content.len().saturating_sub(managed_len)
            } else {
                // No managed section — entire file is non-managed
                content.len()
            }
        })
        .unwrap_or(0);

    let dynamic_budget = CHAR_LIMIT.saturating_sub(non_managed_chars);
    MAX_MANAGED_CHARS.min(dynamic_budget)
}

pub fn estimate_section_chars(section: &AdrSection) -> usize {
    // <!-- adr:{id} start -->\n{content}\n<!-- adr:{id} end -->\n
    let id_len = section.adr_id.len();
    let marker_overhead = markers::ADR_SECTION_START_PREFIX.len()
        + id_len
        + markers::ADR_SECTION_START_SUFFIX.len()
        + 1 // newline after start marker
        + markers::ADR_SECTION_END_PREFIX.len()
        + id_len
        + markers::ADR_SECTION_END_SUFFIX.len()
        + 1; // newline after end marker
    marker_overhead + section.content.len() + 1 // +1 for newline after content
}

pub fn select_sections_within_budget(
    sections: &[AdrSection],
    budget: usize,
) -> (Vec<AdrSection>, usize) {
    let mut remaining = budget;
    let mut selected = Vec::new();
    let mut excluded = 0;

    for section in sections {
        let cost = estimate_section_chars(section);

        // Governance section is always included regardless of budget
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
```

- [ ] **Step 4: Export the budget module**

In `src/generation/mod.rs`, add the new module:

```rust
pub mod budget;
pub mod format;
pub mod markers;
pub mod merge;
pub mod writer;

pub use format::OutputFormat;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd /Users/poile/repos/actualai/actual-cli/.worktrees/actcli-116-token-limit && cargo test --lib generation::budget -- --nocapture`

Expected: All 10 tests pass.

- [ ] **Step 6: Commit**

```bash
cd /Users/poile/repos/actualai/actual-cli/.worktrees/actcli-116-token-limit
git add src/generation/budget.rs src/generation/mod.rs
git commit -m "feat(ACTCLI-116): add character budget module for managed section size enforcement"
```

---

### Task 2: Route all ADRs through per-file output

**Files:**
- Modify: `src/cli/commands/sync/pipeline.rs:483-490`

- [ ] **Step 1: Write a test for V1 ADRs through generate_v2_output**

In `src/cli/commands/sync/v2_output.rs`, add a test confirming V1 ADRs produce valid per-file output:

```rust
    #[test]
    fn test_generate_v2_output_with_v1_adrs() {
        // V1 ADRs have no content_md, no content_json, no schema_version
        let v1_adr = make_v1_adr("adr-v1", "Use Strict Mode");
        assert!(!v1_adr.is_v2());

        let output = generate_v2_output(&[v1_adr]);

        // Should still produce 2 files
        assert_eq!(output.raw_files.len(), 2);

        let docs_file = output
            .raw_files
            .iter()
            .find(|f| f.path.starts_with("docs/adr/"))
            .expect("expected docs/adr/ file");
        assert_eq!(docs_file.path, "docs/adr/use-strict-mode.md");
        // V1 fallback: title + policies
        assert!(docs_file.content.contains("# Use Strict Mode"));
        assert!(docs_file.content.contains("Do something"));

        let rules_file = output
            .raw_files
            .iter()
            .find(|f| f.path.starts_with(".claude/rules/"))
            .expect("expected .claude/rules/ file");
        assert_eq!(rules_file.path, ".claude/rules/use-strict-mode.md");
        // V1 fallback: policies as bullet points
        assert!(rules_file.content.contains("- Do something"));
    }
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cd /Users/poile/repos/actualai/actual-cli/.worktrees/actcli-116-token-limit && cargo test --lib sync::v2_output::tests::test_generate_v2_output_with_v1_adrs -- --nocapture`

Expected: PASS — `generate_v2_output` already handles the V1 fallback via `generate_adr_doc` and `generate_rules_file`.

- [ ] **Step 3: Modify pipeline.rs to route all ADRs through per-file output**

In `src/cli/commands/sync/pipeline.rs`, change lines 483-490 from:

```rust
    // Partition into V1 and V2 ADRs (after cache key computation so the key
    // covers all ADRs, including V2 ones).
    let (v1_adrs, v2_adrs) = partition_adrs(filtered_adrs);
    let v2_result = if v2_adrs.is_empty() {
        None
    } else {
        Some(generate_v2_output(&v2_adrs))
    };
```

To:

```rust
    // Generate per-file output for ALL ADRs (V1 and V2).
    // Every ADR gets docs/adr/<slug>.md + .claude/rules/<slug>.md regardless
    // of schema version. V1 ADRs use the generate_adr_doc/generate_rules_file
    // fallback paths (policies as bullets).
    let v2_result = if filtered_adrs.is_empty() {
        None
    } else {
        Some(generate_v2_output(&filtered_adrs))
    };

    // V1 ADRs still go through tailoring for inline CLAUDE.md content.
    let (v1_adrs, _v2_adrs) = partition_adrs(filtered_adrs);
```

- [ ] **Step 4: Run existing tests to verify nothing is broken**

Run: `cd /Users/poile/repos/actualai/actual-cli/.worktrees/actcli-116-token-limit && cargo test --lib sync::v2_output -- --nocapture`

Expected: All existing v2_output tests still pass.

- [ ] **Step 5: Run full test suite to check for regressions**

Run: `cd /Users/poile/repos/actualai/actual-cli/.worktrees/actcli-116-token-limit && cargo test 2>&1 | tail -20`

Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
cd /Users/poile/repos/actualai/actual-cli/.worktrees/actcli-116-token-limit
git add src/cli/commands/sync/pipeline.rs src/cli/commands/sync/v2_output.rs
git commit -m "feat(ACTCLI-116): route all ADRs through per-file output pipeline"
```

---

### Task 3: Add budget enforcement and TUI warnings to pipeline

**Files:**
- Modify: `src/cli/commands/sync/pipeline.rs:628-670`

- [ ] **Step 1: Add imports to pipeline.rs**

At the top of `src/cli/commands/sync/pipeline.rs`, add the budget import alongside existing generation imports. Find the existing import block and add:

```rust
use crate::generation::budget;
```

- [ ] **Step 2: Add the `apply_content_budget` helper function**

Add this function in `pipeline.rs` (above `run_sync` or at the bottom of the file, before tests):

```rust
/// Result of applying the content budget to the managed section.
struct BudgetResult {
    /// Number of ADR sections excluded from the root file.
    excluded_count: usize,
    /// Whether existing inline ADR sections were detected (migration from V1 inline format).
    migrated: bool,
}

/// Enforce the character budget on the root file's managed section.
///
/// Reads the existing root file (if any) to measure non-managed content,
/// computes the managed-section budget, and trims sections that exceed it.
/// The `v2-governance` section is always preserved regardless of budget.
fn apply_content_budget(
    mut output: TailoringOutput,
    root_dir: &Path,
    format: &OutputFormat,
) -> (TailoringOutput, Option<BudgetResult>) {
    let root_filename = format.filename();
    let root_file_idx = output.files.iter().position(|f| f.path == root_filename);

    let Some(idx) = root_file_idx else {
        return (output, None);
    };

    // Read existing file content to measure non-managed space
    let existing_path = root_dir.join(root_filename);
    let existing_content = std::fs::read_to_string(&existing_path).ok();

    // Detect migration: existing managed section has inline ADR sections
    // (i.e. sections other than v2-governance)
    let migrated = existing_content
        .as_ref()
        .and_then(|c| markers::extract_managed_content(c))
        .map(|managed| {
            let sections = markers::extract_adr_sections(managed);
            sections.iter().any(|s| s.id != "v2-governance")
        })
        .unwrap_or(false);

    // Compute budget and trim sections
    let char_budget = budget::compute_managed_budget(existing_content.as_deref());
    let (trimmed, excluded_count) =
        budget::select_sections_within_budget(&output.files[idx].sections, char_budget);
    output.files[idx].sections = trimmed;

    let info = if migrated || excluded_count > 0 {
        Some(BudgetResult {
            excluded_count,
            migrated,
        })
    } else {
        None
    };

    (output, info)
}
```

- [ ] **Step 3: Wire budget enforcement into the pipeline after governance injection**

In `pipeline.rs`, between the governance injection block (ending ~line 644) and the verbose reasoning block (~line 646), insert the budget enforcement and TUI warnings:

Replace:

```rust
    // Gap 7: surface the AI reasoning per file when --verbose is set, so
    // the user can see why the model generated each output file.
```

With:

```rust
    // Enforce character budget on the root file managed section.
    let (output, budget_result) = apply_content_budget(output, root_dir, &output_format);
    if let Some(ref info) = budget_result {
        if info.migrated {
            pipeline.println(
                "  \u{2139} Migrating inline ADRs to per-file format (docs/adr/ + .claude/rules/)",
            );
        }
        if info.excluded_count > 0 {
            let safe_filename = console::strip_ansi_codes(output_format.filename());
            pipeline.println(&format!(
                "  \u{26a0} {} ADRs excluded from {} to stay within token limit. \
                 Full content available in docs/adr/",
                info.excluded_count, safe_filename,
            ));
        }
    }

    // Gap 7: surface the AI reasoning per file when --verbose is set, so
    // the user can see why the model generated each output file.
```

Note: `\u{2139}` is the ℹ character, `\u{26a0}` is the ⚠ character — matching the existing TUI style.

- [ ] **Step 4: Verify compilation**

Run: `cd /Users/poile/repos/actualai/actual-cli/.worktrees/actcli-116-token-limit && cargo check 2>&1 | tail -10`

Expected: No errors.

- [ ] **Step 5: Run full test suite**

Run: `cd /Users/poile/repos/actualai/actual-cli/.worktrees/actcli-116-token-limit && cargo test 2>&1 | tail -20`

Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
cd /Users/poile/repos/actualai/actual-cli/.worktrees/actcli-116-token-limit
git add src/cli/commands/sync/pipeline.rs
git commit -m "feat(ACTCLI-116): enforce character budget on managed section with TUI warnings"
```

---

### Task 4: Add unit tests for budget enforcement integration

**Files:**
- Modify: `src/generation/budget.rs` (add integration-style tests)

- [ ] **Step 1: Add tests that simulate realistic managed section scenarios**

Append these tests to the `mod tests` block in `src/generation/budget.rs`:

```rust
    // ── Realistic scenario tests ──

    #[test]
    fn test_budget_with_governance_and_many_adrs() {
        // Simulate: governance pointer + 30 ADR sections, each ~4k chars
        let gov = make_section("v2-governance", "<adr_governance source=\"docs/adr/\">\nADRs govern this project.\n</adr_governance>");
        let mut sections = vec![gov];
        for i in 0..30 {
            sections.push(make_section(&format!("adr-{i:03}"), &"x".repeat(4_000)));
        }

        // With MAX_MANAGED_CHARS budget (~120k), about 28 sections fit
        // (each ~4,060 chars with markers → 28 * 4,060 ≈ 113,680)
        let (selected, excluded) = select_sections_within_budget(&sections, MAX_MANAGED_CHARS);
        assert!(selected.len() > 1); // governance + some ADRs
        assert!(selected[0].adr_id == "v2-governance"); // governance always first
        assert!(excluded > 0); // some excluded
        assert_eq!(selected.len() + excluded, 31); // all accounted for

        // Total chars of selected sections should be within budget
        let total_chars: usize = selected.iter().map(|s| estimate_section_chars(s)).sum();
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
```

- [ ] **Step 2: Run the new tests**

Run: `cd /Users/poile/repos/actualai/actual-cli/.worktrees/actcli-116-token-limit && cargo test --lib generation::budget -- --nocapture`

Expected: All 14 tests pass.

- [ ] **Step 3: Commit**

```bash
cd /Users/poile/repos/actualai/actual-cli/.worktrees/actcli-116-token-limit
git add src/generation/budget.rs
git commit -m "test(ACTCLI-116): add integration-style tests for budget enforcement"
```

---

### Task 5: Run full test suite and verify no regressions

**Files:** None (verification only)

- [ ] **Step 1: Run complete test suite**

Run: `cd /Users/poile/repos/actualai/actual-cli/.worktrees/actcli-116-token-limit && cargo test 2>&1 | tail -30`

Expected: All tests pass. Zero failures.

- [ ] **Step 2: Run clippy**

Run: `cd /Users/poile/repos/actualai/actual-cli/.worktrees/actcli-116-token-limit && cargo clippy --all-targets 2>&1 | tail -20`

Expected: No warnings or errors.

- [ ] **Step 3: Verify the changes are complete**

Run: `cd /Users/poile/repos/actualai/actual-cli/.worktrees/actcli-116-token-limit && git log --oneline main..HEAD`

Expected output (4 commits):
```
<hash> test(ACTCLI-116): add integration-style tests for budget enforcement
<hash> feat(ACTCLI-116): enforce character budget on managed section with TUI warnings
<hash> feat(ACTCLI-116): route all ADRs through per-file output pipeline
<hash> feat(ACTCLI-116): add character budget module for managed section size enforcement
```

- [ ] **Step 4: Verify key behavioral changes**

Confirm these behavioral changes exist in the diff:

1. **All ADRs → per-file**: `generate_v2_output(&filtered_adrs)` instead of `generate_v2_output(&v2_adrs)`
2. **Budget module**: `src/generation/budget.rs` exists with `compute_managed_budget`, `select_sections_within_budget`, `estimate_section_chars`
3. **Budget enforcement**: `apply_content_budget` called in pipeline between governance injection and confirm_and_write
4. **Migration detection**: TUI message `"Migrating inline ADRs to per-file format"` emitted when existing inline sections detected
5. **Truncation warning**: TUI message `"N ADRs excluded from FILE to stay within token limit"` emitted when sections trimmed
