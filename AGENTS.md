# Agent Instructions

This project uses **Linear** for issue tracking and **Symphony** for autonomous agent orchestration. Issues live in the **ACTCLI** Linear team.

## Quick Reference

```bash
# Symphony (autonomous orchestration) — lives in actual-software/factory
symphony WORKFLOW.md                           # Run locally (binary from factory repo)
# Or via Docker:
docker run -v $(pwd)/WORKFLOW.md:/app/WORKFLOW.md:ro \
  --env-file .env.local ghcr.io/actual-software/symphony-orchestrator

# Environment
export LINEAR_API_KEY="lin_api_..."   # Required for Symphony and Linear API access
```

### Creating Linear Issues from CLI

When you need to file a follow-up issue, use the Linear GraphQL API via `gh api`:

```bash
# First, find the team ID for actual-cli
TEAM_ID=$(gh api -X POST https://api.linear.app/graphql \
  -H "Authorization: $LINEAR_API_KEY" \
  -f query='{ teams { nodes { id name } } }' \
  --jq '.data.teams.nodes[] | select(.name == "actual-cli") | .id')

# Create an issue
gh api -X POST https://api.linear.app/graphql \
  -H "Authorization: $LINEAR_API_KEY" \
  -f query="mutation {
    issueCreate(input: {
      teamId: \"$TEAM_ID\"
      title: \"<issue title>\"
      description: \"<description with context for a cold-start agent>\"
      priority: 2
    }) {
      issue { identifier title url }
    }
  }"
```

Priority values: 0 = No priority, 1 = Urgent, 2 = High, 3 = Medium, 4 = Low

## How Symphony Works

Symphony is a long-running service that:
1. **Polls** the Linear `ACTCLI` team for issues in active states (`Todo`, `In Progress`, `In Review`)
2. **Creates isolated workspaces** for each issue (clones the repo, sets up toolchains)
3. **Runs Claude Code sessions** to complete the work (implements, tests, creates PRs)
4. **Retries** on failure with exponential backoff
5. **Cleans up** workspaces when issues reach terminal state (`Merged`)

