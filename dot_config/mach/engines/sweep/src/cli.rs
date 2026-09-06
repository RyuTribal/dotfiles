
//! sweep — terminal TUI frontend over the shared engine.
use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::{human, scan_dir, Progress, RawNode, Store};

#[derive(PartialEq)]
enum Mode { Browse, Confirm }

#[derive(PartialEq)]
enum SortMode { SizeDesc, NameAsc }

struct App {
    store: Store,
    cwd: usize,
    list: Vec<usize>,
    cursor: usize,
    sort: SortMode,
    disk_mode: bool,
    mode: Mode,
    status: String,
    errors: u64,
}

impl App {
    fn size_of(&self, idx: usize) -> u64 {
        let n = &self.store.arena[idx];
        if self.disk_mode { n.disk } else { n.apparent }
    }

    fn rebuild(&mut self) {
        let mut items: Vec<usize> = self.store.arena[self.cwd].children.iter().copied()
            .filter(|&c| !self.store.arena[c].deleted).collect();
        match self.sort {
            SortMode::SizeDesc => items.sort_by(|&a, &b| {
                self.size_of(b).cmp(&self.size_of(a))
                    .then_with(|| self.store.arena[a].name.cmp(&self.store.arena[b].name))
            }),
            SortMode::NameAsc => items.sort_by(|&a, &b| self.store.arena[a].name.cmp(&self.store.arena[b].name)),
        }
        self.list = items;
        if self.cursor >= self.list.len() {
            self.cursor = self.list.len().saturating_sub(1);
        }
    }
}

fn centered_rect(w: u16, h: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect::new(x, y, w.min(area.width), h.min(area.height))
}

