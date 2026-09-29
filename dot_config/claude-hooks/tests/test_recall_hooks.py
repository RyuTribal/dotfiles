"""Unit tests for the engrain recall hooks, run against the chezmoi SOURCE
files. The sources carry chezmoi's `executable_` prefix and the pretool hook
imports `engrain-recall.py` by its deployed name, so setUpModule copies both
into a temp dir under their deployed names and imports from there.

Run: python3 -m unittest discover -s ~/.local/share/chezmoi/dot_config/claude-hooks/tests -v
"""
import sys

# Never leave a compiled .pyc behind under the chezmoi SOURCE tree (this
# repo has no .gitignore, so a stray __pycache__/ would get committed by a
# careless `git add -A`) -- set before importing anything else, including
# the hook modules this test loads by hand below.
sys.dont_write_bytecode = True

import importlib.util
import io
import json
import os
import shutil
import tempfile
import unittest

SRC = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TMP = None
recall = None
pretool = None


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _scrub_pycache():
    """Belt-and-suspenders for the chezmoi source tree having no
    .gitignore: `sys.dont_write_bytecode` above stops every OTHER module
    this file touches from caching, but `python -m unittest discover`
    decides whether to cache the entry-point test module's OWN bytecode
    before this module's top-level code (including that assignment) ever
    runs -- so a plain `python3 -m unittest discover` (no `-B`) can still
    leave `tests/__pycache__/` behind no matter what this file sets.
    Removing any `__pycache__` under this dir and its parent, both before
    and after the run, is what actually guarantees the tree is clean."""
    for d in (SRC, os.path.dirname(os.path.abspath(__file__))):
        shutil.rmtree(os.path.join(d, "__pycache__"), ignore_errors=True)


def setUpModule():
    global TMP, recall, pretool
    _scrub_pycache()
    TMP = tempfile.mkdtemp(prefix="engrain-hooks-test-")
    for src, dst in (("executable_engrain-recall.py", "engrain-recall.py"),
                     ("executable_engrain-pretool-recall.py", "engrain-pretool-recall.py")):
        shutil.copy(os.path.join(SRC, src), os.path.join(TMP, dst))
    recall = _load("engrain_recall_t", os.path.join(TMP, "engrain-recall.py"))
    pretool = _load("engrain_pretool_t", os.path.join(TMP, "engrain-pretool-recall.py"))


def tearDownModule():
    shutil.rmtree(TMP, ignore_errors=True)
    _scrub_pycache()


def hit(i, score, explore=False, derived=False, rank=0):
    return {"id": i, "content": "m%d" % i, "score": score, "sim": score, "recency": 0.9,
            "strength": 0.5, "lexical": 0.1, "rank": rank, "explore": explore,
            "derived": derived, "created_at": "2026-09-01T00:00:00Z"}


