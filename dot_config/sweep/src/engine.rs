
//! Shared scan engine, tree arena, and staging-trash logic.
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;

const EXDEV: i32 = 18;

pub struct RawNode {
    pub name: String,
    pub apparent: u64,
    pub disk: u64,
    pub is_dir: bool,
    pub children: Vec<RawNode>,
}

#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub bytes: AtomicU64,
    pub errors: AtomicU64,
}

pub fn scan_dir(path: &Path, name: String, prog: &Progress) -> RawNode {
    let mut node = RawNode { name, apparent: 0, disk: 0, is_dir: true, children: Vec::new() };
    let entries = match fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => {
            prog.errors.fetch_add(1, Ordering::Relaxed);
            return node;
        }
    };
    let mut subdirs: Vec<(PathBuf, String)> = Vec::new();
    for entry in entries.flatten() {
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => { prog.errors.fetch_add(1, Ordering::Relaxed); continue; }
        };
        let fname = entry.file_name().to_string_lossy().into_owned();
        if ft.is_dir() {
            subdirs.push((entry.path(), fname));
        } else {
            match entry.metadata() {
                Ok(md) => {
                    let ap = md.len();
                    let dk = md.blocks() * 512;
                    node.children.push(RawNode { name: fname, apparent: ap, disk: dk, is_dir: false, children: Vec::new() });
                    node.apparent += ap;
                    node.disk += dk;
                    prog.files.fetch_add(1, Ordering::Relaxed);
                    prog.bytes.fetch_add(ap, Ordering::Relaxed);
                }
                Err(_) => { prog.errors.fetch_add(1, Ordering::Relaxed); }
            }
        }
    }
    let scanned: Vec<RawNode> = subdirs.into_par_iter().map(|(p, n)| scan_dir(&p, n, prog)).collect();
    for c in scanned {
        node.apparent += c.apparent;
        node.disk += c.disk;
        node.children.push(c);
    }
    node
}

pub struct Node {
    pub name: String,
    pub apparent: u64,
    pub disk: u64,
    pub is_dir: bool,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub staged: Option<PathBuf>,
    pub deleted: bool,
}

pub fn flatten(raw: RawNode, parent: Option<usize>, arena: &mut Vec<Node>) -> usize {
    let idx = arena.len();
    arena.push(Node {
        name: raw.name, apparent: raw.apparent, disk: raw.disk, is_dir: raw.is_dir,
        parent, children: Vec::new(), staged: None, deleted: false,
    });
    for c in raw.children {
        let ci = flatten(c, Some(idx), arena);
        arena[idx].children.push(ci);
    }
    idx
}

fn copy_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    let md = fs::symlink_metadata(src)?;
    if md.is_dir() {
        fs::create_dir_all(dst)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            copy_recursive(&e.path(), &dst.join(e.file_name()))?;
        }
    } else if md.file_type().is_symlink() {
        let target = fs::read_link(src)?;
        std::os::unix::fs::symlink(target, dst)?;
    } else {
        fs::copy(src, dst)?;
    }
    Ok(())
}

pub fn move_path(src: &Path, dst: &Path) -> io::Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(EXDEV) => {
            copy_recursive(src, dst)?;
            let md = fs::symlink_metadata(src)?;
            if md.is_dir() { fs::remove_dir_all(src) } else { fs::remove_file(src) }
        }
        Err(e) => Err(e),
    }
}

pub fn human(sz: u64) -> String {
    const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
    let mut v = sz as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 { v /= 1024.0; u += 1; }
    if u == 0 { format!("{} {}", sz, UNITS[0]) } else { format!("{:.1} {}", v, UNITS[u]) }
}

/// Tree + staging trash operations shared by TUI and daemon.
pub struct Store {
    pub arena: Vec<Node>,
    pub root: usize,
    pub trash_dir: PathBuf,
    pub freed: u64,
}

impl Store {
    pub fn new(raw: RawNode, trash_dir: PathBuf) -> Self {
        let mut arena = Vec::new();
        let root = flatten(raw, None, &mut arena);
        Store { arena, root, trash_dir, freed: 0 }
    }

    pub fn node_path(&self, idx: usize) -> PathBuf {
        let n = &self.arena[idx];
        match n.parent {
            None => PathBuf::from(&n.name),
            Some(p) => self.node_path(p).join(&n.name),
        }
    }

    pub fn staged_stats(&self) -> (usize, u64) {
        let mut n = 0; let mut sz = 0;
        for node in &self.arena {
            if node.staged.is_some() && !node.deleted { n += 1; sz += node.apparent; }
        }
        (n, sz)
    }

    pub fn mark(&mut self, idx: usize) -> Result<(), String> {
        if idx == self.root { return Err("cannot mark the scan root".into()); }
        if self.arena[idx].staged.is_some() { return Ok(()); }
        let orig = self.node_path(idx);
        let fname = orig.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "item".into());
        let dest = self.trash_dir.join(format!("{:06}_{}", idx, fname));
        move_path(&orig, &dest).map_err(|e| format!("stage failed ({}): {}", fname, e))?;
        self.arena[idx].staged = Some(dest);
        Ok(())
    }

    pub fn unmark(&mut self, idx: usize) -> Result<(), String> {
        if let Some(staged) = self.arena[idx].staged.clone() {
            let orig = self.node_path(idx);
            move_path(&staged, &orig).map_err(|e| format!("restore failed: {}", e))?;
            self.arena[idx].staged = None;
        }
        Ok(())
    }

    pub fn restore_all(&mut self) -> usize {
        let staged: Vec<usize> = (0..self.arena.len())
            .filter(|&i| self.arena[i].staged.is_some() && !self.arena[i].deleted)
            .collect();
        let mut n = 0;
        for idx in staged {
            if self.unmark(idx).is_ok() { n += 1; }
        }
        n
    }

    /// Permanently delete all staged items. Returns (ok, failed, last_error).
    pub fn commit(&mut self) -> (usize, usize, Option<String>) {
        let staged: Vec<(usize, PathBuf)> = (0..self.arena.len())
            .filter_map(|i| {
                if self.arena[i].deleted { return None; }
                self.arena[i].staged.clone().map(|p| (i, p))
            })
            .collect();
        let (mut ok, mut failed, mut last_err) = (0usize, 0usize, None);
        for (idx, path) in staged {
            let res = if self.arena[idx].is_dir { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
            match res {
                Ok(()) => {
                    let (ap, dk) = (self.arena[idx].apparent, self.arena[idx].disk);
                    self.freed += ap;
                    self.arena[idx].deleted = true;
                    self.arena[idx].staged = None;
                    let mut cur = self.arena[idx].parent;
                    while let Some(p) = cur {
                        self.arena[p].apparent = self.arena[p].apparent.saturating_sub(ap);
                        self.arena[p].disk = self.arena[p].disk.saturating_sub(dk);
                        cur = self.arena[p].parent;
                    }
                    ok += 1;
                }
                Err(e) => { failed += 1; last_err = Some(e.to_string()); }
            }
        }
        (ok, failed, last_err)
    }
}
