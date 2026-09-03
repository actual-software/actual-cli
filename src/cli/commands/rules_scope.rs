//! `actual rules index`, `rules select` and `rules eval` — the scope index's
//! user-facing surface.
//!
//! # Design
//!
//! Rendering is split into pure functions taking data and a width, the way
//! [`crate::cli::commands::rules`] does, so every panel and every JSON payload
//! is asserted on without a terminal.
//!
//! `select --explain` exists because a selector nobody can interrogate is a
//! selector nobody can fix. It prints, for each hit, which field carried it and
//! on which terms; the globs that matched a named path; the terms the corpus
//! made worthless; and the filename scan's answer beside its own at the same
//! cap, so the two can be compared on the caller's real input rather than only
//! on a golden set.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cli::args::{RulesEvalArgs, RulesIndexArgs, RulesSelectArgs};
use crate::cli::ui::panel::Panel;
use crate::cli::ui::term_size;
use crate::error::ActualError;
use crate::rules::scope::{
    self, baseline,
    eval::{CaseResult, EvaluationReport, GoldenCase, Scores},
    index::{Field, Match, Query, ScopeIndex, Weights},
    IndexSource, ResolvedIndex,
};

fn repo_root(explicit: Option<&PathBuf>) -> PathBuf {
    explicit
        .cloned()
        .unwrap_or_else(crate::cli::commands::sync::resolve_cwd)
}

// ── rules index ──────────────────────────────────────────────────────────

pub fn exec_index(args: &RulesIndexArgs) -> Result<(), ActualError> {
    let root = repo_root(args.path.as_ref());
    if args.clear {
        let _ = scope::cache::clear_all();
    }
    let resolved = scope::resolve(&root, args.rebuild || args.clear)?;
    if args.json {
        println!("{}", render_index_json(&resolved));
    } else {
        println!(
            "{}",
            render_index_panel(&resolved, &root, term_size::terminal_width())
        );
    }
    Ok(())
}

/// Documents, globs and terms, plus the terms the corpus rendered worthless.
///
/// The ubiquitous-term line is the headline diagnostic: it is where a corpus
/// admits that a filename prefix, or any other word on every document, carries
/// no signal at all.
fn render_index_panel(resolved: &ResolvedIndex, root: &Path, width: usize) -> String {
    let index = &resolved.index;
    let mut panel = Panel::titled("Scope index");
    panel = panel.line(&crate::rules::rules_dir(root).display().to_string());
    panel = panel.separator();
    panel = panel.kv("Documents", &index.len().to_string());
    panel = panel.kv("Source", resolved.source.label());
    panel = panel.kv(
        "Path globs",
        &index
            .documents
            .iter()
            .map(|doc| doc.globs.len())
            .sum::<usize>()
            .to_string(),
    );
    panel = panel.kv(
        "Distinct terms",
        &index.document_frequency.len().to_string(),
    );
    panel = panel.kv(
        "Documents without a path glob",
        &index
            .documents
            .iter()
            .filter(|doc| doc.globs.is_empty())
            .count()
            .to_string(),
    );

    let ubiquitous = index.ubiquitous_terms();
    panel = panel.separator();
    if ubiquitous.is_empty() {
        panel = panel.line("No term appears in every document.");
    } else {
        panel = panel.line("Terms in every document, contributing nothing:");
        panel = panel.line(&format!("  {}", ubiquitous.join(", ")));
    }

    if let Some(report) = &resolved.report {
        if !report.errors.is_empty() {
            panel = panel.separator().line(&format!(
                "{} file(s) failed to parse and are not indexed.",
                report.errors.len()
            ));
        }
    }
    panel.render(width)
}

#[derive(Serialize)]
struct JsonIndexSummary<'a> {
    source: &'a str,
    fingerprint: &'a str,
    format_version: u32,
    documents: usize,
    path_globs: usize,
    distinct_terms: usize,
    documents_without_globs: usize,
    ubiquitous_terms: Vec<&'a str>,
}

fn render_index_json(resolved: &ResolvedIndex) -> String {
    let index = &resolved.index;
    let payload = JsonIndexSummary {
        source: match resolved.source {
            IndexSource::Cached => "cached",
            IndexSource::Built => "built",
            IndexSource::Rebuilt => "rebuilt",
        },
        fingerprint: &index.fingerprint,
        format_version: index.format_version,
        documents: index.len(),
        path_globs: index.documents.iter().map(|d| d.globs.len()).sum(),
        distinct_terms: index.document_frequency.len(),
        documents_without_globs: index
            .documents
            .iter()
            .filter(|d| d.globs.is_empty())
            .count(),
        ubiquitous_terms: index.ubiquitous_terms(),
    };
    to_json(&payload)
}

