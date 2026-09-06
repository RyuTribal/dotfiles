
//! sweepd — Unix-socket JSON daemon for the Quickshell UI.
//!
//! Protocol: newline-delimited JSON over $XDG_RUNTIME_DIR/sweep.sock.
//! Requests:  {"op":"scan","path":"/x"} {"op":"status"} {"op":"ls","id":0}
//!            {"op":"mark","id":3} {"op":"unmark","id":3} {"op":"restore_all"}
//!            {"op":"commit"} {"op":"quit"}
//! Every response is a single JSON line with "ok": true/false.
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::Deserialize;
use serde_json::{json, Value};
use sweep::{human, scan_dir, Progress, Store};

#[derive(Deserialize)]
struct Req {
    op: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    id: Option<usize>,
}

struct State {
    store: Option<Store>,
    scanning: bool,
    scan_path: Option<PathBuf>,
}

fn socket_path() -> PathBuf {
    if let Ok(dir) = env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(dir).join("sweep.sock")
    } else {
        PathBuf::from(format!("/tmp/sweep-{}.sock", libc_geteuid()))
    }
}

// avoid a libc dependency for one call
fn libc_geteuid() -> u32 {
    fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(0)
}
use std::os::unix::fs::MetadataExt;

fn trash_dir() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/share/sweep/trash").join(format!("daemon-{}", std::process::id()))
}

// statvfs via FFI (no libc crate dependency)
#[repr(C)]
struct StatVfs {
    f_bsize: u64,
    f_frsize: u64,
    f_blocks: u64,
    f_bfree: u64,
    f_bavail: u64,
    f_files: u64,
    f_ffree: u64,
    f_favail: u64,
    f_fsid: u64,
    f_flag: u64,
    f_namemax: u64,
    __spare: [i32; 6],
}

extern "C" {
    fn statvfs(path: *const i8, buf: *mut StatVfs) -> i32;
}

fn statvfs_json(target: &str) -> Option<Value> {
    use std::ffi::CString;
    let c = CString::new(target).ok()?;
    let mut sv = unsafe { std::mem::zeroed::<StatVfs>() };
    if unsafe { statvfs(c.as_ptr(), &mut sv) } != 0 {
        return None;
    }
    if sv.f_blocks == 0 {
        return None;
    }
    let total = sv.f_blocks * sv.f_frsize;
    let avail = sv.f_bavail * sv.f_frsize;
    let free = sv.f_bfree * sv.f_frsize;
    let used = total - free;
    // percentage the way df computes it: used / (used + avail)
    let pcent = if used + avail > 0 {
        ((used as f64 / (used + avail) as f64) * 100.0).ceil() as u64
    } else {
        0
    };
    Some(json!({
        "target": target,
        "total": total,
        "used": used,
        "avail": avail,
        "pcent": pcent,
        "total_human": human(total),
        "used_human": human(used),
        "avail_human": human(avail),
    }))
}

fn entry_json(store: &Store, idx: usize) -> Value {
    let n = &store.arena[idx];
    json!({
        "id": idx,
        "name": n.name,
        "size": n.apparent,
        "disk": n.disk,
        "human": human(n.apparent),
        "dir": n.is_dir,
        "marked": n.staged.is_some(),
    })
}

