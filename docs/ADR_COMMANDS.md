# ADR commands

`actual adr` writes, checks and manages Architecture Decision Records (ADRs). Each command that creates, reads, updates or deletes a decision has two pathways. Signed out, it works on the decision files under `docs/adr/` in your repository. After `actual login`, it works on your organization's decisions in Actual AI. `check`, `discover`, `normalize` and `refresh` read the files either way, and `restore`, `accept` and `reject` need a sign-in.

```bash
actual adr create "Use Postgres for storage" --policy "MUST: Services store relational data in Postgres."
actual adr check                      # check every decision file under docs/adr/
actual adr discover                   # which ADRs under docs/adr/ no check reads yet
actual adr normalize --all            # convert them, and re-index any whose doc changed
actual adr list                       # signed out, docs/adr/; signed in, this repository's decisions in Actual AI
actual adr publish <id>
actual adr refresh                    # bring .actual/rules/ in step, as the plugin's hooks do
```

This page describes the CLI on `main`. The signed-out pathways of `list`, `show`, `update`, `publish`, `unpublish` and `delete`, `create` as one command with `new` as its alias, and `refresh` arrive in the first release after 0.15.0. In 0.15.0, `new` and `check` are the commands that work signed out.

## The two pathways

| Command | Signed out: the files under `docs/adr/` | Signed in: your decisions in Actual AI |
|---------|------------------------------------------|----------------------------------------|
| `create`, also `new` | Write a new decision file, accepted unless you pass `--status draft` | Save a new decision, from flags or from a file |
| `list` | List the decision files that carry an ADR header | List your organization's decisions |
| `show <id>` | Show the file whose header names the id | Show the decision |
| `update <id>` | Change the file's title, context, policies, levels or scope | Change the decision's title, context, policies, levels or scope |
| `publish <id>` | Set the file's status to accepted | Publish a draft |
| `unpublish <id>` | Set the file's status to draft | Move an active decision back to draft |
| `delete <id>` | Remove the file; git brings it back | Archive the decision, as Delete does in the web app |
| `restore <id>` | Refused: git brings back a deleted file | Restore an archived decision |
| `accept <id>`, `reject <id>` | Refused: only Actual AI keeps suggestions | Accept or reject a suggestion |

