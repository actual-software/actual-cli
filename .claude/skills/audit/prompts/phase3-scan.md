# Phase 3: Automated Scan

You are a grep-scan agent for a codebase audit. Your job is to run a specific set of automated searches and report structured results.

## Inputs

- `SCAN_CATEGORY`: {{SCAN_CATEGORY}}
- `COMMANDS`: See the scan definition below.

## Scan Definitions

Run the commands for your assigned category. Report every match with file, line, and context.

### suppressed-errors
```bash
rg '\.ok\(\)' --glob '*.rs' --line-number | grep -v '_test\|tests/'
rg 'let _ =' --glob '*.rs' --line-number | grep -v '_test\|tests/'
rg 'drop\(.*\)' --glob '*.rs' --line-number | grep -v '_test\|tests/'
```
**Why it matters:** Discarded Results and silently dropped errors hide failures. Non-trivial operations should propagate errors.

### todo-markers
```bash
rg 'TODO|FIXME|HACK|XXX|STUB|NOCHECKIN' --glob '*.rs' --line-number
rg 'todo!\(\)|unimplemented!\(\)' --glob '*.rs' --line-number
rg 'not implemented|#\[allow' --glob '*.rs' --line-number
```
**Why it matters:** Indicates incomplete work or known technical debt.

### skipped-tests
```bash
rg '#\[ignore\]' --glob '*.rs' --line-number
rg '#\[cfg\(not\(test\)\)\]' --glob '*.rs' --line-number
rg 'fn test.*\{\s*\}' --glob '*.rs' --line-number
```
**Why it matters:** Skipped or empty tests reduce coverage and give false confidence.

### unsafe-usage
```bash
rg 'unsafe ' --glob '*.rs' --line-number
rg 'as \*const\|as \*mut' --glob '*.rs' --line-number
rg 'transmute' --glob '*.rs' --line-number
```
**Why it matters:** Unsafe code bypasses Rust's safety guarantees and requires careful justification and review.

### hardcoded-values
```bash
rg 'Duration::from_secs\(|Duration::from_millis\(' --glob '*.rs' --line-number | grep -v '_test\|tests/'
rg -i 'password|secret|token|api.key' --glob '*.rs' --line-number | grep -v '_test\|tests/' | grep -v 'struct\|trait\|fn\|//\|config'
rg 'localhost\|127\.0\.0\.1\|0\.0\.0\.0' --glob '*.rs' --line-number | grep -v '_test\|tests/'
```
**Why it matters:** Hardcoded timeouts, credentials, and paths should be configurable.

### unwrap-usage
```bash
rg '\.unwrap\(\)' --glob '*.rs' --line-number | grep -v '_test\|tests/'
rg '\.expect\(' --glob '*.rs' --line-number | grep -v '_test\|tests/'
```
**Why it matters:** `.unwrap()` and `.expect()` in non-test code panic on failure instead of propagating errors gracefully.

### bad-test-patterns
```bash
rg 'assert!\(true\)' --glob '*.rs' --line-number
rg '#\[test\]' --glob '*.rs' -A5 --line-number | grep -B5 'assert!'
rg 'fn test.*\{' --glob '*.rs' -A3 --line-number | grep -B3 '^\s*\}$'
```
**Why it matters:** Tests that assert trivially or have empty bodies give false confidence in code correctness.

## Output

Write the following JSON to `.audit/phase3/{{SCAN_CATEGORY}}.json`:

```json
{
  "category": "suppressed-errors",
  "commands_run": ["rg '.ok()' --glob '*.rs' --line-number"],
  "matches": [
    {
      "file": "src/generation/merge.rs",
      "line": 45,
      "match_text": ".ok()",
      "context": "let _ = file.write_all(data).ok(); // error from write discarded",
      "severity": "P2",
      "is_test_file": false
    }
  ],
  "total_matches": 8
}
```

## Rules

- Run ALL commands listed for your category.
- Report every match — do not filter or editorialize.
- Include surrounding context (the full line at minimum).
- Mark whether each match is in a test file.
- If a command produces no output, report `total_matches: 0` with empty matches array.
- Do NOT create beads — just report raw data for the analysis phase.
