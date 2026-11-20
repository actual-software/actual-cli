# ADR commands

`actual adr` writes and checks Architecture Decision Records (ADRs) in your repository, and manages your organization's decisions in Actual AI from the terminal. `new`, `check` and `discover` work on local files and need no sign-in. The other ten commands act on the decisions in your Actual AI account and need `actual login`.

```bash
actual adr new "Use Postgres for storage" --policy "MUST: Services store relational data in Postgres."
actual adr check                      # check every decision file under docs/adr/
actual adr discover                   # copy the ADRs other tools keep into docs/adr/imported/
actual adr list                       # this repository's decisions in Actual AI
actual adr create --file docs/adr/<file>.md
actual adr create --all               # every decision file Actual AI doesn't have yet
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
| `--context POINT` | Why the decision is needed, one point per flag. Optional |
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

With no file named, `adr check` also reads the files under `docs/adr/` that carry no ADR header, and it ends with the files that won't publish. Each comes with its reasons and what to do:

| Reason | What to do |
|--------|------------|
| No ADR header, or a save-check reason such as no policy under `## Decision`, a title on more than one line, or a document over the size limit | Rewrite it in Actual's format: the ADR header, a `# ` title and policies such as `1. MUST: …` under `## Decision`, as `adr new` writes them |
| It looks like a decision in Actual AI, by the rule below | Compare the two; `create --all` skips a look-alike unless you pass `--allow-duplicate` |

The list leaves out files whose id is already a decision in Actual AI, READMEs, indexes and templates, and everything under `docs/adr/drafts/`, where work in progress stays out of Actual AI. The header-less copies `actual adr-bot` writes of your decisions, each beside a `.claude/rules/` file naming the decision, are counted in one line instead. A file without a header doesn't fail the run: the exit status still comes from the files that carry one.

Two decisions look the same when their titles share at least half their content words, or when any policy of one does with any policy of the other. Content words leave out common English words and the policy levels, and the comparison runs locally with no model.

### Finding the ADRs a repository already has

```bash
actual adr discover
actual adr discover --dry-run          # list what would be copied, and copy nothing
actual adr discover --root ../web --json
```

`adr discover` lists the ADR files in the folders ADR tools use, with each file's layout, title and status, and the id its Actual header names. It then copies each one kept outside `docs/adr/` into `docs/adr/imported/<folder>/` under its own name, where `<folder>` is the source's folder with `-` between its parts: `doc/adr/0001-record-architecture-decisions.md` goes to `docs/adr/imported/doc-adr/`. The copies sit where `check` and `create --all` read, and two tools' files of the same name don't collide. The sources stay where they are.

A file is never overwritten: when a copy's path is taken, the run says so and leaves that source alone. `docs/adr/imported/.copied` lists each source copied, and a listed source isn't copied again, even after its copy is renamed or deleted; delete its line to copy it once more. `--dry-run` lists the copies and writes nothing.

The run ends with the files under `docs/adr/` that won't publish, as [`check`](#checking-decision-files) lists them, compared with your decisions in Actual AI when you're signed in unless you pass `--no-cloud`. It sends no file and needs no sign-in.

| Folder | Read for |
|--------|----------|
| `docs/adr/` | Actual, whose `adr new` and repository sync write there |
| The folder `.adr-dir` names, else `doc/adr/` | adr-tools |
| `docs/decisions/` | MADR |
| Each `adrFolder` in `.log4brains.yml`, packages included | log4brains |
| The `backstage.io/adr-location` annotation in `catalog-info.yaml` | Backstage |
| `docs/adrs/`, `docs/architecture/decisions/`, `architecture/decisions/`, `adr/` and `decisions/` | Common folders |

Each folder is read three levels deep. A file's layout is one of four:

| Layout | How it's recognized |
|--------|---------------------|
| `actual` | Actual's header, as `adr new` and the sync write it |
| `madr` | MADR's front matter, its Context and Problem Statement, Considered Options or Decision Outcome section, or a `- Status:` line as log4brains writes it |
| `nygard` | Status, Context, Decision and Consequences sections, as adr-tools writes them |
| `other` | None of the layouts above: any such file in a tool's folder, and in a common folder a numbered one such as `0007-use-postgres.md` |

