#!/usr/bin/env python3
"""kb-pretool-recall — Claude Code PreToolUse hook (Write | Edit | Bash |
Agent | EnterPlanMode | ExitPlanMode).

Tool-time recall. Per-prompt recall (kb-recall.py) is keyed on what the
user typed; the moment that actually needs memory is when the agent is
about to create a file, plan, or delegate — that is where "audit existing
X before building" memories earn their keep. This hook builds a query from
the project name plus the tool's own input (file stem and directory for
Write, the first words of an Agent prompt, "design plan" for plan mode),
runs the same search kb-recall.py uses, and hands hits back as
additionalContext for the tool call.

Edit and Bash are covered too (added 2026-09-08): an Edit is keyed like a
Write (file stem + parent dir); a Bash command is keyed on its command
words and any path stems it names, with flags and shell noise stripped.
Bash fires constantly, so the query is deliberately narrow and the
session dedupe window in kb-recall.py keeps a repeated hit from being
re-injected on every call. Read is deliberately NOT covered: it is pure
exploration and would only add latency with nothing to warn about.

Reuses kb-recall.py's search, formatting, session dedupe window and recall
log, so engagement-gated reinforcement sees these injections exactly like
prompt-time ones.

Contract: never blocks a tool call. Every failure path is silent exit 0.
"""

import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
try:
    import importlib.util
    _spec = importlib.util.spec_from_file_location(
        "kb_recall", os.path.join(os.path.dirname(os.path.abspath(__file__)), "kb-recall.py"))
    kb_recall = importlib.util.module_from_spec(_spec)
    _spec.loader.exec_module(kb_recall)
except Exception:
    sys.exit(0)

MAX_PROMPT_WORDS = 20
STOPWORDS = {"the", "a", "an", "and", "or", "to", "of", "in", "for", "on", "with", "this", "that", "is", "it"}


def words(text, limit):
    out = []
    for w in re.findall(r"[A-Za-z][A-Za-z0-9_-]{2,}", text or ""):
        lw = w.lower()
        if lw in STOPWORDS:
            continue
        out.append(w)
        if len(out) >= limit:
            break
    return out


BASH_NOISE = {
    "sudo", "env", "exec", "then", "else", "elif", "done", "echo", "printf", "true", "false",
    "head", "tail", "sort", "uniq", "grep", "sed", "awk", "cat", "cut", "tee", "xargs", "wc",
    "null", "dev", "tmp", "home", "usr", "bin", "local", "share", "config",
}
MAX_BASH_WORDS = 12


def bash_words(cmd):
    """Query words for a Bash command: the command names and any path
    stems it mentions, minus flags, numbers, redirections, and shell
    plumbing. Bash fires on every shell call, so this stays small and
    specific: "docker compose up react-app" should recall the deploy
    guardrail memory; "ls -la" should recall nothing."""
    out = []
    for tok in re.split(r"[\s|;&<>()`$'\"=]+", cmd):
        if not tok or tok.startswith("-"):
            continue
        # a path: keep its last meaningful components
        if "/" in tok:
            for piece in tok.rstrip("/").split("/")[-2:]:
                piece = os.path.splitext(piece)[0]
                for w in re.split(r"[-_.]+", piece):
                    if len(w) > 2 and w.lower() not in BASH_NOISE and not w.isdigit():
                        out.append(w)
            continue
        for w in re.split(r"[-_.:]+", tok):
            if len(w) > 2 and w.lower() not in BASH_NOISE and not w.isdigit() and w.isascii():
                out.append(w)
    seen, uniq = set(), []
    for w in out:
        lw = w.lower()
        if lw in seen:
            continue
        seen.add(lw)
        uniq.append(w)
        if len(uniq) >= MAX_BASH_WORDS:
            break
    return uniq


def project_name(cwd):
    if not cwd:
        return ""
    base = os.path.basename(os.path.normpath(cwd))
    if not base or base == os.path.basename(os.path.expanduser("~")):
        return ""
    return base


