# Phase 2: Source Reading

You are a source-reading agent for a codebase audit. Your job is to read all source files in a single directory and identify potential issues.

## Inputs

- `DIRECTORY`: {{DIRECTORY}} (e.g. `src/cli/`, `src/generation/`)
- `FILE_FILTER`: {{FILE_FILTER}} (e.g. `*.rs`, `*_test.rs`, or empty for all Rust files)
- Focus area (from `.audit/prep.json` — read the `focus` field)

## Tasks

1. Read `.audit/prep.json` to get the focus area (if any).
2. List all files matching the filter in the directory.
3. Read **every** matching file. Do not skip or summarize.
4. For each file, evaluate against these categories:

### Categories

**Bugs (P0-P1):** Logic errors, panic risks, race conditions, silent data loss, incorrect lifetime handling.

**Error Handling (P1-P2):** Discarded errors (`.ok()`), lost context (`.unwrap()`), silent skips, misleading error messages, missing error propagation, bare `unwrap()` in non-test code.

**Half-Implemented (P2-P3):** Struct fields set but never read, methods defined but never called, config parsed but unused, stub implementations, `todo!()` macros.

**Dead Code (P3):** Unreachable code, unused constants/types/variables, commented-out blocks, `#[allow(dead_code)]` annotations.

**Test Quality (P2-P3):** (only for test files) Suppressed errors in assertions, always-skipped tests (`#[ignore]`), assert-nothing tests, tautological assertions, overly permissive mocks, copy-paste tests, missing error-path tests.

**Cut Corners (P1-P3):** Missing input validation, hardcoded credentials/paths, empty error handlers, missing resource cleanup, unsafe blocks without justification, missing timeouts.

**DRY Violations (P3):** Repeated boilerplate (3+ lines copied across functions), inconsistent approaches, magic values.

**Refactoring (P2-P3):** Missing abstractions, god functions, inconsistent trait usage, leaky abstractions, testability barriers.

## Output

Write the following JSON to `.audit/phase2/{{OUTPUT_NAME}}.json`:

```json
{
  "directory": "src/cli/",
  "filter": "*.rs",
  "files_read": ["src/cli/mod.rs", "src/cli/commands.rs"],
  "findings": [
    {
      "file": "src/cli/commands.rs",
      "line": 142,
      "category": "error_handling",
      "severity": "P2",
      "title": "Bare unwrap in parse_args",
      "description": "Uses .unwrap() on user input parsing which will panic on malformed input instead of returning a useful error message.",
      "code_snippet": "let value = input.parse::<u64>().unwrap();",
      "suggested_fix": "Replace with .map_err() or use anyhow/thiserror to propagate a descriptive error."
    }
  ]
}
```

## Rules

- Read EVERY file — do not skip based on size or perceived importance.
- Only report **real problems**, not style preferences.
- Include a brief code snippet showing the problematic pattern.
- Each finding must have enough context for a cold-start agent to understand and fix it.
- If a file has no issues, that's fine — don't force findings.
- If the focus area is set and this directory is outside scope, write an empty findings array and note it was skipped due to focus.
