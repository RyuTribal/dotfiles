# Bulletproof mid-session capture and tool-time recall

Date: 2026-09-08. Status: approved in conversation.

## Problem

Facts and decisions stated mid-session reach the knowledge bank only through two paths: the model choosing to run `mach kb add` (soft, skill text) or `mach kb ingest-sessions` at SessionEnd / 10-minute idle sweep. Nothing fires mid-session, nothing fires before compaction, and the digest is capped at five facts per session regardless of length. A 62k-line Umoja session had produced zero facts while still open.

Recall is keyed only on the raw user prompt embedding (threshold 0.45). Nothing surfaces memory at the moment the agent is about to create a file, plan, or delegate, which is exactly when "audit before build" memories matter.

## Changes

### 1. Incremental ingest (Rust)

- New table `session_progress (session_id TEXT PRIMARY KEY, last_line INTEGER NOT NULL, updated_at TEXT NOT NULL)`. Schema v11 -> v12. A row means "digested up to raw line `last_line`, session not finished." The existing `ingested_sessions` row keeps meaning "finished."
- `mach kb ingest-sessions --session-id X --partial`: read the transcript, take raw lines after `last_line`, skip if fewer than `PARTIAL_MIN_NEW_LINES` (40), filter that slice to dialogue, run the digest on it, insert facts unreviewed with source `session-digest`, advance `last_line`. Never marks the session ingested, never runs engagement verdicts.
- The final (non-partial) pass digests only lines after `last_line` (so partial and final never extract the same passage twice), runs engagement over the whole dialogue as before, marks ingested, deletes the progress row.
- Digest fact cap scales with dialogue size: `digest_fact_cap(dialogue_lines)` = 5 per 300 dialogue lines, clamped to [5, 25]. The instruction text carries the computed cap.

### 2. Checkpoint hook (`kb-checkpoint.sh`)

- Registered on `Stop` and `PreCompact`.
- `Stop`: bump a per-session counter under `$XDG_RUNTIME_DIR/mach-kb-checkpoint/<session_id>`; when the counter reaches `CHECKPOINT_EVERY_TURNS` (8) or `CHECKPOINT_EVERY_SECS` (900) elapsed since the last checkpoint, reset and spawn detached `mach kb ingest-sessions --session-id <id> --partial`. Exit 0 immediately in every case.
- `PreCompact`: run the same command synchronously with `timeout 90s`, so nothing about to be summarized away is lost. Exit 0 regardless.
- Skips when `MACH_KB_DIGEST=1` (nested automated claude) or when no session id.

### 3. Decision-cue hook (`kb-decision.sh`, UserPromptSubmit)

- Regex on the user prompt for decision language at the start of a sentence: `decision:`, `decided`, `let's go with`, `lets go with`, `we'll use`, `we will use`, `go with`, `from now on`, `final answer`, `approved`, `settled on`, `use X instead`.
- On match: `mach kb add "<prompt, capped 600 chars>" --source "decision-cue <date> session <id>" --unreviewed --no-classify --importance 6`, detached. Prints nothing (stdout would be injected as context). Refuses prompts that look like they carry a secret (`sk-`, `ghp_`, `AKIA`, `-----BEGIN`, `password`, `token=`).
- Reflect's curation and dedupe passes reconcile against the digest later.

### 4. Tool-time recall (`kb-pretool-recall.py`, PreToolUse)

- Matcher: `Write|EnterPlanMode|Agent|ExitPlanMode`.
- Query = project name (basename of `cwd`) + words from the tool input: file stem and parent directory for `Write`, first 20 words of `prompt`/`description` for `Agent`, "design plan" for plan-mode tools.
- Reuses `kb-recall.py`'s socket/subprocess search, dedupe window and recall log (same session id, so engagement gating still applies). Emits `{"hookSpecificOutput": {"hookEventName": "PreToolUse", "additionalContext": "..."}}` on stdout; nothing when no hits. Never blocks.

### 5. Recall query expansion (`kb-recall.py`)

- A second search with `"<project> <prompt>"` when `cwd` names a project (basename not `~`), results unioned with the primary search, same threshold, same dedupe.

### 6. Docs

- kb SKILL.md gains a short section on checkpointing, decision cues and tool-time recall.

## Testing

Rust: unit tests for `digest_fact_cap`, partial slicing, progress round trip, the partial-then-final flow through `run_ingest_sessions` with fakes. Hooks: `bash -n`, `python3 -m py_compile`, and a run of each with a synthetic stdin JSON checking stdout and side effects.