Configuration lives in `WORKFLOW.md` at the repo root. See the [factory repo](https://github.com/actual-software/factory) for full documentation.

### Symphony Architecture

```
WORKFLOW.md
    |
    v
[Workflow Loader] --> [Config Layer] --> [Orchestrator]
                                              |
                        +---------------------+---------------------+
                        |                     |                     |
                   [Linear Client]     [Workspace Manager]   [Agent Runner]
                   (fetch issues)      (create/cleanup dirs) (launch claude)
                        |                     |                     |
                        v                     v                     v
                   Linear API          /tmp/actual-cli-ws/    claude -p ...
                                       ACTCLI-42/
```

## Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create Linear issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Move finished work to appropriate state in Linear
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
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
# 1. Pull latest main before creating worktrees
git checkout main && git pull origin main

# 2. Create worktrees for parallel work
git worktree add .worktrees/task1 -b feature/task1
git worktree add .worktrees/task2 -b feature/task2

# 3. Spawn sub-agents (each works in its own worktree)
# See "Sub-Agent Instructions" below
# Sub-agents push their branch, open a PR, and wait for CI

# 4. After sub-agents report PRs are CI-green, clean up worktrees
git worktree remove .worktrees/task1
git worktree remove .worktrees/task2
git branch -d feature/task1 feature/task2

# 5. PRs are merged via GitHub (by user or orchestrator with permission)
# After merge, pull main and clean remote branches
git pull origin main
git push origin --delete feature/task1 feature/task2
```

### Sub-Agent Instructions Template

When spawning a sub-agent, provide this context:

```markdown
## Setup
- Working directory: /path/to/worktree
- Branch: feature/task-name

## Quality Gates (run before committing)
All of these MUST pass before you push:
1. `cargo build` — compiles without errors (MUST pass)
2. `cargo test` — all unit and integration tests pass (MUST pass, zero failures)
3. `cargo clippy -- -D warnings` — Rust linter (MUST pass, zero warnings)
4. `cargo fmt --check` — formatting check (MUST pass)

If any gate fails, fix the issue before committing. Do NOT push broken code.

## Workflow
1. Do the work
2. Run ALL quality gates locally:
   ```bash
   cargo fmt --check && cargo clippy -- -D warnings && cargo test && cargo build
   ```
   - If tests fail: fix the code or tests until they pass
   - If clippy warns: fix the warnings
   - If fmt fails: run `cargo fmt` to fix formatting
   - Do NOT skip or defer any gate
3. Rebase onto latest main before pushing:
   ```bash
   git fetch origin main && git rebase origin/main
   ```
   - If rebase conflicts: resolve them, then re-run all quality gates
4. Commit and push:
   ```bash
   git add . && git commit -m "<issue-id>: description" && git push -u origin feature/task-name
   ```
5. Create PR:
   ```bash
   gh pr create --title "<issue-id>: description" --body "Closes <issue-id>"
   ```
6. Wait for CI to finish and verify all checks pass:
   ```bash
   gh pr checks <pr-number> --watch
   ```
   - If checks FAIL: read the failure logs, fix the issue, push again, and re-check
   - Repeat until ALL checks pass
7. After CI passes, read all PR comments and reviews (from any reviewer, bot, or CI workflow) and address actionable feedback:
   ```bash
   # Inline review comments (on specific lines of code)
   gh api repos/actual-software/actual-cli/pulls/<pr-number>/comments --jq '.[] | {user: .user.login, body: .body, path: .path, line: .line}'
   # General PR comments (not tied to a specific line)
   gh api repos/actual-software/actual-cli/issues/<pr-number>/comments --jq '.[] | {user: .user.login, body: .body}'
   # Review summaries
   gh pr view <pr-number> --json reviews --jq '.reviews[] | select(.body != "") | {user: .author.login, body: .body}'
   ```
   - If any commenter (Claude, a CI bot, a human reviewer, etc.) has actionable feedback: fix the issues, push, re-verify CI
   - After pushing fixes, resolve any outdated inline review comments that your changes addressed:
     ```bash
     # Find outdated comment node IDs (comments on lines you've changed)
     gh api repos/actual-software/actual-cli/pulls/<pr-number>/comments --jq '.[] | select(.position == null or .outdated == true) | .node_id'
     # Resolve each with GraphQL
     gh api graphql -f query='mutation { minimizeComment(input: {subjectId: "<node_id>", classifier: OUTDATED}) { minimizedComment { isMinimized } } }'
     ```
   - **Recursive review**: after each push, re-read all comments. Repeat until all checks pass AND all actionable feedback is addressed. Do NOT report back after a single pass.
   - **Recursion limit**: if after 10 review-fix cycles there are still unresolved comments, stop and report back to the orchestrator with the PR URL, a summary of what was addressed, and the remaining unresolved feedback. The orchestrator will decide how to proceed.
8. Report back to orchestrator: PR URL + confirmation that all CI checks are green AND all review feedback addressed
9. DO NOT merge the PR — orchestrator or user handles merge


## Rules
- NEVER push directly to main — always use a PR
- ALWAYS rebase onto main before pushing (`git fetch origin main && git rebase origin/main`) — no merge commits
- DO NOT merge the PR
- If code is untestable, refactor it to be testable (see "Refactoring for Testability" in AGENTS.md)
- All quality gates MUST pass locally before pushing
- All CI checks MUST pass before reporting back
- PRs go through a **merge queue** with rebase strategy — a PR is NOT merged until it exits the queue and lands in `main`
```

### Key Points

- **PRs required**: All changes go through PRs. No direct pushes/merges to `main`.
- **CI verification**: Sub-agents must wait for CI and confirm all checks pass before reporting back.
- **No conflicts**: Each agent works in isolated worktree/branch.

## Default Ticket Workflow

When the user gives you a ticket to work on, first determine if it's a **single issue** or a **parent issue with sub-issues**.

### Single Issue

#### Phase 1: Fill in the ticket (explore agent)
Spawn an **explore** agent to research the codebase and produce a detailed issue description. The agent should return:
- What the problem is (observed vs expected behavior)
- Which files/functions are involved
- A concrete implementation plan (what to change, where)
- Testing strategy (what tests to add, what coverage is needed)

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
   # Read all PR comments yourself (inline, general, and review summaries)
   gh api repos/actual-software/actual-cli/pulls/<pr-number>/comments --jq '.[] | {user: .user.login, body: .body, path: .path, line: .line}'
   gh api repos/actual-software/actual-cli/issues/<pr-number>/comments --jq '.[] | {user: .user.login, body: .body}'
   gh pr view <pr-number> --json reviews --jq '.reviews[] | select(.body != "") | {user: .author.login, body: .body}'
   ```
   - **Success criteria**: All checks green AND (no actionable comments from any reviewer/bot OR all actionable comments addressed)
   - If CI is not green or there is unaddressed actionable feedback from any commenter, resume the sub-agent to fix
   - **Resolve addressed comments**: after fixes are pushed and CI is green, resolve any outdated inline review comments that were addressed:
     ```bash
     # Find outdated/addressed comment node IDs
     gh api repos/actual-software/actual-cli/pulls/<pr-number>/comments --jq '.[] | select(.position == null or .outdated == true) | .node_id'
     # Resolve each with GraphQL
     gh api graphql -f query='mutation { minimizeComment(input: {subjectId: "<node_id>", classifier: OUTDATED}) { minimizedComment { isMinimized } } }'
     ```
   - **Recursive verification**: after the sub-agent pushes fixes, re-verify CI AND re-read all comments. Repeat until clean. Each push may trigger new review comments.
   - **Recursion limit**: if after 10 verify-fix cycles the review is still not clean, declare a stalemate and surface the unresolved feedback to the user with the PR URL and a summary of what remains.
   - NEVER report a PR to the user based solely on the sub-agent's claim
7. Orchestrator cleans up worktree and reports PR to user

### Parent Issue with Sub-Issues (Auto-Parallelize)

When given a parent issue with sub-issues, **automatically parallelize independent children using worktrees and sub-agents.** Do not wait for the user to ask for parallelization.

1. **Inspect the parent issue** to see all sub-issues and their dependencies
2. **Identify dependency groups**:
   - Sub-issues with no dependencies on other sub-issues → **parallelize** (batch 1)
   - Sub-issues that depend on batch 1 → **parallelize after batch 1 completes** (batch 2)
   - Continue until all sub-issues are scheduled
3. **Run Phase 1 (explore) for ALL sub-issues** — spawn explore agents in parallel to flesh out every sub-issue's description before any implementation starts
4. **Pull latest main before creating worktrees**: `git checkout main && git pull origin main`
5. **For each parallel batch**, create worktrees and spawn sub-agents:
   ```bash
   # For each sub-issue in the batch:
   git worktree add .worktrees/<issue-id> -b <issue-id>/<short-name>
   # Spawn sub-agent with Sub-Agent Instructions Template
   ```
6. **Wait for all sub-agents in a batch** to report PRs with green CI
7. **Orchestrator verifies each PR independently** — do NOT trust the sub-agent's summary:
   ```bash
   # For each PR in the batch:
   gh pr checks <pr-number> --watch
   gh api repos/actual-software/actual-cli/pulls/<pr-number>/comments --jq '.[] | {user: .user.login, body: .body, path: .path, line: .line}'
   gh api repos/actual-software/actual-cli/issues/<pr-number>/comments --jq '.[] | {user: .user.login, body: .body}'
   gh pr view <pr-number> --json reviews --jq '.reviews[] | select(.body != "") | {user: .author.login, body: .body}'
   ```
   - **Success criteria**: All checks green AND (no actionable comments from any reviewer/bot OR all actionable comments addressed)
   - If CI is not green or there is unaddressed actionable feedback from any commenter, resume the sub-agent to fix
   - **Resolve addressed comments**: after fixes are pushed and CI is green, resolve any outdated inline review comments that were addressed:
     ```bash
     gh api repos/actual-software/actual-cli/pulls/<pr-number>/comments --jq '.[] | select(.position == null or .outdated == true) | .node_id'
     gh api graphql -f query='mutation { minimizeComment(input: {subjectId: "<node_id>", classifier: OUTDATED}) { minimizedComment { isMinimized } } }'
     ```
   - **Recursive verification**: after the sub-agent pushes fixes, re-verify CI AND re-read all comments. Repeat until clean.
   - **Recursion limit**: if after 10 verify-fix cycles a PR is still not clean, declare a stalemate and surface the unresolved feedback to the user with the PR URL and a summary of what remains.
   - NEVER report a PR to the user based solely on the sub-agent's claim
8. **Wait for PRs to land in `main`** through the merge queue — do NOT proceed to the next batch until the merge queue finishes
9. **Start the next batch** (sub-issues that depended on the now-completed batch)
10. **Clean up** worktrees and branches after each batch

**Key rules for parent issues:**
- Do NOT close the parent issue — it stays open while sub-issues are worked
- If a sub-issue fails CI or review, the sub-agent fixes it — do not move on
- If all sub-issues are independent (no deps), run them all in one parallel batch

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

## CI Auto-Remediation (Planned)

> **Note**: This section describes planned functionality that is not yet implemented. Currently, CI failures must be addressed manually by agents during PR review cycles.

The intended workflow is:

1. **CI Failure Detection**: A GitHub Actions workflow detects quality gate failures (test, clippy, fmt, build, coverage)
2. **Linear Issue Creation**: The workflow creates a sub-issue in the `ACTCLI` Linear team with failure details, links to the CI run/PR, and the PR branch name
3. **Agent Discovery**: Symphony picks up the new issue on its next poll cycle and dispatches an agent to fix it
4. **Auto-close**: When the PR's CI passes, the fix issue is automatically closed

Until this is implemented, agents must monitor CI results via `gh pr checks <number> --watch` and fix failures in the normal PR review cycle described in the Sub-Agent Instructions Template above.
