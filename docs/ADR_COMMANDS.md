# ADR commands

`actual adr` writes and checks Architecture Decision Records (ADRs) in your repository, and manages your organization's decisions in Actual AI from the terminal. `new` and `check` work on local files and need no sign-in. The other ten commands act on the decisions in your Actual AI account and need `actual login`.

```bash
actual adr new "Use Postgres for storage" --policy "MUST: Services store relational data in Postgres."
actual adr check                      # check every decision file under docs/adr/
actual adr list                       # this repository's decisions in Actual AI
actual adr create --file docs/adr/<file>.md
actual adr publish <id>
```

## Decision files in the repository

Each decision has two files, laid out the way Actual AI's repository sync writes them:

| File | Content | Read by |
|------|---------|---------|
| `docs/adr/<id>-<title>.md` | A YAML header, then the decision document: title, `Date:` and `Deciders:` lines, Context, Decision with the policies, and any Rationale, Consequences and References | People, and `actual adr check` |
| `.actual/rules/cross-cutting-<topic>-<hash>.md` | The same header, then a `### Rules` list with one rule per policy | `plan-check`, `impl-check` and `rules` |

The header carries the decision's id, its status and the paths it applies to. Both file names come from the id and the title, so when Actual AI later syncs the decision it updates these files instead of adding a second copy.

### Writing a decision

```bash
actual adr new "Use Postgres for storage" \
  --context "Each service picked its own database, and three engines are now in production." \
  --policy "MUST: Services store relational data in Postgres." \
  --policy "MUST NOT: Services open SQLite files outside tests." \
  --deciders "Platform team" \
  --scope 'services/**'
```