// ── rules select ─────────────────────────────────────────────────────────

pub fn exec_select(args: &RulesSelectArgs) -> Result<(), ActualError> {
    let root = repo_root(args.repo.as_ref());
    let resolved = scope::resolve(&root, args.rebuild)?;
    let query = Query::new(args.plan.join(" ")).with_paths(args.files.clone());
    let matches = resolved.index.search(&query, args.limit);

    if args.json {
        println!(
            "{}",
            render_select_json(&resolved.index, &query, &matches, args.limit, args.explain)
        );
    } else {
        println!(
            "{}",
            render_select_panel(
                &resolved.index,
                &query,
                &matches,
                args.explain,
                args.limit,
                term_size::terminal_width(),
            )
        );
    }
    Ok(())
}

/// `0.83  cross-cutting-access-tokens-include-e410`, with the evidence beneath
/// it when `--explain` is on.
fn render_select_panel(
    index: &ScopeIndex,
    query: &Query,
    matches: &[Match],
    explain: bool,
    limit: usize,
    width: usize,
) -> String {
    let mut panel = Panel::titled("Rule selection");
    panel = panel.kv("Plan", &truncate(&query.text, 72));
    let paths = query.all_paths();
    if !paths.is_empty() {
        panel = panel.kv("Paths", &paths.join(", "));
    }
    panel = panel.kv("Indexed documents", &index.len().to_string());

    if matches.is_empty() {
        return panel
            .separator()
            .line("No rule document matched this plan.")
            .render(width);
    }

    panel = panel.separator();
    for hit in matches {
        panel = panel.kv(&format!("{:.2}", hit.score), &hit.slug);
        if !explain {
            continue;
        }
        if let Some(title) = &hit.title {
            panel = panel.line(&format!("      {}", truncate(title, 68)));
        }
        for contribution in &hit.contributions {
            let detail = if contribution.matched.is_empty() {
                String::new()
            } else {
                format!(" — {}", contribution.matched.join(", "))
            };
            panel = panel.line(&format!(
                "      {:<11} {:.2}{}",
                contribution.field.as_str(),
                contribution.weighted,
                detail
            ));
        }
        for glob in hit.matched_globs.iter().take(3) {
            panel = panel.line(&format!(
                "      glob        {} ~ {} ({} segments{})",
                glob.glob,
                glob.query_path,
                glob.segments,
                if glob.exact { ", exact" } else { "" }
            ));
        }
    }

    if explain {
        let ubiquitous = index.ubiquitous_terms();
        panel = panel.separator();
        panel = panel.line(&format!(
            "Terms carrying no signal in this corpus: {}",
            if ubiquitous.is_empty() {
                "none".to_string()
            } else {
                ubiquitous.join(", ")
            }
        ));
        panel = panel.separator();
        panel = panel.line(&format!("Filename scan would have chosen (cap {}):", limit));
        // Same budget as this invocation. Comparing against the status-quo
        // cap of 5 while `--limit` is 10 (the default) or 20 would make the
        // two answers look different for a reason that is not the selector.
        let scan = baseline::select(index, &query.text, limit);
        if scan.is_empty() {
            panel = panel.line("  nothing — no filename segment matched the plan");
        } else {
            for hit in &scan {
                panel = panel.line(&format!(
                    "  {} ({})",
                    hit.slug,
                    hit.matched_terms.join(", ")
                ));
            }
        }
    }

    panel = panel.separator().line(&format!(
        "{} of {} documents shown",
        matches.len().min(limit),
        index.len()
    ));
    panel.render(width)
}

#[derive(Serialize)]
struct JsonSelection<'a> {
    plan: &'a str,
    paths: Vec<String>,
    indexed_documents: usize,
    limit: usize,
    matches: &'a [Match],
    #[serde(skip_serializing_if = "Option::is_none")]
    explain: Option<JsonExplain<'a>>,
}

#[derive(Serialize)]
struct JsonExplain<'a> {
    ubiquitous_terms: Vec<&'a str>,
    filename_scan: Vec<JsonBaselineHit>,
}

#[derive(Serialize)]
struct JsonBaselineHit {
    slug: String,
    matched_terms: Vec<String>,
}