class PromptHookTests(unittest.TestCase):
    def test_explore_hit_survives_threshold_and_cap(self):
        hits = [hit(1, 0.9), hit(2, 0.8), hit(3, 0.7), hit(4, 0.6), hit(9, 0.1, explore=True, rank=7)]
        kept = recall.select_hits(hits, suppressed=set())
        self.assertEqual([h["id"] for h in kept], [1, 2, 3, 9])

    def test_select_hits_skips_suppressed_before_capping(self):
        hits = [hit(1, 0.9), hit(2, 0.8), hit(3, 0.7), hit(4, 0.6)]
        kept = recall.select_hits(hits, suppressed={1})
        self.assertEqual([h["id"] for h in kept], [2, 3, 4])

    def test_select_hits_caps_direct_hits_but_passes_derived_through(self):
        # A derived insight never competes with direct memory hits for the
        # 3 slots -- even one that outscores every direct hit still renders
        # last, after all 3 direct hits, not instead of one of them.
        hits = [hit(0, 0.95, derived=True), hit(1, 0.9), hit(2, 0.8), hit(3, 0.7)]
        kept = recall.select_hits(hits, suppressed=set())
        self.assertEqual([h["id"] for h in kept], [1, 2, 3, 0])

    def test_select_hits_keeps_only_the_best_scoring_hop_whose_seed_is_shown(self):
        hits = [hit(1, 0.9), hit(2, 0.8),
                dict(hit(11, 0.3), via_assoc=1),
                dict(hit(12, 0.5), via_assoc=2)]
        kept = recall.select_hits(hits, suppressed=set())
        self.assertEqual([h["id"] for h in kept], [1, 2, 12])

    def test_select_hits_drops_a_hop_whose_seed_was_cut_by_the_cap(self):
        hits = [hit(1, 0.9), hit(2, 0.8), hit(3, 0.7), hit(4, 0.6),
                dict(hit(14, 0.5), via_assoc=4)]
        kept = recall.select_hits(hits, suppressed=set())
        self.assertEqual([h["id"] for h in kept], [1, 2, 3])

    # --- select_hits: gated responses (a learned gate decided eligibility) ---

    def test_select_hits_gated_keeps_a_below_threshold_hit_ranked_by_gate_p(self):
        # A gate-decided response can carry a direct hit whose raw score
        # sits below SCORE_THRESHOLD -- habituation can push an eligible
        # hit's raw score down on purpose -- so `gated=True` must not
        # re-drop it here, and must rank direct hits by `gate_p`, not raw
        # `score`, since the gate's own probability is the ranking that
        # matters once it already decided eligibility.
        hits = [dict(hit(1, 0.9), gate_p=0.5), dict(hit(2, 0.2), gate_p=0.95)]
        kept = recall.select_hits(hits, suppressed=set(), gated=True)
        self.assertEqual([h["id"] for h in kept], [2, 1], "gate_p, not raw score, orders direct hits when gated")

    def test_select_hits_ungated_still_drops_below_threshold(self):
        # Sanity: with no gate id at all (the default), behavior is exactly
        # as before -- a low-score direct hit is dropped, gate_p or not.
        hits = [dict(hit(1, 0.9), gate_p=0.5), dict(hit(2, 0.2), gate_p=0.95)]
        kept = recall.select_hits(hits, suppressed=set())
        self.assertEqual([h["id"] for h in kept], [1])

    def test_select_hits_gated_still_thresholds_derived_insights(self):
        # The gate never scores derived insights (the Rust side always
        # scores them by plain score), so a low-score insight must still be
        # dropped even in a gated response.
        hits = [hit(1, 0.9), hit(0, 0.1, derived=True)]
        kept = recall.select_hits(hits, suppressed=set(), gated=True)
        self.assertEqual([h["id"] for h in kept], [1])

    def test_feature_entry_shape(self):
        f = recall.feature_entry(dict(hit(5, 0.6, rank=2), via_assoc=3))
        self.assertEqual(f, {"sim": 0.6, "rec": 0.9, "str": 0.5, "score": 0.6, "lex": 0.1, "rank": 2, "assoc": 1})

    def test_subprocess_retry_drops_new_flags(self):
        calls = []

        class P:
            def __init__(self, rc, out=""):
                self.returncode, self.stdout, self.stderr = rc, out, ""

        def fake_run(argv, **kw):
            calls.append(argv)
            if "--via" in argv or "--cwd" in argv:
                return P(1)
            return P(0, json.dumps({"hits": [], "connections": []}))

        orig = recall.subprocess.run
        recall.subprocess.run = fake_run
        try:
            resp = recall.search_via_subprocess("hello world", "proj", "/tmp/proj", via="prompt", exclude=[1])
        finally:
            recall.subprocess.run = orig
        self.assertIsNotNone(resp, "an old binary rejecting --via must still yield recall")
        self.assertNotIn("--via", calls[-1])
        self.assertNotIn("--exclude", calls[-1])