| Flag | Meaning |
|------|---------|
| `--context POINT` | Why the decision is needed, one point per flag |
| `--policy "LEVEL: statement"` | One policy per flag. LEVEL is `MUST`, `MUST NOT`, `SHOULD`, `SHOULD NOT` or `MAY` |
| `--deciders NAMES` | The decision's Deciders line |
| `--scope GLOB` | A path the decision applies to, relative to the repository root, one per flag. A leading `!` excludes. Leave it out for the whole repository |
| `--status accepted\|draft` | See [Accepted or draft](#accepted-or-draft) |
| `--root PATH` | Repository root to write into. Defaults to the current directory |
| `--json` | Print the files written as JSON |

`adr new` runs the same check Actual AI runs before it saves a decision, and writes nothing when the decision would be refused; each reason is listed. A `--policy` without a level is refused before that check runs. The new decision gets a random local id until you push it with `actual adr create --file`. An existing file is never replaced.

### Checking decision files

```bash
actual adr check                       # every file under docs/adr/ that carries a decision header
actual adr check docs/adr/<file>.md    # only these files
actual adr check --no-cloud            # compare only with the files in this repository
```

`adr check` runs Actual AI's save check on each file and lists likely duplicates: pairs of files in the repository and, when you're signed in, decisions in Actual AI. Those are the connected repository's decisions, or the whole organization's when the checkout names no connected repository, and the report says which. It exits 1 when a file fails the check. A likely duplicate is reported without failing the run. When the comparison with Actual AI can't run (signed out, or the request failed), the report says why and the local check still counts.

Two decisions look the same when their titles share at least half their content words, or when any policy of one does with any policy of the other. Content words leave out common English words and the policy levels, and the comparison runs locally with no model.

## Accepted or draft

An accepted decision governs at once: `plan-check` and `impl-check` read its rules. A draft is read by no check until it's published.

`adr new` and `adr create` default to `accepted` when standard input is a terminal and to `draft` when it isn't, as in a coding agent's shell, a script or CI. An agent that writes or pushes a decision therefore leaves a draft for a person to publish. `--status` sets either one explicitly.

## Decisions in Actual AI

Sign in with `actual login`. The default sign-in can read and change decisions; a 403 from Actual AI exits 2 with the step that fixes it, such as signing in again or signing in to the organization the decision belongs to.

Commands that work on one repository use the connected repository your checkout's `origin` remote names. `--repo` picks another by its name, `owner/name` or id. `adr list` with neither lists the whole organization.

| Command | Action |
|---------|--------|
| `actual adr list` | List decisions. `--state active\|draft\|suggested\|updated\|archived` matches the lists on the web app's decisions page; `--limit` (1 to 200, default 50) and `--offset` page through them |
| `actual adr show <id>` | Show one decision with its policies, their ids and its scope |
| `actual adr create` | Save a new decision, from a file or from flags. See [Saving a decision](#saving-a-decision) |
| `actual adr update <id>` | Change the title, context, policies, policy levels or scope |
| `actual adr publish <id>` | Publish a draft so it governs |
| `actual adr unpublish <id>` | Move an active decision back to draft |
| `actual adr delete <id>` | Archive a decision, as Delete does in the web app |
| `actual adr restore <id>` | Restore an archived decision |
| `actual adr accept <id>` | Accept a suggested decision |
| `actual adr reject <id> --reason TEXT` | Reject a suggested decision; the reason is saved with it |

A change Actual AI makes to the repository's rule files arrives through its usual sync pull request, and the command says when it started one.

### Saving a decision

```bash
actual adr create --file docs/adr/<file>.md
actual adr create --title "Use Postgres for storage" --policy "MUST: Services store relational data in Postgres."
```

`create` runs these steps in order and stops at the first that fails:

1. It checks the decision locally with the save check `adr new` uses. A decision that fails is never sent.
2. It reads the repository's decisions in Actual AI and refuses when the file is already one of them, or when a decision looks the same by the rule in [Checking decision files](#checking-decision-files). `--allow-duplicate` sends it anyway. If the decisions can't be read, nothing is sent.
3. It sends the decision once and never retries. When the request can't reach Actual AI, nothing was saved and the error says so. When the request was sent but no answer came back, or the answer couldn't be read, the decision may have been saved: the command exits 3 and asks you to look with `actual adr list` before you run `create` again.
4. With `--file`, it renames both files to the names the sync uses for the new decision's id, and their headers take that id and the status the decision was saved with. A file pushed as a draft therefore stops governing locally until it's published and Actual AI's sync pull request rewrites its files, and the command says when the local status changed. A scope in the file's header is then set on the new decision; if that step fails, the decision stays saved and the command prints the `actual adr update` command that sets the scope.

### Changing a decision

```bash
actual adr show <id>                                     # policy ids are in the output
actual adr update <id> --level <policy id>=SHOULD
actual adr update <id> --policy "<policy id>=MUST: Services store relational data in Postgres." \
                       --policy "MAY: Analytics jobs read from a Postgres replica."
actual adr update <id> --scope 'services/**' --scope '!services/legacy/**'
```

`--policy` gives the whole policy list after the change: prefix a policy's id and `=` to keep that policy, write a new one without an id, and leave out a policy to remove it. `--level` changes one policy's level and can't be combined with `--policy`. `--context` replaces the whole context, and `--clear-scope` applies the decision to its whole repository again.

`update` prints the decision as it was saved. Editing the policy list can give the policies new ids, so take the ids for your next edit from that output or from `actual adr show`.

## Output and exit codes

Every command takes `--json` and prints its result as JSON on standard output. Errors go to standard error.

| Exit code | Meaning |
|-----------|---------|
| 0 | Done |
| 1 | A decision failed a check: the local save check, Actual AI's refusal of its content on `create` or `update`, `adr check`, or `create`'s duplicate refusal |
| 2 | Signed out, a sign-in without access to the decision, or no connected repository could be chosen |
| 3 | Actual AI couldn't be reached or answered with another error, such as an id it doesn't know or publishing a decision that's already active, or a `create` whose outcome is unknown |
| 5 | A local file couldn't be read or written |