fn render_select_json(
    index: &ScopeIndex,
    query: &Query,
    matches: &[Match],
    limit: usize,
    explain: bool,
) -> String {
    let payload = JsonSelection {
        plan: &query.text,
        paths: query.all_paths(),
        indexed_documents: index.len(),
        limit,
        matches,
        explain: explain.then(|| JsonExplain {
            ubiquitous_terms: index.ubiquitous_terms(),
            filename_scan: baseline::select(index, &query.text, limit)
                .into_iter()
                .map(|hit| JsonBaselineHit {
                    slug: hit.slug,
                    matched_terms: hit.matched_terms,
                })
                .collect(),
        }),
    };
    to_json(&payload)
}

// ── rules eval ───────────────────────────────────────────────────────────

pub fn exec_eval(args: &RulesEvalArgs) -> Result<(), ActualError> {
    let root = repo_root(args.repo.as_ref());
    let cases = load_golden_set(&args.golden)?;
    let resolved = scope::resolve(&root, args.rebuild)?;
    let weights = ablated_weights(&args.ablate)?;
    let comparison = run_evaluation(&resolved.index, &cases, args.limit, &weights);

    if args.json {
        println!("{}", to_json(&comparison));
    } else {
        println!(
            "{}",
            render_eval_panel(&comparison, term_size::terminal_width())
        );
    }
    Ok(())
}

