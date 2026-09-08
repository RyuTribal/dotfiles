# `mach kb improve` — self-improvement loop from the knowledge bank to Claude's own config

Date: 2026-09-08. Status: approved in conversation (all decisions below were chosen by the user).

## Goal

Periodically read what the knowledge bank has learned about how the user works with Claude (preferences, corrections, rejections, what works) and let Claude edit its own configuration — skills, the global CLAUDE.md, hooks and settings.json — so the same friction does not recur. Edits are applied autonomously and recorded as chezmoi commits; the user reviews the diff after the fact.

## Decisions

| Question | Choice |
|---|---|
| Output authority | Full auto-apply, tracked by chezmoi commits. No proposal queue. |
| Write targets | `~/.claude/skills/**`, `~/.claude/CLAUDE.md`, `~/.claude/settings.json`, `~/.config/claude-hooks/**`. Nothing else. |
| Where it lives | Hybrid. Rust (`engines/kb/src/improve.rs` + `cli::cmd_improve`) owns gating, evidence bundle, snapshot, verification, commit, outcome logging. One agentic `claude -p` call owns judgment and file edits. |
| Evidence | Knowledge bank (new memories, affective graph edges, mental model, prior improve outcomes) plus per-skill usage counts captured by `ingest-sessions`. No raw transcript excerpts. |
| Cadence | Chained into `mach-reflect.service` after `mach kb reflect`. Cheap DB-only gate: run only when at least `MACH_IMPROVE_MIN_SIGNAL` (default 5) new signal rows exist since the last improve watermark. |
| Closing the loop | Every run outcome (applied, none, failed) is stored as an ordinary memory with `source = "improve <ts> <sha|none|failed>"` and `project = "claude-config"`. The next bundle includes all such memories, so the pass sees its own history and may revert a prior edit. |
| Per-run budget | No cap. Claude decides. |

## Data model

