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
        return "", "", ""
    prompt = data.get("prompt", "")
    session_id = data.get("session_id", "")
    cwd = data.get("cwd", "")
    return (
        prompt if isinstance(prompt, str) else "",
        session_id if isinstance(session_id, str) else "",
        cwd if isinstance(cwd, str) else "",
    )


def project_name(cwd):
    """Basename of the session's working directory, or "" when it is the
    home directory (no project) or unknown."""
    if not cwd:
        return ""
    base = os.path.basename(os.path.normpath(cwd))
    if not base or base == os.path.basename(os.path.expanduser("~")):
        return ""
    return base


def search(query):
    resp = search_via_socket(query)
    if resp is None:
        resp = search_via_subprocess(query)
    return resp


def merge_responses(primary, secondary):
    """Union of two search responses, primary order first, deduped by memory
    id (or by content for derived rows, which carry insight ids in the same
    `id` space). Either side may be None."""
    if not primary and not secondary:
        return None
    out = {"hits": [], "connections": []}
    seen_ids, seen_text, seen_conn = set(), set(), set()
    for resp in (primary, secondary):
        if not resp:
            continue
        for h in resp.get("hits") or []:
            if not isinstance(h, dict):
                continue
            key = ("d" if h.get("derived") else "m", h.get("id"))
            text = (h.get("content") or "").strip()
            if (isinstance(h.get("id"), int) and key in seen_ids) or (text and text in seen_text):
                continue
            seen_ids.add(key)
            seen_text.add(text)
            out["hits"].append(h)
        for c in resp.get("connections") or []:
            k = connection_key(c) if isinstance(c, dict) else None
            if k is None or k in seen_conn:
                continue
            seen_conn.add(k)
            out["connections"].append(c)
    return out


def _valid_response(data):
    """A successful response is `{"hits": [...], "connections": [...]}`
    (see engines/kb/src/cli.rs's `SearchResponse`); an `{"error": ...}`
    object, a bare list (the pre-graph-layer shape), or anything else means
    fall back."""
    return data if isinstance(data, dict) and isinstance(data.get("hits"), list) else None


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
        return _valid_response(data)
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
    return _valid_response(data)


def source_phrase(source, basis=None):
    """Provenance-visible recall: map each source-class to a plain phrase
    saying HOW it was learned, so authority stays legible — something the
    user said directly ("you told me") reads differently from something
    picked up secondhand from a session or meeting transcript. Insights and
    themes never go through this; they keep their own [derived ...] markers,
    a separate axis (a belief ABOUT the user, not a provenance class).

    `basis` is the row's explicit/deductive split (mach kb `memories.basis`,
    "stated" | "inferred" | None). It refines the phrase, never replaces the
    source class: an INFERRED digest line reads "I inferred this from a
    session", a STATED one "you said this in a session". A row with no basis
    (written before the column existed, or by a channel that does not
    classify) keeps the source-only phrasing below."""
    s = (source or "").strip()
    b = (basis or "").strip().lower()
    where = None
    if s.startswith("meeting:"):
        where = "a meeting"
    elif s == "session-digest" or s.startswith("session-digest:") \
            or s == "transcript-backfill" or s.startswith("transcript-backfill:"):
        where = "a session"
    if b == "inferred":
        return "I inferred this from {}".format(where or "context")
    if b == "stated" and where:
        return "you said this in {}".format(where)
    if s.startswith("meeting:"):
        return "from a meeting"
    if s.startswith("memory-backfill"):
        return "from earlier project memory"
    if where == "a session":
        return "picked up from a session"
    # note:/manual/kb design/empty and any unrecognized tag all read as
    # deliberate, reviewed input — an unknown tag can never masquerade as
    # more authoritative than it is.
    return "you told me"


def _suppressed_field(log_path, field):
    """Shared sliding-window read for the last DEDUPE_WINDOW recall-log
    entries' `field` array (a list of ints for "ids", a list of strings for
    "conn"). Read BEFORE this prompt's entry is appended, so "last N" means
    prior prompts. Any read/parse failure just means an empty suppression
    set — same silence-on-error contract for both fields."""
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
        for v in entry.get(field) or []:
            out.add(v)
    return out


