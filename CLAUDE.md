# actual-cli Repository Rules

## MANDATORY: Session Initialization

**BEFORE doing ANY work in a session, you MUST:**

1. Read `AGENTS.md` completely
2. Confirm which workflow you'll follow (single bead, epic, or other)
3. **If a ticket/bead ID was provided**, set the session title to the ticket name (e.g., `actual-cli-abc: Fix the thing`). Use the bead's title as-is from `bd show <id>`.
4. Only then begin work

**If you skip this, STOP immediately and do it now.**

## MANDATORY: Always Use Git Worktrees

**NEVER make code changes directly in the main checkout.** The main checkout is ONLY for orchestration (running `bd` commands, creating worktrees, spawning sub-agents).

For ALL code changes, regardless of size or complexity:
1. Create a worktree: `git worktree add .worktrees/<task> -b <branch-name>`
2. Spawn a sub-agent (`general` type) in that worktree
3. The sub-agent does all code work, commits, pushes, and creates the PR
4. Orchestrator cleans up the worktree after

**No exceptions.** Even single-line fixes go through a worktree + sub-agent. If you are about to edit a file in the main checkout, STOP and create a worktree instead.

## Session Rules

- **At the start of every context window**, re-read `AGENTS.md` before doing any work
- **NEVER report a PR as complete** without running `gh pr checks <number> --watch` and confirming ALL checks are green
- If checks fail: read the failure logs, fix the issue, push again, and re-watch. Repeat until ALL checks pass.
- A PR is not "created" until CI is confirmed green. Do not separate "create PR" from "verify CI" — they are one atomic step.
- After CI passes, **always read Claude code review comments (from Anthropic's Claude GitHub App)** and address feedback:
  ```bash
  gh pr view <number> --json comments --jq '.comments[].body'
  gh pr view <number> --json reviews --jq '.reviews[] | select(.body != "") | .body'
  ```
- Fix issues, push, and re-verify CI until all checks pass AND feedback is addressed
- **Recursive review**: after each push, re-read Claude's comments — each push may trigger new review comments. Repeat the check-fix-push cycle until ALL comments are addressed. Do NOT stop after a single review pass.
- **Recursion limit**: if after 10 review-fix cycles there are still unresolved comments, declare a stalemate and surface the remaining unresolved feedback to the user with the PR URL and a summary of what was addressed vs. what remains.
- **Resolve outdated comments**: when your push addresses a review comment, resolve/minimize the outdated comment on the PR so the review thread stays clean:
  ```bash
  # Get node IDs of outdated comments
  gh api repos/actual-software/actual-cli/pulls/<pr-number>/comments --jq '.[] | select(.position == null or .outdated == true) | .node_id'
  # Minimize each outdated comment
  gh api graphql -f query='mutation { minimizeComment(input: {subjectId: "<node_id>", classifier: OUTDATED}) { minimizedComment { isMinimized } } }'
  ```

## Issue Tracking (Beads)

When you discover a bug, gap, or follow-up work item during any task:

- **Create a bead immediately** — don't just note it mentally or mention it in passing
- **Write enough context for a cold-start sub-agent** to pick it up. Every bead must include:
  - What the problem is (observed vs expected behavior)
  - How to reproduce it (specific commands, flags, or code paths)
  - Which files/functions are involved
  - A suggested fix approach
- Use `--description` when creating beads — issues without descriptions lack context
- Parent the bead under the relevant epic if one exists
- Set priority based on impact (P0 = broken for users, P1 = wrong but workaround exists, P2 = cleanup)

### Beads Sync Workflow

- **After closing/updating beads, always run `bd sync --full`** to commit changes to the `beads-sync` branch
- The `beads-sync` branch is parallel to `main` and never merged — it's solely for tracking bead state
- `.beads/issues.jsonl` is gitignored on `main` to prevent PR conflicts
- The daemon auto-pulls from `beads-sync` to discover new beads from other agents/CI
- In worktrees, use `BEADS_NO_DAEMON=1` to prevent daemon from syncing to wrong branch

## Git Worktrees

When creating git worktrees for parallel work:

- **ALWAYS use `.worktrees/` subdirectory**: `git worktree add .worktrees/<task-name> -b <branch-name>`
- **NEVER use `../` sibling directories** - they trigger permission prompts in Claude Code
- Ensure `.worktrees/` is in `.gitignore`
- Clean up after work: `git worktree remove .worktrees/<task-name>`

## Quality Gates

All PRs MUST pass these before pushing:

1. `cargo fmt --check` — formatting (MUST pass)
2. `cargo clippy -- -D warnings` — Rust linter (zero warnings)
3. `cargo test` — all unit and integration tests pass (zero failures)
4. `cargo build` — compiles without errors

**If any gate fails, fix it before committing. Do NOT push broken code.**

## CI: Claude Code Review Exceptions

The `claude-review` CI check may fail or should be considered non-blocking in these cases:

- **Workflow file changes**: Any PR that modifies `.github/workflows/claude-code-review.yml` will cause `claude-review` to fail because `claude-code-action` validates the workflow file matches `main`. This is expected and the failure can be ignored.
- **Agent instruction changes**: PRs that only modify `CLAUDE.md` or `AGENTS.md` do not need the `claude-review` check to pass — these are documentation/configuration files, not code.

## Git Workflow

**CRITICAL: All changes to `main` MUST go through a PR. No direct pushes or merges to `main`.**

- **NEVER push directly to main** — always use a feature branch + PR
- **NEVER use `gh pr merge`** without explicit user permission
- Always ask before merging any PR
- Create PRs and provide the link, then wait for user to approve/merge
- Do not assume permission to merge even if user says "fix it" or similar
- PRs go through a **merge queue** with rebase strategy

## Session Completion ("Landing the Plane")

**When ending a work session**, you MUST complete ALL steps. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - All gates must pass
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd sync --full
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Verify** - All changes committed AND pushed

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds
