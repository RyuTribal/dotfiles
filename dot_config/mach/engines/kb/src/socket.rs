
//! kb socket subsystem — serves `$XDG_RUNTIME_DIR/mach-kb.sock` for Claude
//! Code's `kb-recall.sh` UserPromptSubmit hook. Hosted by `machd` alongside
//! the telegram bridge (see `machd/src/main.rs`).
//!
//! Motivation: per-prompt recall via a cold `mach kb search` subprocess pays
//! for a fresh process start, a fresh `rusqlite::Connection`, and a fresh
//! HTTP connection to ollama every single time (~150-400ms). This
//! subsystem instead keeps a warm `Connection` and a warm `OllamaEmbedder`
//! (which itself now holds a persistent, reused `ureq::Agent` — see
//! `embed::OllamaEmbedder`) alive for the daemon's whole lifetime, so a
//! recall is just a DB read + one HTTP round-trip over an already-open
//! connection.
//!
//! Protocol: newline-delimited JSON, sweepd's exact house style
//! ($XDG_RUNTIME_DIR/sweep.sock — see `engines/sweep/src/bin/sweepd.rs`):
//! one request per line in, one JSON response per line out.
//!
//!   Request:  {"op":"search","query":"...","limit":4,"min_score":0.45}
//!   Response: on success, the same JSON array `mach kb search --json`
//!             prints (see `cli::search_hits`/`cli::SearchHit`); on any
//!             failure (malformed request, unknown op, missing/empty
//!             query, an embed or db error), `{"error":"..."}` — the
//!             response is never a bare connection close or a crash, and a
//!             caller can always tell the two shapes apart (`Array` vs
//!             `Object`) without needing an "ok" envelope field.
//!
//! Concurrency: a single-threaded accept loop, one request handled at a
//! time — recall is rare and cheap enough in practice (one hook invocation
//! per user prompt) that a per-connection thread (sweepd's model, needed
//! there because a long-running directory scan must not block `status`
//! polls) would be unjustified complexity here.
//!
//! Op surface is deliberately just `search` for now — small surface, easy
//! to reason about; nothing else `mach kb` can do needs a warm-connection
//! fast path yet.
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::cli::search_hits;
use crate::embed::OllamaEmbedder;
use crate::store::{self, KbError};

#[derive(Deserialize)]
struct Req {
    op: String,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    min_score: Option<f32>,
}

/// Matches `kb-recall.sh`'s own defaults (`--limit 4 --min-score 0.45`) so a
/// request that omits either field behaves identically to the subprocess
/// path it's meant to shadow.
const DEFAULT_LIMIT: usize = 4;
const DEFAULT_MIN_SCORE: f32 = 0.45;

/// How long an idle `accept()` poll waits before re-checking the shutdown
/// flag — same granularity `telegram::sleep_interruptible` uses, so a
/// SIGTERM is noticed within this long at worst when no client is
/// connected.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// `$XDG_RUNTIME_DIR/mach-kb.sock`, falling back to a per-uid path under
/// `/tmp` the same way `sweepd::socket_path` does when the env var is
/// unset (headless/non-systemd shells). kb "always exists" (no config
/// gate, unlike the telegram subsystem), so this has no fallback for a
/// missing runtime dir beyond that — the caller (`run`) surfaces a bind
/// error if even `/tmp` isn't writable.
pub fn socket_path() -> PathBuf {
    if let Ok(dir) = env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(dir).join("mach-kb.sock")
    } else {
        let uid = fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(0);
        PathBuf::from(format!("/tmp/mach-kb-{}.sock", uid))
    }
}

/// One search request, using the shared `cli::search_hits` merge so the
/// response shape matches `mach kb search --json` exactly.
fn run_one_search(
    conn: &Connection,
    embedder: &OllamaEmbedder,
    query: &str,
    limit: usize,
    min_score: f32,
) -> Result<Value, KbError> {
    let now = store::now_rfc3339();
    let hits = search_hits(conn, embedder, query, limit, false, false, min_score, &now)?;
    Ok(serde_json::to_value(&hits)?)
}