def suppressed_ids(log_path):
    """Session-deduped injection: an id already injected within the LAST
    DEDUPE_WINDOW recall-log entries is suppressed this time — re-showing
    the same top memories every prompt is pure token waste. The window
    slides; once a memory has been out of it, it may surface again."""
    return {n for n in _suppressed_field(log_path, "ids") if isinstance(n, int)}


def suppressed_conns(log_path):
    """Same sliding-window session dedupe as `suppressed_ids`, but for
    association-graph connections (see `connection_key`) rather than memory
    ids — a connection whose key appeared in the last DEDUPE_WINDOW
    recall-log entries is suppressed this time. Same silence-on-error
    contract."""
    return {k for k in _suppressed_field(log_path, "conn") if isinstance(k, str)}


def connection_key(c):
    """Renders one connection (`ConnectionHit`) as its session-dedupe key —
    "src|predicate|dst" for a 1-hop connection, extended with
    "|src_name2|predicate2|dst_name2" for a 2-hop one, so two connections
    that share a first hop but continue differently are never conflated. `None` when the
    connection is too malformed to key (mirrors `connection_line`'s own
    validity check) — such a connection is never suppressible and never
    logged, exactly like it's never rendered."""
    src = (c.get("src_name") or "").strip()
    predicate = (c.get("predicate") or "").strip()
    dst = (c.get("dst_name") or "").strip()
    if not src or not predicate or not dst:
        return None
    key = "{}|{}|{}".format(src, predicate, dst)
    if c.get("hops") == 2:
        predicate2 = (c.get("predicate2") or "").strip()
        # src_name2: the second edge's own stored source (newer mach kb);
        # absent from an older daemon's JSON, where hop 2 always started at
        # dst_name -- fall back to that so the key stays stable either way.
        src2 = (c.get("src_name2") or dst).strip()
        dst2 = (c.get("dst_name2") or "").strip()
        if not predicate2 or not dst2:
            return None
        key += "|{}|{}|{}".format(src2, predicate2, dst2)
    return key


def connection_line(c):
    """Renders one association-graph connection (engines/kb/src/cli.rs's
    `ConnectionHit`) as either a 1-hop edge — "- [connection] Moses
    —boss-of→ user (learned 2026-09-07)" — or, for a 2-hop spreading-
    activation connection (`hops == 2`), a two-edge chain continuing
    through `predicate2`/`dst_name2` — "- [connection, 2 hops] Moses
    —boss-of→ user —works-on→ Umoja". Never logged/shown as memory ids —
    connections carry no reinforcement semantics in this first version,
    unlike a memory hit's own id (see `suppressed_ids`) — but a connection's
    own rendered key IS now session-deduped the same way, via
    `connection_key`/`suppressed_conns` below."""
    src = (c.get("src_name") or "").strip()
    predicate = (c.get("predicate") or "").strip()
    dst = (c.get("dst_name") or "").strip()
    if not src or not predicate or not dst:
        return None
    if c.get("hops") == 2:
        predicate2 = (c.get("predicate2") or "").strip()
        src2 = (c.get("src_name2") or dst).strip()
        dst2 = (c.get("dst_name2") or "").strip()
        if not predicate2 or not dst2:
            return None
        # Every edge arrives in its STORED direction (mach kb never
        # re-orients an edge to read as a chain). When the second edge
        # starts where the first ends, render one chain; otherwise render
        # the two edges side by side so no arrow is ever shown flipped.
        if src2 == dst:
            return "- [connection, 2 hops] {} —{}→ {} —{}→ {}".format(src, predicate, dst, predicate2, dst2)
        return "- [connection, 2 hops] {} —{}→ {}; {} —{}→ {}".format(src, predicate, dst, src2, predicate2, dst2)
    date = (c.get("evidence_date") or "").strip()
    when = " (learned {})".format(date) if date else ""
    return "- [connection] {} —{}→ {}{}".format(src, predicate, dst, when)