def build_query(tool_name, tool_input, cwd):
    parts = []
    proj = project_name(cwd)
    if proj:
        parts.append(proj)
    ti = tool_input if isinstance(tool_input, dict) else {}
    if tool_name in ("Write", "Edit"):
        path = ti.get("file_path") or ""
        if not isinstance(path, str) or not path:
            return ""
        stem = os.path.splitext(os.path.basename(path))[0]
        parent = os.path.basename(os.path.dirname(path))
        parts += [w for w in re.split(r"[-_./]+", stem) if len(w) > 2]
        if parent and parent not in parts:
            parts.append(parent)
        if tool_name == "Write":
            parts.append("existing implementation duplicate")
        else:
            parts.append("rule convention when editing")
    elif tool_name == "Bash":
        cmd = ti.get("command") or ""
        if not isinstance(cmd, str) or not cmd.strip():
            return ""
        parts += bash_words(cmd)
        if len(parts) <= (1 if proj else 0):
            return ""
    elif tool_name == "Agent":
        text = " ".join(str(ti.get(k) or "") for k in ("description", "prompt"))
        parts += words(text, MAX_PROMPT_WORDS)
    elif tool_name in ("EnterPlanMode", "ExitPlanMode"):
        parts.append("architecture design plan audit existing before building")
    else:
        return ""
    return " ".join(parts).strip()


def main():
    try:
        data = json.load(sys.stdin)
    except Exception:
        return
    if os.environ.get("MACH_KB_DIGEST") == "1":
        return
    tool_name = data.get("tool_name") or ""
    session_id = data.get("session_id") or ""
    query = build_query(tool_name, data.get("tool_input"), data.get("cwd") or "")
    if len(query) < kb_recall.MIN_PROMPT_LEN:
        return

    resp = kb_recall.search_via_socket(query)
    if resp is None:
        resp = kb_recall.search_via_subprocess(query)
    if not resp:
        return
    hits = resp.get("hits") or []
    cards = resp.get("cards") or []
    if not hits and not cards:
        return

    log_path = os.path.join(kb_recall.RECALL_LOG_DIR, session_id + ".jsonl") if session_id else ""
    suppressed = kb_recall.suppressed_ids(log_path)

    lines, ids, scores = [], [], {}
    for h in hits:
        if not isinstance(h, dict):
            continue
        try:
            score = float(h.get("score", 0))
        except (TypeError, ValueError):
            continue
        if score < kb_recall.SCORE_THRESHOLD:
            continue
        content = (h.get("content") or "").strip()
        if not content:
            continue
        date = (h.get("created_at") or "")[:10]
        if h.get("derived"):
            label = "derived theme" if h.get("level") == 2 else "derived belief"
            lines.append("- [{}, {}] {}".format(label, date, content))
        else:
            mem_id = h.get("id")
            if isinstance(mem_id, int) and mem_id in suppressed:
                continue
            how = kb_recall.source_phrase(h.get("source"), h.get("basis"))
            via = h.get("via_assoc")
            if isinstance(via, int):
                edge = (h.get("via_edge") or "").strip()
                if edge.startswith("entity:"):
                    how += "; reached via " + edge.split(":", 1)[1]
                else:
                    how += "; recalled by association"
            lines.append("- [{}] {} ({})".format(date, content, how))
            if isinstance(mem_id, int):
                ids.append(mem_id)
                scores[str(mem_id)] = [
                    round(float(h.get("sim") or 0), 3),
                    round(float(h.get("recency") or 0), 3),
                    round(float(h.get("strength") or 0), 3),
                    round(score, 3),
                ] + ([via] if isinstance(via, int) else [])
    for card in cards:
        if isinstance(card, dict):
            rendered = kb_recall.card_lines(card)
            if rendered:
                lines = rendered + lines

    if not lines:
        return

    what = {
        "Write": "create this file",
        "Edit": "edit this file",
        "Bash": "run this command",
        "Agent": "delegate this",
        "EnterPlanMode": "plan this",
        "ExitPlanMode": "finalize this plan",
    }.get(tool_name, "do this")
    context = ("Before you {}, you remember (from past sessions; check whether what you are about to build "
               "already exists or was already decided):\n{}").format(what, "\n".join(lines))
    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "additionalContext": context,
        }
    }))

    if session_id and ids:
        try:
            os.makedirs(kb_recall.RECALL_LOG_DIR, exist_ok=True)
            import datetime
            ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
            with open(log_path, "a") as f:
                f.write(json.dumps({"ts": ts, "ids": ids, "scores": scores, "via": "pretool:" + tool_name}) + "\n")
        except Exception:
            pass


if __name__ == "__main__":
    try:
        main()
    except Exception:
        pass
    sys.exit(0)