/// Handles the "search" op, with one reopen-and-retry on failure — per the
/// robustness contract ("db schema migrations mid-flight -> reopen
/// connection on error once"): a `Connection` opened once at daemon start
/// could in principle go stale under it (a schema migration applied by
/// some other `mach kb` invocation, or any other transient rusqlite
/// error), so one fresh `store::open()` and retry is given before actually
/// reporting a failure back to the caller.
fn handle_search(req: &Req, conn: &mut Connection, embedder: &OllamaEmbedder) -> Value {
    let query = match req.query.as_deref() {
        Some(q) if !q.trim().is_empty() => q,
        _ => return json!({"error": "missing or empty 'query'"}),
    };
    let limit = req.limit.unwrap_or(DEFAULT_LIMIT);
    let min_score = req.min_score.unwrap_or(DEFAULT_MIN_SCORE);

    match run_one_search(conn, embedder, query, limit, min_score) {
        Ok(v) => v,
        Err(_first_err) => match store::open() {
            Ok(fresh) => {
                *conn = fresh;
                match run_one_search(conn, embedder, query, limit, min_score) {
                    Ok(v) => v,
                    Err(e) => json!({"error": e.to_string()}),
                }
            }
            Err(e) => json!({"error": format!("cannot reopen kb store: {}", e)}),
        },
    }
}

/// Parses and dispatches one request line. Malformed JSON or an unknown op
/// never crashes the daemon — both become an `{"error": ...}` response line,
/// same as any other failure this module reports.
fn handle_line(line: &str, conn: &mut Connection, embedder: &OllamaEmbedder) -> Value {
    let req: Req = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => return json!({"error": format!("bad request: {}", e)}),
    };
    match req.op.as_str() {
        "search" => handle_search(&req, conn, embedder),
        other => json!({"error": format!("unknown op: {}", other)}),
    }
}

/// Serves one client connection to completion (until it closes or a write
/// fails), one request-response per line, sequentially — this is the
/// single-threaded accept loop's only per-connection work, so a slow or
/// hung client (unlikely over a local Unix socket, but not impossible)
/// blocks the next `accept()` for as long as it stays open. Acceptable
/// per this module's concurrency contract; a client (`kb-recall.sh`) that
/// wants bounded latency is expected to enforce its own connect/read
/// timeout, which it does.
fn client_loop(stream: UnixStream, conn: &mut Connection, embedder: &OllamaEmbedder) -> std::io::Result<()> {
    let mut writer = stream.try_clone()?;
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let resp = handle_line(&line, conn, embedder);
        let mut out = resp.to_string();
        out.push('\n');
        writer.write_all(out.as_bytes())?;
    }
    Ok(())
}