def main():
    prompt, session_id, cwd = read_input()
    if not prompt or len(prompt) < MIN_PROMPT_LEN or prompt.startswith("/"):
        return

    resp = search(prompt)
    # Query expansion: the same prompt anchored to the project the session
    # runs in, so "fix the tutorial drift" inside ~/programming/umoja also
    # pulls Umoja-specific memories the bare prompt does not embed near.
    project = project_name(cwd)
    if project and project.lower() not in prompt.lower():
        resp = merge_responses(resp, search("{} {}".format(project, prompt)))
    if not resp:
        return
    hits = resp.get("hits") or []
    connections = resp.get("connections") or []
    if not hits and not connections:
        return

    log_path = os.path.join(RECALL_LOG_DIR, session_id + ".jsonl") if session_id else ""
    suppressed = suppressed_ids(log_path)
    suppressed_conn_keys = suppressed_conns(log_path)

    lines = []
    ids = []
    # per-id ranking components [sim, recency, strength, score(, via_assoc)]
    # -- logged, never shown; the offline data a future ranking-parameter
    # search (evolutionary or otherwise) will be fitted against, paired with
    # ingest-sessions' engaged/shown verdicts
    scores = {}
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
            via = h.get("via_assoc")
            how = source_phrase(h.get("source"), h.get("basis"))
            if isinstance(via, int):
                # spreading activation over Hebbian memory_assoc edges: this
                # did not match the prompt, it has been useful alongside a
                # hit that did
                how = how + "; recalled by association"
            lines.append("- [{}] {} ({})".format(date, content, how))
            if isinstance(mem_id, int):
                ids.append(mem_id)
                scores[str(mem_id)] = [
                    round(float(h.get("sim") or 0), 3),
                    round(float(h.get("recency") or 0), 3),
                    round(float(h.get("strength") or 0), 3),
                    round(score, 3),
                ] + ([via] if isinstance(via, int) else [])

    # Same sliding-window session dedupe as memory hits (`suppressed`,
    # above), now applied to connections too via their own rendered key —
    # a connection re-shown every prompt is the same token waste a repeated
    # memory hit is. Only connections actually kept are rendered/logged.
    conn_keys = []
    connection_lines = []
    for c in connections:
        if not isinstance(c, dict):
            continue
        key = connection_key(c)
        if key is None or key in suppressed_conn_keys:
            continue
        line = connection_line(c)
        if not line:
            continue
        connection_lines.append(line)
        conn_keys.append(key)

    if lines or connection_lines:
        print("You remember (your memory of this user from past sessions — "
              "use it first rather than re-exploring; each entry notes how "
              "it was learned):")
        for l in lines:
            print(l)
        # Connections render after memory lines — a plain association the
        # graph layer (`mach kb reflect`'s extraction pass) has derived
        # alongside whatever verbatim memories matched. Never logged/
        # suppressed by memory id (a connection has none), but its own
        # rendered key IS now session-deduped the same way ids are — see
        # `conn_keys` above.
        for l in connection_lines:
            print(l)

    # Only ids/connection keys actually rendered above are logged — the
    # engagement sweep (`mach kb ingest-sessions`) depends on the "ids"
    # field meaning "shown", not "considered"; "conn" exists purely for this
    # hook's own session dedupe and carries no reinforcement semantics, so
    # ingest.rs's own parsing must (and does) simply ignore it. Best-effort:
    # any failure means no log line, never a failed hook.
    if session_id and (ids or conn_keys):
        try:
            os.makedirs(RECALL_LOG_DIR, exist_ok=True)
            import datetime
            ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
            entry = {"ts": ts, "ids": ids}
            if scores:
                entry["scores"] = scores
            if conn_keys:
                entry["conn"] = conn_keys
            with open(log_path, "a") as f:
                f.write(json.dumps(entry) + "\n")
        except Exception:
            pass


if __name__ == "__main__":
    try:
        main()
    except Exception:
        pass
    sys.exit(0)
