---
name: index-project
description: Use when the user asks to index, memorize, or "learn" a coding project ("index this project", "memorize this repo", "add this project to memory"), or when starting substantive work in a coding project that has no project-index memories yet (check recall / `mach kb search "<project> architecture"`). Explores the codebase and commits a detailed, durable mental map of it to memory (`mach kb`). Coding projects only — never plain document folders, media directories, or one-off scratch dirs.
---

# index-project — commit a codebase to memory

After one run, future sessions recall this project's shape from memory
instead of re-exploring it cold. This is the deliberate counterpart to the
passive session-digest channel: digests capture what a conversation touched;
an index captures the whole map at once.

Same voice rule as all memory: these become things you *know* about the
project ("Umoja's hub talks to ground stations through the ModemDriver
trait"), never database rows you fetched.

## What belongs in memory vs what stays in the repo

Save the durable mental map — what stays true for months and would
otherwise be re-derived every session:

- **Identity**: what the project is, who it's for, where it runs.
- **Architecture**: major components and how they talk (protocols, queues,
  sockets, APIs between them).
- **Stack**: languages, frameworks, the load-bearing dependencies (the ones
  whose replacement would be a project, not a chore).
- **Layout**: what lives in which top-level directory, where the entry
  points are, where tests live.
- **Workflows**: how to build, test, run, deploy — the actual commands.
- **Constraints and gotchas**: things that bite — invariants from docs or
  comments, known footguns, "never do X here" rules, environment quirks.
- **State** (date-stamped): active branch focus, in-flight refactors,
  significant TODOs. Prefix with "as of YYYY-MM-DD" — state facts go stale
  by design and the date lets later recall weigh them.
- **People**: owners, reviewers, stakeholders when the repo or user makes
  them evident — attributed by name ("Moses reviews Umoja hub changes").

Do NOT save: individual function signatures, line numbers, code that a
`grep` answers faster than recall, anything that changes weekly, or
restatements of a CLAUDE.md the session already loads. Fine-grained code
detail belongs to the repo; memory holds the map, not the streets.

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
   this is a **re-index**: save only what changed or is missing — the add
   classifier supersedes stale near-duplicates automatically, so save
   updated facts normally and let it reconcile.
4. **Explore properly.** README and docs, manifests, entry points, the
   directory tree, CI config, test layout. Detailed means detailed: read
   enough real code to describe the architecture truthfully, not just the
   README's claim of it. For a large repo, an Explore agent for breadth
   plus direct reads of the load-bearing files is the right shape.
5. **Save 10–25 facts**, one `mach kb add` each:

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
6. **Report**: fact count, one-line summary of what memory now covers, and
   anything deliberately left out.

## Re-indexing

Re-run after a major refactor, or whenever recall about the project proves
stale mid-session (wrong architecture claims, dead paths). Don't re-run on
a schedule — the session-digest channel keeps incremental drift covered;
this skill is for the map, and maps get redrawn when the territory
changes, not weekly.
