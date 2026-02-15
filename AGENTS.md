# Agent Instructions

This project uses **bd** (beads) for issue tracking. Run `bd onboard` to get started.

## Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --status in_progress  # Claim work
bd close <id>         # Complete work
bd sync               # Sync with git
```

## Beads Sync Workflow

**This project uses the `beads-sync` branch for tracking beads state.**

### How It Works

- Beads state (`.beads/issues.jsonl`) is committed to the `beads-sync` branch
- The `beads-sync` branch is **never merged** into `main` - it's parallel to main
- All agents sync beads to `beads-sync` after closing/updating issues
- The daemon auto-pulls from `beads-sync` to discover new beads from other agents/CI

### Configuration

The repository is configured with:
```yaml
# .beads/config.yaml
sync-branch: "beads-sync"
```

This ensures `bd sync` commits to `beads-sync` instead of requiring manual branch configuration.

### Key Commands

```bash
bd sync                  # Export to JSONL, commit to beads-sync, push
git fetch origin beads-sync  # Pull latest beads from remote (daemon does this automatically)
bd sync --pull          # Pull from beads-sync and import to local database
```

### Important Rules

1. **ALWAYS run `bd sync` after closing or updating beads** - this pushes state to `beads-sync`
2. **The daemon auto-syncs** - it watches `beads-sync` and imports changes automatically
3. **Never merge `beads-sync` into `main`** - they are parallel branches
4. **`.beads/issues.jsonl` is gitignored on main** - it only exists in `beads-sync`
5. **In worktrees, use `BEADS_NO_DAEMON=1`** - prevents daemon from syncing to wrong branch

### Why This Approach?

- **Distributed discovery**: Agents and CI can create beads, others discover them via git pull
- **No PR conflicts**: Beads state doesn't conflict with code PRs
- **Audit trail**: Full git history of all bead changes
- **Team sync**: Everyone sees the same beads state without manual database sharing

## Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd sync
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds

## Parallel Agent Orchestration

For large tasks, use **git worktrees** to enable parallel sub-agent work without conflicts.

**CRITICAL: All changes to `main` MUST go through a PR. No direct pushes or merges to `main`.**

### Orchestrator Pattern

The orchestrator (main agent) manages the workflow:

```bash
# 1. Create epic with subtasks
bd create "Epic title" -p 1
bd create "Subtask 1" -p 1 --parent <epic-id>
bd create "Subtask 2" -p 1 --parent <epic-id>
bd dep add <verification-task> <subtask-1>  # Set up blockers

# 2. Pull latest main before creating worktrees
git checkout main && git pull origin main

# 3. Create worktrees for parallel work
git worktree add .worktrees/task1 -b feature/task1
git worktree add .worktrees/task2 -b feature/task2

# 4. Spawn sub-agents (each works in its own worktree)
# See "Sub-Agent Instructions" below
# Sub-agents push their branch, open a PR, and wait for CI

# 5. After sub-agents report PRs are CI-green, clean up worktrees
git worktree remove .worktrees/task1
git worktree remove .worktrees/task2
git branch -d feature/task1 feature/task2

# 6. PRs are merged via GitHub (by user or orchestrator with permission)
# After merge, pull main and clean remote branches
git pull origin main
git push origin --delete feature/task1 feature/task2
```

### Sub-Agent Instructions Template

When spawning a sub-agent, provide this context:

```markdown
## Setup
- Working directory: /path/to/worktree
- Set BEADS_NO_DAEMON=1 for all bd commands (required in worktrees)
- Branch: feature/task-name
- IMPORTANT: .beads/issues.jsonl is gitignored on main to prevent PR conflicts. Beads state is tracked on the `beads-sync` branch and synced via `bd sync`.

## Quality Gates (run before committing)
All of these MUST pass before you push:
1. `cargo build` — compiles without errors (MUST pass)
2. `cargo test` — all unit and integration tests pass (MUST pass, zero failures)
3. `cargo clippy -- -D warnings` — Rust linter (MUST pass, zero warnings)
4. `cargo fmt --check` — formatting check (MUST pass)

