#!/usr/bin/env python3
"""engrain-pretool-recall — Claude Code PreToolUse hook (Write | Edit | Bash |
Agent | EnterPlanMode | ExitPlanMode).

Tool-time recall. Per-prompt recall (engrain-recall.py) is keyed on what the
user typed; the moment that actually needs memory is when the agent is
about to create a file, plan, or delegate — that is where "audit existing
X before building" memories earn their keep. This hook builds a query from
the project name plus the tool's own input (file stem and directory for
Write, the first words of an Agent prompt, "design plan" for plan mode),
runs the same search engrain-recall.py uses, and hands hits back as
additionalContext for the tool call.

Edit and Bash are covered too (added 2026-09-08): an Edit is keyed like a
Write (file stem + parent dir); a Bash command is keyed on its command
words and any path stems it names, with flags and shell noise stripped.
Bash fires constantly, so the query is deliberately narrow and the
session dedupe window in engrain-recall.py keeps a repeated hit from being
re-injected on every call. Read is deliberately NOT covered: it is pure
exploration and would only add latency with nothing to warn about.

Reuses engrain-recall.py's search, formatting, session dedupe window and recall
log, so engagement-gated reinforcement sees these injections exactly like
prompt-time ones. Memory hits keep that sliding window (`suppressed_ids`,
`DEDUPE_WINDOW`); an entity card or the project card is different — the
longest single thing recall can inject, so once one has appeared anywhere
in the session's recall log (a prompt line or an earlier pretool line) it
is never injected again by this hook (`session_seen_cards`, unbounded,
unlike the memory-hit window).

Write/Edit/Bash queries are further enriched (added 2026-09-28) with
`intent_text`: the agent's own last words of prose from the transcript
tail, read from at most the last TAIL_BYTES and capped at
INTENT_MAX_CHARS. It answers "why am I about to do this", which a bare
file stem or command word list cannot. Reading is bounded and fails
soft — a missing/huge/torn transcript, or a turn with no text before it
(back-to-back tool calls), just yields no intent and the tool-derived
query is used alone, exactly as before. A subagent's own transcript
lines (`"isSidechain": true`) are skipped outright: only the main
thread's own words count as intent.

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
        "engrain_recall", os.path.join(os.path.dirname(os.path.abspath(__file__)), "engrain-recall.py"))
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


# Bash recall fires only where memory can change an outcome: commands that
# change state, or the first time this session touches a project. Measured
# 2026-09-28: Bash produced ~77% of all injections at 34% use; guardrails on
# Bash were used 26% of the time. Value None = any subcommand counts; a
# frozenset restricts to specific state-changing subcommands only. psql is
# deliberately absent (reads and writes look identical from the command
# line, and it is used for read-only queries far more often than DDL/DML in
# practice, so it stays off the allowlist entirely rather than guessing).
STATE_CHANGING = {
    "git": frozenset({"push", "commit", "reset", "rebase", "merge", "checkout",
                       "clean", "stash", "tag", "filter-repo"}),
    "docker": None,
    "systemctl": None,
    "rm": None,
    "mv": None,
    "sudo": None,
    "cargo": frozenset({"install", "publish"}),
    "pacman": None,
    "paru": None,
    "rsync": None,
    "scp": None,
    "chezmoi": frozenset({"apply", "add", "re-add", "forget", "update"}),
    "kubectl": frozenset({"apply", "delete"}),
    "terraform": frozenset({"apply", "destroy"}),
    "alembic": frozenset({"upgrade", "downgrade"}),
}
SCRIPT_CUES = ("deploy", "migrat", "release", "install.sh")
_SCRIPT_INTERPRETERS = {"python", "python3", "python2", "sh", "bash", "zsh", "ruby", "perl", "node", "nodejs"}


def _segments(cmd):
    return [s.strip() for s in re.split(r"&&|\|\||;|\|", cmd) if s.strip()]


def _script_cue_tokens(toks):
    """The token(s) SCRIPT_CUES may match against: the segment's own first
    token (a script invoked directly, e.g. `./deploy.sh`), or -- when that
    first token is a bare interpreter -- the token right after it too (the
    script it runs, e.g. `python scripts/migrate.py`). Never any later
    argument: matching a cue anywhere in the whole segment would also fire
    on `grep -rn deploy src` or `rg migrate`, where the word is data being
    searched FOR, not a command being run."""
    if not toks:
        return []
    out = [toks[0]]
    if os.path.basename(toks[0]) in _SCRIPT_INTERPRETERS and len(toks) > 1:
        out.append(toks[1])
    return out


def _matches_script_cue(toks):
    for tok in _script_cue_tokens(toks):
        base = os.path.basename(tok)
        if any(cue in base for cue in SCRIPT_CUES):
            return True
    return False


def _is_manage_py_migrate(toks):
    """Django-style migrations, however `manage.py` is invoked -- bare
    (`./manage.py migrate`), through an interpreter (`python`/`python3
    manage.py migrate`), or through a runner (`uv run manage.py migrate`).
    The spec's allowlist names "migrations/deploy scripts" explicitly, and
    this pattern is common enough, and narrow enough (an exact `migrate`
    token strictly AFTER a `manage.py` token) to warrant its own rule
    rather than stretching SCRIPT_CUES's tighter script-token-only match to
    cover it -- `grep -rn migrate manage.py` still does not match, since
    "migrate" there comes before "manage.py", not after."""
    for i, tok in enumerate(toks):
        if os.path.basename(tok) == "manage.py" and "migrate" in toks[i + 1:]:
            return True
    return False


# Commands that merely wrap another command without themselves changing
# anything: peeled off before reading the "real" head, so `timeout 30 git
# push origin main` is read as `git push ...`, not as an unrecognized
# `timeout`. `sudo` is deliberately NOT here -- it already counts as
# state-changing on its own (STATE_CHANGING["sudo"] is None) regardless of
# what it wraps, so there is nothing to peel through.
_WRAPPER_HEADS = {"timeout", "nohup", "nice", "time", "env", "stdbuf", "ionice"}
# Wrapper flags that take their value as a SEPARATE following token (e.g.
# `nice -n 10 cmd`, not `nice -n10 cmd`) -- consumed as a pair so the value
# itself is never mistaken for the wrapped command's own head.
_WRAPPER_VALUE_FLAGS = {
    "timeout": {"-s", "--signal", "-k", "--kill-after"},
    "nice": {"-n", "--adjustment"},
    "ionice": {"-c", "--class", "-n", "--classdata", "-p", "--pid"},
}


def _strip_one_wrapper(toks):
    """Peel exactly one leading wrapper command (plus its own flags/args)
    off `toks`, or return it unchanged if the head is not a recognized
    wrapper. Leading `VAR=val` assignments are dropped first (a wrapper may
    follow one, e.g. `FOO=1 timeout 30 cmd`)."""
    while toks and re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", toks[0]):
        toks = toks[1:]
    if not toks:
        return toks
    head = os.path.basename(toks[0])
    if head not in _WRAPPER_HEADS:
        return toks
    toks = toks[1:]
    value_flags = _WRAPPER_VALUE_FLAGS.get(head, set())
    while toks and toks[0].startswith("-"):
        flag = toks[0]
        toks = toks[1:]
        if flag in value_flags and toks:
            toks = toks[1:]
    if head == "env":
        # `env [-i] [-u NAME] NAME=VALUE... COMMAND` -- assignments (not
        # flags) may keep appearing before the real command.
        while toks and not toks[0].startswith("-") and "=" in toks[0]:
            toks = toks[1:]
    elif head == "timeout" and toks:
        toks = toks[1:]  # the mandatory DURATION positional argument
    return toks


def _strip_wrappers(toks, depth=3):
    for _ in range(depth):
        stripped = _strip_one_wrapper(toks)
        if stripped == toks:
            break
        toks = stripped
    return toks


def _extract_dollar_paren(cmd):
    """Every `$(...)` command-substitution body in `cmd` (one level of
    nested parens tracked, which is enough for shell substitutions -- a
    command deep enough to need more is vanishingly rare, and recursion
    into it is depth-limited anyway)."""
    out = []
    i, n = 0, len(cmd)
    while i < n:
        j = cmd.find("$(", i)
        if j == -1:
            break
        depth, k = 1, j + 2
        while k < n and depth > 0:
            if cmd[k] == "(":
                depth += 1
            elif cmd[k] == ")":
                depth -= 1
            k += 1
        out.append(cmd[j + 2:k - 1] if depth == 0 else cmd[j + 2:k])
        i = k
    return out


def _extract_backticks(cmd):
    return re.findall(r"`([^`]*)`", cmd)


# `bash -c "..."` / `sh -lc '...'` / `zsh -c "..."` -- the quoted script
# body some other command hands to a shell to run.
_DASH_C_RE = re.compile(r"\b(?:bash|sh|zsh)\b[^\n]*?\s(?:-c|-lc)\s+(['\"])((?:\\.|(?!\1).)*)\1")
# A heredoc whose INTERPRETER (the word on the same line as `<<`) is
# bash/sh/zsh -- e.g. `bash <<EOF` ... `EOF`. Deliberately narrow: a
# heredoc fed to `cat`, `tee`, `python`, etc. is DATA, not a command to
# recurse into (`cat > notes.txt <<EOF` containing the words "git push"
# must never be read as running them).
_HEREDOC_RE = re.compile(
    r"(?m)^[^\n]*\b(?:bash|sh|zsh)\b[^\n]*<<-?\s*(['\"]?)(\w+)\1[^\n]*\n"
    r"(.*?)\n[ \t]*\2[ \t]*$",
    re.DOTALL,
)


def _extract_dash_c_bodies(cmd):
    return [m.group(2) for m in _DASH_C_RE.finditer(cmd)]


def _extract_heredoc_bodies(cmd):
    return [m.group(3) for m in _HEREDOC_RE.finditer(cmd)]


def _nested_commands(cmd):
    """Every command string embedded in `cmd` that a plain token scan would
    never see: `$(...)` and backtick substitutions, the quoted body of a
    `bash|sh|zsh -c`/`-lc` call, and a heredoc body fed to one of those
    shells specifically (never to `cat`/`tee`/etc -- see `_HEREDOC_RE`)."""
    return (_extract_dollar_paren(cmd) + _extract_backticks(cmd)
            + _extract_dash_c_bodies(cmd) + _extract_heredoc_bodies(cmd))


def bash_is_state_changing(cmd, _depth=3):
    """True if any `&&`/`||`/`;`/`|`-separated segment of `cmd` runs a
    command from STATE_CHANGING (with the right subcommand, when the entry
    restricts to one), a script whose own name carries a SCRIPT_CUES word,
    or a `manage.py migrate` invocation -- reading past any wrapper command
    (`timeout`, `nohup`, `nice`, `time`, `env`, `stdbuf`, `ionice`) to find
    the real head. `cd` heads are skipped outright -- they never change
    state on their own and only set up the segment that follows. Also
    recurses (depth-limited) into `$(...)`/backtick substitutions, a
    `bash -c "..."` body, and a bash/sh/zsh heredoc body, since a
    state-changing command hidden inside one of those is still a
    state-changing command."""
    cmd = cmd or ""
    for seg in _segments(cmd):
        toks = [t for t in seg.split() if not re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", t)]
        toks = _strip_wrappers(toks)
        if not toks:
            continue
        head = os.path.basename(toks[0])
        if head == "cd":
            continue
        if _matches_script_cue(toks) or _is_manage_py_migrate(toks):
            return True
        if head not in STATE_CHANGING:
            continue
        subs = STATE_CHANGING[head]
        if subs is None:
            return True
        rest = [t for t in toks[1:] if not t.startswith("-")]
        if rest and rest[0] in subs:
            return True
    if _depth > 0:
        for nested in _nested_commands(cmd):
            if bash_is_state_changing(nested, _depth - 1):
                return True
    return False


def _root_upward(path, max_hops=12):
    """Nearest ancestor of `path` holding a `.git` dir, or None."""
    if not path:
        return None
    p = os.path.normpath(os.path.expanduser(path))
    for _ in range(max_hops):
        if os.path.isdir(os.path.join(p, ".git")):
            return p
        parent = os.path.dirname(p)
        if parent == p:
            return None
        p = parent
    return None


def project_roots_in(cmd, cwd):
    """Every project root a Bash call touches: the hook's own cwd project
    root always counts first (a bare `cargo test` in a fresh project must
    still fire once, even though it names no path token), plus the nearest
    root above every path-looking token the command mentions."""
    roots = []
    cwd_root = _root_upward(cwd)
    if cwd_root:
        roots.append(cwd_root)
    for tok in re.split(r"[\s|;&<>()`'\"=]+", cmd or ""):
        if "/" not in tok and not tok.startswith("~"):
            continue
        p = os.path.expanduser(tok)
        if not os.path.isabs(p):
            p = os.path.join(cwd or "/", p)
        r = _root_upward(p)
        if r and r not in roots:
            roots.append(r)
    return roots


def session_seen_roots(log_path):
    """Every project root logged (via the `roots` key) anywhere in this
    session's recall log so far -- unbounded, like `session_seen_cards`,
    because "first touch" means once per session, not once per sliding
    window."""
    seen = set()
    if not log_path:
        return seen
    try:
        with open(log_path) as f:
            for ln in f:
                try:
                    seen.update(json.loads(ln).get("roots") or [])
                except Exception:
                    continue
    except Exception:
        pass
    return seen


def bash_should_recall(cmd, cwd, log_path):
    """(recall?, newly touched roots to log). Without a session log path
    there is nowhere to remember a root was ever touched before, so every
    call would look like a first touch forever -- first-touch is disabled
    outright in that case (only the state-changing gate applies) rather
    than firing recall on every single Bash call in a session-less
    context."""
    if not log_path:
        return bash_is_state_changing(cmd), []
    seen = session_seen_roots(log_path)
    new_roots = [r for r in project_roots_in(cmd, cwd) if r not in seen]
    return (bash_is_state_changing(cmd) or bool(new_roots)), new_roots


def project_name(cwd):
    if not cwd:
        return ""
    base = os.path.basename(os.path.normpath(cwd))
    if not base or base == os.path.basename(os.path.expanduser("~")):
        return ""
    return base


INTENT_MAX_CHARS = 400
TAIL_BYTES = 65536


def _is_user_prompt(entry):
    """A real user turn (string content or a text block), not a tool_result."""
    if entry.get("type") != "user":
        return False
    c = (entry.get("message") or {}).get("content")
    if isinstance(c, str):
        return bool(c.strip())
    return isinstance(c, list) and any(isinstance(b, dict) and b.get("type") == "text" for b in c)


def intent_text(transcript_path):
    """The agent's own words about why it is doing what it is about to do:
    the last assistant text block after the latest real user prompt, from
    the transcript's last TAIL_BYTES. '' on anything unexpected -- the
    caller then keeps the tool-derived query.

    A `"isSidechain": true` entry (a subagent's own transcript lines,
    interleaved into the main transcript) is skipped entirely -- it is
    never a source of intent text, and it never acts as a turn boundary
    either, so a sidechain line sitting between the main thread's last
    prompt and its last text does not hide that text."""
    try:
        with open(transcript_path, "rb") as f:
            f.seek(0, os.SEEK_END)
            size = f.tell()
            f.seek(max(0, size - TAIL_BYTES))
            tail = f.read().decode("utf-8", errors="replace")
    except Exception:
        return ""
    for line in reversed(tail.splitlines()):
        try:
            entry = json.loads(line)
        except Exception:
            continue
        if not isinstance(entry, dict):
            continue
        if entry.get("isSidechain"):
            continue
        if _is_user_prompt(entry):
            return ""
        if entry.get("type") != "assistant":
            continue
        content = (entry.get("message") or {}).get("content")
        if not isinstance(content, list):
            continue
        texts = [b.get("text", "") for b in content if isinstance(b, dict) and b.get("type") == "text"]
        text = " ".join(t.strip() for t in texts if t and t.strip())
        if text:
            return text[:INTENT_MAX_CHARS]
    return ""


def build_query(tool_name, tool_input, cwd, intent=""):
    parts = []
    proj = project_name(cwd)
    if proj:
        parts.append(proj)
    if intent:
        parts.append(intent)
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
        # Whether recall fires for Bash at all is decided by the outer
        # gate (bash_should_recall's allowlist / first-touch check in
        # main()), never here -- this is only about whether build_query has
        # anything meaningful to search on. That emptiness check counts
        # COMMAND words only, computed before intent is considered: a
        # command that yields no words of its own (e.g. "sudo" alone, all
        # noise) still produces a query once an intent is present, because
        # the intent text is itself a meaningful query on its own.
        bwords = bash_words(cmd)
        if not bwords and not intent:
            return ""
        parts += bwords
    elif tool_name == "Agent":
        text = " ".join(str(ti.get(k) or "") for k in ("description", "prompt"))
        parts += words(text, MAX_PROMPT_WORDS)
    elif tool_name in ("EnterPlanMode", "ExitPlanMode"):
        parts.append("architecture design plan audit existing before building")
    else:
        return ""
    return " ".join(parts).strip()


def _log_bash_roots(new_roots, session_id, log_path):
    """Log a first-touch root on its own, ahead of an early return that
    would otherwise happen before the normal end-of-hook log write below
    (no search response, nothing above threshold, nothing left to render).
    Without this, a root touched by a command whose search came up empty
    would never be recorded as seen, and would count as a first touch
    again on the very next command against the same project."""
    if not (session_id and new_roots):
        return
    try:
        os.makedirs(kb_recall.RECALL_LOG_DIR, exist_ok=True)
        import datetime
        ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        with open(log_path, "a") as f:
            f.write(json.dumps({"ts": ts, "roots": new_roots}) + "\n")
    except Exception:
        pass


def main():
    try:
        data = json.load(sys.stdin)
    except Exception:
        return
    if os.environ.get("ENGRAIN_DIGEST") == "1":
        return
    tool_name = data.get("tool_name") or ""
    session_id = data.get("session_id") or ""
    cwd = data.get("cwd") or ""
    log_path = os.path.join(kb_recall.RECALL_LOG_DIR, session_id + ".jsonl") if session_id else ""

    # Bash recall fires only where memory can change an outcome: see
    # bash_should_recall (STATE_CHANGING gate, first-touch project roots).
    # A command that is neither gets no search at all -- `new_roots` is
    # necessarily empty in that case too, so no first-touch log line is
    # ever lost by returning here.
    new_roots = []
    if tool_name == "Bash":
        ti = data.get("tool_input") if isinstance(data.get("tool_input"), dict) else {}
        cmd = ti.get("command")
        if not isinstance(cmd, str):
            cmd = ""
        ok, new_roots = bash_should_recall(cmd, cwd, log_path)
        if not ok:
            return

    # Intent enrichment (R2): the agent's own last words about why it is
    # about to do this, pulled from the transcript tail. Write/Edit/Bash
    # only -- Agent and plan-mode calls already carry intent prose in their
    # own tool_input (a prompt, a plan body), so re-reading the transcript
    # for them would be redundant.
    intent = intent_text(data.get("transcript_path") or "") if tool_name in ("Write", "Edit", "Bash") else ""
    query = build_query(tool_name, data.get("tool_input"), cwd, intent)
    if len(query) < kb_recall.MIN_PROMPT_LEN:
        _log_bash_roots(new_roots, session_id, log_path)
        return

    # project_name() (the basename) stays the fallback label folded into
    # the query text by build_query() above; `cwd` itself now also rides
    # along on the search call so the Rust side can resolve the actual
    # session project against the `projects` registry (see engrain-recall.py's
    # own main() for the same split). `via` is "pretool:<ToolName>" (not
    # "prompt", search()'s default) so the Rust recall policy's habituation
    # and gating see tool-time recall as its own context, distinct from
    # per-prompt recall.
    project = project_name(cwd)
    suppressed = kb_recall.suppressed_ids(log_path)
    resp = kb_recall.search(query, project, cwd, "pretool:" + tool_name, suppressed, True)
    if not resp:
        _log_bash_roots(new_roots, session_id, log_path)
        return
    hits = resp.get("hits") or []
    cards = resp.get("cards") or []
    project_card = kb_recall.project_card_text(resp)
    if not hits and not cards and not project_card:
        _log_bash_roots(new_roots, session_id, log_path)
        return

    # Unbounded "ever shown this session" check for cards (entity cards and
    # the project card) — unlike suppressed (memory hits), which keeps its
    # sliding DEDUPE_WINDOW. A card already logged by ANY earlier line of
    # this session's recall log, prompt or pretool, is never re-injected
    # here.
    seen_cards = kb_recall.session_seen_cards(log_path)

    # `feats`/`explore_ids` mirror engrain-recall.py's own main(): the
    # per-id ranking features this injection was ranked on, logged for
    # `engrain ingest-sessions` to fold into the gate's training set.
    lines, ids, scores, feats, explore_ids = [], [], {}, {}, []
    gated = bool(resp.get("gate"))
    for h in kb_recall.select_hits(hits, suppressed, gated=gated):
        if not isinstance(h, dict):
            continue
        try:
            score = float(h.get("score", 0))
        except (TypeError, ValueError):
            continue
        content = (h.get("content") or "").strip()
        if not content:
            continue
        date = (h.get("created_at") or "")[:10]
        if h.get("derived"):
            label = "self-knowledge" if h.get("source") == "derived-self" \
                else "my opinion" if h.get("source") == "derived-opinion" \
                else "derived theme" if h.get("level") == 2 else "derived belief"
            lines.append("- [{}, {}] {}".format(label, date, content))
        else:
            mem_id = h.get("id")
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
                feats[str(mem_id)] = kb_recall.feature_entry(h)
                if h.get("explore"):
                    explore_ids.append(mem_id)
    card_keys = []
    for card in cards:
        if not isinstance(card, dict):
            continue
        name = (card.get("entity") or "").strip()
        if not name or name in seen_cards:
            continue
        rendered = kb_recall.card_lines(card)
        if not rendered:
            continue
        lines = rendered + lines
        card_keys.append(name)

    if project_card and kb_recall.PROJECT_CARD_KEY not in seen_cards:
        rendered = ["- Project:"] + ["  " + l.strip() for l in project_card.splitlines() if l.strip()]
        lines = rendered + lines
        card_keys.append(kb_recall.PROJECT_CARD_KEY)

    if not lines:
        _log_bash_roots(new_roots, session_id, log_path)
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

    # Logged whenever there are memory ids, card_keys, OR new_roots: a
    # card-only injection (no scored memory hit — e.g. a fresh project card
    # with nothing else above threshold) must still be recorded, or
    # session_seen_cards would never see it and this hook would keep
    # re-injecting the same card forever; a first-touch root must be
    # recorded here too so it isn't logged twice by both this line and
    # _log_bash_roots (only one of the two ever runs per call).
    if session_id and (ids or card_keys or new_roots):
        try:
            os.makedirs(kb_recall.RECALL_LOG_DIR, exist_ok=True)
            import datetime
            ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
            entry = {"ts": ts, "via": "pretool:" + tool_name}
            if cwd:
                entry["cwd"] = cwd
            if ids:
                entry["ids"] = ids
                entry["scores"] = scores
            if card_keys:
                entry["cards"] = card_keys
            if feats:
                entry["feat"] = feats
            if explore_ids:
                entry["explore"] = explore_ids
            if resp.get("gate"):
                entry["gate"] = resp["gate"]
            if new_roots:
                entry["roots"] = new_roots
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