- New singleton table `improve_state (id=1, last_run_at, last_memory_id, last_relation_id, last_completed_at)`, mirroring `reflect_state`. Migration v10 -> v11.
- New column `ingested_sessions.skill_usage TEXT` holding JSON `{ "<skill>": {"invocations": n, "corrections": n} }`, filled by `run_ingest_sessions` from the raw transcript with a no-LLM heuristic (`ingest::skill_usage_from_transcript`). A correction is a human user message that directly follows a `Skill` tool call and starts with a correction cue (no / wrong / stop / don't / undo / revert / not that / why did you).
- No other schema changes. Improve outcomes are memories.

## Gate (no network, no claude spawn)

1. Signal count since watermark = new active memories (`id > last_memory_id`) + new active relations (`id > last_relation_id`) whose predicate is in {`prefers`, `rejects`, `values`, `frustrated-by`, `wants`, `avoids`, `dislikes`, `corrects`}. Below threshold: mark completed, exit 0.
2. `reflect::claude_reachable()` false: exit 0 without marking completed (same offline-defer semantics as reflect).
3. Every write target must be chezmoi-managed (`chezmoi managed --path-style absolute`). Any target unmanaged: fail run, record failure memory. Note: directories are managed as a whole, so a new skill directory under a managed `skills/` is acceptable and gets `chezmoi add`ed afterwards.
4. Chezmoi source repo must have a clean `git status --porcelain` and `chezmoi status` must list none of the write targets as modified. Otherwise fail run (never mix human WIP with an auto commit).

## Bundle (prompt sections)

1. Mental model — same rows `mach kb model` prints.
2. New memories since watermark (id, date, content, source), plus every active memory whose source starts with `improve ` (prior outcomes), plus every active memory with `project = "claude-config"`.
3. Affective relations since watermark rendered as `src predicate dst (evidence m<id>: <content>)`.
4. Skill usage totals over the last 30 days of ingested sessions: skill, invocations, corrections, sessions.
5. Inventory of current write targets: path and byte size for each `SKILL.md`, `CLAUDE.md`, `settings.json` and each file under `claude-hooks/`. Contents are not inlined; Claude reads what it needs.
6. Instructions and the output contract (below).

## Claude call

`claude -p --model <MACH_IMPROVE_MODEL, default "sonnet"> --no-session-persistence --permission-prompts none --allowedTools <list>` with the prompt on stdin, `MACH_KB_DIGEST=1` in the environment, timeout `MACH_IMPROVE_TIMEOUT_SECS` (default 900). Working directory is the user's home.

Allowed tools: `Read`, `Glob`, `Grep`, `Edit(~/.claude/skills/**)`, `Write(~/.claude/skills/**)`, `Edit(~/.claude/CLAUDE.md)`, `Edit(~/.claude/settings.json)`, `Edit(~/.config/claude-hooks/**)`, `Write(~/.config/claude-hooks/**)`, `Bash(bash -n *)`, `Bash(python3 -m json.tool *)`. Everything else is denied. `--no-session-persistence` keeps the run out of `~/.claude/projects`, so `ingest-sessions` never digests it.

Prompt duties: read a file before editing it; prefer extending an existing skill over creating a new one; never duplicate an existing skill's job; new skills get YAML frontmatter with `name` matching the directory and a `description` with trigger phrases, body in normal prose; hook scripts stay bash, never `set -e`, every path exits 0, must pass `bash -n`; `settings.json` must stay valid JSON; CLAUDE.md rules may be tightened or appended, a deletion must cite the evidence memory that contradicts it; revert a prior improve edit when the same signal persisted or worsened after it; never write secrets or touch paths outside the allowlist; doing nothing is the expected outcome most runs.

Output contract — the reply must end with:

```
IMPROVE-RESULT
action: edit|create|revert|none
files: <path>, <path>
rationale: <one paragraph>
evidence: m12, m45, r7
```

## Guard, verify, commit (Rust, after the call)

1. Snapshot: before the call, copy every write-target tree into `$XDG_RUNTIME_DIR/mach-improve/<ts>/` (fallback `/tmp`). Restore = delete each target tree and copy the snapshot back, which also removes files Claude created.
2. Missing or malformed `IMPROVE-RESULT`, non-zero exit, or timeout: restore snapshot, run failed.
3. `chezmoi status`: any managed path modified that is outside the allowlist: `chezmoi apply --force <path>`, restore snapshot, run failed.
4. `action: none`: the diff of the write targets against the snapshot must be empty. Otherwise restore, failed.
5. Per-file verification of everything that differs from the snapshot:
   - `claude-hooks/*.sh`: `bash -n` passes, contains `exit 0`, is executable.
   - `settings.json`: parses as JSON, `hooks` is an object, every hook `command` that references a file under `claude-hooks/` points at an existing file.
   - `SKILL.md`: frontmatter present, `name` equals the directory name, `description` non-empty.
   - `CLAUDE.md`: non-empty and not shrunk by more than 20 percent.
   Any failure: restore snapshot, failed.
6. `chezmoi add` new paths, `chezmoi re-add` modified ones, then `git commit` in the chezmoi source repo with message `improve: <action> <basenames>` and a body carrying the rationale and evidence ids. No push.
7. `mach kb add` an outcome memory (success, none, or failure with reason), `--project claude-config`, `--no-classify`, importance 6, `source = "improve <ts> <sha|none|failed>"`.
8. `notify-send` one line on an applied change or a failure. Silent on `none`.
9. Watermarks (`last_memory_id`, `last_relation_id`, `last_run_at`) advance only when the run completed (applied or clean `none`), never on an LLM failure. `last_completed_at` is set on every completion including the cheap "below threshold" exit, for `mach kb health`.

## Health

`mach kb health` gains an `improve` check: fails when the last completion is older than 48 hours, or when the three most recent improve outcome memories are all failures.

## Testing

Pure functions in `improve.rs` and `ingest.rs` are unit-tested without spawning claude: signal counting, prompt building, result parsing, path allowlisting, chezmoi status parsing, skill-usage extraction, every verifier against fixture files in a scratch directory, snapshot/restore round trip. The orchestration in `cli::cmd_improve` is exercised through a trait (`ImproveLlm`) with a fake that writes files into a scratch tree so the guard path is tested end to end.

## Prerequisites (one-time, by hand)

- `chezmoi add ~/.claude/settings.json` (it is not managed today; the pass refuses to run otherwise).
- Reinstall the systemd unit after adding the `ExecStart=%h/.local/bin/mach kb improve` line.