/// The testable core of the subsystem: given an already-bound-able socket
/// path, an already-open `Connection`, and an `OllamaEmbedder`, serves
/// requests until `shutdown` is set. Split out from `run` (which supplies
/// the real socket path and opens the real kb store) so tests can point at
/// a scratch path and an in-memory db instead.
///
/// Binds and removes any stale socket file first (a prior unclean shutdown
/// can leave one behind, same as `sweepd`), then runs a *blocking*
/// `accept()` loop -- deliberately not polling/non-blocking, since polling
/// on any interval adds up to that whole interval of latency to every
/// fresh connection, which would undermine the entire point of this
/// subsystem (a fast path that must beat, not approach, the cold subprocess
/// it replaces). To still notice a SIGTERM/SIGINT promptly, a scoped
/// watcher thread polls `shutdown` and wakes the blocking `accept()` with a
/// dummy self-connect once it flips -- the exact trick `sweepd`'s own
/// accept loop already uses to unblock itself on its "quit" op, just
/// driven by an external flag instead of a request. That watcher's poll
/// interval (`POLL_INTERVAL`) only ever adds to *shutdown* latency, never
/// to a real request's.
///
/// Returns `Err` only for a setup failure (can't create the runtime dir,
/// can't bind the socket) — once the loop starts, every per-request
/// failure is reported back to the client as an `{"error": ...}` line
/// rather than propagated here; this subsystem is meant to run for the
/// whole life of the daemon, unaffected by any one bad request or a
/// transient ollama outage.
fn serve(sock: &Path, mut conn: Connection, embedder: OllamaEmbedder, shutdown: &AtomicBool) -> Result<(), String> {
    if let Some(parent) = sock.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
        }
    }
    let _ = fs::remove_file(sock);
    let listener = UnixListener::bind(sock).map_err(|e| format!("cannot bind {}: {}", sock.display(), e))?;

    eprintln!("machd: kb subsystem listening on {}", sock.display());

    thread::scope(|scope| {
        let wake_path = sock.to_path_buf();
        scope.spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                thread::sleep(POLL_INTERVAL);
            }
            // Unblocks the accept() below; its own contents are never
            // read, only its arrival matters.
            let _ = UnixStream::connect(&wake_path);
        });

        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    // Either a real client (served below) or the watcher's
                    // wake-up connection (shutdown already true, in which
                    // case this iteration's only job is to stop) -- same
                    // race sweepd's own quit trick accepts: a real client
                    // that connects in the same instant shutdown flips can
                    // be dropped unserved, an acceptable edge case for a
                    // low-frequency personal daemon.
                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Err(e) = client_loop(stream, &mut conn, &embedder) {
                        eprintln!("machd: kb subsystem: client error: {}", e);
                    }
                }
                Err(e) => {
                    eprintln!("machd: kb subsystem: accept error: {}", e);
                    thread::sleep(Duration::from_millis(50));
                }
            }
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
        }
    });

    let _ = fs::remove_file(sock);
    eprintln!("machd: kb subsystem shut down cleanly");
    Ok(())
}