fn draw(app: &mut App, f: &mut Frame) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1), Constraint::Length(2)])
        .split(f.area());

    let cwd_path = app.store.node_path(app.cwd);
    let total = app.size_of(app.cwd);
    let mode = if app.disk_mode { "disk" } else { "apparent" };
    let mut header = vec![
        Span::styled(" sweep ", Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(cwd_path.display().to_string(), Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!("  {}  [{}]", human(total), mode)),
    ];
    if app.errors > 0 {
        header.push(Span::styled(format!("  {} unreadable", app.errors), Style::default().fg(Color::Yellow)));
    }
    f.render_widget(Paragraph::new(Line::from(header)), chunks[0]);

    let max_sz = app.list.iter().map(|&i| app.size_of(i)).max().unwrap_or(1).max(1);
    let items: Vec<ListItem> = app.list.iter().map(|&i| {
        let n = &app.store.arena[i];
        let sz = app.size_of(i);
        let filled = ((sz as u128 * 10) / max_sz as u128) as usize;
        let bar: String = "#".repeat(filled) + &" ".repeat(10 - filled);
        let pct = if total > 0 { sz as f64 * 100.0 / total as f64 } else { 0.0 };
        let marked = n.staged.is_some();
        let marker = if marked { "X " } else { "  " };
        let name = if n.is_dir { format!("{}/", n.name) } else { n.name.clone() };
        let name_style = if marked {
            Style::default().fg(Color::Red).add_modifier(Modifier::CROSSED_OUT)
        } else if n.is_dir {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else { Style::default() };
        ListItem::new(Line::from(vec![
            Span::raw(format!("{:>9} ", human(sz))),
            Span::styled(format!("[{}] ", bar), Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{:>5.1}%  ", pct)),
            Span::styled(marker, Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::styled(name, name_style),
        ]))
    }).collect();
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    if !app.list.is_empty() { state.select(Some(app.cursor)); }
    f.render_stateful_widget(list, chunks[1], &mut state);

    let (n_staged, sz_staged) = app.store.staged_stats();
    let staged_line = if n_staged > 0 {
        Line::from(Span::styled(
            format!(" {} item(s) staged for deletion ({}) — w: delete permanently, u: restore", n_staged, human(sz_staged)),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)))
    } else {
        Line::from(Span::styled(format!(" {}", app.status), Style::default().fg(Color::Yellow)))
    };
    let keys = Line::from(Span::styled(
        " space:mark  enter:open  h:up  w:commit-delete  u:restore-all  s:sort  a:size-mode  q:quit",
        Style::default().fg(Color::DarkGray)));
    f.render_widget(Paragraph::new(vec![staged_line, keys]), chunks[2]);

    if app.mode == Mode::Confirm {
        let (n, sz) = app.store.staged_stats();
        let area = centered_rect(56, 5, f.area());
        f.render_widget(Clear, area);
        let text = vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  Permanently delete {} item(s), {}?  [y/N]", n, human(sz)),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))),
        ];
        let block = Block::default().borders(Borders::ALL).title(" confirm ").style(Style::default().bg(Color::Black));
        f.render_widget(Paragraph::new(text).block(block), area);
    }
}

fn collect_files(raw: &RawNode, prefix: &std::path::Path, out: &mut Vec<(u64, PathBuf)>) {
    for c in &raw.children {
        let p = prefix.join(&c.name);
        if c.is_dir { collect_files(c, &p, out); } else { out.push((c.apparent, p)); }
    }
}

fn run_top(root_path: &std::path::Path, top: usize) {
    let prog = Progress::default();
    let raw = scan_dir(root_path, root_path.display().to_string(), &prog);
    println!("total: {}  ({} files, {} unreadable)",
        human(raw.apparent), prog.files.load(Ordering::Relaxed), prog.errors.load(Ordering::Relaxed));
    let mut files = Vec::new();
    collect_files(&raw, root_path, &mut files);
    files.sort_by(|a, b| b.0.cmp(&a.0));
    for (sz, p) in files.into_iter().take(top) {
        println!("{:>10}  {}", human(sz), p.display());
    }
}

/// Runs the sweep TUI/CLI given the arguments following the program name
/// (or, from `mach`, the arguments following the `sweep` subcommand).
pub fn run(args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut path = PathBuf::from(".");
    let mut top: Option<usize> = None;
    let mut args = args;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--top" => top = Some(args.next().and_then(|v| v.parse().ok()).unwrap_or(20)),
            "-h" | "--help" => {
                println!("sweep — interactive disk usage browser & staged deleter");
                println!();
                println!("usage: sweep [PATH]           interactive TUI (default: .)");
                println!("       sweep --top N [PATH]   print N largest files and exit");
                println!();
                println!("TUI keys: space=mark(→trash) enter=open h=up w=commit-delete");
                println!("          u=restore-all s=sort a=apparent/disk g/G=top/bottom q=quit");
                return Ok(());
            }
            other => path = PathBuf::from(other),
        }
    }
    let path = fs::canonicalize(&path)?;
    if let Some(n) = top { run_top(&path, n); return Ok(()); }

    let home = env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let trash_dir = PathBuf::from(home).join(".local/share/sweep/trash").join(format!("session-{}", std::process::id()));
    fs::create_dir_all(&trash_dir)?;

    let prog = Arc::new(Progress::default());
    let (tx, rx) = mpsc::channel();
    {
        let prog = prog.clone();
        let path = path.clone();
        thread::spawn(move || {
            let raw = scan_dir(&path, path.display().to_string(), &prog);
            let _ = tx.send(raw);
        });
    }

    let mut terminal = ratatui::init();
    let mut app: Option<App> = None;
    let mut quit = false;
    let mut freed_total = 0u64;

    while !quit {
        if app.is_none() {
            if let Ok(raw) = rx.try_recv() {
                let store = Store::new(raw, trash_dir.clone());
                let cwd = store.root;
                let mut a = App {
                    store, cwd, list: Vec::new(), cursor: 0,
                    sort: SortMode::SizeDesc, disk_mode: false, mode: Mode::Browse,
                    status: "scan complete".into(),
                    errors: prog.errors.load(Ordering::Relaxed),
                };
                a.rebuild();
                app = Some(a);
            }
        }

        match app.as_mut() {
            None => {
                let files = prog.files.load(Ordering::Relaxed);
                let bytes = prog.bytes.load(Ordering::Relaxed);
                let spin = ['|', '/', '-', '\\'];
                let tick = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                    .unwrap().subsec_millis() as usize / 250;
                terminal.draw(|f| {
                    let area = centered_rect(50, 3, f.area());
                    let text = format!(" {} scanning…  {} files, {}", spin[tick % 4], files, human(bytes));
                    f.render_widget(Paragraph::new(text).block(Block::default().borders(Borders::ALL).title(" sweep ")), area);
                })?;
            }
            Some(a) => { terminal.draw(|f| draw(a, f))?; }
        }

        if !event::poll(Duration::from_millis(100))? { continue; }
        let key = match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => k,
            _ => continue,
        };
        let ctrl_c = key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);

        match app.as_mut() {
            None => { if ctrl_c || key.code == KeyCode::Char('q') { quit = true; } }
            Some(a) => match a.mode {
                Mode::Confirm => match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        let (ok, failed, err) = a.store.commit();
                        a.status = if failed == 0 {
                            format!("permanently deleted {} item(s)", ok)
                        } else {
                            format!("deleted {}, {} failed: {}", ok, failed, err.unwrap_or_default())
                        };
                        a.mode = Mode::Browse;
                        a.rebuild();
                    }
                    _ => { a.mode = Mode::Browse; a.status = "commit cancelled".into(); }
                },
                Mode::Browse => {
                    if ctrl_c {
                        a.store.restore_all();
                        freed_total = a.store.freed;
                        quit = true;
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('q') => {
                            a.store.restore_all();
                            freed_total = a.store.freed;
                            quit = true;
                        }
                        KeyCode::Char('j') | KeyCode::Down => { if a.cursor + 1 < a.list.len() { a.cursor += 1; } }
                        KeyCode::Char('k') | KeyCode::Up => { a.cursor = a.cursor.saturating_sub(1); }
                        KeyCode::Char('g') => a.cursor = 0,
                        KeyCode::Char('G') => a.cursor = a.list.len().saturating_sub(1),
                        KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                            if let Some(&idx) = a.list.get(a.cursor) {
                                if a.store.arena[idx].is_dir {
                                    if a.store.arena[idx].staged.is_some() {
                                        a.status = "staged for deletion — unmark (space) to enter".into();
                                    } else {
                                        a.cwd = idx; a.cursor = 0; a.rebuild();
                                    }
                                }
                            }
                        }
                        KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => {
                            if let Some(p) = a.store.arena[a.cwd].parent {
                                let old = a.cwd;
                                a.cwd = p;
                                a.rebuild();
                                a.cursor = a.list.iter().position(|&i| i == old).unwrap_or(0);
                            }
                        }
                        KeyCode::Char(' ') | KeyCode::Char('d') => {
                            if let Some(&idx) = a.list.get(a.cursor) {
                                let res = if a.store.arena[idx].staged.is_some() {
                                    a.store.unmark(idx)
                                } else {
                                    a.store.mark(idx)
                                };
                                a.status = match res {
                                    Ok(()) => format!("toggled: {}", a.store.arena[idx].name),
                                    Err(e) => e,
                                };
                                if a.cursor + 1 < a.list.len() { a.cursor += 1; }
                            }
                        }
                        KeyCode::Char('u') => {
                            let n = a.store.restore_all();
                            a.status = format!("restored {} item(s)", n);
                        }
                        KeyCode::Char('s') => {
                            a.sort = if a.sort == SortMode::SizeDesc { SortMode::NameAsc } else { SortMode::SizeDesc };
                            a.rebuild();
                        }
                        KeyCode::Char('a') => { a.disk_mode = !a.disk_mode; a.rebuild(); }
                        KeyCode::Char('w') => {
                            let (n, _) = a.store.staged_stats();
                            if n == 0 { a.status = "nothing staged — mark items with space first".into(); }
                            else { a.mode = Mode::Confirm; }
                        }
                        _ => {}
                    }
                }
            },
        }
    }

    ratatui::restore();
    let _ = fs::remove_dir_all(&trash_dir);
    if freed_total > 0 { println!("sweep: freed {}", human(freed_total)); }
    Ok(())
}