fn handle(req: Req, st: &Arc<Mutex<State>>, prog: &Arc<Progress>, trash: &PathBuf) -> (Value, bool) {
    let mut quit = false;
    let resp = match req.op.as_str() {
        "scan" => {
            let path = PathBuf::from(req.path.unwrap_or_else(|| ".".into()));
            let path = match fs::canonicalize(&path) {
                Ok(p) => p,
                Err(e) => return (json!({"ok": false, "error": format!("bad path: {}", e)}), false),
            };
            let mut s = st.lock().unwrap();
            if s.scanning {
                json!({"ok": false, "error": "scan already in progress"})
            } else {
                // restore anything staged from a previous tree before dropping it
                if let Some(store) = s.store.as_mut() { store.restore_all(); }
                s.store = None;
                s.scanning = true;
                s.scan_path = Some(path.clone());
                prog.files.store(0, Ordering::Relaxed);
                prog.bytes.store(0, Ordering::Relaxed);
                prog.errors.store(0, Ordering::Relaxed);
                let st2 = Arc::clone(st);
                let prog2 = Arc::clone(prog);
                let trash2 = trash.clone();
                thread::spawn(move || {
                    let raw = scan_dir(&path, path.display().to_string(), &prog2);
                    let mut s = st2.lock().unwrap();
                    s.store = Some(Store::new(raw, trash2));
                    s.scanning = false;
                });
                json!({"ok": true, "scanning": true})
            }
        }
        "mounts" => {
            // df-equivalent: real block-device mounts with usage, via statvfs.
            let mut list: Vec<Value> = Vec::new();
            if let Ok(mounts) = fs::read_to_string("/proc/mounts") {
                for line in mounts.lines() {
                    let mut it = line.split_whitespace();
                    let (src, target) = match (it.next(), it.next()) {
                        (Some(s), Some(t)) => (s, t),
                        _ => continue,
                    };
                    if !src.starts_with("/dev/") || target.starts_with("/boot/efi") {
                        continue;
                    }
                    // unescape octal (\040 = space) in mount target
                    let target = target.replace("\\040", " ");
                    if let Some(v) = statvfs_json(&target) {
                        // skip duplicate mounts of the same device (bind mounts)
                        if !list.iter().any(|e| e["target"] == v["target"]) {
                            list.push(v);
                        }
                    }
                }
            }
            json!({"ok": true, "mounts": list})
        }
        "status" => {
            let s = st.lock().unwrap();
            json!({
                "ok": true,
                "scanning": s.scanning,
                "ready": s.store.is_some(),
                "files": prog.files.load(Ordering::Relaxed),
                "bytes": prog.bytes.load(Ordering::Relaxed),
                "bytes_human": human(prog.bytes.load(Ordering::Relaxed)),
                "errors": prog.errors.load(Ordering::Relaxed),
                "path": s.scan_path.as_ref().map(|p| p.display().to_string()),
            })
        }
        "ls" => {
            let s = st.lock().unwrap();
            match s.store.as_ref() {
                None => json!({"ok": false, "error": "no scan loaded"}),
                Some(store) => {
                    let id = req.id.unwrap_or(store.root);
                    if id >= store.arena.len() || store.arena[id].deleted {
                        json!({"ok": false, "error": "bad id"})
                    } else {
                        let mut kids: Vec<usize> = store.arena[id].children.iter().copied()
                            .filter(|&c| !store.arena[c].deleted).collect();
                        kids.sort_by(|&a, &b| store.arena[b].apparent.cmp(&store.arena[a].apparent)
                            .then_with(|| store.arena[a].name.cmp(&store.arena[b].name)));
                        let entries: Vec<Value> = kids.iter().map(|&c| entry_json(store, c)).collect();
                        let (n_staged, sz_staged) = store.staged_stats();
                        json!({
                            "ok": true,
                            "id": id,
                            "parent": store.arena[id].parent,
                            "path": store.node_path(id).display().to_string(),
                            "total": store.arena[id].apparent,
                            "total_human": human(store.arena[id].apparent),
                            "staged_count": n_staged,
                            "staged_bytes": sz_staged,
                            "staged_human": human(sz_staged),
                            "freed_human": human(store.freed),
                            "entries": entries,
                        })
                    }
                }
            }
        }
        "mark" | "unmark" => {
            let mut s = st.lock().unwrap();
            match s.store.as_mut() {
                None => json!({"ok": false, "error": "no scan loaded"}),
                Some(store) => {
                    let id = match req.id {
                        Some(i) if i < store.arena.len() => i,
                        _ => return (json!({"ok": false, "error": "bad id"}), false),
                    };
                    let res = if req.op == "mark" { store.mark(id) } else { store.unmark(id) };
                    match res {
                        Ok(()) => json!({"ok": true, "id": id, "marked": store.arena[id].staged.is_some()}),
                        Err(e) => json!({"ok": false, "error": e}),
                    }
                }
            }
        }
        "restore_all" => {
            let mut s = st.lock().unwrap();
            match s.store.as_mut() {
                None => json!({"ok": false, "error": "no scan loaded"}),
                Some(store) => {
                    let n = store.restore_all();
                    json!({"ok": true, "restored": n})
                }
            }
        }
        "commit" => {
            let mut s = st.lock().unwrap();
            match s.store.as_mut() {
                None => json!({"ok": false, "error": "no scan loaded"}),
                Some(store) => {
                    let (ok, failed, err) = store.commit();
                    json!({
                        "ok": failed == 0,
                        "deleted": ok,
                        "failed": failed,
                        "error": err,
                        "freed_human": human(store.freed),
                    })
                }
            }
        }
        "quit" => {
            let mut s = st.lock().unwrap();
            if let Some(store) = s.store.as_mut() { store.restore_all(); }
            quit = true;
            json!({"ok": true})
        }
        other => json!({"ok": false, "error": format!("unknown op: {}", other)}),
    };
    (resp, quit)
}

fn client_loop(stream: UnixStream, st: Arc<Mutex<State>>, prog: Arc<Progress>, trash: PathBuf) -> bool {
    let mut writer = match stream.try_clone() { Ok(w) => w, Err(_) => return false };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }
        let (resp, quit) = match serde_json::from_str::<Req>(&line) {
            Ok(req) => handle(req, &st, &prog, &trash),
            Err(e) => (json!({"ok": false, "error": format!("bad request: {}", e)}), false),
        };
        let mut out = resp.to_string();
        out.push('\n');
        if writer.write_all(out.as_bytes()).is_err() { break; }
        if quit { return true; }
    }
    false
}

fn main() {
    let sock = socket_path();
    let _ = fs::remove_file(&sock);
    let trash = trash_dir();
    fs::create_dir_all(&trash).expect("cannot create trash dir");

    let listener = UnixListener::bind(&sock).expect("cannot bind socket");
    eprintln!("sweepd listening on {}", sock.display());

    let st = Arc::new(Mutex::new(State { store: None, scanning: false, scan_path: None }));
    let prog = Arc::new(Progress::default());
    let quit_flag = Arc::new(AtomicBool::new(false));

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let st2 = Arc::clone(&st);
                let prog2 = Arc::clone(&prog);
                let trash2 = trash.clone();
                let quit2 = Arc::clone(&quit_flag);
                thread::spawn(move || {
                    if client_loop(s, st2, prog2, trash2) {
                        quit2.store(true, Ordering::Relaxed);
                        // wake the accept loop so it can exit
                        let _ = UnixStream::connect(socket_path());
                    }
                });
                if quit_flag.load(Ordering::Relaxed) {
                    break;
                }
            }
            Err(e) => eprintln!("accept error: {}", e),
        }
        if quit_flag.load(Ordering::Relaxed) {
            break;
        }
    }
    let _ = fs::remove_file(&sock);
    let _ = fs::remove_dir_all(&trash);
}