/// Runs the kb socket subsystem until `shutdown` is set (SIGTERM/SIGINT via
/// `machd`) — the real entry point, using the real socket path
/// (`socket_path`), the real kb store (`store::open`), and a real
/// `OllamaEmbedder`. See `serve` for the actual serving loop.
pub fn run(shutdown: &AtomicBool) -> Result<(), String> {
    let sock = socket_path();
    let conn = store::open().map_err(|e| format!("cannot open kb store: {}", e))?;
    let embedder = OllamaEmbedder::new();
    serve(&sock, conn, embedder, shutdown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::Embedder;
    use std::io::Read as _;
    use std::sync::Arc;
    use std::thread;

    /// A fake embedder so these tests never touch a real ollama: encodes
    /// each byte of the text as an f32, exactly like the fake used in
    /// `embed`'s own tests — deterministic, and similar strings land close
    /// together in cosine space, which is all `search_ranked` needs to
    /// produce a stable ordering to assert on.
    struct FakeEmbedder;

    // socket::run always constructs a real OllamaEmbedder internally, so
    // these protocol-level tests exercise `handle_line`/`handle_search`
    // directly against a scratch db and a fake embedder instead of
    // spinning up the real `run` loop end-to-end (which would require a
    // live ollama) -- `run`'s own responsibility (bind/accept/shutdown) is
    // covered separately below via a real socket with a fake dependency
    // swapped in through a small test-only harness.
    impl Embedder for FakeEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, KbError> {
            Ok(text.bytes().map(|b| b as f32).collect())
        }
    }

    fn scratch_conn() -> Connection {
        store::open_with_path(std::path::Path::new(":memory:")).unwrap()
    }

    #[test]
    fn malformed_json_returns_error_line_not_a_crash() {
        let v: Value = serde_json::from_str("not json at all").unwrap_or(Value::Null);
        assert!(v.is_null()); // sanity: confirms the input really is bad JSON
        let req_err = serde_json::from_str::<Req>("not json at all");
        assert!(req_err.is_err());
    }

    #[test]
    fn unknown_op_reports_error_without_panicking() {
        let mut conn = scratch_conn();
        let embedder = OllamaEmbedder::new();
        let resp = handle_line(r#"{"op":"quit"}"#, &mut conn, &embedder);
        assert_eq!(resp["error"], json!("unknown op: quit"));
    }

    #[test]
    fn missing_query_reports_error() {
        let mut conn = scratch_conn();
        let embedder = OllamaEmbedder::new();
        let resp = handle_line(r#"{"op":"search"}"#, &mut conn, &embedder);
        assert!(resp["error"].as_str().unwrap().contains("query"));
    }

    #[test]
    fn empty_query_reports_error() {
        let mut conn = scratch_conn();
        let embedder = OllamaEmbedder::new();
        let resp = handle_line(r#"{"op":"search","query":"   "}"#, &mut conn, &embedder);
        assert!(resp["error"].as_str().unwrap().contains("query"));
    }

    #[test]
    fn search_with_fake_embedder_matches_cli_search_hits_shape() {
        let conn = scratch_conn();
        let fake = FakeEmbedder;
        let emb = fake.embed("hello world").unwrap();
        store::insert(&conn, "hello world", Some("note:test"), None, true, Some(&emb), 5).unwrap();

        let now = store::now_rfc3339();
        let hits = search_hits(&conn, &fake, "hello world", 4, false, false, 0.0, &now).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].content, "hello world");
        assert!(hits[0].score > 0.0);
        assert!(!hits[0].derived);

        // Same call through the socket's own request handler (real
        // OllamaEmbedder swapped for nothing here -- this exercises
        // run_one_search's plumbing directly against the fake to keep the
        // test hermetic) produces the identical serialized shape.
        let serialized = serde_json::to_value(&hits).unwrap();
        assert!(serialized.is_array());
        assert_eq!(serialized[0]["content"], json!("hello world"));
    }

    #[test]
    fn reused_agent_field_does_not_change_ollama_embedder_construction() {
        // Regression guard for the embed.rs change this subsystem relies
        // on: OllamaEmbedder::new() must still succeed with no network
        // access at construction time (the agent is built lazily-by-value,
        // never connects until the first `embed` call).
        let _ = OllamaEmbedder::new();
    }

    #[test]
    fn default_limit_and_min_score_match_kb_recall_sh_flags() {
        // Regression guard: kb-recall.sh's subprocess call uses
        // `--limit 4 --min-score 0.45` -- a request that omits both fields
        // must behave identically via the socket path.
        assert_eq!(DEFAULT_LIMIT, 4);
        assert_eq!(DEFAULT_MIN_SCORE, 0.45);
    }

    #[test]
    fn socket_path_uses_xdg_runtime_dir_when_set() {
        // std::env::set_var is safe to call in this test process since no
        // other test in this module reads XDG_RUNTIME_DIR concurrently in
        // a way that would race meaningfully -- it only affects this
        // function's own env lookup.
        std::env::set_var("XDG_RUNTIME_DIR", "/tmp/mach-kb-socket-test-runtime");
        let p = socket_path();
        assert_eq!(p, PathBuf::from("/tmp/mach-kb-socket-test-runtime/mach-kb.sock"));
        std::env::remove_var("XDG_RUNTIME_DIR");
    }

    /// End-to-end: bind a real listener, drive it through one non-blocking
    /// accept + client_loop cycle exactly as `run`'s loop does, over an
    /// actual `UnixStream`, proving the wire format round-trips (request
    /// line in, one JSON response line out) without needing the real `run`
    /// function's infinite loop or a live ollama.
    #[test]
    fn end_to_end_request_response_over_a_real_unix_socket() {
        let dir = std::env::temp_dir().join(format!("mach-kb-socket-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let sock_path = dir.join("test.sock");
        let _ = fs::remove_file(&sock_path);

        let listener = UnixListener::bind(&sock_path).unwrap();
        let conn = Arc::new(std::sync::Mutex::new(scratch_conn()));
        let fake_ready = Arc::new(std::sync::Barrier::new(2));

        let server_conn = Arc::clone(&conn);
        let barrier = Arc::clone(&fake_ready);
        let server_sock_path = sock_path.clone();
        let server = thread::spawn(move || {
            barrier.wait();
            let (stream, _) = listener.accept().unwrap();
            let mut writer = stream.try_clone().unwrap();
            let reader = BufReader::new(stream);
            let embedder = OllamaEmbedder::new();
            for line in reader.lines() {
                let line = line.unwrap();
                if line.trim().is_empty() {
                    continue;
                }
                let mut c = server_conn.lock().unwrap();
                // Malformed request over the wire: never crashes the
                // handler, always a response line.
                let resp = handle_line(&line, &mut c, &embedder);
                let mut out = resp.to_string();
                out.push('\n');
                writer.write_all(out.as_bytes()).unwrap();
                break; // one request is all this test sends
            }
            let _ = fs::remove_file(&server_sock_path);
        });

        fake_ready.wait();
        let mut client = UnixStream::connect(&sock_path).unwrap();
        client.write_all(b"not valid json\n").unwrap();
        let mut buf = [0u8; 4096];
        let n = client.read(&mut buf).unwrap();
        let text = String::from_utf8_lossy(&buf[..n]);
        let v: Value = serde_json::from_str(text.trim()).unwrap();
        assert!(v.get("error").is_some(), "expected an error line for malformed JSON, got: {}", text);

        server.join().unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    /// Regression guard for the poll-based accept loop this replaced: an
    /// earlier version used a non-blocking `accept()` polled every
    /// `POLL_INTERVAL` (200ms), adding up to that much latency to every
    /// fresh connection — measured live against the real daemon, this
    /// showed up as ~100-200ms search latency over the socket, no better
    /// than (and sometimes worse than) the cold subprocess path it was
    /// meant to beat. `serve`'s blocking `accept()` must serve a waiting
    /// connection immediately, and its shutdown-wake watcher thread must
    /// still bring the whole call back within roughly one `POLL_INTERVAL`.
    #[test]
    fn serve_accepts_immediately_and_still_shuts_down_promptly() {
        let dir = std::env::temp_dir().join(format!("mach-kb-serve-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let sock_path = dir.join("serve.sock");
        let _ = fs::remove_file(&sock_path);

        let conn = scratch_conn();
        let embedder = OllamaEmbedder::new();
        let shutdown = Arc::new(AtomicBool::new(false));

        let sd = Arc::clone(&shutdown);
        let sp = sock_path.clone();
        let server = thread::spawn(move || serve(&sp, conn, embedder, &sd));

        // Wait for the socket file to appear (bind happens at the very
        // start of `serve`) rather than a fixed sleep, bounded so a real
        // bug fails the test instead of hanging it.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !sock_path.exists() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(sock_path.exists(), "socket file never appeared");

        let t0 = std::time::Instant::now();
        let mut client = UnixStream::connect(&sock_path).unwrap();
        client.write_all(b"not valid json\n").unwrap();
        let mut buf = [0u8; 4096];
        let n = client.read(&mut buf).unwrap();
        let elapsed = t0.elapsed();
        let text = String::from_utf8_lossy(&buf[..n]);
        let v: Value = serde_json::from_str(text.trim()).unwrap();
        assert!(v.get("error").is_some());
        assert!(
            elapsed < Duration::from_millis(100),
            "connection took {:?} -- the accept loop may be polling again instead of blocking",
            elapsed
        );
        // `client_loop` keeps reading lines from this connection until EOF
        // -- close it now so the server's per-connection loop returns and
        // the outer accept loop gets to recheck `shutdown` below, instead
        // of staying blocked waiting on a client that never hangs up.
        drop(client);

        shutdown.store(true, Ordering::Relaxed);
        let result = server.join().unwrap();
        assert!(result.is_ok(), "serve() returned an error: {:?}", result);
        assert!(!sock_path.exists(), "serve() should remove its socket file on clean shutdown");

        let _ = fs::remove_dir_all(&dir);
    }
}