If any gate fails, fix the issue before committing. Do NOT push broken code.

## Workflow
1. Run `BEADS_NO_DAEMON=1 bd update <child-issue-id> --claim` (claim the specific child bead, NOT the parent epic)
2. Do the work
3. Run ALL quality gates locally:
   ```bash
   cargo fmt --check && cargo clippy -- -D warnings && cargo test && cargo build
   ```
   - If tests fail: fix the code or tests until they pass
   - If clippy warns: fix the warnings
   - If fmt fails: run `cargo fmt` to fix formatting
   - Do NOT skip or defer any gate
4. Rebase onto latest main before pushing:
   ```bash
   git fetch origin main && git rebase origin/main
   ```
   - If rebase conflicts: resolve them, then re-run all quality gates
5. Commit and push:
   ```bash
   git add . && git commit -m "<issue-id>: description" && git push -u origin feature/task-name
   ```
6. Create PR:
   ```bash
   gh pr create --title "<issue-id>: description" --body "Closes <issue-id>"
   ```
7. Wait for CI to finish and verify all checks pass:
   ```bash
   gh pr checks <pr-number> --watch
   ```
   - If checks FAIL: read the failure logs, fix the issue, push again, and re-check
   - Repeat until ALL checks pass
8. After CI passes, read Claude code review comments (from Anthropic's Claude GitHub App) and address feedback:
   ```bash
   gh pr view <pr-number> --json comments --jq '.comments[].body'
   gh pr view <pr-number> --json reviews --jq '.reviews[] | select(.body != "") | .body'
   ```
   - If Claude has actionable feedback: fix the issues, push, re-verify CI
    - After pushing fixes, resolve any outdated review comments that your changes addressed:
      ```bash
      # Find outdated comment IDs (comments on lines you've changed)
      gh api repos/actual-software/actual-cli/pulls/<pr-number>/comments --jq '.[] | select(.position == null or .outdated == true) | .node_id'
      # Resolve each with GraphQL
      gh api graphql -f query='mutation { minimizeComment(input: {subjectId: "<node_id>", classifier: OUTDATED}) { minimizedComment { isMinimized } } }'
      ```
   - **Recursive review**: after each push, re-read Claude's comments. Repeat until all checks pass AND all actionable feedback is addressed. Do NOT report back after a single pass.
   - **Recursion limit**: if after 10 review-fix cycles there are still unresolved comments, stop and report back to the orchestrator with the PR URL, a summary of what was addressed, and the remaining unresolved feedback. The orchestrator will decide how to proceed.
9. Report back to orchestrator: PR URL + confirmation that all CI checks are green AND all review feedback addressed
10. DO NOT close the beads issue — orchestrator closes after PR lands in `main`
11. DO NOT merge the PR — orchestrator or user handles merge


## Rules
- NEVER push directly to main — always use a PR
- ALWAYS rebase onto main before pushing (`git fetch origin main && git rebase origin/main`) — no merge commits
- DO NOT close the beads issue — orchestrator closes after PR lands in `main` (through the merge queue)
- DO NOT merge the PR
- If code is untestable, refactor it to be testable (see "Refactoring for Testability" in AGENTS.md)
- All quality gates MUST pass locally before pushing
- All CI checks MUST pass before reporting back
- PRs go through a **merge queue** with rebase strategy — a PR is NOT merged until it exits the queue and lands in `main`
```

### Key Points

- **PRs required**: All changes go through PRs. No direct pushes/merges to `main`.
- **CI verification**: Sub-agents must wait for CI and confirm all checks pass before reporting back.
- **BEADS_NO_DAEMON=1**: Required in worktrees to prevent daemon from committing to wrong branch.
- **Atomic claiming**: `bd update <id> --claim` sets status + assignee atomically.
- **Dependency blocking**: Use `bd dep add` so verification tasks wait for all work.
- **No conflicts**: Each agent works in isolated worktree/branch.

## Default Ticket Workflow

When the user gives you a ticket/bead to work on, first determine if it's a **single bead** or an **epic with children**.

### Single Bead

**Before starting any work, claim the bead:**
```bash
bd update <issue-id> --claim
```

#### Phase 1: Fill in the ticket (explore agent)
Spawn an **explore** agent to research the codebase and produce a detailed issue description. The agent should return:
- What the problem is (observed vs expected behavior)
- Which files/functions are involved
- A concrete implementation plan (what to change, where)
- Testing strategy (what tests to add, what coverage is needed)

Update the bead description with the agent's findings using `bd update <id> --description "..."`.

#### Phase 2: Implement (general agent in worktree)
Once the ticket is fleshed out, create a git worktree and spawn a **general** agent to do the work:

1. **Pull latest main**: `git checkout main && git pull origin main`
2. Create worktree: `git worktree add .worktrees/<task> -b <branch-name>`
3. Spawn sub-agent with the full Sub-Agent Instructions Template (see above), including:
   - The detailed description from Phase 1
   - Exact files to change, implementation approach, and testing requirements
   - CI requirements (tests, clippy, fmt, build)
4. The sub-agent pushes, creates a PR, waits for CI, reads Claude code review feedback, fixes issues, and loops until all checks are green and feedback is addressed
5. Sub-agent reports back with PR URL + CI confirmation
6. **Orchestrator verifies independently** — do NOT trust the sub-agent's summary:
   ```bash
   # Verify CI is actually green
   gh pr checks <pr-number> --watch
   # Read Claude review comments yourself
   gh pr view <pr-number> --json comments --jq '.comments[].body'
   gh pr view <pr-number> --json reviews --jq '.reviews[] | select(.body != "") | .body'
   ```
   - **Success criteria**: All checks green AND (no Claude review comments OR all actionable comments addressed)
   - If CI is not green or Claude has unaddressed actionable feedback, resume the sub-agent to fix
   - **Resolve addressed comments**: after fixes are pushed and CI is green, resolve any outdated review comments that were addressed:
     ```bash
     # Find outdated/addressed comment node IDs
     gh api repos/actual-software/actual-cli/pulls/<pr-number>/comments --jq '.[] | select(.position == null or .outdated == true) | .node_id'
     # Resolve each with GraphQL
     gh api graphql -f query='mutation { minimizeComment(input: {subjectId: "<node_id>", classifier: OUTDATED}) { minimizedComment { isMinimized } } }'
     ```
   - **Recursive verification**: after the sub-agent pushes fixes, re-verify CI AND re-read Claude comments. Repeat until clean. Each push may trigger new review comments.
   - **Recursion limit**: if after 10 verify-fix cycles the review is still not clean, declare a stalemate and surface the unresolved feedback to the user with the PR URL and a summary of what remains.
   - NEVER report a PR to the user based solely on the sub-agent's claim
7. Orchestrator cleans up worktree and reports PR to user
8. **Close the bead immediately** after PR lands in `main` (exits the merge queue): `bd close <id> -m "Fixed in PR #<N>, <what changed>"`

### Epic (Auto-Parallelize)

When given an epic, **automatically parallelize independent children using worktrees and sub-agents.** Do not wait for the user to ask for parallelization.

1. **Inspect the epic**: Run `bd show <epic-id>` to see all children and their dependencies
2. **Identify dependency groups**:
   - Children with no dependencies on other children → **parallelize** (batch 1)
   - Children that depend on batch 1 → **parallelize after batch 1 completes** (batch 2)
   - Continue until all children are scheduled
3. **Run Phase 1 (explore) for ALL children** — spawn explore agents in parallel to flesh out every child bead's description before any implementation starts
4. **Pull latest main before creating worktrees**: `git checkout main && git pull origin main`
5. **For each parallel batch**, create worktrees and spawn sub-agents:
   ```bash
   # For each child in the batch:
   git worktree add .worktrees/<child-id> -b <child-id>/<short-name>
   # Spawn sub-agent with Sub-Agent Instructions Template
   # Sub-agent claims the child bead: bd update <child-id> --claim
   ```
6. **Wait for all sub-agents in a batch** to report PRs with green CI
7. **Orchestrator verifies each PR independently** — do NOT trust the sub-agent's summary:
   ```bash
   # For each PR in the batch:
   gh pr checks <pr-number> --watch
   gh pr view <pr-number> --json comments --jq '.comments[].body'
   gh pr view <pr-number> --json reviews --jq '.reviews[] | select(.body != "") | .body'
   ```
   - **Success criteria**: All checks green AND (no Claude review comments OR all actionable comments addressed)
   - If CI is not green or Claude has unaddressed actionable feedback, resume the sub-agent to fix
   - **Resolve addressed comments**: after fixes are pushed and CI is green, resolve any outdated review comments that were addressed:
     ```bash
     gh api repos/actual-software/actual-cli/pulls/<pr-number>/comments --jq '.[] | select(.position == null or .outdated == true) | .node_id'
     gh api graphql -f query='mutation { minimizeComment(input: {subjectId: "<node_id>", classifier: OUTDATED}) { minimizedComment { isMinimized } } }'
     ```
   - **Recursive verification**: after the sub-agent pushes fixes, re-verify CI AND re-read Claude comments. Repeat until clean.
   - **Recursion limit**: if after 10 verify-fix cycles a PR is still not clean, declare a stalemate and surface the unresolved feedback to the user with the PR URL and a summary of what remains.
   - NEVER report a PR to the user based solely on the sub-agent's claim
8. **Wait for PRs to land in `main`** through the merge queue — do NOT proceed to the next batch or close beads until the merge queue finishes
9. **Close each child bead** as its PR lands in `main`: `bd close <child-id> -m "Fixed in PR #<N>"`
10. **Start the next batch** (children that depended on the now-completed batch)
11. **Clean up** worktrees and branches after each batch
12. **Check if the epic should be closed**:
    - **Long-lived epics** (e.g., bug trackers, ongoing maintenance) stay OPEN indefinitely — only close children
    - **Short-lived epics** (e.g., feature development, time-boxed projects) close when all children complete: `bd close <epic-id> -m "All N subtasks completed"`
    - Check the epic description or ask the user if unsure

**Key rules for epics:**
- Do NOT claim the epic itself — it stays `open` while children are worked
- Claim each **child bead individually** via the sub-agent
- Never close a child bead until its PR is in `main` (not just queued)
- If a child fails CI or review, the sub-agent fixes it — do not move on and close it
- If all children are independent (no deps), run them all in one parallel batch
- **CRITICAL**: Long-lived epics stay OPEN — only close the children, never the epic

## Refactoring for Testability

When writing tests, if code cannot be tested due to external dependencies:

1. **Create traits** for external dependencies (Rust's equivalent of interfaces)
2. **Accept trait objects** or generics in constructors (dependency injection)
3. **Use mocks** in tests to simulate external behavior (e.g., `mockall` crate)
4. **Test error paths** not just happy paths

Example pattern:
```rust
// Trait for external dependency
#[async_trait::async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(&self, name: &str, args: &[&str]) -> Result<Vec<u8>, Error>;
}