/// Apply `--ablate` switches to the default weights.
///
/// An unknown field name is an error rather than a silent no-op: an ablation
/// that quietly measured nothing would be reported as a result.
pub(crate) fn ablated_weights(ablate: &[String]) -> Result<Weights, ActualError> {
    let mut weights = Weights::default();
    for name in ablate {
        let field = Field::ALL
            .iter()
            .find(|field| field.as_str() == name.to_ascii_lowercase())
            .ok_or_else(|| {
                ActualError::ConfigError(format!(
                    "unknown signal `{name}`; expected one of: {}",
                    Field::ALL
                        .iter()
                        .map(|f| f.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
        weights = weights.without(*field);
    }
    Ok(weights)
}

/// Read a golden set from JSON, reporting the file that failed rather than the
/// bare serde message — a golden set is usually hand-written, and "which file"
/// is half the fix.
pub(crate) fn load_golden_set(path: &Path) -> Result<Vec<GoldenCase>, ActualError> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        ActualError::ConfigError(format!("failed to read golden set {}: {e}", path.display()))
    })?;
    serde_json::from_str(&text).map_err(|e| {
        ActualError::ConfigError(format!(
            "failed to parse golden set {}: {e}",
            path.display()
        ))
    })
}

/// Both selectors, over the same cases, at the same cap.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Comparison {
    pub(crate) limit: usize,
    pub(crate) cases: usize,
    pub(crate) weights: Weights,
    pub(crate) scope_index: EvaluationReport,
    pub(crate) filename_scan: EvaluationReport,
}

impl Comparison {
    /// True when the index beats the filename scan on pooled F1 — the single
    /// claim this task has to support.
    pub fn index_wins(&self) -> bool {
        self.scope_index.micro.f1 > self.filename_scan.micro.f1
    }
}

/// Score both selectors on `cases`, each allowed `limit` documents.
///
/// The cap is shared deliberately. Giving the index a larger budget than the
/// status quo would buy recall with an advantage the status quo never had, and
/// the comparison would prove nothing.
pub(crate) fn run_evaluation(
    index: &ScopeIndex,
    cases: &[GoldenCase],
    limit: usize,
    weights: &Weights,
) -> Comparison {
    let mut index_cases = Vec::with_capacity(cases.len());
    let mut scan_cases = Vec::with_capacity(cases.len());

    for case in cases {
        let query = Query::new(case.plan.clone()).with_paths(case.paths.clone());
        let selected: Vec<String> = index
            .search_weighted(&query, limit, weights)
            .into_iter()
            .map(|hit| hit.slug)
            .collect();
        index_cases.push(CaseResult {
            name: case.name.clone(),
            scores: Scores::measure(&selected, &case.expected),
            selected,
            expected: case.expected.clone(),
        });

        let scanned: Vec<String> = baseline::select(index, &case.plan, limit)
            .into_iter()
            .map(|hit| hit.slug)
            .collect();
        scan_cases.push(CaseResult {
            name: case.name.clone(),
            scores: Scores::measure(&scanned, &case.expected),
            selected: scanned,
            expected: case.expected.clone(),
        });
    }

    Comparison {
        limit,
        cases: cases.len(),
        weights: *weights,
        scope_index: EvaluationReport::new("scope-index", index_cases),
        filename_scan: EvaluationReport::new("filename-scan", scan_cases),
    }
}

fn render_eval_panel(comparison: &Comparison, width: usize) -> String {
    let mut panel = Panel::titled("Scope index evaluation");
    panel = panel.kv("Cases", &comparison.cases.to_string());
    panel = panel.kv("Documents per selection", &comparison.limit.to_string());
    let disabled: Vec<&str> = Field::ALL
        .iter()
        .filter(|field| comparison.weights.get(**field) == 0.0)
        .map(|field| field.as_str())
        .collect();
    if !disabled.is_empty() {
        panel = panel.kv("Signals switched off", &disabled.join(", "));
    }
    panel = panel.separator();
    panel = panel.line(&comparison.scope_index.summary_line());
    panel = panel.line(&comparison.filename_scan.summary_line());
    panel = panel.separator();
    panel = panel.line(&format!(
        "Scope index {} the filename scan on pooled F1 ({:.2} vs {:.2}).",
        if comparison.index_wins() {
            "beats"
        } else {
            "does not beat"
        },
        comparison.scope_index.micro.f1,
        comparison.filename_scan.micro.f1,
    ));

    let misses = comparison.scope_index.total_misses();
    if !misses.is_empty() {
        panel = panel
            .separator()
            .line("Plans that retrieved no expected rule:");
        for case in misses {
            panel = panel.line(&format!("  {}", case.name));
        }
    }
    panel.render(width)
}

// ── shared ───────────────────────────────────────────────────────────────

fn to_json<T: Serialize>(payload: &T) -> String {
    serde_json::to_string_pretty(payload)
        .expect("scope index report is serializable — this is a programmer error")
}

/// Shorten to `width` characters with an ellipsis, counting characters rather
/// than bytes so a multi-byte plan cannot panic.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::{tempdir, TempDir};

    use crate::rules::scope::eval::GoldenCase;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    /// A cache hit carries no load report, because it never reads the rule
    /// files. The panel must still render, and must omit the parse-failure line
    /// it can no longer know about.
    #[test]
    fn test_index_panel_renders_a_cache_hit_without_a_load_report() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = sample();

        scope::resolve(root.path(), true).unwrap();
        let cached = scope::resolve(root.path(), false).unwrap();
        assert!(cached.report.is_none(), "expected a cache hit");

        let out = render_index_panel(&cached, root.path(), 100);
        assert!(out.contains("(cached)"), "{out}");
        assert!(!out.contains("failed to parse"), "{out}");
    }

    /// The eval panel states a verdict either way. Only the losing wording was
    /// ever rendered, so the winning sentence went out unchecked.
    #[test]
    fn test_eval_panel_states_that_the_index_beats_the_scan() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = sample();
        let index = resolved(root.path()).index;

        // Phrased to avoid every filename segment, so the scan finds nothing
        // while the prose and title signals still reach the right document.
        let cases = vec![GoldenCase {
            name: "rs256-key-rotation".to_string(),
            plan: "rotate RS256 keys used by the public API".to_string(),
            paths: Vec::new(),
            expected: vec!["cross-cutting-token-signing-e410".to_string()],
        }];

        let comparison = run_evaluation(&index, &cases, 5, &Weights::default());
        assert!(comparison.index_wins(), "{comparison:?}");
        let out = render_eval_panel(&comparison, 100);
        assert!(out.contains("Scope index beats the filename scan"), "{out}");
    }

    /// The JSON `source` field must report each of the three ways an index can
    /// be obtained. Only the forced rebuild was ever exercised, so a mislabelled
    /// cache hit would have shipped unnoticed.
    #[test]
    fn test_index_json_reports_built_then_cached_then_rebuilt() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);
        let root = sample();

        for expected in ["built", "cached"] {
            let outcome = scope::resolve(root.path(), false).unwrap();
            let value: serde_json::Value =
                serde_json::from_str(&render_index_json(&outcome)).unwrap();
            assert_eq!(value["source"], expected);
        }

        let outcome = scope::resolve(root.path(), true).unwrap();
        let value: serde_json::Value = serde_json::from_str(&render_index_json(&outcome)).unwrap();
        assert_eq!(value["source"], "rebuilt");
    }

    /// When no term is shared by every document there is nothing to retire, and
    /// `--explain` must say "none" rather than printing an empty list.
    #[test]
    fn test_select_explain_reports_no_worthless_terms_when_none_are_shared() {
        const WIDGETS: &str =
            "# Widgets\n\nGoverns widgets.\n\n### Rules\n\n- **R-W-001** MUST: w.\n";
        const GADGETS: &str =
            "# Gadgets\n\nConcerns gadgets.\n\n### Rules\n\n- **R-G-001** MUST: g.\n";

        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        // Two documents with disjoint vocabulary and no shared filename
        // segment, so the ubiquitous set is genuinely empty.
        let root = seed(&[("widgets.md", WIDGETS), ("gadgets.md", GADGETS)]);
        let index = resolved(root.path()).index;
        let ubiquitous = index.ubiquitous_terms();
        assert!(ubiquitous.is_empty(), "{ubiquitous:?}");

        let query = Query::new("widgets");
        let matches = index.search(&query, 5);
        let out = render_select_panel(&index, &query, &matches, true, 5, 100);
        assert!(
            out.contains("Terms carrying no signal in this corpus: none"),
            "{out}"
        );
    }
    const OAUTH: &str = "# Adopt RS256: Token Signing\n\nThese rules are ALWAYS ACTIVE for OAuth token issuance and token signing in `services/auth/oauth/`.\n\n### Rules\n\n- **R-A-001** MUST: sign with RS256.\n\n### Verify\n\n```bash\ngrep -r \"jwt.sign\" services/auth/oauth/ --include=\"*.ts\"\n```\n";
    const TERRAFORM: &str = "# Pin Terraform Providers\n\nThese rules are ALWAYS ACTIVE for Terraform configuration in `infra/terraform/`.\n\n### Rules\n\n- **R-B-001** MUST: pin providers.\n";
    const BAD: &str = "no rules section here\n";

    /// Helper: an isolated config directory, so no test touches the real cache.
    /// Returns the guards, which must outlive the test body.
    fn isolated_config(home: &TempDir) -> (EnvGuard, EnvGuard) {
        (
            EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap()),
            EnvGuard::remove("ACTUAL_CONFIG"),
        )
    }

    /// Helper: a repository root whose `.actual/rules/` holds `files`.
    fn seed(files: &[(&str, &str)]) -> TempDir {
        let root = tempdir().unwrap();
        let dir = crate::rules::rules_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        for (name, contents) in files {
            std::fs::write(dir.join(name), contents).unwrap();
        }
        root
    }

    /// Helper: the two-cluster sample every rendering test uses.
    fn sample() -> TempDir {
        seed(&[
            ("cross-cutting-token-signing-e410.md", OAUTH),
            ("cross-cutting-provider-pinning-c3d4.md", TERRAFORM),
        ])
    }

    fn resolved(root: &Path) -> ResolvedIndex {
        scope::resolve(root, true).unwrap()
    }

    // ── rules index ──────────────────────────────────────────────────────

    #[test]
    fn test_index_panel_reports_counts_and_the_dead_prefix() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = sample();
        let out = render_index_panel(&resolved(root.path()), root.path(), 100);

        assert!(out.contains("Scope index"));
        assert!(out.contains(".actual/rules"));
        assert!(out.contains("Documents: 2"));
        assert!(out.contains("(rebuilt)"));
        // The headline diagnostic: the shared filename prefix carries nothing.
        assert!(out.contains("Terms in every document, contributing nothing:"));
        assert!(out.contains("cross"));
        assert!(out.contains("cutting"));
    }

    #[test]
    fn test_index_panel_reports_files_that_failed_to_parse() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = seed(&[("a.md", OAUTH), ("b.md", BAD)]);
        let out = render_index_panel(&resolved(root.path()), root.path(), 100);
        assert!(out.contains("1 file(s) failed to parse"));
    }

    #[test]
    fn test_index_panel_for_an_empty_rule_set() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = tempdir().unwrap();
        let out = render_index_panel(&resolved(root.path()), root.path(), 80);
        assert!(out.contains("Documents: 0"));
        assert!(out.contains("No term appears in every document."));
    }

    #[test]
    fn test_index_json_carries_the_summary() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = sample();
        let value: serde_json::Value =
            serde_json::from_str(&render_index_json(&resolved(root.path()))).unwrap();

        assert_eq!(value["documents"], 2);
        assert_eq!(value["source"], "rebuilt");
        assert_eq!(value["format_version"], 1);
        assert!(value["path_globs"].as_u64().unwrap() >= 1);
        assert!(value["fingerprint"].as_str().unwrap().len() > 16);
        let ubiquitous = value["ubiquitous_terms"].as_array().unwrap();
        assert!(ubiquitous.iter().any(|t| t == "cross"));
    }

    #[test]
    fn test_exec_index_renders_both_formats() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = sample();
        for json in [false, true] {
            let args = RulesIndexArgs {
                path: Some(root.path().to_path_buf()),
                rebuild: true,
                clear: false,
                json,
            };
            assert!(exec_index(&args).is_ok());
        }
    }

    /// `--clear` drops every cached index, including those left by other
    /// repositories, then rebuilds this one.
    #[test]
    fn test_exec_index_clear_prunes_every_cached_index() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let other = sample();
        scope::resolve(other.path(), true).unwrap();
        let other_dir = crate::rules::rules_dir(other.path());
        assert!(scope::cache::load(&other_dir, &scope::cache::fingerprint(&other_dir)).is_some());

        let root = sample();
        let args = RulesIndexArgs {
            path: Some(root.path().to_path_buf()),
            rebuild: false,
            clear: true,
            json: true,
        };
        assert!(exec_index(&args).is_ok());

        assert!(
            scope::cache::load(&other_dir, &scope::cache::fingerprint(&other_dir)).is_none(),
            "indexes from other repositories must be pruned"
        );
        let dir = crate::rules::rules_dir(root.path());
        assert!(
            scope::cache::load(&dir, &scope::cache::fingerprint(&dir)).is_some(),
            "this repository's index is rebuilt after the prune"
        );
    }

    // ── rules select ─────────────────────────────────────────────────────

    /// Helper: a select panel for `plan` over the sample corpus.
    fn select_panel(root: &Path, plan: &str, files: Vec<String>, explain: bool) -> String {
        let index = resolved(root).index;
        let query = Query::new(plan).with_paths(files);
        let matches = index.search(&query, 5);
        render_select_panel(&index, &query, &matches, explain, 5, 110)
    }

    #[test]
    fn test_select_panel_ranks_the_matching_cluster() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = sample();
        let out = select_panel(
            root.path(),
            "rotate the OAuth signing key",
            Vec::new(),
            false,
        );
        assert!(out.contains("Rule selection"));
        assert!(out.contains("cross-cutting-token-signing-e410"));
        assert!(!out.contains("cross-cutting-provider-pinning-c3d4"));
        assert!(out.contains("of 2 documents shown"));
    }

    #[test]
    fn test_select_panel_reports_no_match() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = sample();
        let out = select_panel(
            root.path(),
            "kubernetes ingress controller",
            Vec::new(),
            false,
        );
        assert!(out.contains("No rule document matched this plan."));
    }

    /// `--explain` is the diagnosability requirement: every hit must show which
    /// signal carried it, and what the status quo would have chosen instead.
    #[test]
    fn test_select_panel_explain_shows_evidence_and_the_status_quo() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = sample();
        let out = select_panel(
            root.path(),
            "rotate the OAuth signing key",
            Vec::new(),
            true,
        );

        assert!(out.contains("scope"));
        assert!(out.contains("token"));
        assert!(out.contains("Terms carrying no signal in this corpus:"));
        assert!(out.contains("Filename scan would have chosen (cap 5):"));
    }

    /// The filename scan is capped at the same `--limit` as the index, so
    /// `--explain` compares the two selectors on equal budget.
    #[test]
    fn test_select_explain_caps_the_filename_scan_at_the_selection_limit() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = sample();
        let index = resolved(root.path()).index;
        // Both filenames carry `cross-cutting-`; a cap of 1 must keep only one
        // of them. `token signing` keeps the index from returning no matches,
        // which would skip the explain block entirely.
        let query = Query::new("token signing cross-cutting");
        let matches = index.search(&query, 1);

        let panel = render_select_panel(&index, &query, &matches, true, 1, 110);
        assert!(panel.contains("Filename scan would have chosen (cap 1):"));

        let value: serde_json::Value =
            serde_json::from_str(&render_select_json(&index, &query, &matches, 1, true)).unwrap();
        assert_eq!(value["limit"], 1);
        assert_eq!(
            value["explain"]["filename_scan"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn test_select_panel_explain_reports_an_empty_filename_scan() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = sample();
        // `issuance` appears in the scope prose but in no filename.
        let out = select_panel(root.path(), "issuance", Vec::new(), true);
        assert!(out.contains("nothing — no filename segment matched the plan"));
    }

    #[test]
    fn test_select_panel_shows_a_matched_glob_for_a_named_path() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = sample();
        let out = select_panel(
            root.path(),
            "change signing",
            vec!["services/auth/oauth/token.ts".to_string()],
            true,
        );
        assert!(out.contains("Paths: services/auth/oauth/token.ts"));
        assert!(out.contains("services/auth/oauth/**"));
        assert!(out.contains("exact"));
    }

    #[test]
    fn test_select_json_carries_matches_and_explanation() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = sample();
        let index = resolved(root.path()).index;
        let query = Query::new("rotate the OAuth signing key");
        let matches = index.search(&query, 5);

        let value: serde_json::Value =
            serde_json::from_str(&render_select_json(&index, &query, &matches, 5, true)).unwrap();
        assert_eq!(value["indexed_documents"], 2);
        assert_eq!(
            value["matches"][0]["slug"],
            "cross-cutting-token-signing-e410"
        );
        assert!(value["explain"]["ubiquitous_terms"].is_array());
        assert!(value["explain"]["filename_scan"].is_array());

        // Without `--explain` the diagnostic block is absent, not empty.
        let plain: serde_json::Value =
            serde_json::from_str(&render_select_json(&index, &query, &matches, 5, false)).unwrap();
        assert!(plain.get("explain").is_none());
    }

    #[test]
    fn test_exec_select_renders_both_formats() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = sample();
        for (json, explain) in [(false, false), (false, true), (true, true)] {
            let args = RulesSelectArgs {
                plan: vec![
                    "rotate".to_string(),
                    "signing".to_string(),
                    "keys".to_string(),
                ],
                repo: Some(root.path().to_path_buf()),
                files: vec!["services/auth/oauth/token.ts".to_string()],
                limit: 5,
                explain,
                json,
                rebuild: true,
            };
            assert!(exec_select(&args).is_ok());
        }
    }

    // ── rules eval ───────────────────────────────────────────────────────

    /// Helper: a golden set written to a temp file, and its path.
    fn write_golden(dir: &TempDir, cases: &[GoldenCase]) -> PathBuf {
        let path = dir.path().join("golden.json");
        std::fs::write(&path, serde_json::to_string(cases).unwrap()).unwrap();
        path
    }

    fn oauth_case() -> GoldenCase {
        GoldenCase {
            name: "oauth".to_string(),
            plan: "rotate the OAuth signing key and shorten token lifetime".to_string(),
            paths: Vec::new(),
            expected: vec!["cross-cutting-token-signing-e410".to_string()],
        }
    }

    #[test]
    fn test_run_evaluation_scores_both_selectors_at_the_same_cap() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = sample();
        let comparison = run_evaluation(
            &resolved(root.path()).index,
            &[oauth_case()],
            2,
            &Weights::default(),
        );
        assert_eq!(comparison.cases, 1);
        assert_eq!(comparison.limit, 2);
        assert_eq!(comparison.scope_index.cases.len(), 1);
        assert_eq!(comparison.filename_scan.cases.len(), 1);
        assert_eq!(comparison.scope_index.micro.true_positives, 1);
    }

    #[test]
    fn test_eval_panel_states_the_comparison_and_lists_misses() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = sample();
        let mut miss = oauth_case();
        miss.name = "impossible".to_string();
        miss.plan = "kubernetes ingress controller".to_string();

        let comparison = run_evaluation(
            &resolved(root.path()).index,
            &[oauth_case(), miss],
            2,
            &Weights::default(),
        );
        let out = render_eval_panel(&comparison, 110);
        assert!(out.contains("Scope index evaluation"));
        assert!(out.contains("Cases: 2"));
        assert!(out.contains("scope-index"));
        assert!(out.contains("filename-scan"));
        assert!(out.contains("Plans that retrieved no expected rule:"));
        assert!(out.contains("impossible"));
    }

    #[test]
    fn test_eval_panel_names_the_signals_switched_off() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = sample();
        let weights = Weights::default()
            .without(Field::Scope)
            .without(Field::Title);
        let comparison = run_evaluation(&resolved(root.path()).index, &[oauth_case()], 2, &weights);
        let out = render_eval_panel(&comparison, 110);
        assert!(out.contains("Signals switched off: scope, title"));
    }

    #[test]
    fn test_ablated_weights_zeroes_named_signals() {
        let weights = ablated_weights(&["path".to_string(), "SLUG".to_string()]).unwrap();
        assert_eq!(weights.get(Field::Path), 0.0);
        assert_eq!(weights.get(Field::Slug), 0.0);
        assert_eq!(
            weights.get(Field::Scope),
            Weights::default().get(Field::Scope)
        );
        assert_eq!(ablated_weights(&[]).unwrap(), Weights::default());
    }

    /// An unknown signal name is an error, not a silent no-op: an ablation that
    /// measured nothing would be reported as a result.
    #[test]
    fn test_ablated_weights_rejects_an_unknown_signal() {
        let err = ablated_weights(&["nonsense".to_string()]).unwrap_err();
        assert!(err.to_string().contains("unknown signal `nonsense`"));
        assert!(err.to_string().contains("path-terms"));
    }

    #[test]
    fn test_load_golden_set_reads_cases() {
        let dir = tempdir().unwrap();
        let path = write_golden(&dir, &[oauth_case()]);
        let cases = load_golden_set(&path).unwrap();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].name, "oauth");
    }

    #[test]
    fn test_load_golden_set_names_the_file_it_could_not_read_or_parse() {
        let missing = load_golden_set(Path::new("/no/such/golden.json")).unwrap_err();
        assert!(missing.to_string().contains("failed to read golden set"));
        assert!(missing.to_string().contains("golden.json"));

        let dir = tempdir().unwrap();
        let path = dir.path().join("broken.json");
        std::fs::write(&path, "{ not json").unwrap();
        let broken = load_golden_set(&path).unwrap_err();
        assert!(broken.to_string().contains("failed to parse golden set"));
        assert!(broken.to_string().contains("broken.json"));
    }

    #[test]
    fn test_exec_eval_renders_both_formats() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = sample();
        let golden_dir = tempdir().unwrap();
        let golden = write_golden(&golden_dir, &[oauth_case()]);

        for json in [false, true] {
            let args = RulesEvalArgs {
                golden: golden.clone(),
                repo: Some(root.path().to_path_buf()),
                limit: 5,
                ablate: vec!["slug".to_string()],
                json,
                rebuild: false,
            };
            assert!(exec_eval(&args).is_ok());
        }
    }

    #[test]
    fn test_exec_eval_rejects_an_unknown_ablation() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = sample();
        let golden_dir = tempdir().unwrap();
        let golden = write_golden(&golden_dir, &[oauth_case()]);

        let args = RulesEvalArgs {
            golden,
            repo: Some(root.path().to_path_buf()),
            limit: 5,
            ablate: vec!["nope".to_string()],
            json: false,
            rebuild: false,
        };
        assert!(exec_eval(&args).is_err());
    }

    /// `--rebuild` skips a still-valid cache, so a measurement cannot silently
    /// score against stale signal data.
    #[test]
    fn test_exec_eval_rebuild_ignores_a_cached_index() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let home = tempdir().unwrap();
        let _guards = isolated_config(&home);

        let root = sample();
        let first = scope::resolve(root.path(), true).unwrap();
        assert_eq!(first.index.len(), 2);

        let dir = crate::rules::rules_dir(root.path());
        let mut tampered = first.index.clone();
        tampered.documents.clear();
        tampered.document_frequency.clear();
        scope::cache::store(&dir, &tampered);
        assert!(scope::cache::load(&dir, &tampered.fingerprint)
            .unwrap()
            .is_empty());

        let golden_dir = tempdir().unwrap();
        let args = RulesEvalArgs {
            golden: write_golden(&golden_dir, &[oauth_case()]),
            repo: Some(root.path().to_path_buf()),
            limit: 5,
            ablate: Vec::new(),
            json: true,
            rebuild: true,
        };
        assert!(exec_eval(&args).is_ok());

        let rebuilt = scope::cache::load(&dir, &scope::cache::fingerprint(&dir)).unwrap();
        assert_eq!(rebuilt.len(), 2);
    }

    // ── shared helpers ───────────────────────────────────────────────────

    #[test]
    fn test_truncate_counts_characters_not_bytes() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdef", 4), "abc…");
        // A multi-byte plan must not panic on a byte-boundary slice.
        assert_eq!(truncate("ααααα", 3), "αα…");
        assert_eq!(truncate("", 0), "");
    }
}
