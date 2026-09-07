#!/usr/bin/env python3
"""kb-recall — Claude Code UserPromptSubmit hook (single-process).

Searches `mach kb` for memories related to the incoming prompt and prints
them to stdout; Claude Code adds a UserPromptSubmit hook's plain-text stdout
as additionalContext automatically on exit 0.

This used to be a bash script spawning four separate python3 processes per
prompt (prompt extraction, session-id extraction, socket client, formatter)
plus mktemp files between them — process startup dominated the hook's
latency once the search itself got cheap. Everything now runs in this one
interpreter; kb-recall.sh remains only as the configured entry point.

Engagement-gated reinforcement: this hook never reinforces anything itself
(impression is not engagement). It appends the ids it actually injects, per
prompt, to ~/.local/share/mach/recall-log/<session_id>.jsonl; `mach kb
ingest-sessions` later pairs that log against the session transcript and
reinforces only what the conversation engaged with (engines/kb/src/ingest.rs).

Contract: this hook must NEVER block a prompt. Every failure path degrades
to silence and exit 0.
"""

import json
import os
import socket
import subprocess
import sys

SCORE_THRESHOLD = 0.45
MIN_PROMPT_LEN = 12
SEARCH_LIMIT = 4
SOCKET_TIMEOUT = 0.5
SUBPROCESS_TIMEOUT = 2.0
DEDUPE_WINDOW = 10

RECALL_LOG_DIR = os.path.expanduser("~/.local/share/mach/recall-log")
MACH_BIN = os.environ.get("MACH_BIN", "mach")


def read_input():
    try:
        data = json.load(sys.stdin)
    except Exception:
        return "", ""
    prompt = data.get("prompt", "")
    session_id = data.get("session_id", "")
    return (
        prompt if isinstance(prompt, str) else "",
        session_id if isinstance(session_id, str) else "",
    )


def search_via_socket(query):
    """machd's kb socket subsystem (engines/kb/src/socket.rs) keeps a warm
    db connection + a warm ollama HTTP agent alive. Tried first; ANY failure
    (daemon down, socket missing, timeout, error response) returns None and
    the subprocess path takes over — graceful degradation."""
    runtime_dir = os.environ.get("XDG_RUNTIME_DIR")
    if not runtime_dir:
        return None
    sock_path = os.path.join(runtime_dir, "mach-kb.sock")
    try:
        if not os.path.exists(sock_path):
            return None
        req = json.dumps({
            "op": "search",
            "query": query,
            "limit": SEARCH_LIMIT,
            "min_score": SCORE_THRESHOLD,
        }) + "\n"
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(SOCKET_TIMEOUT)
        s.connect(sock_path)
        s.sendall(req.encode())
        buf = b""
        while b"\n" not in buf:
            chunk = s.recv(65536)
            if not chunk:
                break
            buf += chunk
        s.close()
        data = json.loads(buf.split(b"\n", 1)[0])
        # Only a JSON array is a successful hit list (matches `mach kb
        # search --json` exactly); an {"error": ...} object means fall back.
        return data if isinstance(data, list) else None
    except Exception:
        return None


def search_via_subprocess(query):
    try:
        proc = subprocess.run(
            [MACH_BIN, "kb", "search", query,
             "--limit", str(SEARCH_LIMIT), "--json",
             "--min-score", str(SCORE_THRESHOLD)],
            capture_output=True, text=True, timeout=SUBPROCESS_TIMEOUT,
        )
    except Exception:
        return None
    if proc.returncode != 0:
        return None
    # `mach kb search` degrades to a substring fallback (every hit forced to
    # score 1.0) when it can't reach ollama — too noisy for automatic
    # injection, so treat its stderr warning the same as an outage.
    err = (proc.stderr or "").lower()
    if "cannot reach ollama" in err or "falling back to substring match" in err:
        return None
    try:
        data = json.loads(proc.stdout)
    except Exception:
        return None
    return data if isinstance(data, list) else None