An index, a README or a template is listed apart, as not an ADR, and isn't copied. A folder a configuration file names that leads outside the repository isn't read, nor is a URL in `catalog-info.yaml`, and the list says so; nothing is copied into a folder that leads outside it. `adr new` writes a random local id into the header, so an id in the list doesn't mean Actual AI has the decision: `create --all` finds that out. Only files in Actual's format can be sent with `create`, so a copy publishes once it's rewritten in that format.

## Accepted or draft

An accepted decision governs at once: `plan-check` and `impl-check` read its rules. A draft is read by no check until it's published.

`adr new` and `adr create` default to `accepted` when standard input is a terminal and to `draft` when it isn't, as in a coding agent's shell, a script or CI. An agent that writes or pushes a decision therefore leaves a draft for a person to publish. `--status` sets either one explicitly.

## Decisions in Actual AI

Sign in with `actual login`. The default sign-in can read and change decisions; a 403 from Actual AI exits 2 with the step that fixes it, such as signing in again or signing in to the organization the decision belongs to. These commands call the same decision routes as the web app, so a decision changed here reads the same there.

Commands that work on one repository use the connected repository your checkout's `origin` remote names. `--repo` picks another by its name, `owner/name` or id. `adr list` with neither lists the whole organization.

| Command | Action |
|---------|--------|
| `actual adr list` | List decisions. `--state active\|draft\|suggested\|updated\|archived` matches the lists on the web app's decisions page; `--limit` (1 to 200, default 50) and `--offset` page through them |
| `actual adr show <id>` | Show one decision with its policies, their ids and its scope |
| `actual adr create` | Save a new decision, from a file or from flags. See [Saving a decision](#saving-a-decision) |
| `actual adr update <id>` | Change the title, context, policies, policy levels or scope |
| `actual adr publish <id>` | Publish a draft so it governs |
| `actual adr unpublish <id>` | Move an active decision back to draft |
| `actual adr delete <id>` | Archive an active decision or a draft, as Delete does in the web app |
| `actual adr restore <id>` | Restore an archived decision to the list it was archived from |
| `actual adr accept <id>` | Accept a suggestion |
| `actual adr reject <id> --reason TEXT` | Reject a suggestion; the reason, up to 10,000 characters, is saved with it |

Before a change, the command reads your organization's decisions and checks the decision's state by the web app's rules, and sends nothing when the state doesn't allow it. Only a draft can be published and only an active decision unpublished. `delete` takes an active decision or a draft, `restore` an archived one, and `accept` and `reject` a suggestion, either a new decision or a suggested change to an active one. `show` and the changes take a decision's id as `list` prints it. For a decision Actual AI grouped from several policies, the id of any of those policies' rows works too. A grouped draft can't be published yet, and a grouped decision can't be restored yet.

A change to the repository's rule files arrives through Actual AI's usual sync pull request.

### Saving a decision

```bash
actual adr create --file docs/adr/<file>.md
actual adr create --title "Use Postgres for storage" --policy "MUST: Services store relational data in Postgres."
```

`create` runs these steps in order and stops at the first that fails:

1. It reads the decision from the file or the flags. A file that doesn't parse, flags with no `--policy`, or a `--policy` without a level is refused before anything is sent.
2. It reads the repository's decisions in Actual AI and refuses when the file is already one of them, or when a decision looks the same by the rule in [Checking decision files](#checking-decision-files). `--allow-duplicate` sends it anyway. If the decisions can't be read, nothing is sent.
3. It sends the decision once and never retries. A decision from flags is saved the way the web app's New Decision saves one: a draft, or an accepted decision in one request. A file is saved the way the web app's Import saves it, which keeps the whole document and always saves a draft. Actual AI checks the decision as it saves it, and a refusal lists the reasons. When the request can't reach Actual AI, nothing was saved and the error says so. When the request was sent but no answer came back, or the answer couldn't be read, the decision may have been saved: the command exits 3 and asks you to look with `actual adr list` before you run `create` again.
4. With `--file`, a scope in the file's header is set on the new draft, and an accepted save then publishes it. If either step fails, the decision stays a saved draft and the command prints the commands that finish the job.
5. With `--file`, it renames both files to the names the sync uses for the new decision's id, and their headers take that id and the status the decision was saved with. A file pushed as a draft therefore stops governing locally until it's published and Actual AI's sync pull request rewrites its files, and the command says when the local status changed.

`--deciders` belongs to `adr new`: a decision saved from flags has no Deciders line, and a file keeps its own.

### Saving every decision file

```bash
actual adr create --all
```

`create --all` sends each decision file under `docs/adr/` that Actual AI doesn't have yet, one at a time, through the steps `create --file` takes. It reads the repository's decisions once, then takes the files in path order:

- A file whose id is already one of those decisions is skipped, so a second run sends nothing twice.
- Each file keeps the status its header names. An accepted decision is saved, published and written into the repository by Actual AI's sync pull request, and a draft stays a draft, whoever is at the terminal; `--all` takes no `--status`. A status under which a decision doesn't govern, such as `superseded` or `rejected`, keeps the file from being sent.
- A file that looks like a decision in Actual AI, or like one saved earlier in the run, is skipped by the rule in [Checking decision files](#checking-decision-files). `--allow-duplicate` sends it anyway.
- A file that doesn't parse, or that Actual AI refuses, isn't saved, and the run goes on to the next.

Each saved pair is renamed to the names the sync uses for its new id, as with `--file`, and a step after the save that fails leaves a draft and says how to finish. A line on standard error reports each file as the run reaches it, and the summary lists what was skipped or not saved, with the reason. A file takes up to four requests: the save, a read of the repository's decisions and a write when its header names a scope, and the publish when it's accepted. The run stops at the first save whose outcome is unknown (exit 3), and at the first error that isn't about the file, such as an expired sign-in, and names the files it never sent.

Signed out, `create --all` sends nothing. With a file to send, it exits 2 and names [`check`](#checking-decision-files), which lists the files that won't publish and why, and [`discover`](#finding-the-adrs-a-repository-already-has), which copies the ADRs other tools keep into `docs/adr/imported/`. Both work without a sign-in.

### Changing a decision

```bash
actual adr show <id>                                     # policy ids are in the output
actual adr update <id> --level <policy id>=SHOULD
actual adr update <id> --policy "<policy id>=MUST: Services store relational data in Postgres." \
                       --policy "MAY: Analytics jobs read from a Postgres replica."
actual adr update <id> --scope 'services/**' --scope '!services/legacy/**'
```

`--policy` gives the whole policy list after the change: prefix a policy's id and `=` to keep that policy, write a new one without an id, and leave out a policy to remove it. `--level` changes one policy's level and can't be combined with `--policy`. `--context` replaces the whole context, and an empty `--context ''` clears it. `--clear-scope` applies the decision to its whole repository again. A blank `--title`, or a policy with no statement, is refused before anything is sent. `update` saves the whole decision the way the web app's editor does, with the decision's current title, context and scope where the flags change nothing. An archived decision can't be changed until it's restored.

`update` prints the decision as it was saved. Editing the policy list can give the policies new ids, so take the ids for your next edit from that output or from `actual adr show`.

## Output and exit codes

Every command takes `--json` and prints its result as JSON on standard output. Errors go to standard error.

| Exit code | Meaning |
|-----------|---------|
| 0 | Done |
| 1 | A decision failed a check: the local save check, a `--policy` or `--level` value that doesn't parse, a blank `update --title` or policy statement, Actual AI's refusal of its content on `create` or `update`, `adr check`, `create`'s duplicate refusal, or a file `create --all` didn't save |
| 2 | Signed out, a sign-in without access to the decision, no connected repository could be chosen, or a value refused while the command line is read, such as a blank `--reason` |
| 3 | Actual AI couldn't be reached or answered with another error; no decision has the id given, or `update` names a policy the decision doesn't have; the decision can't take the change (such as publishing a decision that's already active); or a `create` whose outcome is unknown |
| 5 | A local file couldn't be read or written |