A stored sign-in picks the pathway, whether or not it still works: a lapsed one makes a command fail with the step that fixes it, and never falls back to the files. Signed out, a change to a decision file brings its rule file under `.actual/rules/` in step at once, as [Keeping .actual/rules in step](#keeping-actualrules-in-step) describes, so `plan-check` and `impl-check` read the change on their next run.

## Decision files in the repository

A decision kept in the repository has two files, laid out the way Actual AI's repository sync writes them:

| File | Content | Read by |
|------|---------|---------|
| `docs/adr/<id>-<title>.md` | A YAML header, then the decision document: title, `Date:` and `Deciders:` lines, Context, Decision with the policies, and any Rationale, Consequences and References | People, and `actual adr check` |
| `.actual/rules/cross-cutting-<topic>-<hash>.md` | The same header, then a `### Rules` list with one rule per policy | `plan-check`, `impl-check` and `rules` |

The header carries the decision's id, its status and the paths it applies to. Both file names come from the id and the title, so when Actual AI later syncs the decision it updates these files instead of adding a second copy.

### Writing a decision

```bash
actual adr create "Use Postgres for storage" \
  --context "Each service picked its own database, and three engines are now in production." \
  --policy "MUST: Services store relational data in Postgres." \
  --policy "MUST NOT: Services open SQLite files outside tests." \
  --deciders "Platform team" \
  --scope 'services/**'
```

Signed out, `create` writes the decision file under `docs/adr/` and, for an accepted decision, its rule file under `.actual/rules/`. `new` is the same command, and the title can also come as `--title`.

| Flag | Meaning |
|------|---------|
| `--context POINT` | Why the decision is needed, one point per flag. Optional |
| `--policy "LEVEL: statement"` | One policy per flag. LEVEL is `MUST`, `MUST NOT`, `SHOULD`, `SHOULD NOT` or `MAY` |
| `--deciders NAMES` | The decision's Deciders line. Signed out only: a decision in Actual AI has none |
| `--scope GLOB` | A path the decision applies to, relative to the repository root, one per flag. A leading `!` excludes. Leave it out for the whole repository |
| `--status accepted\|draft` | See [Accepted or draft](#accepted-or-draft) |
| `--root PATH` | Repository root to write into. Defaults to the current directory |
| `--json` | Print the files written as JSON |

`create` runs the same check Actual AI runs before it saves a decision, and writes nothing when the decision would be refused; each reason is listed. A `--policy` without a level is refused before that check runs. The new decision gets a random local id, and an existing file is never replaced. When the rule file can't be written, the decision file is removed again and the command fails.

`--file`, `--repo` and `--allow-duplicate` belong to the signed-in pathway, and signed out each one is refused with what to do instead: a file already under `docs/adr/` needs no sending, since `actual adr check` checks it and `actual adr refresh` writes its rule file.

### Checking decision files

```bash
actual adr check                       # every file under docs/adr/ that carries a decision header
actual adr check docs/adr/<file>.md    # only these files
actual adr check --no-cloud            # compare only with the files in this repository
```

`adr check` runs Actual AI's save check on each file and lists likely duplicates: pairs of files in the repository and, when you're signed in, decisions in Actual AI. Those are the connected repository's decisions, or the whole organization's when the checkout names no connected repository, and the report says which. It exits 1 when a file fails the check. A likely duplicate is reported without failing the run. When the comparison with Actual AI can't run (signed out, or the request failed), the report says why and the local check still counts.

Two decisions look the same when their titles share at least half their content words, or when any policy of one does with any policy of the other. Content words leave out common English words and the policy levels, and the comparison runs locally with no model.

### ADRs written elsewhere

An ADR someone wrote by hand, or a coding agent wrote, governs nothing until it has a rule file under `.actual/rules/`. `discover` finds those ADRs and `normalize` converts them.

```bash
actual adr discover                         # every file under docs/adr/, by state
actual adr normalize --all --dry-run        # what normalize would write: each converted doc as a diff
actual adr normalize --all                  # convert, and re-index docs that changed
actual adr normalize docs/adr/0007-x.md     # only this file
```

`discover` gives each file one of these states:

| State | Meaning | `normalize` |
|-------|---------|-------------|
| `synced` | Actual AI's sync wrote it | leaves it |
| `synced-edited` | A synced doc with uncommitted changes, or, when you're signed in, a title or policies that differ from the decision in Actual AI. The next sync overwrites the change | leaves it: save the change with `actual adr update`, or revert it |
| `managed` | This CLI wrote it, and its rule file is up to date | leaves it |
| `stale` | This CLI wrote it, and the doc's title, status, scope or policies changed since (or the doc moved) | writes the rule file again |
| `missing-rule` | The doc has a decision header but no rule file | writes the rule file |
| `broken` | The doc had a decision header but no longer reads as a decision, or lost its header | converts it again, keeping its id |
| `unmanaged` | An ADR in another format | converts it |
| `possible-duplicate` | Like `unmanaged`, but it looks like a decision the repository already has | asks first |
| `legacy` | Written by `actual adr-bot`, with a `.claude/rules/` file beside it | leaves it unless you pass `--legacy` |
| `orphan-rule` | A rule file whose decision has no doc | leaves it |
| `ignored` | `README.md`, `index.md`, `toc.md`, `summary.md`, a template (`template.md`, `adr-template.md`, `_template.md`), a `0000-*` file, or a file containing `<!-- actual:ignore -->` | leaves it |
| `unreadable` | The file couldn't be read, such as one that isn't UTF-8 | leaves it |

A rule file this CLI writes records the doc it came from under `metadata.actual.source`: the doc's path and a digest of its title, status, scope and policies. That record tells a CLI-written decision from a synced one, and shows when the doc has changed since. Rewording a doc's context or rationale doesn't make its rule file stale. `adr new` writes the record too.

`normalize` converts without a model whenever the file's layout allows: a Context File, a Nygard ADR (Status, Context, Decision, Consequences), a MADR (front matter, Context and Problem Statement, Decision Outcome) and adr-bot's layout all convert directly. A decision line with an upper-case RFC-2119 keyword (`Services MUST …`) becomes a policy, and so does each bullet under a `## Policies` heading, at MUST. Sections the Context File has no place for, such as Considered Options, are carried over after it word for word, so nothing the author wrote is lost. A decision section's `###` sub-sections map as their own sections would, so MADR's `### Consequences` under Decision Outcome fill the Consequences lists and its `### Confirmation` is carried as a section of its own; neutral consequences, which the Context File has no list for, are carried as `Neutral Consequences`. Front matter keys nothing reads, such as MADR's `consulted` and `informed`, are carried as a `Front Matter` section, and MADR 4's `decision-makers` fills the Deciders line. A carried section whose heading the Context File reads, such as a Decision written as prose, is renamed `Original Decision`. A context, rationale or references section that holds a code block, a table or a sub-heading is carried whole the same way (`Original Context`), since a list would flatten it. A status that says more than its one word, such as `Superseded by ADR-0009`, is carried as `Original Status`; the header takes the word, and the Context File's own `Status:` line, which Actual AI keeps to `proposed` or `accepted`, reads `proposed` for anything not accepted. A date written as `March 2, 2019` or `2019/03/02` becomes `2019-03-02`, and one that can't be read is carried as `Original Date` while the Context File takes the day `normalize` ran. The doc keeps its file name unless you pass `--rename`. `--dry-run` shows each converted doc as a diff and names the rule file it would write, without showing that file; a re-index is listed as `would re-index` with its rule file, and no diff.

A decision converted again (`broken`, or `missing-rule` when its doc no longer reads as a decision) keeps the scope its header declares, or, when the header is gone, the scope its rule file declares, and a re-index keeps a header's package root. When neither can be read, the scope is asked about. A glob is checked against the repository's files the way the checks match it: brace lists such as `src/**/*.{ts,tsx}` expand, and hidden directories such as `.github/` count.

When the layout gives no policy, because the decision is written as prose, `normalize` asks a model through the configured runner (`--runner`, `--model`, `--no-model`). No Actual AI account is needed: a signed-in `claude` CLI or an API key is enough. The model reads the docs one at a time before any question is asked, so for many docs this takes a while; at a terminal, `normalize` shows which doc it's reading and how many are left on standard error. Nothing is drawn with `--json` or when standard error isn't a terminal. The report notes each doc whose policies a model wrote, so a person can review them.

What's still open after that becomes a question: is this a decision at all, which policies, governing or draft, and which paths when a glob matches no file. At a terminal, `normalize` asks, and says once before the first question that `--yes` would take the safe answers instead. Without one, it writes nothing for that file and lists the questions in its report (with `--json`, as `questions`). There are two ways to answer:

- `--answers answers.json` takes the answers, keyed by each doc's path from the repository root, with the field each question names: `{"docs/adr/0007-x.md": {"status": "accepted", "policies": ["MUST: …"], "scope": ["src/**"]}}`.
- `--yes` takes the safe answer to every question. The decision is written as a draft, which no check reads, and a file with no policy is left alone.

`--extracted extracted.json` takes extractions in the same shape in place of a model, for an agent that reads the ADR itself.

`--push` saves each decision it writes to Actual AI, as `actual adr create --file` does, and needs `actual login`. Actual AI's sync then owns the files. When a file is written but can't be saved (signed out, a refusal, or a status other than accepted or draft), the report says why and the command exits 3.

ADRs `actual adr-bot` wrote are reported as `legacy` and left alone. adr-bot writes them again on its next run, and Actual AI's sync replaces them for a signed-in repository. `normalize --legacy` converts them in place, keeping each one's id, and lists its `.claude/rules/` file for you to remove.

## Accepted or draft

An accepted decision governs at once: `plan-check` and `impl-check` read its rules. A draft is read by no check until it's published.

Signed out, `create` writes an accepted decision unless you pass `--status draft`, from a terminal, a coding agent's shell, a script or CI alike. A draft file gets no rule file until `actual adr publish` accepts it. Signed in, `create` defaults to `accepted` when standard input is a terminal and to `draft` when it isn't, so an agent that saves a decision to Actual AI leaves a draft for a person to publish there. `--status` sets either one explicitly.

## Keeping .actual/rules in step

```bash
actual adr refresh
actual adr refresh --json --timeout-ms 800
```

`adr refresh` brings the rule files under `.actual/rules/` in step with your decisions. It's the step the plugin's hooks run on every call, and it runs on its own whenever you want it to.

- **Signed out**, each accepted decision file under `docs/adr/` gets a rule file. A file you delete, or whose status stops being accepted, takes its rule file with it.
- **Signed in**, each active decision of the connected repository your checkout's `origin` remote names gets a rule file, written from Actual AI. A decision that is archived, unpublished or gone takes its rule file with it. The read waits up to `--timeout-ms` (1,000 by default, 1 to 60,000), and when Actual AI is slower or can't be reached, every rule file stays as it is until the next refresh. The refresh never renews a sign-in itself: when the access token is about to expire, it leaves the rule files as they are and starts the renewal in the background for the next refresh, and once Actual AI has turned a renewal down, it asks you to sign in again with `actual login` and starts no other.

A rule file is written in the CLI's own words, under the name and header Actual AI's sync uses, and rewritten only when its decision changed after the file was written. A rule file the refresh didn't write, such as one from Actual AI's sync pull request, is left as it is until its decision next changes. One with no ADR header, or naming a decision the refresh doesn't follow, is never touched. Since the sync pull request writes the same folder, commit or delete the CLI's copies before you pull a sync merge.

To tell what changed, the refresh keeps a record for each checkout under `adr-refresh/` in the user config directory (by default `~/.actualai/actual/`): each decision's rule content when its file was last written or first seen, the list below as it was last reported, and the connected repository it last read. A missing or damaged record only means the existing rule files are met again. A record the refresh can't save is listed in its `problems`, since until it's saved, rule files don't follow later edits or deletions of their decisions.

The refresh also lists each file under `docs/adr/` that isn't normalized, with the reasons, when that list changes. A normalized file has a version 2 ADR header naming its id and passes `actual adr check`, as `create` writes it; anything else gets no rule file, and a decision whose file stops being normalized keeps the rule file it had. READMEs, indexes, templates and everything under `docs/adr/drafts/` are left out.

The refresh exits 0 whatever happens, so a hook that runs it can't fail on it. `--json` prints what it did: `mode` (`signed-out` or `signed-in`), the rule files `written`, `rewritten` and `removed`, `kept` when Actual AI couldn't be read, the `unnormalized` files with their reasons, `unnormalized_changed`, a `message` set when a person should hear about it, and any `problems`. `rules brief`, `plan-check` and `impl-check` never refresh by themselves, so a check you run directly judges the rule files as they are.

## Decision files signed out

Signed out, `list`, `show`, `update`, `publish`, `unpublish` and `delete` work on the decision files under `docs/adr/`, leaving out `docs/adr/drafts/` as the refresh does. A command that takes an id finds the file whose ADR header names it, the id `list` prints, and refuses one found only under `docs/adr/drafts/`, saying to move it into `docs/adr/` first. A file stores no policy ids, so `show` gives each policy the rule id its rule file prints, such as `R-0B6F-001`, and `update` takes those ids with the flags it takes signed in, described in [Changing a decision](#changing-a-decision).

- `list` takes `--state active` or `--state draft`, with `--limit` and `--offset`, and says how many files it left out for having no ADR header. `--repo` and the states only Actual AI keeps (`suggested`, `updated`, `archived`) are refused.
- `update` writes the whole file back, so it refuses a file with text or header fields outside the layout `create` writes, rather than drop them; edit such a file yourself. A file under the sync's name moves to the name for its new title.
- `publish` sets an unaccepted file's status to accepted and `unpublish` sets an accepted file's status to draft, each after the file passes the check an accepted decision has to pass: a file that fails it gets no rule file, and one that already has a rule file keeps it until the file passes again. Each changes the header's status and the Context File's `Status:` line.
- `delete` removes the file and its rule file. For a file git tracks, `git restore <file>` brings it back while the deletion is uncommitted, and the next refresh writes its rule file again. A file git never tracked can't be brought back, and `delete` says so.

Each change brings the decision's rule file in step before the command returns, and says which rule file it wrote, rewrote or removed. `restore`, `accept` and `reject` act on records only Actual AI keeps, its archive and its suggestions, so signed out they're refused with what to do instead.

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

`actual adr refresh`, which the plugin's hooks run, writes the rule files of the repository's active decisions into `.actual/rules/`, and Actual AI's sync pull request writes its own copies of the same files.

### Saving a decision

```bash
actual adr create --file docs/adr/<file>.md
actual adr create "Use Postgres for storage" --policy "MUST: Services store relational data in Postgres."
```

`create` runs these steps in order and stops at the first that fails:

1. It reads the decision from the file or the flags. A file that doesn't parse, flags with no `--policy`, or a `--policy` without a level is refused before anything is sent.
2. It reads the repository's decisions in Actual AI and refuses when the file is already one of them, or when a decision looks the same by the rule in [Checking decision files](#checking-decision-files). `--allow-duplicate` sends it anyway. If the decisions can't be read, nothing is sent.
3. It sends the decision once and never retries. A decision from flags is saved the way the web app's New Decision saves one: a draft, or an accepted decision in one request. A file is saved the way the web app's Import saves it, which keeps the whole document and always saves a draft. Actual AI checks the decision as it saves it, and a refusal lists the reasons. When the request can't reach Actual AI, nothing was saved and the error says so. When the request was sent but no answer came back, or the answer couldn't be read, the decision may have been saved: the command exits 3 and asks you to look with `actual adr list` before you run `create` again.
4. A scope, from `--scope` or from the file's header, is set on the new draft, and an accepted save then publishes it. If either step fails, the decision stays a saved draft and the command prints the commands that finish the job.
5. With `--file`, it renames both files to the names the sync uses for the new decision's id, and their headers take that id and the status the decision was saved with. A file pushed as a draft therefore stops governing locally until it's published and Actual AI's sync pull request rewrites its files, and the command says when the local status changed.

Signed in, `--deciders` is refused: a decision saved from flags has no Deciders line, and a file keeps its own.

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

Every command takes `--json` and prints its result as JSON on standard output. Errors go to standard error. `refresh` always exits 0.

| Exit code | Meaning |
|-----------|---------|
| 0 | Done |
| 1 | A decision failed a check: the local save check, a `--policy` or `--level` value that doesn't parse, a blank `update --title` or policy statement, Actual AI's refusal of its content on `create` or `update`, `adr check`, or `create`'s duplicate refusal |
| 2 | A command or flag the pathway can't take, such as `accept` or `create --file` signed out; a sign-in without access to the decision; no connected repository could be chosen; or a value refused while the command line is read, such as a blank `--reason` |
| 3 | Actual AI couldn't be reached or answered with another error; no decision, or no decision file signed out, has the id given, or `update` names a policy the decision doesn't have; the decision can't take the change (such as publishing a decision that's already active, or updating a file `update` can't write back whole); or a `create` whose outcome is unknown |
| 5 | A local file couldn't be read or written |
