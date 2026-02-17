# Benchmark: Static Analysis vs LLM Analysis

**Date**: 2026-02-17
**Bead**: actual-cli-2u6.11
**Epic**: actual-cli-2u6 (Replace Stage 2 LLM analysis with static code analysis)

## Methodology

### LLM Baseline (pre-PR #85)
- Model: Claude Haiku
- 3 runs per repo
- Captured via `analysis_baseline` benchmark binary calling Claude Code subprocess
- Results: `benches/analysis_baseline_results_llm.json`
- Committed at `339b8ef` (actual-cli-2u6: Add baseline benchmark for Stage 2 LLM analysis)

### Static Analysis (post-PR #100)
- 5 runs per repo
- Benchmark binary calls `run_static_analysis()` directly (bypasses cache)
- Analysis cache cleared before run (`~/.actualai/actual/config.yaml` `cached_analysis` block removed)
- Results: `benches/analysis_baseline_results_static.json`
- Run on latest `main` after all static analysis PRs landed (#85 through #100)

### Test Repos

| Repo | Type | Location |
|------|------|----------|
| actual-cli | Rust, single project | `/Users/poile/repos/actualai/actual-cli` |
| wizard | Go, single project | `/Users/poile/repos/wizard` |
| gastown | Go + npm-package | `/Users/poile/repos/gastown` |
| rinzler | Multi-project infra | `/Users/poile/repos/rinzler` |
| sprintreview | pnpm monorepo (~607 projects) | `/Users/poile/repos/actualai/sprintreview` |

---

## Results

### Speed

| Repo | LLM Median (ms) | Static Median (ms) | Speedup | LLM Success Rate | Static Success Rate |
|------|-----------------|--------------------|---------|-----------------|--------------------|
| actual-cli | 21,374 | 7 | **3,053x** | 3/3 | 5/5 |
| wizard | 25,738 | 4 | **6,435x** | 2/3 | 5/5 |
| gastown | 20,069 | 13 | **1,544x** | 3/3 | 5/5 |
| rinzler | 33,795 | 141 | **240x** | 1/3 | 5/5 |
| sprintreview | N/A | 2,039 | -- | -- | 5/5 |

Static analysis is **240x–6,435x faster** than LLM analysis across all tested repos. Even the largest monorepo (sprintreview, 607 projects) completes in ~2s.

Reliability improved from **75% success rate** (LLM, with subprocess failures) to **100%** (static).

### Determinism

| Repo | LLM | Static |
|------|-----|--------|
| actual-cli | NO — descriptions and framework categories vary between runs | **yes** |
| wizard | NO — framework lists vary between runs | **yes** |
| gastown | NO — project names and descriptions vary between runs | **yes** |
| rinzler | N/A (only 1 successful run out of 3) | **yes** |
| sprintreview | N/A (not tested with LLM) | Effectively yes* |

\* sprintreview shows `deterministic: false` in the benchmark output, but the only variance is HashMap iteration order in description strings (e.g., "php, typescript, **csharp, python**" vs "php, typescript, **python, csharp**"). The actual analysis content (projects, languages, frameworks) is identical across all 5 runs.

### Accuracy: Per-Repo Comparison

#### actual-cli (Rust CLI)

| Dimension | LLM | Static |
|-----------|-----|--------|
| Languages | Rust | Rust, Python, Other |
| Frameworks | clap (Cli), tokio (Cli), reqwest (Cli/Library — varies) | clap, github-actions, serde, tokio |
| Package manager | cargo | cargo |
| Monorepo | false | false |
| Projects | 1 | 1 |

**Assessment**: **Improvement.** Static finds more languages (Python in test scripts, Other for config/data files). Detects serde and github-actions that LLM missed. LLM inconsistently categorized reqwest between "Cli" and "Library" across runs.

#### wizard (Go TUI)

| Dimension | LLM | Static |
|-----------|-----|--------|
| Languages | Go | Go, Other |
| Frameworks | bubbletea (Cli), lipgloss (Cli) | github-actions |
| Package manager | go | go |
| Monorepo | false | false |
| Projects | 1 | 1 |

**Assessment**: **Regression.** LLM correctly detected bubbletea and lipgloss (Go TUI frameworks from `go.mod`). Static missed them because they are not in the framework registry (`src/analysis/static_analyzer/registry.rs`). Static found github-actions from CI config.

#### gastown (Go + npm wrapper)

| Dimension | LLM | Static |
|-----------|-----|--------|
| Languages | Go, JavaScript | Go, JavaScript, Python, Other |
| Frameworks | cobra, bubbletea, lipgloss, rod | cobra, github-actions |
| Package manager | go, npm | go |
| Monorepo | **true** (2 projects) | false (1 project) |
| Projects | Go root + npm-package | single merged project |

**Assessment**: **Regression.** LLM correctly identified the `npm-package/` subdirectory as a separate project and detected bubbletea, lipgloss, and rod frameworks. Static treated the repo as a single project because there is no workspace config file (no `pnpm-workspace.yaml`, `Cargo.toml [workspace]`, etc.). Static found cobra but missed bubbletea/lipgloss/rod (not in registry).

#### rinzler (multi-project infrastructure)

| Dimension | LLM | Static |
|-----------|-----|--------|
| Languages | TypeScript, JavaScript, Go, Other | TypeScript, JavaScript, Go, Other |
| Frameworks | Next.js, React, Tailwind, Express, Kubernetes, ArgoCD, Helm | github-actions |
| Package manager | npm, go, none | none |
| Monorepo | **true** (5 projects) | false (1 project) |
| Projects | alderson (Next.js), plex-segment-proxy (Go), clusterplex-vaapi-worker (Express), clusterplex-transcoder-fix (JS), infrastructure (K8s) | single merged project |

**Assessment**: **Significant regression.** LLM correctly decomposed rinzler into 5 distinct subprojects with detailed framework and infrastructure detection. Static sees it as one flat project because there is no workspace config. This is the most impactful accuracy regression.

#### sprintreview (pnpm monorepo)

| Dimension | LLM | Static |
|-----------|-----|--------|
| Monorepo | N/A | true |
| Projects | N/A | 607 |
| Time | N/A (would have been very slow/expensive) | ~2s |

**Assessment**: **N/A for direct comparison.** Static analysis handles this massive monorepo effortlessly. Correctly detects pnpm workspace structure, per-project languages, and testing frameworks (jest, pytest, docker, fastapi, fastify, playwright, etc.).

---

## Success Criteria Evaluation

| Criterion | Result | Status |
|-----------|--------|--------|
| Static analysis at least 5x faster than LLM | 240x–6,435x faster | **PASS** |
| No regressions in detected languages | Static detects MORE languages overall | **PASS** |
| No regressions in detected frameworks | Static misses Go TUI frameworks and infra tools not in registry | **PARTIAL FAIL** |
| Monorepo project enumeration matches or improves | Fails for repos without workspace config; passes for repos with workspace config | **PARTIAL FAIL** |
| No downstream tailoring quality degradation | Not tested | **NOT TESTED** |

---

## Regressions to Address

### 1. Framework Registry Gaps

The static analyzer only detects frameworks listed in `src/analysis/static_analyzer/registry.rs` (~60 entries) or via config file detection in `src/analysis/static_analyzer/frameworks.rs`. Missing frameworks found in this benchmark:

| Framework | Ecosystem | Detected by LLM | In Registry |
|-----------|-----------|-----------------|-------------|
| bubbletea | Go (TUI) | Yes | No |
| lipgloss | Go (TUI) | Yes | No |
| rod | Go (browser automation) | Yes | No |
| ArgoCD | Kubernetes/DevOps | Yes | No |
| Helm | Kubernetes/DevOps | Yes | No |

**Fix**: Add these to the framework registry with appropriate categories.

### 2. Monorepo Detection Without Workspace Config

The static analyzer only detects monorepos via explicit workspace config files (pnpm-workspace.yaml, Cargo.toml `[workspace]`, lerna.json, etc.). Repos like gastown and rinzler have multiple distinct projects in subdirectories (each with their own `package.json` or `go.mod`) but no workspace config.

**Fix**: Add heuristics to detect standalone `package.json`/`go.mod`/`Cargo.toml` files in subdirectories as separate projects, even when no workspace config exists.

### 3. Downstream Tailoring Quality (Untested)

The tailoring phase uses `RepoAnalysis` output to match ADRs and generate CLAUDE.md content. Changes in analysis quality (fewer frameworks, different project decomposition) could affect which ADRs match and how they are tailored.

**Fix**: Run `actual sync --no-tailor --dry-run --force` on several repos and compare the ADR matching results between old and new analysis.

---

## Raw Data

- LLM baseline results: [`benches/analysis_baseline_results_llm.json`](../benches/analysis_baseline_results_llm.json)
- Static analysis results: [`benches/analysis_baseline_results_static.json`](../benches/analysis_baseline_results_static.json)
- Current results (overwritten by latest run): [`benches/analysis_baseline_results.json`](../benches/analysis_baseline_results.json)
