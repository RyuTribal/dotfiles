---
name: kb
description: Use on EVERY substantive user prompt — first to triage the auto-injected knowledge-bank recall (use as-is, verify, or relearn), and whenever the user says "remember this", "save to knowledge bank", "what do you know about", or asks about past sessions. Wraps `mach kb` — a personal, vectorized, cross-session knowledge bank stored at ~/.local/share/mach/kb.db.
---

# kb — personal knowledge bank

`mach kb` is a small local tool that stores durable facts about the user
(preferences, projects, people, decisions) with embeddings, so they can be
found later by meaning, not just keyword. A `UserPromptSubmit` hook already
searches it automatically on every prompt and injects any close matches as
context — this skill is for triaging that recall and for *deliberate*
saves and searches you do yourself.

## Memory-first protocol (every prompt)

The recall hook fires on every prompt. Your job per prompt:

1. **No recall block injected?** For substantive questions (about the
   user, their projects, people, past work — not one-off code mechanics),
   run `mach kb search "<reformulated query>" --json` yourself once
   before falling back to exploration. The hook's threshold is
   conservative; a rephrased manual search often hits.
2. **Recall answers the question, and nothing suggests it's stale** →
   answer from it directly. Cite that it came from memory. Zero or one
   cheap verification command (an `ls`, a `--version`) is fine; a
   spelunking expedition is not.
3. **Recall is relevant but old, partial, or contradicted by something
   in view** → verify the load-bearing part cheaply, then answer.
   If reality moved on, save the corrected fact (`mach kb add` — the
   classifier will supersede the stale one).
4. **Recall is thin or off-target** → investigate normally (explore,
   spelunk, read the project). Afterwards, if you learned durable
   user-facts, save them — that's the "relearning" loop: explore once,
   remember, never re-derive a third time.

Never invoke exploration skills to re-establish what an injected recall
already states.

## Saving a fact

```
mach kb add "<fact>" --source "<context>" --project "<project>" --importance 6
```

- `--source` and `--project` are optional but cheap context for later —
  use them (e.g. `--source "conversation"`, `--project "mach"`).
- `--importance N` is 1-10 (default 5) and sets how slowly the memory
  decays — it seeds the initial forgetting-curve stability. Deliberate
  saves through this skill should pass `--importance 6`; save higher only
  for things that matter well beyond the current conversation.
- Deliberate saves through this skill are reviewed by default (no
  `--unreviewed` flag) — you decided the fact is worth keeping, so it
  doesn't need to sit in the `mach kb review` queue first. `--unreviewed`
  exists for the automated session-digest hook, not for this path.
- If the new fact closely matches (>0.75 similarity) an existing memory,
  `add` automatically asks a small classifier call whether to add it
  alongside, treat it as an update/replacement of the old one, or skip it
  as a duplicate — no need to check for similar existing memories yourself
  first.
- Convert relative dates to absolute ones before saving (e.g. "next
  Friday" → "2026-09-12", not "next Friday" — a fact read back next month
  needs to still make sense).

## Searching

```
mach kb search "<query>" --json
```

Add `--limit N` to control result count and `--all` to include unreviewed
candidates. Read `score` in the JSON output — treat anything below ~0.45
as noise. `score` is a blend of similarity, recency, and how reinforced the
memory is (the JSON also breaks these out individually as `sim`, `recency`,
`strength`). Don't pass `--touch` here — that reinforces a memory as if it
were actually recalled and injected as context, and belongs only to the
automated recall hook, not a manual search you run yourself.

## What qualifies as worth saving

Durable, cross-session facts about the user, their people, their projects,
or decisions they've made — the kind of thing that would still be true and
useful weeks or months from now:

- Stated preferences ("prefers pacman over pip", "wants terse commit
  messages")
- Ongoing projects and their goals/constraints
- People (names, roles, relationships to the user) mentioned as relevant
  to future work
- Decisions and commitments ("using WAL mode for kb going forward")

What does **not** qualify: transient session mechanics, code-level
implementation detail that belongs in the codebase itself, or anything
that's just restating what's already in a CLAUDE.md or the repo.

**Never store secrets or credentials** — no passwords, API keys, tokens,
or account numbers, ever, regardless of how the user phrases the request.
If asked to save one, decline and point to a proper secret manager instead.
