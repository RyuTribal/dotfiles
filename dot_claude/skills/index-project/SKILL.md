---
name: index-project
description: Use when the user asks to index, memorize, or "learn" a coding project ("index this project", "memorize this repo", "add this project to memory"), or when starting substantive work in a coding project that has no project-index memories yet (check recall / `mach kb search "<project> architecture"`). Writes the durable narrative map of a codebase to memory (`mach kb`) — identity, architecture, workflows, gotchas — and makes sure its code content is registered for the automatic code index (`mach kb index`). Coding projects only — never plain document folders, media directories, or one-off scratch dirs.
---

# index-project — commit a project's map to memory

Two separate things happen under this skill's name, and only one of them is
still yours to do by hand.

**Code content — chunks, per-chunk context headers, embeddings — is
automatic.** `mach kb index` runs nightly via `mach-index.timer`
(03:30; it and `mach kb reflect` share `~/.local/share/mach/kb-job.lock` —
`mach kb index` takes it itself, reflect via `flock(1)` — so they never
write kb.db at the same time, and neither does a manual index run —
whichever comes second waits) over every registered project whose
root is a git repo, incrementally, forever. `install.sh` installs that
timer but does not enable it: after a manual first run
(`mach kb index --dry-run`, `mach kb index`, `mach kb index status`) enable
it with `systemctl --user enable --now mach-index.timer`. The first fill of
all projects was estimated at ~35 nights at the default budget of 300
(an upper bound — the scope pass skips vendored/generated directories);
each full-budget night is roughly an hour of header calls. Never
open files across a codebase and hand-write facts about what's in them —
that used to be this skill's whole job before the code index existed; now
it is a scheduled job's job, not something an agent decides to do file by
file.

**The directory-level "what/how" half of the narrative layer is now
automated too — the rest still isn't.** Phase 2 of the code index (landed
2026-09-24) writes file, module (depth 1–2), and repo summaries straight
from source in the same `mach kb index` run that does chunking and
headers — no separate step, no extra command. A module summary states its
purpose, main components, control flow, extension points, and gotchas;
a short LEAD of each (first paragraph, ≤600 chars) is mirrored into
memory as an index-owned row (`source = code-index:<project>:<dir>/`,
`basis = derived`) ending in a pointer, `(full: mach kb ask --project <p>
… overview <dir>/)` — so plain recall surfaces the gist and tells you
where the rest is, not the whole summary. The FULL text lives only in the
code index: `mach kb ask --project <p>` reads it through its
`overview`/`search` tools. A `--path-prefix` index run never touches the
repo-level summary. Verified end-to-end on `helios-picking` (2026-09-24, real
`claude -p` calls against a bank copy): the generated module summary named
the same component list, per-frame data flow, and gotchas (silent
no-provider misses, `.before()` ordering, the "renderer must never appear
in `target_link_libraries`" invariant) an agent used to have to read every
file by hand to write down — and it regenerates itself the moment a child
file's summary changes, so it never goes stale the way a hand-written fact
does.

**The dated "what changed and why" half is automated too, since
2026-09-24.** The last stage of the same `mach kb index` run summarizes
every distinct commit-history month (`code_history`, `index_history`
sonnet calls, budgeted and resumable like everything else): per author,
areas touched, and the intent commit messages actually state, one summary
per `YYYY-MM`, newest-first — the stored month text IS that one-paragraph
summary (not the raw log) — mirrored into memory as a dated,
never-superseded-except-by-regeneration row (`source =
code-history:<project>:<YYYY-MM>`, its lead the first ~600 chars of the
summary). Only months whose commit set changed cost anything (one cheap
`git log` pass decides; unchanged months spawn no numstat and no call).
`mach kb ask --project <p>` reads it through `history {}` / `history
{"month"}` / `history {"path"}`, which list citable `path@<commit7>`
tokens (history answers cite those). `mach kb index --dry-run
[--history-only]` counts the months a run would regenerate. Verified
end-to-end on `helios` (2026-09-24, real `claude -p` calls against a bank
copy): all 9 of its distinct commit months, each summary's stated author,
date, and commit count cross-checked exactly against `git log`. Run just
this stage on its own with `mach kb index --project <name> --history-only`
(requires `--project`, rejected together with `--path-prefix`) — skips
scope/chunk/header/summary work entirely, useful to backfill a project's
whole history without paying for a full index first; measured cost across
every registered git repo, 2026-09-24: **129 calls total** for a first
fill of every project's entire commit history.

What it still can't give you: **identity** (who the project is for, where
it runs), **workflows** beyond what the project card already derives
mechanically, **current** state (an in-flight refactor, a deliberately
unfinished piece, an uncommitted local change — the history layer above
only ever sees committed months), **people** beyond commit authorship, and
any judgment call the code and its commit messages don't state out loud
(why a boundary was drawn, which invariant is load-bearing vs. incidental,
a decision made in conversation and never written down). Those still need
a human-shaped read and still get written with `mach kb add`. This is what
the rest of this skill covers.