// Constructor with injection
pub fn new_service(runner: Box<dyn CommandRunner>) -> Service { ... }

// Test with mock
#[cfg(test)]
mod tests {
    use mockall::mock;

    mock! {
        Runner {}
        #[async_trait::async_trait]
        impl CommandRunner for Runner {
            async fn run(&self, name: &str, args: &[&str]) -> Result<Vec<u8>, Error>;
        }
    }

    #[tokio::test]
    async fn test_service() {
        let mut mock = MockRunner::new();
        mock.expect_run().returning(|_, _| Ok(b"success".to_vec()));
        let svc = new_service(Box::new(mock));
        // test...
    }
}
```

## CI Auto-Remediation

When CI fails on a PR, a fix bead is automatically created and synced to the `beads-sync` branch.

### How It Works

1. **CI Failure Detection**: When any quality gate fails (test, clippy, fmt, build), GitHub Actions triggers the `poiley/bdgha` action
2. **Parent Bead Detection**: Action auto-detects parent bead ID from PR title (e.g., `actual-cli-abc: Fix thing`), branch name, or recent commits
3. **Fix Bead Creation**: Creates child bead with:
   - Structured failure summary and details
   - Links to CI logs and artifacts
   - PR branch name for pushing fixes
   - Step-by-step resolution checklist
4. **Parent Status Update**: Marks parent bead as `blocked` and creates dependency
5. **Git Sync**: Commits fix bead to `beads-sync` branch
6. **Agent Discovery**: Your `bd daemon` auto-pulls from `beads-sync` every 5 seconds

### Agent Workflow

When a fix bead is created:

1. **Discovery**: Your `bd daemon` auto-pulls from `beads-sync` branch
2. **Finding Work**: Run `bd ready` or filter by label:
   ```bash
   bd list --labels ci-failure --status open
   ```
3. **Claiming**: Claim the fix bead atomically:
   ```bash
   bd update <fix-bead-id> --claim
   ```
4. **Review**: Read the fix bead description which contains:
   - Summary of failures (test names, clippy warnings, fmt issues, etc.)
   - Links to CI logs and downloadable artifacts
   - PR branch name
   - Resolution checklist with exact commands
5. **Fixing**: 
   - Checkout the PR branch: `git fetch && git checkout <branch-name>`
   - Fix the specific failures listed
   - Run quality gates locally: `cargo fmt --check && cargo clippy -- -D warnings && cargo test && cargo build`
6. **Pushing**: Push to the same PR branch:
   ```bash
   git add .
   git commit -m "actual-cli-abc: Fix CI failures"
   git push origin <branch-name>
   ```
7. **Verification**: Monitor CI to ensure all checks pass
8. **Auto-close**: Fix bead automatically closes when parent PR CI passes

### Fix Bead Labels

Fix beads are created with these labels for easy filtering:
- `ci-failure` - All CI auto-remediation beads
- `auto-remediation` - Created automatically by GitHub Actions
- `test_failure` | `clippy_warning` | `fmt_error` | `build_error` - Specific failure type

### Configuration

CI auto-remediation behavior is controlled in `.beads/config.yaml`:

```yaml
ci:
  auto-remediation:
    enabled: true                    # Enable/disable feature
    assign-to-pr-author: true        # Assign fix beads to PR author
    fix-bead-priority: 1             # P1 (high priority)
    block-parent: true               # Mark parent as blocked
    auto-close-on-success: true      # Auto-close when CI passes
sync-branch: beads-sync              # Git branch for bead sync
```

To disable auto-remediation, set `enabled: false` in config.

### Example Fix Bead

When CI fails, you'll see a bead like this:

```
actual-cli-fix-abc: Fix CI: test_failure in actual-cli-abc

Labels: ci-failure, auto-remediation, test_failure
Status: open
Assignee: poiley
Parent: actual-cli-abc

## Summary
3 tests failed:
- config::dotpath::tests::test_set_batch_size
- generation::merge::tests::test_replace_markers
- api::retry::tests::test_backoff_timing

## Resources
- CI Run: https://github.com/actual-software/actual-cli/actions/runs/12345
- PR Branch: `actual-cli-abc/feature-name`

## Resolution Checklist
1. Claim: `bd update actual-cli-fix-abc --claim`
2. Checkout: `git fetch && git checkout actual-cli-abc/feature-name`
3. Fix failures
4. Verify: `cargo fmt --check && cargo clippy -- -D warnings && cargo test && cargo build`
5. Push: `git push origin actual-cli-abc/feature-name`
6. Monitor CI
```