class IntentTests(unittest.TestCase):
    """intent_text: the last assistant text block after the latest real user
    prompt, from the transcript's last TAIL_BYTES; build_query folding that
    text into the query (the Bash emptiness check counts command words
    only, computed before intent is considered)."""

    def _write(self, entries):
        fd, path = tempfile.mkstemp(suffix=".jsonl")
        with os.fdopen(fd, "w") as f:
            for e in entries:
                f.write(json.dumps(e) + "\n")
        return path

    def test_last_assistant_text_after_latest_user_prompt(self):
        p = self._write([
            {"type": "user", "message": {"content": "old prompt"}},
            {"type": "assistant", "message": {"content": [{"type": "text", "text": "old intent"}]}},
            {"type": "user", "message": {"content": "new prompt"}},
            {"type": "assistant", "message": {"content": [{"type": "text", "text": "Checking what's live before building."}]}},
            {"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "Bash"}]}},
            {"type": "user", "message": {"content": [{"type": "tool_result", "content": "x"}]}},
        ])
        self.assertEqual(pretool.intent_text(p), "Checking what's live before building.")

    def test_no_text_since_latest_prompt_falls_back_to_empty(self):
        p = self._write([
            {"type": "assistant", "message": {"content": [{"type": "text", "text": "stale"}]}},
            {"type": "user", "message": {"content": "new prompt"}},
            {"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "Bash"}]}},
        ])
        self.assertEqual(pretool.intent_text(p), "")

    def test_unreadable_huge_or_torn_transcript_is_empty_not_error(self):
        self.assertEqual(pretool.intent_text("/nonexistent/x.jsonl"), "")
        fd, path = tempfile.mkstemp()
        with os.fdopen(fd, "w") as f:
            f.write("x" * 200000 + '\n{"type":"assistant","message":{"content":[{"type":"text","text":"' + "y" * 1000 + '"}]}}\n{"torn')
        out = pretool.intent_text(path)
        self.assertLessEqual(len(out), pretool.INTENT_MAX_CHARS)

    def test_no_thinking_or_tool_use_blocks_counted_as_intent(self):
        # Thinking blocks and tool_use blocks are never intent text, even
        # when they are the only content on the last assistant turn.
        p = self._write([
            {"type": "user", "message": {"content": "prompt"}},
            {"type": "assistant", "message": {"content": [
                {"type": "thinking", "thinking": "planning silently"},
                {"type": "tool_use", "name": "Read"},
            ]}},
        ])
        self.assertEqual(pretool.intent_text(p), "")

    def test_build_query_prefers_intent(self):
        q = pretool.build_query("Edit", {"file_path": "/p/umoja/src/auth.rs"}, "/p/umoja", intent="fix token refresh race")
        self.assertTrue(q.startswith("umoja fix token refresh race"))
        self.assertIn("auth", q)

    def test_bash_with_no_command_words_but_intent_is_not_empty(self):
        # Once the outer gate has let a Bash call through, intent text
        # alone is a meaningful query even if the command itself yields no
        # words (e.g. everything in it is filtered as noise).
        q = pretool.build_query("Bash", {"command": "sudo"}, "/p/umoja", intent="checking live config first")
        self.assertIn("checking live config first", q)

    def test_bash_with_no_command_words_and_no_intent_is_empty(self):
        self.assertEqual(pretool.build_query("Bash", {"command": "sudo"}, "", intent=""), "")

    def test_sidechain_entries_are_skipped_entirely(self):
        # A subagent's own transcript lines
        # (isSidechain: true) are interleaved into the main transcript but
        # must never supply intent, and must never act as a turn boundary
        # either -- the main thread's own last text still wins even though
        # a sidechain line sits after it.
        p = self._write([
            {"type": "user", "message": {"content": "new prompt"}},
            {"type": "assistant", "message": {"content": [
                {"type": "text", "text": "Main thread intent before delegating."}]}},
            {"type": "assistant", "isSidechain": True,
             "message": {"content": [{"type": "text", "text": "Subagent internal chatter"}]}},
        ])
        self.assertEqual(pretool.intent_text(p), "Main thread intent before delegating.")

    def test_sidechain_only_after_prompt_falls_back_to_empty(self):
        # No main-thread text exists after the latest prompt at all -- only
        # sidechain chatter -- so intent must be empty, not the subagent's
        # words.
        p = self._write([
            {"type": "user", "message": {"content": "new prompt"}},
            {"type": "assistant", "isSidechain": True,
             "message": {"content": [{"type": "text", "text": "Subagent chatter only"}]}},
        ])
        self.assertEqual(pretool.intent_text(p), "")

    def test_real_transcript_shape_thinking_text_tool_use_on_separate_lines(self):
        # A real transcript emits one JSONL line
        # PER content block, not one line per turn -- thinking, text and
        # tool_use each land on their own line, and a tool_result comes
        # back as its own type:"user" entry (not a fresh prompt).
        p = self._write([
            {"type": "user", "message": {"content": "new prompt"}},
            {"type": "assistant", "message": {"content": [
                {"type": "thinking", "thinking": "let me plan this out"}]}},
            {"type": "assistant", "message": {"content": [
                {"type": "text", "text": "Checking prod config before deploying."}]}},
            {"type": "assistant", "message": {"content": [
                {"type": "tool_use", "name": "Bash", "input": {"command": "ls"}}]}},
            {"type": "user", "message": {"content": [
                {"type": "tool_result", "content": "file1\nfile2"}]}},
        ])
        self.assertEqual(pretool.intent_text(p), "Checking prod config before deploying.")

    def test_utf8_multibyte_character_straddles_the_tail_cutoff(self):
        # TAIL_BYTES is a byte-count seek, so it can
        # land in the middle of a multibyte UTF-8 sequence. That must never
        # raise, and the (corrupted, discarded) partial line it falls in
        # must not stop later, clean lines in the tail from being read
        # correctly.
        tail_bytes = pretool.TAIL_BYTES
        euro = "€".encode("utf-8")  # 3 bytes: e2 82 ac
        self.assertEqual(len(euro), 3)
        intent_line = (json.dumps({"type": "assistant", "message": {"content": [
            {"type": "text", "text": "café review done"}]}}) + "\n").encode("utf-8")
        # Pad + a newline + intent_line must total exactly TAIL_BYTES - 2
        # bytes, so that -- whatever the prefix length -- the seek point
        # (size - TAIL_BYTES) lands exactly 1 byte into the 3-byte euro
        # sign, splitting it.
        pad_len = tail_bytes - 3 - len(intent_line)
        self.assertGreaterEqual(pad_len, 0)
        rest = b"z" * pad_len + b"\n" + intent_line
        prefix = b"x" * 10
        data = prefix + euro + rest
        fd, path = tempfile.mkstemp()
        with os.fdopen(fd, "wb") as f:
            f.write(data)
        size = len(data)
        seek_offset = size - tail_bytes
        self.assertTrue(len(prefix) < seek_offset < len(prefix) + len(euro),
                        "test setup must actually split the multibyte character")
        out = pretool.intent_text(path)
        self.assertEqual(out, "café review done")


class BashGateTests(unittest.TestCase):
    """bash_is_state_changing: the allowlist (STATE_CHANGING) and the
    SCRIPT_CUES script-token rule."""

    def test_state_changing_commands(self):
        for cmd in [
            "git push origin main", "cd x && git commit -m hi", "sudo pacman -S foo",
            "docker compose up -d", "systemctl --user restart engrain", "rm -rf build",
            "FOO=1 cargo install --path .", "chezmoi apply ~/.bashrc", "./scripts/deploy.sh",
            "rsync -a a/ b/", "scp file.txt host:/tmp/",
            "git merge feature", "git checkout -b topic", "git clean -fd", "git stash",
            "git tag v1.0", "git filter-repo --path secrets",
            "paru -S foo", "kubectl apply -f x.yaml", "kubectl delete pod x",
            "terraform apply", "terraform destroy", "alembic upgrade head", "alembic downgrade -1",
            # The state-changing list is the spec PLUS extras,
            # not a replacement -- cargo publish and chezmoi's other
            # mutating subcommands are state-changing too.
            "cargo publish", "chezmoi add ~/.bashrc", "chezmoi re-add", "chezmoi forget host",
            "chezmoi update",
            # Migrations are named in the spec's own allowlist ("migrations
            # /deploy scripts"); manage.py migrate fires however it's run.
            "python manage.py migrate", "python3 manage.py migrate", "./manage.py migrate",
            "uv run manage.py migrate",
        ]:
            self.assertTrue(pretool.bash_is_state_changing(cmd), cmd)

    def test_read_only_commands(self):
        for cmd in [
            "ls -la", "grep -rn foo src", "cat README.md | head", "sqlite3 db 'select 1'",
            "cargo test --workspace", "git status", "git log --oneline", "cargo build",
            "kubectl get pods", "terraform plan", "alembic current",
        ]:
            self.assertFalse(pretool.bash_is_state_changing(cmd), cmd)

    def test_psql_is_not_on_the_allowlist(self):
        # psql is explicitly NOT on the allowlist, unlike every
        # other DB/infra CLI here -- reads and writes look identical from
        # the command line, so it never counts as state-changing.
        self.assertFalse(pretool.bash_is_state_changing("psql -c 'delete from t'"))
        self.assertFalse(pretool.bash_is_state_changing("psql -c 'select 1'"))

    def test_script_cue_examples(self):
        # SCRIPT_CUES match the script token (the first token,
        # or the basename of a script path) -- NOT a substring of the whole
        # command.
        for cmd in ["cargo build --release", "grep -rn deploy src", "rg migrate"]:
            self.assertFalse(pretool.bash_is_state_changing(cmd), cmd)
        for cmd in ["./deploy.sh", "scripts/migrate.py"]:
            self.assertTrue(pretool.bash_is_state_changing(cmd), cmd)

    def test_manage_py_migrate_does_not_false_positive(self):
        # The `manage.py migrate` rule requires "migrate" strictly AFTER a
        # manage.py token -- these commands mention both words but never
        # in that order, so they must stay read-only.
        for cmd in ["grep -rn migrate manage.py", "cat manage.py | grep migrate",
                    "python manage.py makemigrations", "python manage.py test"]:
            self.assertFalse(pretool.bash_is_state_changing(cmd), cmd)

    def test_compound_commands_any_segment_matches(self):
        self.assertTrue(pretool.bash_is_state_changing("cd /tmp && git push origin main"))
        self.assertTrue(pretool.bash_is_state_changing("echo hi ; docker compose up -d"))
        self.assertTrue(pretool.bash_is_state_changing("ls -la | rm -rf build"))
        self.assertTrue(pretool.bash_is_state_changing("false || sudo systemctl restart x"))
        self.assertFalse(pretool.bash_is_state_changing("ls -la && cargo build"))


class BashWrappedCommandTests(unittest.TestCase):
    """bash_is_state_changing must see through
    wrapper commands and shell-quoting tricks to the real command hiding
    inside, not just the literal head token."""

    def test_leading_wrapper_commands_are_stripped(self):
        for cmd in [
            "timeout 30 git push origin main",
            "timeout -k 5 30 git push origin main",
            "timeout --signal=KILL 30 git push origin main",
            "nohup git push origin main",
            "nice -n 10 rm -rf build",
            "nice --adjustment 10 rm -rf build",
            "time git push origin main",
            "env FOO=1 BAR=2 git push origin main",
            "stdbuf -oL git push origin main",
            "ionice -c 3 rm -rf build",
            # a wrapper may itself follow a leading assignment
            "FOO=1 timeout 30 git push origin main",
        ]:
            self.assertTrue(pretool.bash_is_state_changing(cmd), cmd)

    def test_wrapped_read_only_commands_stay_read_only(self):
        # Stripping the wrapper must not make an innocent command look
        # state-changing -- only the real head decides.
        for cmd in ["timeout 30 cargo build --release", "nice -n 10 cargo test --workspace",
                    "env FOO=1 git status"]:
            self.assertFalse(pretool.bash_is_state_changing(cmd), cmd)

    def test_sudo_still_fires_immediately_without_wrapper_peeling(self):
        # sudo is not in _WRAPPER_HEADS -- it already counts as
        # state-changing on its own regardless of what it wraps.
        self.assertTrue(pretool.bash_is_state_changing("sudo ls -la"))

    def test_dollar_paren_command_substitution(self):
        self.assertTrue(pretool.bash_is_state_changing("echo $(git push origin main)"))
        self.assertFalse(pretool.bash_is_state_changing("echo $(ls -la)"))

    def test_backtick_command_substitution(self):
        self.assertTrue(pretool.bash_is_state_changing("echo `git push origin main`"))
        self.assertFalse(pretool.bash_is_state_changing("echo `ls -la`"))

    def test_shell_dash_c_body(self):
        self.assertTrue(pretool.bash_is_state_changing('bash -c "git push origin main"'))
        self.assertTrue(pretool.bash_is_state_changing("sh -lc 'rm -rf build'"))
        self.assertFalse(pretool.bash_is_state_changing('bash -c "cargo build --release"'))

    def test_heredoc_fed_to_a_shell_recurses(self):
        self.assertTrue(pretool.bash_is_state_changing("bash <<EOF\ngit push origin main\nEOF"))
        self.assertTrue(pretool.bash_is_state_changing("sh <<'DONE'\nrm -rf build\nDONE"))
        self.assertFalse(pretool.bash_is_state_changing("bash <<EOF\ncargo build --release\nEOF"))

    def test_heredoc_fed_to_cat_is_data_not_a_command(self):
        # A real case from transcripts: a heredoc into `cat` (or tee,
        # python, etc) is writing TEXT that happens to contain the words
        # "git push" -- it must never be read as running them.
        cmd = ("cat > deploy_notes.txt <<EOF\n"
               "Remember to git push origin main after review\n"
               "EOF")
        self.assertFalse(pretool.bash_is_state_changing(cmd))


class BashFirstTouchTests(unittest.TestCase):
    """bash_should_recall: first-touch project roots."""

    def test_first_touch_of_a_project_root_via_path_token(self):
        root = tempfile.mkdtemp()
        try:
            os.makedirs(os.path.join(root, ".git"))
            os.makedirs(os.path.join(root, "src"))
            log = os.path.join(root, "log.jsonl")
            ok, roots = pretool.bash_should_recall("grep -rn x %s/src" % root, "/", log)
            self.assertTrue(ok)
            self.assertEqual(roots, [root])
            with open(log, "w") as f:
                f.write(json.dumps({"ts": "t", "roots": [root]}) + "\n")
            ok, roots = pretool.bash_should_recall("grep -rn x %s/src" % root, "/", log)
            self.assertFalse(ok, "second touch of a read-only command recalls nothing")
            self.assertEqual(roots, [])
        finally:
            shutil.rmtree(root, ignore_errors=True)

    def test_first_touch_counts_the_hooks_cwd_root_too(self):
        # A bare `cargo test` in a new project fires once, even though
        # the command names no path itself -- the cwd's own project root
        # counts as touched.
        root = tempfile.mkdtemp()
        try:
            os.makedirs(os.path.join(root, ".git"))
            log = os.path.join(root, "log.jsonl")
            ok, roots = pretool.bash_should_recall("cargo test", root, log)
            self.assertTrue(ok)
            self.assertEqual(roots, [root])
            # Same root, same session log -- now a read-only command with
            # no new root and no state change recalls nothing.
            with open(log, "w") as f:
                f.write(json.dumps({"ts": "t", "roots": [root]}) + "\n")
            ok, roots = pretool.bash_should_recall("cargo test", root, log)
            self.assertFalse(ok)
            self.assertEqual(roots, [])
        finally:
            shutil.rmtree(root, ignore_errors=True)

    def test_first_touch_disabled_without_a_session_log(self):
        # With no session_id (so main() passes log_path="") a root can
        # never be remembered as seen, so first-touch must not fire at
        # all -- only the state-changing gate applies.
        root = tempfile.mkdtemp()
        try:
            os.makedirs(os.path.join(root, ".git"))
            ok, roots = pretool.bash_should_recall("cargo test", root, "")
            self.assertFalse(ok)
            self.assertEqual(roots, [])
            # A state-changing command still fires even with no log path.
            ok, roots = pretool.bash_should_recall("rm -rf build", root, "")
            self.assertTrue(ok)
            self.assertEqual(roots, [])
        finally:
            shutil.rmtree(root, ignore_errors=True)

    def test_state_changing_command_in_an_already_seen_root_still_fires(self):
        root = tempfile.mkdtemp()
        try:
            os.makedirs(os.path.join(root, ".git"))
            log = os.path.join(root, "log.jsonl")
            with open(log, "w") as f:
                f.write(json.dumps({"ts": "t", "roots": [root]}) + "\n")
            ok, roots = pretool.bash_should_recall("git push origin main", root, log)
            self.assertTrue(ok)
            self.assertEqual(roots, [], "root already seen -- nothing new to log")
        finally:
            shutil.rmtree(root, ignore_errors=True)


class PretoolMainTests(unittest.TestCase):
    """End-to-end main(): via routing, and first-touch roots logged on
    their own ahead of every early return."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.orig_log_dir = pretool.kb_recall.RECALL_LOG_DIR
        pretool.kb_recall.RECALL_LOG_DIR = self.tmp
        self.orig_search = pretool.kb_recall.search
        self.orig_stdin = sys.stdin
        self.orig_stdout = sys.stdout

    def tearDown(self):
        pretool.kb_recall.RECALL_LOG_DIR = self.orig_log_dir
        pretool.kb_recall.search = self.orig_search
        sys.stdin = self.orig_stdin
        sys.stdout = self.orig_stdout
        shutil.rmtree(self.tmp, ignore_errors=True)

    def _log_path(self, session_id):
        return os.path.join(self.tmp, session_id + ".jsonl")

    def _run(self, payload, search_return, calls):
        def fake_search(query, project, cwd, via, exclude, explore):
            calls.append({"query": query, "project": project, "cwd": cwd, "via": via,
                          "exclude": exclude, "explore": explore})
            return search_return
        pretool.kb_recall.search = fake_search
        sys.stdin = io.StringIO(json.dumps(payload))
        out = io.StringIO()
        sys.stdout = out
        pretool.main()
        return out.getvalue()

    def test_via_is_pretool_plus_tool_name(self):
        calls = []
        hits = [{"id": 1, "content": "remember this", "score": 0.9, "sim": 0.9,
                 "recency": 0.9, "strength": 0.5, "created_at": "2026-09-01T00:00:00Z"}]
        payload = {"tool_name": "Bash", "session_id": "s1", "cwd": "/tmp",
                   "tool_input": {"command": "git push origin main"}}
        out = self._run(payload, {"hits": hits, "cards": []}, calls)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0]["via"], "pretool:Bash")
        self.assertIn("additionalContext", out)
        with open(self._log_path("s1")) as f:
            entry = json.loads(f.readlines()[-1])
        self.assertEqual(entry["via"], "pretool:Bash")
        self.assertEqual(entry["ids"], [1])

    def test_feat_explore_and_gate_are_logged(self):
        calls = []
        hits = [
            {"id": 1, "content": "kept direct", "score": 0.9, "sim": 0.9, "recency": 0.9,
             "strength": 0.5, "lexical": 0.2, "rank": 0, "created_at": "2026-09-01T00:00:00Z"},
            {"id": 9, "content": "exploration pick", "score": 0.1, "sim": 0.1, "recency": 0.1,
             "strength": 0.1, "lexical": 0.0, "rank": 7, "explore": True,
             "created_at": "2026-09-01T00:00:00Z"},
        ]
        payload = {"tool_name": "Bash", "session_id": "s6", "cwd": "/tmp",
                   "tool_input": {"command": "git push origin main"}}
        self._run(payload, {"hits": hits, "cards": [], "gate": "model-7"}, calls)
        with open(self._log_path("s6")) as f:
            entry = json.loads(f.readlines()[-1])
        self.assertEqual(entry["gate"], "model-7")
        self.assertEqual(entry["explore"], [9])
        self.assertIn("1", entry["feat"])
        self.assertIn("9", entry["feat"])
        self.assertEqual(entry["feat"]["1"]["rank"], 0)

    def test_roots_logged_even_when_the_search_response_has_no_hits_or_cards(self):
        root = tempfile.mkdtemp()
        try:
            os.makedirs(os.path.join(root, ".git"))
            calls = []
            payload = {"tool_name": "Bash", "session_id": "s2", "cwd": root,
                       "tool_input": {"command": "cargo test"}}
            out = self._run(payload, {"hits": [], "cards": []}, calls)
            self.assertEqual(out, "", "nothing to inject -- no additionalContext printed")
            self.assertEqual(len(calls), 1, "a first-touch root must still trigger a search")
            with open(self._log_path("s2")) as f:
                entry = json.loads(f.readlines()[-1])
            self.assertEqual(entry.get("roots"), [root])
            self.assertNotIn("via", entry, "the bare roots-only log line carries no via")
        finally:
            shutil.rmtree(root, ignore_errors=True)

    def test_roots_logged_even_when_search_returns_nothing_at_all(self):
        root = tempfile.mkdtemp()
        try:
            os.makedirs(os.path.join(root, ".git"))
            calls = []
            payload = {"tool_name": "Bash", "session_id": "s3", "cwd": root,
                       "tool_input": {"command": "cargo test"}}
            out = self._run(payload, None, calls)
            self.assertEqual(out, "")
            with open(self._log_path("s3")) as f:
                entry = json.loads(f.readlines()[-1])
            self.assertEqual(entry.get("roots"), [root])
        finally:
            shutil.rmtree(root, ignore_errors=True)

    def test_roots_logged_even_when_hits_are_all_below_threshold(self):
        root = tempfile.mkdtemp()
        try:
            os.makedirs(os.path.join(root, ".git"))
            calls = []
            low_hit = [{"id": 5, "content": "weak match", "score": 0.1, "sim": 0.1,
                        "recency": 0.1, "strength": 0.1, "created_at": "2026-09-01T00:00:00Z"}]
            payload = {"tool_name": "Bash", "session_id": "s4", "cwd": root,
                       "tool_input": {"command": "cargo test"}}
            out = self._run(payload, {"hits": low_hit, "cards": []}, calls)
            self.assertEqual(out, "", "below-threshold hit renders nothing")
            with open(self._log_path("s4")) as f:
                entry = json.loads(f.readlines()[-1])
            self.assertEqual(entry.get("roots"), [root])
        finally:
            shutil.rmtree(root, ignore_errors=True)

    def test_non_state_changing_command_in_a_known_root_never_searches(self):
        root = tempfile.mkdtemp()
        try:
            os.makedirs(os.path.join(root, ".git"))
            log = self._log_path("s5")
            os.makedirs(self.tmp, exist_ok=True)
            with open(log, "w") as f:
                f.write(json.dumps({"ts": "t", "roots": [root]}) + "\n")
            calls = []
            payload = {"tool_name": "Bash", "session_id": "s5", "cwd": root,
                       "tool_input": {"command": "cargo build"}}
            out = self._run(payload, {"hits": [], "cards": []}, calls)
            self.assertEqual(out, "")
            self.assertEqual(calls, [], "gate must return before any search")
        finally:
            shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()