def source_phrase(source):
    """Provenance-visible recall: map each source-class to a plain phrase
    saying HOW it was learned, so authority stays legible — something the
    user said directly ("you told me") reads differently from something
    picked up secondhand from a session or meeting transcript. Insights and
    themes never go through this; they keep their own [derived ...] markers,
    a separate axis (a belief ABOUT the user, not a provenance class)."""
    s = (source or "").strip()
    if s.startswith("meeting:"):
        return "from a meeting"
    if s.startswith("memory-backfill"):
        return "from earlier project memory"
    if s == "session-digest" or s.startswith("session-digest:") \
            or s == "transcript-backfill" or s.startswith("transcript-backfill:"):
        return "picked up from a session"
    # note:/manual/kb design/empty and any unrecognized tag all read as
    # deliberate, reviewed input — an unknown tag can never masquerade as
    # more authoritative than it is.
    return "you told me"


def suppressed_ids(log_path):
    """Session-deduped injection: an id already injected within the LAST
    DEDUPE_WINDOW recall-log entries is suppressed this time — re-showing
    the same top memories every prompt is pure token waste. The window
    slides; once a memory has been out of it, it may surface again. Read
    BEFORE this prompt's entry is appended, so "last N" means prior prompts.
    Any read/parse failure just means an empty suppression set."""
    out = set()
    if not log_path:
        return out
    try:
        with open(log_path) as f:
            lines = [ln for ln in f if ln.strip()]
    except Exception:
        return out
    for ln in lines[-DEDUPE_WINDOW:]:
        try:
            entry = json.loads(ln)
        except Exception:
            continue
        for n in entry.get("ids") or []:
            if isinstance(n, int):
                out.add(n)
    return out


def main():
    prompt, session_id = read_input()
    if not prompt or len(prompt) < MIN_PROMPT_LEN or prompt.startswith("/"):
        return

    hits = search_via_socket(prompt)
    if hits is None:
        hits = search_via_subprocess(prompt)
    if not hits:
        return

    log_path = os.path.join(RECALL_LOG_DIR, session_id + ".jsonl") if session_id else ""
    suppressed = suppressed_ids(log_path)

    lines = []
    ids = []
    for h in hits:
        if not isinstance(h, dict):
            continue
        try:
            score = float(h.get("score", 0))
        except (TypeError, ValueError):
            continue
        if score < SCORE_THRESHOLD:
            continue
        content = (h.get("content") or "").strip()
        if not content:
            continue
        date = (h.get("created_at") or "")[:10]
        if h.get("derived"):
            # An insight (level 2: theme) from `mach kb reflect` — a belief
            # derived from evidence, not something the user said verbatim.
            # Never reinforced (no strength term) so never logged below, and
            # never subject to the dedupe window (no memory id to dedupe on).
            confidence = h.get("confidence")
            conf = " (confidence {:.2})".format(confidence) \
                if isinstance(confidence, (int, float)) else ""
            label = "derived theme" if h.get("level") == 2 else "derived belief"
            lines.append("- [{}, {}]{} {}".format(label, date, conf, content))
        else:
            mem_id = h.get("id")
            if isinstance(mem_id, int) and mem_id in suppressed:
                continue
            lines.append("- [{}] {} ({})".format(date, content, source_phrase(h.get("source"))))
            if isinstance(mem_id, int):
                ids.append(mem_id)

    if lines:
        print("You remember (your memory of this user from past sessions — "
              "use it first rather than re-exploring; each entry notes how "
              "it was learned):")
        for l in lines:
            print(l)

    # Only ids actually rendered above are logged — the engagement sweep
    # (`mach kb ingest-sessions`) depends on this log meaning "shown", not
    # "considered". Best-effort: any failure means no log line, never a
    # failed hook.
    if session_id and ids:
        try:
            os.makedirs(RECALL_LOG_DIR, exist_ok=True)
            import datetime
            ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
            with open(log_path, "a") as f:
                f.write(json.dumps({"ts": ts, "ids": ids}) + "\n")
        except Exception:
            pass


if __name__ == "__main__":
    try:
        main()
    except Exception:
        pass
    sys.exit(0)