Same voice rule as all memory: these become things you *know* about the
project ("Umoja's hub talks to ground stations through the ModemDriver
trait"), never database rows you fetched.

## The automatic code index

```
mach kb index [--project P] [--budget N] [--path-prefix P] [--history-only] [--dry-run]
mach kb index status [--project P]
mach kb index scope <project> [--set <dir>=<category>]...
```

`mach kb index` compares each project's committed tree at `HEAD` against
its stored per-file blobs, chunks in-scope files whose blob changed
(tree-sitter), gets each dirty file an LLM context header plus a file
summary (one combined call), and embeds its chunks. Once every dirty file
in a run is done, the same invocation regenerates any stale-or-missing
module (depth 1–2) or repo summary bottom-up, sharing the same budget —
this is the phase-2 step described above; it needs no separate command or
flag. An unchanged blob is never re-chunked, so budget-limited runs always
converge. `--budget` (default 300) caps LLM calls — headers, file
summaries, and module/repo summaries all count as one call each — for the
run and stops cleanly once spent — left-over `dirty` files and
stale-or-missing summaries finish next time, including next night's
scheduled run. `--dry-run` prints the plan (files per project, calls
needed) and changes nothing — cheap to run just to see where a project
stands, but note its call estimate only counts file-level work (scope pass
+ headers), not module/repo summary calls, since which directories need a
summary depends on what's actually dirty when the run happens; expect a
real run to spend a modest number of calls above the `--dry-run` figure
for module/repo summaries (see "The code index" in the kb skill for a
measured example). `--path-prefix` requires `--project`, accepts a
trailing slash, finishes only the `dirty` files and file summaries under
the prefix, never advances the indexed-head watermark, and
regenerates only module summaries under the prefix (never the repo
summary or its memory); an unknown
`--project` or a non-numeric `--budget` is an error. One project's error
(say, a repo with no commits) is printed on its line and the run carries
on with the next project. After 5 failed LLM calls in a row a run stops
early and reports it ("stopped after 5 failed calls in a row"; per-project
`headers-failed=`/`months-failed=` counts show what wrote nothing) — wait
before re-running rather than looping. If the embedder is down, the run makes no
header calls (so none are wasted) and later runs backfill missing
embeddings for free.

Files that look like secrets — `.env*`, `*.pem`, `*.key`, `*.p12`,
`*.pfx`, `id_rsa*`, `id_ed25519*`, `*.kdbx`, `*.keystore`, any path with
`credentials`/`secret` in it, or content with a private key, GitHub/`sk-`/
Slack/AWS/Google token or JWT — are recorded `skipped` and never chunked,
sent to an LLM, or readable through `ask`. You don't need to scope them out
by hand.

`mach kb index status [--project P]` shows, per project: indexed commit vs
current `HEAD`, file counts by status (`indexed`/`fallback`/`dirty`/
`skipped`, with the skip reasons by pattern name), chunk count, embedded
percentage, and `summaries_given_up` (files whose summary call succeeded
twice in a row with no parseable `SUMMARY:` line — they stay `indexed`,
just permanently without one until their blob next changes) — or
`skipped (not a git repo)` for a registered project with no `.git`.

`mach kb index scope <project> [--set <dir>=<category>]` lists (no
`--set`) or overrides a directory's category (`product`/`tests`/`docs`
indexed; `vendored`/`generated`/`assets`/`build-output` are not). A scope
pass runs once per project and only reruns when a new top-level directory
appears; if its LLM call fails the project is skipped that night rather
than indexed on guesses. `--set` writes a `"user"` decision the scope pass
never overwrites (it errors if the directory has no tracked files or the
category is unknown), and the next run indexes or drops that directory's
files accordingly — reach for it if something that should be searchable
(or shouldn't be) got misclassified.

**Run it by hand when:**
- A project was just registered (or has never shown up in `mach kb index
  status`) and the user wants to ask detailed code questions about it now,
  not after tonight's 03:30 run — `mach kb index --project <name>`
  (`--dry-run` first if the call count is worth knowing before spending
  it).
- `mach kb index status --project <name>` shows files stuck `dirty` across
  more than one run (a persistent LLM failure) and the task at hand needs
  that file's content indexed.
- A directory is visibly miscategorized — vendored code that should be
  searchable, or generated output that shouldn't be — use `mach kb index
  scope <name> --set <dir>=<category>`.
- The user wants a project's commit-history months backfilled now without
  waiting for (or paying for) a full scope/chunk/header pass — `mach kb
  index --project <name> --history-only`.

Otherwise, leave it to the timer. Frequent manual runs against the same
project just spend the next scheduled run's budget for no benefit.

## What belongs in memory (the narrative layer)

Save the durable mental map — what stays true for months and would
otherwise be re-derived every session:

- **Identity**: what the project is, who it's for, where it runs.
- **Architecture**: major components and how they talk (protocols, queues,
  sockets, APIs between them).
- **Stack**: languages, frameworks, the load-bearing dependencies (the ones
  whose replacement would be a project, not a chore).
- **Workflows**: how to build, test, run, deploy — the actual commands.
  (Build/test commands and the repo layout itself are already covered,
  no LLM call, by the project card `mach kb projects refresh` rebuilds
  every reflect run — don't duplicate those; add a workflow fact only for
  what a card can't hold, like deploy steps or a multi-command sequence.)
- **Constraints and gotchas**: things that bite — invariants from docs or
  comments, known footguns, "never do X here" rules, environment quirks.
- **State** (date-stamped): active branch focus, in-flight refactors,
  significant TODOs. Prefix with "as of YYYY-MM-DD" — state facts go stale
  by design and the date lets later recall weigh them.
- **People**: owners, reviewers, stakeholders when the repo or user makes
  them evident — attributed by name ("Moses reviews Umoja hub changes").

Do NOT save: individual function signatures, line numbers, code that a
`grep` or `mach kb index status`-backed search answers faster than recall,
anything that changes weekly, or restatements of a CLAUDE.md the session
already loads. Fine-grained code detail belongs to the code index, not
this skill.

**Never index secrets.** No credential values, no `.env` contents, no
tokens — describe that a config exists and where it lives ("telegram token
lives outside the repo in ~/.local/share/mach/telegram.toml"), never quote
its contents. Same rule as everywhere in the kb.

## Procedure

1. **Confirm it's a coding project.** Source files, a build manifest
   (Cargo.toml, package.json, go.mod, pyproject.toml, Makefile...), or the
   user calling it one. If it's a documents folder or scratch dir, say so
   and stop.
2. **Name it.** Manifest name or directory name; ask only if genuinely
   ambiguous. Use lowercase for `--project` (matches existing convention:
   umoja, mach, helios).
3. **Check what memory already holds**:
   `mach kb search "<project> architecture" --json` and
   `mach kb search "<project>" --json`. If a project-index already exists
   this is a **re-index**: save only what changed or is missing — see
   "Re-indexing" below.
4. **Check the code index's own state**: `mach kb index status --project
   <name>`. If it's never been indexed (or the project isn't git-tracked
   yet), say so in the report — code-level detail search won't be
   available for it until it is. Run `mach kb index --project <name>`
   yourself only per the "run it by hand" cases above.
5. **Gather the narrative facts.** README and docs, manifests, entry
   points, CI config, test layout — enough to describe the architecture
   truthfully, not just restate a README's claim of it. This no longer
   needs to be an exhaustive read of the codebase: the code index makes
   fine-grained detail separately searchable, so a light pass (plus a
   targeted read or a quick Explore lookup for any one specific claim
   you're unsure of) is enough for the identity/architecture/workflow
   level this skill writes.
6. **Save 10–25 facts**, one `mach kb add` each:

   ```
   mach kb add "<self-contained fact>" --project "<name>" \
       --source "project-index:<name>" --importance 6
   ```

   - **Pass `--no-classify` on index runs.** Index facts about one project
     are similar by construction (same project, same source, overlapping
     vocabulary) and the add classifier will wrongly supersede one with the
     next; on a re-index, drop `--no-classify` only for facts that really do
     replace an old claim. Verified failure mode: a machd-subsystems fact
     superseded the workspace-overview fact on mach's own first index.
   - importance 7 for identity and architecture facts, 6 for the rest,
     5 for state facts (they should decay faster).
   - Each fact self-contained — readable alone, months later, with the
     project named in the sentence ("In mach, ..."), absolute dates only.
   - Attribute people by name inside the fact, same as the note channel.
7. **Report**: fact count, one-line summary of what memory now covers, the
   code index's status line for this project, and anything deliberately
   left out.

## Re-indexing (updating the narrative map)

The derivable half — layout, manifests, entry points, test command,
remote — is rebuilt automatically every reflect run by `mach kb projects
refresh`, no LLM call, never stale. The code-content half — what's
actually in the files — is the automatic code index above, also never
hand-rebuilt. What this section covers is the interpretive half:
architecture, invariants, workflows, decisions. Those cannot be
recomputed, so they still need you.

You will be told when. At SessionStart, a project whose narrative index has
fallen past `PROJECT_DRIFT_COMMITS` commits or `PROJECT_DRIFT_DAYS` days
behind prints a drift notice. Re-index when you see one, or when recall
about the project proves stale mid-session (wrong architecture claims,
dead paths).

After re-indexing, reset the watermark or the notice will keep firing:

    mach kb projects mark-indexed <project>

If the project's directory is gone for good instead (moved, deleted,
renamed outside mach's tracking), there is nothing left to re-index — drop
the registry row so `mach kb health` stops flagging it:

    mach kb projects forget <project>

This only removes the tracking row. Every `project-index:<name>` memory it
built stays in the bank untouched, and so does anything the code index has
built for it; `forget` is not the same as deleting either.

Project identity is a fingerprint (`git:<root-commit>`, or `path:<abs>` for
non-git directories), not the folder name, and the project key is the
LOWERCASED basename. A rename or a case change is detected on the next
`mach kb projects refresh` and every `project-index:<name>` memory is
re-tagged automatically. Before this existed, `~/programming/Expedite`
tagged its sessions "Expedite" while its 28 index rows sat under
"expedite", unreachable.

An update is a diff, not a rewrite:

1. Enumerate the current index EXHAUSTIVELY. `mach kb search` returns
   RANKED hits, so it silently shows only the top slice — a re-index driven
   by search reviews a fraction of the facts and then resets the watermark,
   which is worse than not re-indexing at all, because the drift notice
   stops firing over facts nobody read. Query the rows directly instead:

   ```
   sqlite3 ~/.local/share/mach/kb.db "select id, content from memories
     where source='project-index:<project>' and invalidated_at is null
     order by id"
   ```

   Count the rows first and hold that number: your review must account for
   every one of them.
2. Compare each existing fact against present reality.
3. **Still true** → leave it alone (no re-add — a duplicate wastes a
   dedupe-pass judgment).
4. **Changed** → add the corrected fact WITHOUT `--no-classify`, so the
   classifier supersedes the stale one (this is the one case where the
   bulk-index flag rule inverts — you WANT the update semantics here).
   NOT for a fact scoped to a date ("<project> state as of YYYY-MM-DD",
   "As of YYYY-MM-DD, X was N"): that is history, and superseding it
   destroys the only record of what was true then. Add the new snapshot
   with `--no-classify` and its own date, and leave the old one active.
   An uncommitted working-tree edit is likewise in-progress, not landed —
   record it as its own fact, never as grounds to supersede a
   committed-state fact.
5. **Gone entirely** (component deleted, workflow removed) → supersession
   has nothing new to attach to; state the removal as a fact ("As of
   YYYY-MM-DD, <project> no longer has X; replaced by Y") — a removal is
   knowledge too, and it invalidates the old claim through the
   contradiction pass.
6. **New territory** → index it like step 6 of the main procedure, with
   `--no-classify`.

Relationship edges need no separate handling: index facts are ordinary
memories, so the reflect graph pass extracts/updates entities and edges
from them organically on its next run — superseded facts stop feeding the
graph, corrected ones feed corrected edges through edge contradiction.
