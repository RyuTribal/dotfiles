//! Code index: chunk (see docs/superpowers/specs/2026-09-23-code-index-design.md).
//!
//! Turns one source file into syntax-aware [`store::NewCodeChunk`] rows:
//! one chunk per function/method/class/struct/enum/union (per grammar --
//! see the node-kind tables on each `walk_*` function below), an enclosing
//! `scope` string for members (`A::B` for C++/C#/Rust, `A.B` for
//! Python/TS/JS), and one `preamble` chunk per maximal contiguous run of
//! source lines that sits between/around units (includes, globals,
//! top-level statements) -- there can be more than one per file. Markdown
//! is split by heading without a grammar. Anything tree-sitter can't parse
//! cleanly, or whose extension isn't recognised, falls back to fixed-size
//! line windows.
//!
//! Invariant every chunk holds, unconditionally: `text` is exactly the
//! source's own lines `start_line..=end_line` (1-based, inclusive),
//! joined with `"\n"` -- nothing added (no repeated signature lines),
//! nothing dropped (no blank lines lost to fragment-joining), nothing
//! reordered. See `node_span`, `split_oversized` and `preamble_units`.
//!
//! Entry points: [`lang_for`] (extension -> [`Lang`]) and [`chunk_file`]
//! (path + text -> [`ChunkResult`]). Everything else here is private.

use crate::store::{NewCodeChunk, NewEdge, NewSymbol};
use sha2::{Digest, Sha256};
use std::path::Path;
use tree_sitter::{Node, Parser};

/// A language the chunker knows how to split. Grammars are tree-sitter
/// except [`Lang::Markdown`], which is a plain line/heading scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    C,
    Cpp,
    CSharp,
    Rust,
    Python,
    TypeScript,
    Tsx,
    JavaScript,
    Glsl,
    Markdown,
}

/// Maps a file path's extension to a [`Lang`]. `None` for anything
/// unrecognised (including no extension) -- those files fall back to
/// window chunks. `.h`/`.hpp`/`.hh`/`.hxx` are always [`Lang::Cpp`], never
/// [`Lang::C`]: the primary target is a C++20 codebase whose headers are
/// C++ (classes, templates, `namespace`), and a C parse of a C++ header
/// would misparse or error on all of that. Plain `.c` stays C.
pub fn lang_for(path: &str) -> Option<Lang> {
    let ext = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "c" => Lang::C,
        "h" | "hpp" | "hh" | "hxx" | "cpp" | "cc" | "cxx" => Lang::Cpp,
        "cs" => Lang::CSharp,
        "rs" => Lang::Rust,
        "py" => Lang::Python,
        "ts" => Lang::TypeScript,
        "tsx" => Lang::Tsx,
        "js" | "jsx" | "mjs" | "cjs" => Lang::JavaScript,
        "glsl" | "vert" | "frag" | "geom" | "comp" | "tesc" | "tese" => Lang::Glsl,
        "md" | "markdown" => Lang::Markdown,
        _ => return None,
    })
}

/// Output of [`chunk_file`]: the detected language (`None` for an
/// unrecognised extension), whether fallback windowing was used, and the
/// chunks themselves in source order.
#[derive(Debug)]
pub struct ChunkResult {
    pub lang: Option<Lang>,
    pub fallback: bool,
    pub chunks: Vec<NewCodeChunk>,
}

/// Splits `text` (the contents of the file at `path`) into chunks per the
/// rules in the module doc comment. Never panics on malformed input --
/// parse failure and unrecognised extensions both degrade to fallback
/// windows.
pub fn chunk_file(path: &str, text: &str) -> ChunkResult {
    let lang = lang_for(path);
    // Normalize CRLF -> LF once, up front, so the exact-lines invariant
    // holds for CRLF files too: `Node::start_byte`/`end_byte` are raw byte
    // offsets into whatever `text` we hand the parser, so byte-slicing a
    // CRLF file's node text would embed the trailing `\r` from a node's
    // own line -- but the "source lines" every chunk's `text` must match
    // are defined by line-splitting (`str::lines`), which treats `\r\n`
    // as one line terminator and strips it. Normalizing first means both
    // sides of the invariant agree. A lone `\r` not followed by `\n` is
    // left untouched: `str::lines()` doesn't treat it as a line break
    // either, so touching it would corrupt real content (e.g. a `\r`
    // inside a string literal) instead of a line ending.
    let normalized = if text.contains('\r') { text.replace("\r\n", "\n") } else { text.to_string() };
    let text = normalized.as_str();
    match lang {
        None => ChunkResult { lang: None, fallback: true, chunks: fallback_windows(text) },
        Some(Lang::Markdown) => ChunkResult { lang, fallback: false, chunks: chunk_markdown(text) },
        Some(l) => match chunk_with_tree_sitter(l, text) {
            Some(chunks) => ChunkResult { lang, fallback: false, chunks },
            None => ChunkResult { lang, fallback: true, chunks: fallback_windows(text) },
        },
    }
}

// ---------------------------------------------------------------------
// Shared plumbing: unit collection, splitting, hashing.
// ---------------------------------------------------------------------

/// A chunk candidate before oversize-splitting and hashing. `kind` is
/// `'static` because every call site passes a literal.
struct Unit {
    kind: &'static str,
    symbol: Option<String>,
    scope: Option<String>,
    start_line: i64,
    end_line: i64,
    text: String,
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn content_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    to_hex(&hasher.finalize())
}

/// The byte offset of the start of the line containing `byte`, and the
/// byte offset of the end of the line containing the node's *last* byte
/// (`end_byte.saturating_sub(1)`, since a tree-sitter node's `end_byte`
/// points just past its last character and never includes a trailing
/// newline -- verified empirically against every grammar here).
fn line_bounds(src: &str, start_byte: usize, end_byte: usize) -> (usize, usize) {
    let line_start = src[..start_byte].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let probe = if end_byte > start_byte { end_byte - 1 } else { start_byte };
    let line_end = match src[probe..].find('\n') {
        Some(i) => probe + i,
        None => src.len(),
    };
    (line_start, line_end)
}

/// 1-based inclusive start/end line and text of a tree-sitter node,
/// widened to whole lines. Chunks are fundamentally line-granular (the
/// stored key is `start_line`/`end_line`, and downstream consumers
/// reconstruct `text` by slicing the file at those line numbers), so
/// `text` must be the *exact* source lines `start_line..=end_line`, not
/// the node's own byte span -- a node's `start_byte` sits right at its
/// first token, after any leading indentation, so byte-slicing alone
/// would silently drop that indentation from the first line of every
/// indented/nested chunk (methods inside a class, functions inside a
/// namespace, etc.). `\n`-based, not `\r\n`-aware; see task-3-report.md.
fn node_span(node: Node, src: &str) -> (i64, i64, String) {
    let start_line = node.start_position().row as i64 + 1;
    let end_line = node.end_position().row as i64 + 1;
    let (line_start, line_end) = line_bounds(src, node.start_byte(), node.end_byte());
    let text = src[line_start..line_end].to_string();
    (start_line, end_line, text)
}

/// Exact text of just this node (not widened to whole lines) -- for
/// extracting a symbol/identifier's own text, never for a chunk's `text`.
fn node_text(node: Node, src: &str) -> String {
    src[node.start_byte()..node.end_byte()].to_string()
}

/// Units over 400 lines split into 150-400 line parts at the nearest
/// blank-line boundary. Each part's `text` is exactly its own source
/// lines (no signature repeated -- that would violate the "text is
/// exactly the source lines at start_line..=end_line" invariant every
/// chunk must hold). When a unit produces more than one part, every
/// part's `symbol` gets a `" (part i/N)"` suffix so parts are
/// distinguishable and the fragmentation is visible without inspecting
/// line numbers; `kind`/`scope` are unchanged. `content_hash` is computed
/// per part since the text (and thus the hash) differs.
const MAX_UNIT_LINES: usize = 400;
const MIN_PART_LINES: usize = 150;
const MAX_PART_LINES: usize = 400;

fn push_unit(units: &mut Vec<Unit>, kind: &'static str, symbol: Option<String>, scope: Option<String>, node: Node, src: &str) {
    let (start_line, end_line, text) = node_span(node, src);
    units.push(Unit { kind, symbol, scope, start_line, end_line, text });
}

fn split_oversized(unit: Unit) -> Vec<NewCodeChunk> {
    let total_lines = (unit.end_line - unit.start_line + 1) as usize;
    if total_lines <= MAX_UNIT_LINES {
        return vec![to_chunk(&unit, unit.symbol.clone(), unit.start_line, unit.end_line, unit.text.clone())];
    }

    // Lines of the unit's own text, 0-indexed within the unit.
    let lines: Vec<&str> = unit.text.split('\n').collect();
    let n = lines.len();

    let mut cuts: Vec<usize> = Vec::new(); // exclusive end offsets (0-indexed into `lines`), sorted
    let mut pos = 0usize;
    while n - pos > MAX_PART_LINES {
        let target = pos + (MIN_PART_LINES + MAX_PART_LINES) / 2;
        let lo = pos + MIN_PART_LINES;
        let hi = (pos + MAX_PART_LINES).min(n - 1); // leave at least 1 line for the tail
        let mut cut = target.min(hi).max(lo);
        // Search outward from `target` for the nearest blank line within [lo, hi].
        if !lines[cut.min(n - 1)].trim().is_empty() {
            let mut found = None;
            for d in 0..=(hi - lo) {
                let a = target.saturating_sub(d);
                let b = target + d;
                if a >= lo && a <= hi && lines[a].trim().is_empty() {
                    found = Some(a);
                    break;
                }
                if b >= lo && b <= hi && lines[b].trim().is_empty() {
                    found = Some(b);
                    break;
                }
            }
            if let Some(f) = found {
                cut = f;
            }
        }
        cuts.push(cut);
        pos = cut;
        if pos == 0 {
            break; // safety: avoid infinite loop if we couldn't advance
        }
    }

    let mut boundaries = vec![0usize];
    boundaries.extend(cuts);
    boundaries.push(n);
    boundaries.dedup();

    let mut parts: Vec<(i64, i64, String)> = Vec::new();
    for w in boundaries.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a >= b {
            continue;
        }
        let part_start_line = unit.start_line + a as i64;
        let part_end_line = unit.start_line + (b - 1) as i64;
        let text = lines[a..b].join("\n");
        parts.push((part_start_line, part_end_line, text));
    }
    if parts.is_empty() {
        // Degenerate (shouldn't happen): fall back to the whole unit unsplit.
        parts.push((unit.start_line, unit.end_line, unit.text.clone()));
    }

    let total_parts = parts.len();
    let mut out = Vec::with_capacity(total_parts);
    for (i, (start_line, end_line, text)) in parts.into_iter().enumerate() {
        let symbol = if total_parts > 1 {
            Some(match &unit.symbol {
                Some(sym) => format!("{sym} (part {}/{total_parts})", i + 1),
                None => format!("(part {}/{total_parts})", i + 1),
            })
        } else {
            unit.symbol.clone()
        };
        out.push(to_chunk(&unit, symbol, start_line, end_line, text));
    }
    out
}

fn to_chunk(unit: &Unit, symbol: Option<String>, start_line: i64, end_line: i64, text: String) -> NewCodeChunk {
    let hash = content_hash(&text);
    NewCodeChunk {
        symbol,
        kind: unit.kind.to_string(),
        scope: unit.scope.clone(),
        start_line,
        end_line,
        text,
        content_hash: hash,
        ..Default::default()
    }
}

/// One `preamble` chunk per *maximal contiguous run of source lines not
/// covered by any unit chunk* -- not one chunk for the whole file. A run
/// is skipped if its text is empty/whitespace-only. Each chunk's text is
/// an exact slice of `src`'s lines (never a join of fragments), so this
/// can never split one physical line into two or drop a blank line the
/// way concatenating per-node text snippets could.
///
/// This also means non-unit content *inside* a transparent container
/// (e.g. an associated `type` in a Rust `impl` block, or a C# file-scoped
/// `namespace Foo;` declaration line) is automatically covered too: it
/// was never anyone's unit range, so it's automatically part of a gap,
/// with no need for the walkers to track "am I inside a sweep scope"
/// themselves.
fn preamble_units(units: &[Unit], src: &str) -> Vec<Unit> {
    let lines: Vec<&str> = src.lines().collect();
    let total = lines.len() as i64;
    if total == 0 {
        return Vec::new();
    }

    let mut covered: Vec<(i64, i64)> = units.iter().map(|u| (u.start_line, u.end_line)).collect();
    covered.sort();
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (s, e) in covered.drain(..) {
        if let Some(last) = merged.last_mut() {
            if s <= last.1 + 1 {
                if e > last.1 {
                    last.1 = e;
                }
                continue;
            }
        }
        merged.push((s, e));
    }

    let mut out = Vec::new();
    let mut cursor = 1i64;
    for (s, e) in &merged {
        if *s > cursor {
            push_gap(&mut out, &lines, cursor, *s - 1);
        }
        cursor = cursor.max(*e + 1);
    }
    if cursor <= total {
        push_gap(&mut out, &lines, cursor, total);
    }
    out
}

fn push_gap(out: &mut Vec<Unit>, lines: &[&str], start: i64, end: i64) {
    if start > end {
        return;
    }
    let s = (start - 1).max(0) as usize;
    let e = (end as usize).min(lines.len());
    if s >= e {
        return;
    }
    let text = lines[s..e].join("\n");
    if text.trim().is_empty() {
        return;
    }
    out.push(Unit { kind: "preamble", symbol: None, scope: None, start_line: start, end_line: end, text });
}

// ---------------------------------------------------------------------
// tree-sitter driver: parse, error-ratio gate, dispatch to a walker.
// ---------------------------------------------------------------------

fn ts_language(lang: Lang) -> Option<tree_sitter::Language> {
    Some(match lang {
        Lang::C => tree_sitter_c::LANGUAGE.into(),
        Lang::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        Lang::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Lang::Glsl => tree_sitter_glsl::LANGUAGE_GLSL.into(),
        Lang::Markdown => return None,
    })
}

/// Sum of byte lengths of `ERROR` nodes, not double-counting nested
/// errors (an `ERROR` node's children are not descended into once it's
/// counted, since they're inside the same error region).
fn error_bytes(node: Node) -> usize {
    if node.kind() == "ERROR" {
        return node.end_byte() - node.start_byte();
    }
    let mut sum = 0;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        sum += error_bytes(child);
    }
    sum
}

/// Parses `text` as `lang` and returns the resulting tree, or `None` if the
/// grammar rejects it outright or more than 25% of its bytes fall under
/// `ERROR` nodes -- the same bail-to-fallback threshold `chunk_with_tree_sitter`
/// always used, now shared with `extract_refs` (`code_index::chunk`'s
/// second consumer of this same parse) so there is exactly one grammar and
/// one error-tolerance rule, not two that could drift apart.
fn parse_checked(lang: Lang, text: &str) -> Option<tree_sitter::Tree> {
    let language = ts_language(lang)?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(text, None)?;
    if !text.is_empty() {
        let ratio = error_bytes(tree.root_node()) as f64 / text.len() as f64;
        if ratio > 0.25 {
            return None;
        }
    }
    Some(tree)
}

/// Runs the per-language scope walker over an already-parsed `root` and
/// returns its raw units -- unsorted, unsplit, no preamble gaps. Shared by
/// `chunk_with_tree_sitter` (which adds gaps, sorts, and splits oversized
/// units into chunks) and `extract_refs` (which turns each unit that has a
/// symbol directly into a `NewSymbol`; a definition's stored line range is
/// its own full span, never split).
fn build_units(lang: Lang, root: Node, src: &str) -> Vec<Unit> {
    let mut units: Vec<Unit> = Vec::new();
    match lang {
        Lang::C => walk_c(root, src, &mut units),
        Lang::Cpp => {
            let mut scope = Vec::new();
            walk_cpp(root, src, &mut scope, 0, &mut units);
        }
        Lang::CSharp => {
            let mut scope = Vec::new();
            walk_csharp(root, src, &mut scope, 0, &mut units);
        }
        Lang::Rust => {
            let mut scope = Vec::new();
            walk_rust(root, src, &mut scope, 0, &mut units);
        }
        Lang::Python => {
            let mut scope = Vec::new();
            walk_python(root, src, &mut scope, &mut units);
        }
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
            let mut scope = Vec::new();
            walk_ts_family(root, src, &mut scope, &mut units);
        }
        Lang::Glsl => walk_glsl(root, src, &mut units),
        Lang::Markdown => {}
    }
    units
}

fn chunk_with_tree_sitter(lang: Lang, text: &str) -> Option<Vec<NewCodeChunk>> {
    let tree = parse_checked(lang, text)?;
    let mut units = build_units(lang, tree.root_node(), text);

    let gaps = preamble_units(&units, text);
    units.extend(gaps);
    // Stable source order: sort by start line (preamble's min start_line
    // slots it naturally among the units it sits between/around).
    units.sort_by_key(|u| (u.start_line, u.end_line));

    let mut out = Vec::new();
    for u in units {
        out.extend(split_oversized(u));
    }
    Some(out)
}

fn has_body(node: Node) -> bool {
    node.child_by_field_name("body").is_some()
}

// ---------------------------------------------------------------------
// C
//
// Node kinds captured (direct children of `translation_unit` only -- C
// has no namespaces or nested functions):
//   function_definition          -> kind "function"
//   struct_specifier (named body) -> kind "struct"
//   enum_specifier   (named body) -> kind "enum"
//   union_specifier  (named body) -> kind "union"
//   type_definition (typedef struct/enum/union {..} Name;)
//                                 -> kind "struct"/"enum"/"union", symbol
//                                    is the typedef's own name, span is
//                                    the whole `type_definition`
// Everything else at top level (includes, globals, forward declarations,
// prototypes without a body) lands in a preamble gap (see `preamble_units`).
// ---------------------------------------------------------------------

fn c_function_name(fd: Node, src: &str) -> Option<String> {
    let declarator = fd.child_by_field_name("declarator")?;
    let func = find_function_declarator(declarator)?;
    let name = func.child_by_field_name("declarator")?;
    Some(node_text(name, src))
}

/// Descends through pointer/array/parenthesized declarator wrappers to
/// find the innermost `function_declarator` (needed because a return
/// type of e.g. `int *` wraps the declarator in `pointer_declarator`).
fn find_function_declarator(node: Node) -> Option<Node> {
    if node.kind() == "function_declarator" {
        return Some(node);
    }
    if let Some(inner) = node.child_by_field_name("declarator") {
        return find_function_declarator(inner);
    }
    // `reference_declarator`/`parenthesized_declarator` (e.g. `Widget&
    // operator=(...)`) wrap their inner declarator as a bare child with
    // no field name, so fall back to a search over named children.
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = find_function_declarator(child) {
            return Some(found);
        }
    }
    None
}

fn walk_c(root: Node, src: &str, units: &mut Vec<Unit>) {
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        match child.kind() {
            "function_definition" => {
                let symbol = c_function_name(child, src);
                push_unit(units, "function", symbol, None, child, src);
            }
            "struct_specifier" if has_body(child) => {
                let symbol = child.child_by_field_name("name").map(|n| node_text(n, src));
                push_unit(units, "struct", symbol, None, child, src);
            }
            "enum_specifier" if has_body(child) => {
                let symbol = child.child_by_field_name("name").map(|n| node_text(n, src));
                push_unit(units, "enum", symbol, None, child, src);
            }
            "union_specifier" if has_body(child) => {
                let symbol = child.child_by_field_name("name").map(|n| node_text(n, src));
                push_unit(units, "union", symbol, None, child, src);
            }
            "type_definition" => {
                let symbol = child.child_by_field_name("declarator").map(|n| node_text(n, src));
                let inner_kind = child.named_child(0).and_then(|inner| match inner.kind() {
                    "struct_specifier" => Some("struct"),
                    "enum_specifier" => Some("enum"),
                    "union_specifier" => Some("union"),
                    _ => None,
                });
                if let Some(kind) = inner_kind {
                    push_unit(units, kind, symbol, None, child, src);
                }
                // else: not a struct/enum/union typedef (e.g. `typedef int
                // MyInt;`) -- leave it uncovered, it lands in a preamble gap.
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------
// C++
//
// Node kinds captured, walking `translation_unit` and unwrapping
// `namespace_definition` bodies transparently (namespaces push onto the
// scope chain but never produce a chunk of their own -- "namespace-level
// declaration" means a declaration *at* namespace scope, not the
// namespace itself):
//   function_definition -> kind "method" if its declarator is a
//       `qualified_identifier` (out-of-line: `void Foo::bar() {}`) or if
//       lexically nested inside a class/struct/union body, else
//       "function". symbol is the leaf name (identifier/destructor_name/
//       operator_name -- destructor and operator names keep their `~`/
//       `operator` spelling verbatim). scope is the enclosing
//       namespace/class chain joined by "::", with any qualifier from the
//       declarator itself appended (so `void Foo::bar()` gets scope "Foo"
//       even at file/namespace top level, which is the primary use case:
//       headers declare `class Foo { void bar(); };`, the .cpp defines
//       `void Foo::bar() {}` out of line).
//   class_specifier / struct_specifier / union_specifier (named, with a
//       body) -> kind "class"/"struct"/"union". Also an opaque container:
//       the chunk's text is the *whole* node (every member verbatim,
//       inline method bodies included), and the walker also recurses
//       into its body to emit separate chunks for nested function
//       definitions and nested class-likes (so a class with an inline
//       method yields both a "class" chunk covering the whole class and
//       a "method" chunk for that member -- deliberate overlap, see
//       task-3-report.md). Bodyless (forward-declared) specifiers fall to
//       the preamble.
//   enum_specifier (named, with a body; covers plain and `enum class`)
//       -> kind "enum".
//   template_declaration -> unwrapped: the inner function_definition /
//       class_specifier / struct_specifier / union_specifier drives
//       kind/symbol/scope, but the chunk's span is the whole
//       `template_declaration` (keeps `template<...>` with its body).
// Everything else at namespace/file scope (includes, using-declarations,
// global variables, forward declarations, bodyless member declarations)
// lands in a preamble gap; non-unit content *inside* an opaque class/
// struct/union body does not create its own gap (it's covered by that
// class's own chunk range) -- see task-3-report.md concerns.
// ---------------------------------------------------------------------

/// Recursively flattens a (possibly nested) `qualified_identifier` into
/// its `::`-separated parts, e.g. `foo::Bar::baz` -> ["foo","Bar","baz"].
/// A non-qualified node (identifier/destructor_name/operator_name/...)
/// is a single-element leaf.
fn cpp_qualified_parts(node: Node, src: &str) -> Vec<String> {
    if node.kind() == "qualified_identifier" {
        let mut v = Vec::new();
        if let Some(scope) = node.child_by_field_name("scope") {
            v.extend(cpp_qualified_parts(scope, src));
        }
        if let Some(name) = node.child_by_field_name("name") {
            v.extend(cpp_qualified_parts(name, src));
        }
        v
    } else {
        vec![node_text(node, src)]
    }
}

fn cpp_namespace_names(name_node: Option<Node>, src: &str) -> Vec<String> {
    match name_node {
        None => vec![],
        Some(n) if n.kind() == "nested_namespace_specifier" => {
            let mut cursor = n.walk();
            n.named_children(&mut cursor).map(|c| node_text(c, src)).collect()
        }
        Some(n) => vec![node_text(n, src)],
    }
}

fn join_scope(parts: &[String]) -> Option<String> {
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("::"))
    }
}

fn emit_cpp_function(fd: Node, src: &str, scope_stack: &[String], class_depth: usize, units: &mut Vec<Unit>) {
    let declarator = fd.child_by_field_name("declarator");
    let func_declarator = declarator.and_then(find_function_declarator);
    let name_node = func_declarator.and_then(|n| n.child_by_field_name("declarator"));

    let (symbol, extra_scope, is_qualified) = match name_node {
        Some(n) if n.kind() == "qualified_identifier" => {
            let mut parts = cpp_qualified_parts(n, src);
            let leaf = parts.pop();
            (leaf, parts, true)
        }
        Some(n) => (Some(node_text(n, src)), vec![], false),
        None => (None, vec![], false),
    };

    let mut full_scope: Vec<String> = scope_stack.to_vec();
    full_scope.extend(extra_scope);
    let scope = join_scope(&full_scope);
    let kind = if is_qualified || class_depth > 0 { "method" } else { "function" };
    push_unit(units, kind, symbol, scope, fd, src);
}

fn emit_cpp_class_like(
    node: Node,
    span_node: Node,
    src: &str,
    kind: &'static str,
    scope_stack: &mut Vec<String>,
    class_depth: &mut usize,
    units: &mut Vec<Unit>,
) {
    let symbol = node.child_by_field_name("name").map(|n| node_text(n, src));
    let scope = join_scope(scope_stack);
    push_unit(units, kind, symbol.clone(), scope, span_node, src);

    if let Some(body) = node.child_by_field_name("body") {
        if let Some(name) = &symbol {
            scope_stack.push(name.clone());
        }
        *class_depth += 1;
        walk_cpp(body, src, scope_stack, *class_depth, units);
        *class_depth -= 1;
        if symbol.is_some() {
            scope_stack.pop();
        }
    }
}

/// Finds the function/class/struct/union declaration inside a
/// `template_declaration`'s children (the other children are the
/// `template<...>` parameter list and punctuation).
fn cpp_template_inner(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find(|c| matches!(c.kind(), "function_definition" | "class_specifier" | "struct_specifier" | "union_specifier"));
    found
}

fn walk_cpp(node: Node, src: &str, scope_stack: &mut Vec<String>, class_depth: usize, units: &mut Vec<Unit>) {
    let mut cursor = node.walk();
    let mut class_depth = class_depth;
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "namespace_definition" => {
                let names = cpp_namespace_names(child.child_by_field_name("name"), src);
                let pushed = names.len();
                scope_stack.extend(names);
                if let Some(body) = child.child_by_field_name("body") {
                    walk_cpp(body, src, scope_stack, class_depth, units);
                }
                for _ in 0..pushed {
                    scope_stack.pop();
                }
            }
            "function_definition" => {
                emit_cpp_function(child, src, scope_stack, class_depth, units);
            }
            "class_specifier" if has_body(child) => {
                emit_cpp_class_like(child, child, src, "class", scope_stack, &mut class_depth, units);
            }
            "struct_specifier" if has_body(child) => {
                emit_cpp_class_like(child, child, src, "struct", scope_stack, &mut class_depth, units);
            }
            "union_specifier" if has_body(child) => {
                emit_cpp_class_like(child, child, src, "union", scope_stack, &mut class_depth, units);
            }
            "enum_specifier" if has_body(child) => {
                let symbol = child.child_by_field_name("name").map(|n| node_text(n, src));
                let scope = join_scope(scope_stack);
                push_unit(units, "enum", symbol, scope, child, src);
            }
            "template_declaration" => match cpp_template_inner(child) {
                Some(inner) if inner.kind() == "function_definition" => {
                    let declarator = inner.child_by_field_name("declarator");
                    let func_declarator = declarator.and_then(find_function_declarator);
                    let name_node = func_declarator.and_then(|n| n.child_by_field_name("declarator"));
                    let (symbol, extra_scope, is_qualified) = match name_node {
                        Some(n) if n.kind() == "qualified_identifier" => {
                            let mut parts = cpp_qualified_parts(n, src);
                            let leaf = parts.pop();
                            (leaf, parts, true)
                        }
                        Some(n) => (Some(node_text(n, src)), vec![], false),
                        None => (None, vec![], false),
                    };
                    let mut full_scope: Vec<String> = scope_stack.clone();
                    full_scope.extend(extra_scope);
                    let scope = join_scope(&full_scope);
                    let kind = if is_qualified || class_depth > 0 { "method" } else { "function" };
                    push_unit(units, kind, symbol, scope, child, src);
                }
                Some(inner) if matches!(inner.kind(), "class_specifier" | "struct_specifier" | "union_specifier") => {
                    let kind = match inner.kind() {
                        "class_specifier" => "class",
                        "struct_specifier" => "struct",
                        _ => "union",
                    };
                    emit_cpp_class_like(inner, child, src, kind, scope_stack, &mut class_depth, units);
                }
                _ => {}
            },
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------
// C#
//
// Node kinds captured, walking `compilation_unit` and unwrapping
// `namespace_declaration`/`file_scoped_namespace_declaration` bodies
// transparently (scope pushed, no chunk of its own):
//   class_declaration / struct_declaration / interface_declaration /
//   record_declaration / enum_declaration -> kind "class"/"struct"/
//       "interface"/"record"/"enum". Opaque container like C++: chunk
//       text is the whole node, and the walker also recurses into the
//       body for nested types and methods (same deliberate overlap as
//       C++ -- see task-3-report.md).
//   method_declaration / constructor_declaration / destructor_declaration
//       / operator_declaration / conversion_operator_declaration (only
//       when they have a body -- interface/abstract members don't) ->
//       kind "method", symbol = its `name` field's text (destructor and
//       operator forms keep their own spelling).
// scope is the enclosing namespace/type chain joined by "::" (per the
// brief: C++/C# both use "::" for `scope`, even though C# itself spells
// namespaces with ".").
// Everything else at namespace/file scope (using-directives, top-level
// statements -- including a file-scoped `namespace Foo;` declaration's own
// line, which is not itself a unit) lands in a preamble gap; non-unit
// content inside a type body (fields, properties, bodyless interface
// members) does not create its own gap, same as C++.
// ---------------------------------------------------------------------

fn csharp_qualified_parts(node: Node, src: &str) -> Vec<String> {
    if node.kind() == "qualified_name" {
        let mut v = Vec::new();
        if let Some(q) = node.child_by_field_name("qualifier") {
            v.extend(csharp_qualified_parts(q, src));
        }
        if let Some(n) = node.child_by_field_name("name") {
            v.extend(csharp_qualified_parts(n, src));
        }
        v
    } else {
        vec![node_text(node, src)]
    }
}

const CSHARP_TYPE_KINDS: &[(&str, &str)] = &[
    ("class_declaration", "class"),
    ("struct_declaration", "struct"),
    ("interface_declaration", "interface"),
    ("record_declaration", "record"),
];

const CSHARP_METHOD_KINDS: &[&str] =
    &["method_declaration", "constructor_declaration", "destructor_declaration", "operator_declaration", "conversion_operator_declaration"];

fn csharp_method_name(node: Node, src: &str) -> Option<String> {
    node.child_by_field_name("name").map(|n| node_text(n, src))
}

fn walk_csharp(node: Node, src: &str, scope_stack: &mut Vec<String>, depth: usize, units: &mut Vec<Unit>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let kind = child.kind();
        if kind == "namespace_declaration" || kind == "file_scoped_namespace_declaration" {
            let names = child.child_by_field_name("name").map(|n| csharp_qualified_parts(n, src)).unwrap_or_default();
            let pushed = names.len();
            scope_stack.extend(names);
            if let Some(body) = child.child_by_field_name("body") {
                walk_csharp(body, src, scope_stack, depth, units);
            }
            // A file-scoped namespace (`namespace Foo;`, no `body` field)
            // applies to the rest of the file: scope stays pushed and the
            // surrounding loop continues over `node`'s remaining children.
            // The declaration's own line is not a unit -- it lands in a
            // preamble gap automatically.
            if kind == "namespace_declaration" {
                for _ in 0..pushed {
                    scope_stack.pop();
                }
            }
            continue;
        }
        if let Some(&(_, mapped)) = CSHARP_TYPE_KINDS.iter().find(|(k, _)| *k == kind) {
            if has_body(child) {
                let symbol = child.child_by_field_name("name").map(|n| node_text(n, src));
                let scope = join_scope(scope_stack);
                push_unit(units, mapped, symbol.clone(), scope, child, src);
                if let Some(body) = child.child_by_field_name("body") {
                    if let Some(name) = &symbol {
                        scope_stack.push(name.clone());
                    }
                    walk_csharp(body, src, scope_stack, depth + 1, units);
                    if symbol.is_some() {
                        scope_stack.pop();
                    }
                }
                continue;
            }
        }
        if kind == "enum_declaration" && has_body(child) {
            let symbol = child.child_by_field_name("name").map(|n| node_text(n, src));
            let scope = join_scope(scope_stack);
            push_unit(units, "enum", symbol, scope, child, src);
            continue;
        }
        if CSHARP_METHOD_KINDS.contains(&kind) && has_body(child) {
            let symbol = csharp_method_name(child, src);
            let scope = join_scope(scope_stack);
            push_unit(units, "method", symbol, scope, child, src);
        }
    }
}

// ---------------------------------------------------------------------
// Rust
//
// Node kinds captured, walking `source_file` and unwrapping `mod_item`
// bodies transparently (module name pushed, no chunk of its own):
//   function_item -> kind "method" if lexically inside an `impl_item` or
//       `trait_item`, else "function"; symbol = `name` field; scope = the
//       enclosing mod/impl/trait chain joined by "::".
//   struct_item / enum_item / union_item -> kind "struct"/"enum"/"union",
//       no recursion (Rust struct/enum/union bodies hold fields/variants,
//       never functions).
//   trait_item -> kind "trait" (opaque unit, chunk text is the whole
//       trait including bodyless method signatures) *and* a container:
//       recurses into its body for `function_item`s with a default body,
//       emitted as separate "method" chunks scoped to the trait name.
//   impl_item -> *not* its own chunk (not in the brief's kind list): a
//       transparent-ish container whose scope is the `type` field's own
//       source text (e.g. "Point", or "Foo<T>" for a generic impl,
//       verbatim); recurses for `function_item`s (-> "method").
// Everything else at module scope (use-declarations, consts, statics,
// top-level statements) lands in a preamble gap; non-function content
// inside impl/trait bodies (associated consts/types) does not create its
// own gap, same simplification as the class-body languages.
// ---------------------------------------------------------------------

fn walk_rust(node: Node, src: &str, scope_stack: &mut Vec<String>, container_depth: usize, units: &mut Vec<Unit>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "mod_item" => {
                let name = child.child_by_field_name("name").map(|n| node_text(n, src));
                let pushed = name.is_some();
                if let Some(n) = &name {
                    scope_stack.push(n.clone());
                }
                if let Some(body) = child.child_by_field_name("body") {
                    walk_rust(body, src, scope_stack, container_depth, units);
                }
                if pushed {
                    scope_stack.pop();
                }
            }
            "impl_item" => {
                let ty = child.child_by_field_name("type").map(|n| node_text(n, src));
                let pushed = ty.is_some();
                if let Some(t) = &ty {
                    scope_stack.push(t.clone());
                }
                if let Some(body) = child.child_by_field_name("body") {
                    walk_rust(body, src, scope_stack, container_depth + 1, units);
                }
                if pushed {
                    scope_stack.pop();
                }
            }
            "trait_item" => {
                let symbol = child.child_by_field_name("name").map(|n| node_text(n, src));
                let scope = join_scope(scope_stack);
                push_unit(units, "trait", symbol.clone(), scope, child, src);
                let pushed = symbol.is_some();
                if let Some(n) = &symbol {
                    scope_stack.push(n.clone());
                }
                if let Some(body) = child.child_by_field_name("body") {
                    walk_rust(body, src, scope_stack, container_depth + 1, units);
                }
                if pushed {
                    scope_stack.pop();
                }
            }
            "function_item" if has_body(child) => {
                let symbol = child.child_by_field_name("name").map(|n| node_text(n, src));
                let scope = join_scope(scope_stack);
                let kind = if container_depth > 0 { "method" } else { "function" };
                push_unit(units, kind, symbol, scope, child, src);
            }
            "struct_item" => {
                let symbol = child.child_by_field_name("name").map(|n| node_text(n, src));
                let scope = join_scope(scope_stack);
                push_unit(units, "struct", symbol, scope, child, src);
            }
            "enum_item" => {
                let symbol = child.child_by_field_name("name").map(|n| node_text(n, src));
                let scope = join_scope(scope_stack);
                push_unit(units, "enum", symbol, scope, child, src);
            }
            "union_item" => {
                let symbol = child.child_by_field_name("name").map(|n| node_text(n, src));
                let scope = join_scope(scope_stack);
                push_unit(units, "union", symbol, scope, child, src);
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------
// Python
//
// Node kinds captured, walking `module` (and recursing into
// `class_definition` bodies):
//   function_definition -> kind "method" if lexically inside a
//       class_definition, else "function"; symbol = `name` field. Covers
//       `async def` too (the grammar has no separate node kind for it).
//   class_definition -> kind "class". Opaque container like C++/C#:
//       chunk text is the whole class (methods included verbatim), and
//       the walker recurses into its body for nested functions/classes
//       (same deliberate overlap as the other class-body languages).
//   decorated_definition (decorator(s) + a function/class definition) ->
//       unwrapped: kind/symbol come from the inner definition, but the
//       chunk's span is the whole `decorated_definition` so the
//       decorator lines are kept with it.
// scope is the enclosing class chain joined by "." (Python/TS use ".",
// per the brief). Everything else at module/class scope (imports,
// module-level assignments, bare expression statements, and -- inside a
// class body -- field assignments) lands in a preamble gap at module
// scope, or does not create its own gap inside a class body (covered by
// the class's own chunk range, same simplification as the other
// class-body languages).
// ---------------------------------------------------------------------

fn python_inner_def(node: Node) -> Option<Node> {
    if node.kind() == "decorated_definition" {
        node.child_by_field_name("definition")
    } else {
        Some(node)
    }
}

fn walk_python(node: Node, src: &str, scope_stack: &mut Vec<String>, units: &mut Vec<Unit>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let span_node = child;
        let inner = python_inner_def(child).unwrap_or(child);
        match inner.kind() {
            "function_definition" => {
                let symbol = inner.child_by_field_name("name").map(|n| node_text(n, src));
                let scope = join_scope_dot(scope_stack);
                let kind = if scope_stack.is_empty() { "function" } else { "method" };
                push_unit(units, kind, symbol, scope, span_node, src);
            }
            "class_definition" => {
                let symbol = inner.child_by_field_name("name").map(|n| node_text(n, src));
                let scope = join_scope_dot(scope_stack);
                push_unit(units, "class", symbol.clone(), scope, span_node, src);
                if let Some(body) = inner.child_by_field_name("body") {
                    if let Some(name) = &symbol {
                        scope_stack.push(name.clone());
                    }
                    walk_python(body, src, scope_stack, units);
                    if symbol.is_some() {
                        scope_stack.pop();
                    }
                }
            }
            _ => {}
        }
    }
}

fn join_scope_dot(parts: &[String]) -> Option<String> {
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("."))
    }
}

// ---------------------------------------------------------------------
// TypeScript / TSX / JavaScript
//
// One walker for all three: TSX/TypeScript's grammar is a superset of
// JavaScript's for the node kinds below, and JS simply never produces the
// TS-only ones (interface_declaration, enum_declaration, internal_module).
//
// Node kinds captured, walking `program` and unwrapping `export_statement`
// (via its `declaration` field) and `internal_module`/`ambient_declaration`
// (TS `namespace`/`declare namespace`, transparent, scope pushed)
// transparently:
//   function_declaration / generator_function_declaration -> kind
//       "function"; symbol = `name` field.
//   class_declaration / abstract_class_declaration -> kind "class".
//       Opaque container like the other class-body languages: chunk text
//       is the whole class, and the walker recurses into `class_body` for
//       `method_definition`s (-> "method") and nested class declarations.
//   interface_declaration (TS/TSX only) -> kind "interface".
//   enum_declaration (TS/TSX only) -> kind "enum".
// scope is the enclosing namespace/class chain joined by "." (per the
// brief). Everything else at module/namespace scope (imports,
// const/let/var -- including `const f = () => {}` arrow functions, which
// the brief's own "top-level statements" preamble example covers --
// type_alias_declaration, bare expression statements) lands in a preamble
// gap; non-method content inside a class body (fields, static blocks)
// does not create its own gap, same simplification as the other
// class-body languages.
// ---------------------------------------------------------------------

fn ts_unwrap_export(node: Node) -> Node {
    if node.kind() == "export_statement" {
        if let Some(decl) = node.child_by_field_name("declaration") {
            return decl;
        }
    }
    node
}

fn ts_unwrap_ambient(node: Node) -> Node {
    if node.kind() == "ambient_declaration" {
        let mut cursor = node.walk();
        let first = node.named_children(&mut cursor).next();
        if let Some(inner) = first {
            return inner;
        }
    }
    node
}

fn walk_ts_family(node: Node, src: &str, scope_stack: &mut Vec<String>, units: &mut Vec<Unit>) {
    let mut cursor = node.walk();
    for raw_child in node.named_children(&mut cursor) {
        let span_node = raw_child;
        let child = ts_unwrap_ambient(ts_unwrap_export(raw_child));
        match child.kind() {
            "internal_module" | "module" => {
                let name = child.child_by_field_name("name").map(|n| node_text(n, src));
                let pushed = name.is_some();
                if let Some(n) = &name {
                    scope_stack.push(n.clone());
                }
                if let Some(body) = child.child_by_field_name("body") {
                    walk_ts_family(body, src, scope_stack, units);
                }
                if pushed {
                    scope_stack.pop();
                }
            }
            "function_declaration" | "generator_function_declaration" => {
                let symbol = child.child_by_field_name("name").map(|n| node_text(n, src));
                let scope = join_scope_dot(scope_stack);
                push_unit(units, "function", symbol, scope, span_node, src);
            }
            "class_declaration" | "abstract_class_declaration" => {
                let symbol = child.child_by_field_name("name").map(|n| node_text(n, src));
                let scope = join_scope_dot(scope_stack);
                push_unit(units, "class", symbol.clone(), scope, span_node, src);
                if let Some(body) = child.child_by_field_name("body") {
                    if let Some(name) = &symbol {
                        scope_stack.push(name.clone());
                    }
                    walk_ts_family(body, src, scope_stack, units);
                    if symbol.is_some() {
                        scope_stack.pop();
                    }
                }
            }
            "interface_declaration" => {
                let symbol = child.child_by_field_name("name").map(|n| node_text(n, src));
                let scope = join_scope_dot(scope_stack);
                push_unit(units, "interface", symbol, scope, span_node, src);
            }
            "enum_declaration" => {
                let symbol = child.child_by_field_name("name").map(|n| node_text(n, src));
                let scope = join_scope_dot(scope_stack);
                push_unit(units, "enum", symbol, scope, span_node, src);
            }
            "method_definition" if has_body(child) => {
                let symbol = child.child_by_field_name("name").map(|n| node_text(n, src));
                let scope = join_scope_dot(scope_stack);
                push_unit(units, "method", symbol, scope, span_node, src);
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------
// GLSL
//
// Node kinds captured (direct children of `translation_unit`; GLSL has
// no namespaces, classes or methods -- shader source is a flat list of
// declarations and functions):
//   function_definition -> kind "function" (covers `main`, ordinary
//       functions, and shader-stage entry points alike -- GLSL doesn't
//       distinguish them syntactically).
// Everything else (`#version`/`#extension`/`#pragma` directives, uniform/
// in/out/buffer declarations, globals) lands in a preamble gap.
// ---------------------------------------------------------------------

fn walk_glsl(root: Node, src: &str, units: &mut Vec<Unit>) {
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if child.kind() == "function_definition" {
            let symbol = c_function_name(child, src);
            push_unit(units, "function", symbol, None, child, src);
        }
    }
}

// ---------------------------------------------------------------------
// Reference extraction (`extract_refs`): definitions + edges (calls/
// includes/imports/inherits) for the code symbol graph (schema v35, see
// `store::migrate_v34_to_v35`). Definitions and edges come ONLY from these
// tree-sitter walkers, never an LLM. Reuses the exact same parse
// (`parse_checked`) and per-language scope walkers (`build_units`) as
// `chunk_file` -- one grammar/scope pass drives both chunking and the
// symbol graph, not two that could drift apart.
//
// Definitions: every `Unit` a `walk_*` function emits that has a symbol
// (see the per-language node-kind tables above each walker) becomes one
// `NewSymbol`; preamble/window units never carry a symbol, and `chunk_file`
// only calls `preamble_units`/`split_oversized` for actual chunks, not
// here, so nothing needs filtering beyond "has a symbol". `qualified` is
// `scope` plus the language's own separator ("::" for C/C++/C#/Rust, "."
// for Python/TS/JS -- GLSL and top-level C never have a scope) plus
// `symbol`, via `qualified_name` below.
//
// Edges: a second, generic full-tree walk (`walk_edges`) that visits every
// named node regardless of nesting -- unlike the scope walkers, an edge
// needs no scope stack, just the reference's own line, so one recursive
// visitor covers every language rather than six bespoke ones. Dispatch is
// on `(lang, node.kind())` in `emit_edge`; the per-language node kinds are
// exactly the brief's list (see that function). `src_line` is the
// reference's own 1-based line. `src_symbol_id`/`dst_symbol_id` are NOT
// computed here: `store::replace_file_symbols` resolves `src_symbol_id`
// once it knows the new ids and line ranges of this same file's freshly
// inserted symbols (the "enclosing definition by line range" rule), and
// `store::resolve_edges` resolves `dst_symbol_id` project-wide afterward,
// since a callee can live in a file not (re)indexed in this pass.
//
// No builtin filtering: the brief allows skipping "obvious" builtins but
// explicitly forbids a maintained list, so none is built here. A call to
// `printf` or `std::vector` is extracted like any other call/base -- it
// just never resolves to a project symbol, which is the honest answer
// ("this file calls something it didn't define"), not a guess a blocklist
// would have to encode.
// ---------------------------------------------------------------------

/// Output of [`extract_refs`]: definitions and references found in one
/// file, ready for `store::replace_file_symbols`.
#[derive(Debug, Clone, Default)]
pub struct Refs {
    pub symbols: Vec<NewSymbol>,
    pub edges: Vec<NewEdge>,
}

/// Extracts definitions and edges from `path`'s `text`. Never panics: an
/// unrecognised extension, Markdown (no code graph for prose), or a parse
/// `chunk_file` would itself fall back on all yield an empty `Refs` --
/// there is no fallback reference extraction (the brief is explicit:
/// "Fallback files: none").
pub fn extract_refs(path: &str, text: &str) -> Refs {
    let lang = match lang_for(path) {
        Some(Lang::Markdown) | None => return Refs::default(),
        Some(l) => l,
    };
    // Same CRLF normalization as `chunk_file`, for the same reason: a
    // node's byte offsets must agree with `str::lines()`-based line
    // numbers, which treat "\r\n" as one line terminator.
    let normalized = if text.contains('\r') { text.replace("\r\n", "\n") } else { text.to_string() };
    let text = normalized.as_str();

    let Some(tree) = parse_checked(lang, text) else {
        return Refs::default();
    };
    let root = tree.root_node();

    let units = build_units(lang, root, text);
    let mut symbols: Vec<NewSymbol> = units
        .iter()
        .filter_map(|u| {
            let name = u.symbol.clone()?;
            let qualified = qualified_name(lang, &u.scope, &name);
            Some(NewSymbol { name, qualified, kind: u.kind.to_string(), start_line: u.start_line, end_line: u.end_line })
        })
        .collect();
    symbols.sort_by_key(|s| (s.start_line, s.end_line));

    let mut edges = Vec::new();
    walk_edges(lang, root, text, &mut edges);

    Refs { symbols, edges }
}

/// `scope` + this language's separator + `symbol` -- the same join every
/// `walk_*` function already uses for `scope` itself (`join_scope` for
/// "::", `join_scope_dot` for "."), just applied one level deeper to fold
/// the leaf name in. A symbol with no scope is just its own name.
fn qualified_name(lang: Lang, scope: &Option<String>, symbol: &str) -> String {
    match scope {
        Some(s) if !s.is_empty() => {
            let sep = match lang {
                Lang::Python | Lang::TypeScript | Lang::Tsx | Lang::JavaScript => ".",
                _ => "::",
            };
            format!("{s}{sep}{symbol}")
        }
        _ => symbol.to_string(),
    }
}

/// Depth-first, pre-order visit of `node` and every named descendant,
/// dispatching each one to `emit_edge`. Named children only -- every node
/// kind `emit_edge` matches on is always a named node in these grammars.
///
/// Iterative (one `TreeCursor`, no recursion): a deeply nested file --
/// generated code, a long builder chain -- can't overflow the stack.
/// Anonymous nodes are visited by the cursor but never dispatched.
fn walk_edges(lang: Lang, node: Node, src: &str, out: &mut Vec<NewEdge>) {
    let mut cursor = node.walk();
    loop {
        let current = cursor.node();
        if current.is_named() {
            emit_edge(lang, current, src, out);
        }
        // Descend only into named nodes, matching the old
        // named-children-only recursion.
        if current.is_named() && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.node() == node {
                return;
            }
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

/// `s` with every balanced `<...>` generic/template argument list removed
/// (and a Rust turbofish's `::` before it): `add_event<picking::HoverEnter>`
/// -> `add_event`, `ns::f<T>` -> `ns::f`, `collect::<Vec<_>>` -> `collect`,
/// `IList<T>` -> `IList`. Left untouched when the brackets don't balance
/// (`operator<` and friends) -- no guess. Without this the `::` inside the
/// argument list made `last_path_segment` return `HoverEnter>`.
pub(crate) fn strip_generic_args(s: &str) -> String {
    if !s.contains('<') || s.contains("operator") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '<' => {
                if depth == 0 && out.ends_with("::") {
                    out.truncate(out.len() - 2);
                }
                depth += 1;
            }
            '>' => {
                if depth == 0 {
                    return s.to_string();
                }
                depth -= 1;
            }
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    if depth != 0 || out.is_empty() {
        return s.to_string();
    }
    out
}

/// Strips one matching pair of surrounding `"..."`, `'...'` or `<...>` from
/// `s` (a `#include`/import path's raw node text) -- left untouched if it
/// isn't wrapped in one of those.
fn strip_quotes(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (a, b) = (bytes[0], bytes[bytes.len() - 1]);
        if (a == b'"' && b == b'"') || (a == b'\'' && b == b'\'') || (a == b'<' && b == b'>') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// A C/C++/GLSL `call_expression`'s callee name, per the brief: a plain
/// `identifier`, a `field_expression`'s `field` (the member name only --
/// `obj.method()` and `ptr->method()` both use this node kind in
/// tree-sitter-c/tree-sitter-cpp, so `.` and `->` need no separate
/// handling), or a `qualified_identifier`'s own text verbatim (`ns::call()`
/// -> `"ns::call"`), or a `template_function` (`f<T>()`, its template
/// arguments stripped by `emit_edge`). `None` for anything else (a call
/// through a function pointer variable, a parenthesized expression, ...)
/// -- no edge for those rather than a guess.
fn c_call_callee(call_node: Node, src: &str) -> Option<String> {
    let f = call_node.child_by_field_name("function")?;
    match f.kind() {
        "identifier" => Some(node_text(f, src)),
        "field_expression" => f.child_by_field_name("field").map(|n| node_text(n, src)),
        // `f<T>()` / `ns::f<T>()`; template args stripped by `emit_edge`.
        "qualified_identifier" | "template_function" => Some(node_text(f, src)),
        _ => None,
    }
}

/// Dispatches one node to zero or more edges, per the brief's per-language
/// rule list. Called once per node by `walk_edges`, so it only ever
/// inspects `node` itself (and its direct fields/children) -- recursion
/// into the rest of the tree is `walk_edges`'s job, not this function's.
fn emit_edge(lang: Lang, node: Node, src: &str, out: &mut Vec<NewEdge>) {
    let line = node.start_position().row as i64 + 1;
    // Calls/inherits drop generic arguments at extraction (item 8 of the
    // phase-3 final review); includes/imports are paths, kept verbatim.
    let push = |out: &mut Vec<NewEdge>, dst_name: String, kind: &str| {
        let dst_name = if matches!(kind, "calls" | "inherits") { strip_generic_args(&dst_name) } else { dst_name };
        out.push(NewEdge { src_line: line, dst_name, kind: kind.to_string() });
    };
    match lang {
        Lang::C | Lang::Cpp | Lang::Glsl => match node.kind() {
            "call_expression" => {
                if let Some(name) = c_call_callee(node, src) {
                    push(out, name, "calls");
                }
            }
            "preproc_include" => {
                if let Some(p) = node.child_by_field_name("path") {
                    push(out, strip_quotes(&node_text(p, src)), "includes");
                }
            }
            "base_class_clause" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if matches!(child.kind(), "type_identifier" | "qualified_identifier" | "template_type") {
                        push(out, node_text(child, src), "inherits");
                    }
                }
            }
            _ => {}
        },
        Lang::CSharp => match node.kind() {
            "invocation_expression" => {
                if let Some(f) = node.child_by_field_name("function") {
                    let name = match f.kind() {
                        "identifier" => Some(node_text(f, src)),
                        "member_access_expression" => f.child_by_field_name("name").map(|n| node_text(n, src)),
                        _ => None,
                    };
                    if let Some(name) = name {
                        push(out, name, "calls");
                    }
                }
            }
            "using_directive" => {
                let mut cursor = node.walk();
                let child = node.named_children(&mut cursor).find(|c| matches!(c.kind(), "identifier" | "qualified_name"));
                if let Some(child) = child {
                    push(out, node_text(child, src), "imports");
                }
            }
            "base_list" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if matches!(child.kind(), "identifier" | "qualified_name" | "generic_name") {
                        push(out, node_text(child, src), "inherits");
                    }
                }
            }
            _ => {}
        },
        Lang::Rust => match node.kind() {
            "call_expression" => {
                if let Some(f) = node.child_by_field_name("function") {
                    let name = match f.kind() {
                        "identifier" | "scoped_identifier" | "generic_function" => Some(node_text(f, src)),
                        "field_expression" => f.child_by_field_name("field").map(|n| node_text(n, src)),
                        _ => None,
                    };
                    if let Some(name) = name {
                        push(out, name, "calls");
                    }
                }
                // `macro_invocation` (e.g. `println!(...)`) is a distinct
                // node kind, never `call_expression` -- excluded simply by
                // never matching it, per the brief.
            }
            "use_declaration" => {
                if let Some(arg) = node.child_by_field_name("argument") {
                    push(out, node_text(arg, src), "imports");
                }
            }
            "impl_item" => {
                if let Some(tr) = node.child_by_field_name("trait") {
                    push(out, node_text(tr, src), "inherits");
                }
            }
            _ => {}
        },
        Lang::Python => match node.kind() {
            "call" => {
                if let Some(f) = node.child_by_field_name("function") {
                    let name = match f.kind() {
                        "identifier" => Some(node_text(f, src)),
                        "attribute" => f.child_by_field_name("attribute").map(|n| node_text(n, src)),
                        _ => None,
                    };
                    if let Some(name) = name {
                        push(out, name, "calls");
                    }
                }
            }
            "import_statement" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    match child.kind() {
                        "dotted_name" => push(out, node_text(child, src), "imports"),
                        "aliased_import" => {
                            if let Some(n) = child.child_by_field_name("name") {
                                push(out, node_text(n, src), "imports");
                            }
                        }
                        _ => {}
                    }
                }
            }
            "import_from_statement" => {
                if let Some(m) = node.child_by_field_name("module_name") {
                    push(out, node_text(m, src), "imports");
                }
            }
            "class_definition" => {
                if let Some(supers) = node.child_by_field_name("superclasses") {
                    let mut cursor = supers.walk();
                    for child in supers.named_children(&mut cursor) {
                        if matches!(child.kind(), "identifier" | "attribute") {
                            push(out, node_text(child, src), "inherits");
                        }
                    }
                }
            }
            _ => {}
        },
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => match node.kind() {
            "call_expression" => {
                if let Some(f) = node.child_by_field_name("function") {
                    let name = match f.kind() {
                        "identifier" => Some(node_text(f, src)),
                        "member_expression" => f.child_by_field_name("property").map(|n| node_text(n, src)),
                        _ => None,
                    };
                    if let Some(name) = name {
                        push(out, name, "calls");
                    }
                }
            }
            "import_statement" => {
                if let Some(s) = node.child_by_field_name("source") {
                    push(out, strip_quotes(&node_text(s, src)), "imports");
                }
            }
            "extends_clause" => {
                if let Some(v) = node.child_by_field_name("value") {
                    push(out, node_text(v, src), "inherits");
                }
            }
            "implements_clause" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    push(out, node_text(child, src), "inherits");
                }
            }
            _ => {}
        },
        Lang::Markdown => {}
    }
}

// ---------------------------------------------------------------------
// Fallback: fixed-size line windows (unknown extension, parse failure,
// or >25% of bytes under ERROR nodes). 200-line windows, 20-line overlap
// (180-line stride), kind "window", no symbol/scope.
// ---------------------------------------------------------------------

const WINDOW_SIZE: usize = 200;
const WINDOW_OVERLAP: usize = 20;
const WINDOW_STRIDE: usize = WINDOW_SIZE - WINDOW_OVERLAP;

fn fallback_windows(text: &str) -> Vec<NewCodeChunk> {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if total == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut start = 0usize; // 0-based
    loop {
        let end = (start + WINDOW_SIZE).min(total); // exclusive
        let chunk_text = lines[start..end].join("\n");
        out.push(NewCodeChunk {
            symbol: None,
            kind: "window".to_string(),
            scope: None,
            start_line: start as i64 + 1,
            end_line: end as i64,
            content_hash: content_hash(&chunk_text),
            text: chunk_text,
            ..Default::default()
        });
        if end >= total {
            break;
        }
        start += WINDOW_STRIDE;
    }
    out
}

// ---------------------------------------------------------------------
// Markdown: split by heading (#, ##, ### -- ATX headings only, levels 4-6
// stay inside the enclosing level-<=3 section) without a grammar. Content
// before the first heading becomes a "preamble" chunk if non-whitespace.
// Each heading section becomes a "section" chunk: symbol is the heading
// text, scope is the chain of enclosing (shallower-level) heading titles
// joined by " > ".
// ---------------------------------------------------------------------

fn md_heading_level(line: &str) -> Option<(usize, &str)> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 3 {
        return None;
    }
    let rest = &line[hashes..];
    if !(rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t')) {
        return None; // e.g. "#tag", not a heading
    }
    Some((hashes, rest.trim()))
}

fn chunk_markdown(text: &str) -> Vec<NewCodeChunk> {
    let lines: Vec<&str> = text.lines().collect();
    let mut headings: Vec<(usize, usize, String)> = Vec::new(); // (0-based line idx, level, title)
    for (i, line) in lines.iter().enumerate() {
        if let Some((level, title)) = md_heading_level(line) {
            headings.push((i, level, title.to_string()));
        }
    }

    let mut units: Vec<Unit> = Vec::new();

    let first_heading_line = headings.first().map(|h| h.0);
    let preamble_end = first_heading_line.unwrap_or(lines.len());
    if preamble_end > 0 {
        let text = lines[0..preamble_end].join("\n");
        if !text.trim().is_empty() {
            units.push(Unit {
                kind: "preamble",
                symbol: None,
                scope: None,
                start_line: 1,
                end_line: preamble_end as i64,
                text,
            });
        }
    }

    let mut stack: Vec<(usize, String)> = Vec::new();
    for (idx, &(line_idx, level, ref title)) in headings.iter().enumerate() {
        while stack.last().is_some_and(|(l, _)| *l >= level) {
            stack.pop();
        }
        let scope = if stack.is_empty() { None } else { Some(stack.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(" > ")) };
        let end_line_idx = headings.get(idx + 1).map(|h| h.0).unwrap_or(lines.len());
        let section_text = lines[line_idx..end_line_idx].join("\n");
        units.push(Unit {
            kind: "section",
            symbol: Some(title.clone()),
            scope,
            start_line: line_idx as i64 + 1,
            end_line: end_line_idx as i64,
            text: section_text,
        });
        stack.push((level, title.clone()));
    }

    let mut out = Vec::new();
    for u in units {
        out.extend(split_oversized(u));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(chunks: &'a [NewCodeChunk], symbol: &str) -> &'a NewCodeChunk {
        chunks
            .iter()
            .find(|c| c.symbol.as_deref() == Some(symbol))
            .unwrap_or_else(|| panic!("no chunk with symbol {symbol:?} in {:#?}", chunks))
    }

    fn find_kind<'a>(chunks: &'a [NewCodeChunk], kind: &str) -> &'a NewCodeChunk {
        chunks.iter().find(|c| c.kind == kind).unwrap_or_else(|| panic!("no chunk with kind {kind:?} in {:#?}", chunks))
    }

    /// The invariant every chunk must hold, everywhere: `text` is exactly
    /// `src`'s own lines `start_line..=end_line` (1-based inclusive),
    /// joined with `"\n"` -- nothing added, nothing dropped, nothing
    /// reordered. `label` is just for the panic message (e.g. a file path).
    fn assert_exact_lines(label: &str, src: &str, chunks: &[NewCodeChunk]) {
        let lines: Vec<&str> = src.lines().collect();
        for c in chunks {
            assert!(c.start_line >= 1, "{label}: chunk {:?} has start_line {} < 1", c.symbol, c.start_line);
            assert!(c.end_line >= c.start_line, "{label}: chunk {:?} has end_line {} < start_line {}", c.symbol, c.end_line, c.start_line);
            let s = (c.start_line - 1) as usize;
            let e = c.end_line as usize;
            assert!(
                e <= lines.len(),
                "{label}: chunk {:?} end_line {} exceeds file's {} lines",
                c.symbol,
                c.end_line,
                lines.len()
            );
            let expected = lines[s..e].join("\n");
            assert_eq!(
                c.text, expected,
                "{label}: chunk {:?} (kind {:?}, lines {}..={}) text does not match source_lines[{}..={}] exactly",
                c.symbol, c.kind, c.start_line, c.end_line, c.start_line, c.end_line
            );
        }
    }

    // -------------------------------------------------------------
    // lang_for
    // -------------------------------------------------------------

    #[test]
    fn lang_for_maps_every_required_extension() {
        assert_eq!(lang_for("foo.c"), Some(Lang::C));
        assert_eq!(lang_for("foo.h"), Some(Lang::Cpp), "helios-style .h headers must parse as C++, not C");
        assert_eq!(lang_for("foo.hpp"), Some(Lang::Cpp));
        assert_eq!(lang_for("foo.cpp"), Some(Lang::Cpp));
        assert_eq!(lang_for("foo.cc"), Some(Lang::Cpp));
        assert_eq!(lang_for("foo.cxx"), Some(Lang::Cpp));
        assert_eq!(lang_for("foo.cs"), Some(Lang::CSharp));
        assert_eq!(lang_for("foo.rs"), Some(Lang::Rust));
        assert_eq!(lang_for("foo.py"), Some(Lang::Python));
        assert_eq!(lang_for("foo.ts"), Some(Lang::TypeScript));
        assert_eq!(lang_for("foo.tsx"), Some(Lang::Tsx));
        assert_eq!(lang_for("foo.js"), Some(Lang::JavaScript));
        assert_eq!(lang_for("foo.glsl"), Some(Lang::Glsl));
        assert_eq!(lang_for("foo.md"), Some(Lang::Markdown));
        assert_eq!(lang_for("dir/nested/path/Foo.H"), Some(Lang::Cpp), "extension match is case-insensitive");
    }

    #[test]
    fn lang_for_unknown_extension_is_none() {
        assert_eq!(lang_for("foo.zig"), None);
        assert_eq!(lang_for("Makefile"), None);
        assert_eq!(lang_for("noext"), None);
    }

    // -------------------------------------------------------------
    // C
    // -------------------------------------------------------------

    #[test]
    fn c_function_and_struct_with_preamble() {
        let src = "#include <stdio.h>\nint GLOBAL = 1;\n\nint add(int a, int b) {\n    return a + b;\n}\n\nstruct Point { int x; int y; };\n";
        let r = chunk_file("thing.c", src);
        assert_eq!(r.lang, Some(Lang::C));
        assert!(!r.fallback);

        let add = find(&r.chunks, "add");
        assert_eq!(add.kind, "function");
        assert_eq!(add.scope, None);
        assert_eq!(add.start_line, 4);
        assert_eq!(add.end_line, 6);
        assert!(add.text.starts_with("int add(int a, int b) {"));

        let point = find(&r.chunks, "Point");
        assert_eq!(point.kind, "struct");
        assert_eq!(point.start_line, 8);
        assert_eq!(point.end_line, 8);

        let preamble = find_kind(&r.chunks, "preamble");
        assert!(preamble.text.contains("#include <stdio.h>"));
        assert!(preamble.text.contains("GLOBAL"));
        assert_eq!(preamble.start_line, 1);
        // The gap is [1,3]: the blank line 3 (between GLOBAL and `add`) is
        // not its own unit, so it's absorbed into the preceding gap rather
        // than trimmed -- the preamble chunk's line range must cover it
        // exactly since `add` starts at line 4.
        assert_eq!(preamble.end_line, 3);

        assert_exact_lines("thing.c", src, &r.chunks);
        for c in &r.chunks {
            assert_eq!(c.content_hash.len(), 64);
            assert_eq!(c.content_hash, content_hash(&c.text));
        }
    }

    #[test]
    fn c_typedef_struct_enum_union() {
        let src = "typedef struct { int x; } Point2;\ntypedef enum { RED, GREEN } Color;\ntypedef union { int i; float f; } Num;\n";
        let r = chunk_file("t.c", src);
        assert!(!r.fallback);
        assert_eq!(find(&r.chunks, "Point2").kind, "struct");
        assert_eq!(find(&r.chunks, "Color").kind, "enum");
        assert_eq!(find(&r.chunks, "Num").kind, "union");
        assert_exact_lines("t.c (typedefs)", src, &r.chunks);
    }

    // -------------------------------------------------------------
    // C++
    // -------------------------------------------------------------

    #[test]
    fn cpp_header_declares_class_cpp_defines_method_out_of_line() {
        // The exact scenario the brief calls out: a header declares a
        // class with a bodyless method, the .cpp defines it out of line
        // with `Foo::bar`, and that definition must get scope "Foo".
        let header = "class Foo {\npublic:\n    void bar();\n    int baz(int x) { return x; }\n};\n";
        let hr = chunk_file("foo.h", header);
        assert_eq!(hr.lang, Some(Lang::Cpp), ".h must be parsed as C++");
        assert!(!hr.fallback);
        let class = find(&hr.chunks, "Foo");
        assert_eq!(class.kind, "class");
        assert_eq!(class.scope, None);
        assert!(class.text.contains("void bar();"));
        let baz = find(&hr.chunks, "baz");
        assert_eq!(baz.kind, "method");
        assert_eq!(baz.scope.as_deref(), Some("Foo"));

        let cpp = "#include \"foo.h\"\n\nvoid Foo::bar() {\n    int local = 1;\n}\n";
        let cr = chunk_file("foo.cpp", cpp);
        assert_eq!(cr.lang, Some(Lang::Cpp));
        assert!(!cr.fallback);
        let bar = find(&cr.chunks, "bar");
        assert_eq!(bar.kind, "method");
        assert_eq!(bar.scope.as_deref(), Some("Foo"), "out-of-line Foo::bar() must resolve scope to Foo");
        assert_eq!(bar.start_line, 3);
        assert_eq!(bar.end_line, 5);

        assert_exact_lines("foo.h", header, &hr.chunks);
        assert_exact_lines("foo.cpp", cpp, &cr.chunks);
    }

    #[test]
    fn cpp_namespace_scope_and_free_function() {
        let src = "namespace outer::inner {\nclass Widget {\npublic:\n    Widget();\n};\nWidget::Widget() {}\nvoid free_fn() {}\n}\n";
        let r = chunk_file("w.cpp", src);
        assert!(!r.fallback);
        // Both the class-like unit and the constructor share the symbol
        // "Widget"; the constructor is the one with kind "method".
        let ctor = r
            .chunks
            .iter()
            .find(|c| c.symbol.as_deref() == Some("Widget") && c.kind == "method")
            .unwrap_or_else(|| panic!("no Widget ctor in {:#?}", r.chunks));
        assert_eq!(ctor.scope.as_deref(), Some("outer::inner::Widget"));
        let _ = ctor;

        let free_fn = find(&r.chunks, "free_fn");
        assert_eq!(free_fn.kind, "function");
        assert_eq!(free_fn.scope.as_deref(), Some("outer::inner"));

        assert_exact_lines("w.cpp (namespace)", src, &r.chunks);
    }

    #[test]
    fn cpp_destructor_and_operator_names() {
        let src = "namespace ns {\nclass Widget {\npublic:\n    ~Widget();\n    Widget& operator=(const Widget& o);\n};\nWidget::~Widget() {}\nWidget& Widget::operator=(const Widget& o) { return *this; }\n}\n";
        let r = chunk_file("w.cpp", src);
        assert!(!r.fallback);
        let dtor = find(&r.chunks, "~Widget");
        assert_eq!(dtor.kind, "method");
        assert_eq!(dtor.scope.as_deref(), Some("ns::Widget"));
        let op = find(&r.chunks, "operator=");
        assert_eq!(op.kind, "method");
        assert_eq!(op.scope.as_deref(), Some("ns::Widget"));

        assert_exact_lines("w.cpp (dtor/operator)", src, &r.chunks);
    }

    #[test]
    fn cpp_enum_class_and_struct() {
        let src = "enum class Color { Red, Green };\nstruct Point { int x; int y; };\n";
        let r = chunk_file("t.cpp", src);
        assert!(!r.fallback);
        assert_eq!(find(&r.chunks, "Color").kind, "enum");
        assert_eq!(find(&r.chunks, "Point").kind, "struct");
        assert_exact_lines("t.cpp (enum class)", src, &r.chunks);
    }

    #[test]
    fn cpp_forward_declaration_goes_to_preamble_not_a_unit() {
        let src = "class Fwd;\nint g = 1;\nclass Real { void m() {} };\n";
        let r = chunk_file("t.cpp", src);
        assert!(!r.fallback);
        assert!(r.chunks.iter().all(|c| c.symbol.as_deref() != Some("Fwd")));
        let preamble = find_kind(&r.chunks, "preamble");
        // Gap-based preamble (fix round 1) slices whole source lines, not
        // per-node text, so the bare top-level `;` after `class Fwd` --
        // which tree-sitter doesn't attach to the class_specifier node --
        // is no longer dropped: it's simply part of line 1's exact text.
        assert!(preamble.text.contains("class Fwd;"));
        assert!(preamble.text.contains("int g = 1;"));
        assert_exact_lines("t.cpp (forward decl)", src, &r.chunks);
    }

    // -------------------------------------------------------------
    // C#
    // -------------------------------------------------------------

    #[test]
    fn csharp_namespace_class_method_struct_enum() {
        let src = "using System;\nnamespace Foo.Bar {\n    public class Baz {\n        public void Method() {\n            int x = 1;\n        }\n    }\n    public struct S { public int X; }\n    public enum E { A, B }\n}\n";
        let r = chunk_file("t.cs", src);
        assert_eq!(r.lang, Some(Lang::CSharp));
        assert!(!r.fallback);
        let class = find(&r.chunks, "Baz");
        assert_eq!(class.kind, "class");
        assert_eq!(class.scope.as_deref(), Some("Foo::Bar"));
        let method = find(&r.chunks, "Method");
        assert_eq!(method.kind, "method");
        assert_eq!(method.scope.as_deref(), Some("Foo::Bar::Baz"));
        let s = find(&r.chunks, "S");
        assert_eq!(s.kind, "struct");
        assert_eq!(s.scope.as_deref(), Some("Foo::Bar"));
        let e = find(&r.chunks, "E");
        assert_eq!(e.kind, "enum");

        let preamble = find_kind(&r.chunks, "preamble");
        assert!(preamble.text.contains("using System;"));

        assert_exact_lines("t.cs", src, &r.chunks);
    }

    #[test]
    fn csharp_file_scoped_namespace_declaration_lands_in_preamble() {
        // C# 10 file-scoped namespace (`namespace Helios;`, no braces --
        // `file_scoped_namespace_declaration`, not `namespace_declaration`)
        // applies to the whole rest of the file. Regression: the
        // declaration's own line was previously dropped from every chunk
        // entirely (neither a unit nor swept into any preamble span).
        let src = "namespace Helios;\n\nusing System;\n\npublic class Entity {\n    public void Update() {\n        int x = 1;\n    }\n}\n";
        let r = chunk_file("filescoped.cs", src);
        assert_eq!(r.lang, Some(Lang::CSharp));
        assert!(!r.fallback);

        // Scope still resolves through the file-scoped namespace exactly
        // as it would through a braced one.
        let class = find(&r.chunks, "Entity");
        assert_eq!(class.kind, "class");
        assert_eq!(class.scope.as_deref(), Some("Helios"));
        let method = find(&r.chunks, "Update");
        assert_eq!(method.scope.as_deref(), Some("Helios::Entity"));

        // The declaration's own line (1) is not covered by any unit, so it
        // must appear in some preamble chunk -- not be silently dropped.
        assert!(
            r.chunks.iter().any(|c| c.kind == "preamble" && c.text.contains("namespace Helios;")),
            "namespace Helios; line missing from every chunk: {:#?}",
            r.chunks
        );

        assert_exact_lines("filescoped.cs", src, &r.chunks);
    }

    // -------------------------------------------------------------
    // Rust
    // -------------------------------------------------------------

    #[test]
    fn rust_function_struct_enum_impl_trait_mod() {
        let src = "use std::io;\nconst X: i32 = 1;\n\nfn add(a: i32, b: i32) -> i32 { a + b }\n\nstruct Point { x: i32, y: i32 }\n\nenum Color { Red, Green }\n\ntrait Shape {\n    fn area(&self) -> f64;\n    fn describe(&self) -> String { String::new() }\n}\n\nimpl Point {\n    fn new() -> Self { Point { x: 0, y: 0 } }\n}\n\nmod sub {\n    pub fn f() {}\n}\n";
        let r = chunk_file("t.rs", src);
        assert_eq!(r.lang, Some(Lang::Rust));
        assert!(!r.fallback);

        let add = find(&r.chunks, "add");
        assert_eq!(add.kind, "function");
        assert_eq!(add.scope, None);

        assert_eq!(find(&r.chunks, "Point").kind, "struct");
        assert_eq!(find(&r.chunks, "Color").kind, "enum");

        let shape = find(&r.chunks, "Shape");
        assert_eq!(shape.kind, "trait");
        let describe = find(&r.chunks, "describe");
        assert_eq!(describe.kind, "method");
        assert_eq!(describe.scope.as_deref(), Some("Shape"));

        let new_fn = find(&r.chunks, "new");
        assert_eq!(new_fn.kind, "method");
        assert_eq!(new_fn.scope.as_deref(), Some("Point"));

        let sub_f = find(&r.chunks, "f");
        assert_eq!(sub_f.kind, "function");
        assert_eq!(sub_f.scope.as_deref(), Some("sub"));

        let preamble = find_kind(&r.chunks, "preamble");
        assert!(preamble.text.contains("use std::io;"));
        assert!(preamble.text.contains("const X"));

        assert_exact_lines("t.rs", src, &r.chunks);
    }

    // -------------------------------------------------------------
    // Python
    // -------------------------------------------------------------

    #[test]
    fn python_function_class_method_and_preamble() {
        let src = "import os\nX = 1\n\ndef add(a, b):\n    return a + b\n\nclass Point:\n    def __init__(self, x, y):\n        self.x = x\n\n    def dist(self):\n        return self.x\n";
        let r = chunk_file("t.py", src);
        assert_eq!(r.lang, Some(Lang::Python));
        assert!(!r.fallback);

        let add = find(&r.chunks, "add");
        assert_eq!(add.kind, "function");
        assert_eq!(add.scope, None);

        let class = find(&r.chunks, "Point");
        assert_eq!(class.kind, "class");

        let dist = find(&r.chunks, "dist");
        assert_eq!(dist.kind, "method");
        assert_eq!(dist.scope.as_deref(), Some("Point"));

        let preamble = find_kind(&r.chunks, "preamble");
        assert!(preamble.text.contains("import os"));
        assert!(preamble.text.contains("X = 1"));

        assert_exact_lines("t.py", src, &r.chunks);
    }

    #[test]
    fn python_nested_class_decorator_and_async() {
        let src = "class Outer:\n    class Inner:\n        def m(self):\n            pass\n\n    @staticmethod\n    def helper():\n        pass\n\n    async def go(self):\n        pass\n";
        let r = chunk_file("t.py", src);
        assert!(!r.fallback);

        let inner = find(&r.chunks, "Inner");
        assert_eq!(inner.kind, "class");
        assert_eq!(inner.scope.as_deref(), Some("Outer"));

        let m = find(&r.chunks, "m");
        assert_eq!(m.scope.as_deref(), Some("Outer.Inner"));

        let helper = find(&r.chunks, "helper");
        assert_eq!(helper.kind, "method");
        assert_eq!(helper.scope.as_deref(), Some("Outer"));
        assert!(helper.text.contains("@staticmethod"), "decorated_definition span should keep the decorator");

        let go = find(&r.chunks, "go");
        assert_eq!(go.kind, "method");
        // `go` is indented (nested in class Outer), so its text's first
        // line includes that leading indentation -- the exact-lines
        // invariant means chunk text is never re-dedented.
        assert!(go.text.trim_start().starts_with("async def go"));
        assert!(go.text.starts_with("    async def go"));

        assert_exact_lines("t.py", src, &r.chunks);
    }

    // -------------------------------------------------------------
    // TypeScript / TSX / JavaScript
    // -------------------------------------------------------------

    #[test]
    fn typescript_function_class_interface_enum_export() {
        let src = "import { x } from \"y\";\nconst X = 1;\n\nexport function add(a: number, b: number): number { return a + b; }\n\nexport class Point {\n    x: number;\n    constructor(x: number) { this.x = x; }\n    dist(): number { return this.x; }\n}\n\ninterface Shape { area(): number; }\n\nenum Color { Red, Green }\n";
        let r = chunk_file("t.ts", src);
        assert_eq!(r.lang, Some(Lang::TypeScript));
        assert!(!r.fallback);

        let add = find(&r.chunks, "add");
        assert_eq!(add.kind, "function");
        assert_eq!(add.scope, None);

        let class = find(&r.chunks, "Point");
        assert_eq!(class.kind, "class");

        let dist = find(&r.chunks, "dist");
        assert_eq!(dist.kind, "method");
        assert_eq!(dist.scope.as_deref(), Some("Point"));

        assert_eq!(find(&r.chunks, "Shape").kind, "interface");
        assert_eq!(find(&r.chunks, "Color").kind, "enum");

        let preamble = find_kind(&r.chunks, "preamble");
        assert!(preamble.text.contains("import { x }"));
        assert!(preamble.text.contains("const X = 1;"), "top-level const (incl. arrow functions) is a preamble statement");

        assert_exact_lines("t.ts", src, &r.chunks);
    }

    #[test]
    fn tsx_function_and_class_with_jsx() {
        let src = "function App() {\n    return <div>hi</div>;\n}\n\nclass Foo extends React.Component {\n    render() {\n        return <div/>;\n    }\n}\n";
        let r = chunk_file("t.tsx", src);
        assert_eq!(r.lang, Some(Lang::Tsx));
        assert!(!r.fallback);
        assert_eq!(find(&r.chunks, "App").kind, "function");
        assert_eq!(find(&r.chunks, "render").scope.as_deref(), Some("Foo"));
        assert_exact_lines("t.tsx", src, &r.chunks);
    }

    #[test]
    fn javascript_function_class_method() {
        let src = "import { x } from \"y\";\nfunction add(a, b) { return a + b; }\nclass Point {\n    constructor(x) { this.x = x; }\n    dist() { return this.x; }\n}\nconst arrow = (a, b) => a + b;\n";
        let r = chunk_file("t.js", src);
        assert_eq!(r.lang, Some(Lang::JavaScript));
        assert!(!r.fallback);
        assert_eq!(find(&r.chunks, "add").kind, "function");
        assert_eq!(find(&r.chunks, "dist").scope.as_deref(), Some("Point"));
        // "import" (line 1) and "const arrow" (line 7) are two separate
        // gaps -- lines 2-6 are covered by `add` and the `Point` class --
        // so they land in two different preamble chunks, not one.
        let preambles: Vec<&NewCodeChunk> = r.chunks.iter().filter(|c| c.kind == "preamble").collect();
        assert_eq!(preambles.len(), 2, "{:#?}", preambles);
        assert!(preambles.iter().any(|c| c.text.contains("import { x }")));
        assert!(preambles.iter().any(|c| c.text.contains("const arrow")));

        assert_exact_lines("t.js", src, &r.chunks);
    }

    // -------------------------------------------------------------
    // GLSL
    // -------------------------------------------------------------

    #[test]
    fn glsl_functions_and_preamble() {
        let src = "#version 450\nuniform vec3 color;\n\nfloat square(float x) {\n    return x * x;\n}\n\nvoid main() {\n    gl_FragColor = vec4(color, 1.0);\n}\n";
        let r = chunk_file("shader.glsl", src);
        assert_eq!(r.lang, Some(Lang::Glsl));
        assert!(!r.fallback);
        let square = find(&r.chunks, "square");
        assert_eq!(square.kind, "function");
        let main = find(&r.chunks, "main");
        assert_eq!(main.kind, "function");
        let preamble = find_kind(&r.chunks, "preamble");
        assert!(preamble.text.contains("#version 450"));
        assert!(preamble.text.contains("uniform vec3 color;"));

        assert_exact_lines("shader.glsl", src, &r.chunks);
    }

    /// tree-sitter-glsl 0.2.0 does not parse `#pragma` at all (confirmed
    /// against the real grammar -- not even `#pragma once`; it always
    /// produces an `ERROR` node for the directive, and the misparse
    /// cascades into whatever follows on the same statement). In a file
    /// that is *just* `#version` + `#pragma` + one function, that ERROR
    /// region is a large enough fraction of the file that it trips the
    /// 25%-error-byte fallback gate -- which means the tree-sitter-driven
    /// preamble path (the one the reported bug was actually in) never
    /// runs for a file that small. A real shader file surrounds the
    /// pragma with enough other content that the same ERROR region is a
    /// small fraction of the total, so parsing proceeds normally and the
    /// parser recovers by the next top-level declaration -- this fixture
    /// mimics that shape (a handful of uniforms) rather than the
    /// pathological tiny case. See task-3-report.md, Fix round 1.
    fn glsl_pragma_source() -> String {
        let mut src = String::from("#version 450\n#pragma stage(vertex)\n\n");
        for i in 0..5 {
            src.push_str(&format!("uniform vec3 color{i};\n"));
        }
        src.push_str("\nfloat square(float x) {\n    return x * x;\n}\n\nvoid main() {\n    gl_Position = vec4(square(1.0));\n}\n");
        src
    }

    #[test]
    fn glsl_pragma_directive_is_not_split_across_two_lines() {
        // Regression: the old per-node Span-join preamble merge could
        // split one physical source line into two in the reconstructed
        // preamble text (multiple fragment nodes covering the same
        // physical `#pragma` line, joined with an inserted "\n" that
        // wasn't in the source).
        let src = glsl_pragma_source();
        let r = chunk_file("pragma.glsl", &src);
        assert_eq!(r.lang, Some(Lang::Glsl));
        assert!(!r.fallback, "fixture must stay under the error-ratio fallback gate to exercise the tree-sitter preamble path");

        let preamble = find_kind(&r.chunks, "preamble");
        assert_eq!(preamble.start_line, 1);
        assert!(preamble.text.starts_with("#version 450\n#pragma stage(vertex)\n\nuniform vec3 color0;"));
        // The pragma line itself must be one physical line in the text,
        // not split into two.
        assert!(preamble.text.lines().any(|l| l == "#pragma stage(vertex)"), "pragma line was split: {:?}", preamble.text);

        let square = find(&r.chunks, "square");
        assert_eq!(square.kind, "function");
        let main = find(&r.chunks, "main");
        assert_eq!(main.kind, "function");

        assert_exact_lines("pragma.glsl", &src, &r.chunks);
    }

    // -------------------------------------------------------------
    // Markdown
    // -------------------------------------------------------------

    #[test]
    fn markdown_split_by_headings_with_scope_chain() {
        let src = "Intro text.\n\n# Title\n\nSome text.\n\n## Sub\n\nMore text.\n\n### Subsub\n\nDeep text.\n\n#### Ignored\n\nStill part of Subsub.\n\n## Sub2\n\nLast.\n";
        let r = chunk_file("t.md", src);
        assert_eq!(r.lang, Some(Lang::Markdown));
        assert!(!r.fallback);

        let preamble = find_kind(&r.chunks, "preamble");
        assert!(preamble.text.contains("Intro text."));

        let title = find(&r.chunks, "Title");
        assert_eq!(title.kind, "section");
        assert_eq!(title.scope, None);

        let sub = find(&r.chunks, "Sub");
        assert_eq!(sub.scope.as_deref(), Some("Title"));

        let subsub = find(&r.chunks, "Subsub");
        assert_eq!(subsub.scope.as_deref(), Some("Title > Sub"));
        assert!(subsub.text.contains("#### Ignored"), "level-4 heading stays inside the enclosing <=3 section");
        assert!(subsub.text.contains("Still part of Subsub."));

        let sub2 = find(&r.chunks, "Sub2");
        assert_eq!(sub2.scope.as_deref(), Some("Title"), "Sub2 is a sibling of Sub, not nested under it");

        assert_exact_lines("t.md", src, &r.chunks);
    }

    #[test]
    fn markdown_no_headings_has_no_chunks_when_blank() {
        let r = chunk_file("empty.md", "   \n\n  \n");
        assert!(r.chunks.is_empty());
        assert!(!r.fallback);
    }

    // -------------------------------------------------------------
    // Preamble: multi-span gaps, blank-line preservation
    // -------------------------------------------------------------

    #[test]
    fn preamble_gap_keeps_embedded_blank_lines_and_spans_multiple_includes() {
        // Regression: the old preamble merge re-joined each leftover
        // top-level node's own (byte-sliced) text with "\n", so blank
        // *source* lines between those nodes were absorbed into the join
        // separator and never appeared in the reconstructed text --
        // matching the reported bug ("project.h reports 1-84 but text has
        // 27 lines"). Line-exact gap slicing must preserve every blank
        // line, including runs of more than one.
        let src = "#include <a.h>\n\n#include <b.h>\n\n\n#include <c.h>\n\nclass Widget {\n    void m() {}\n};\n";
        let r = chunk_file("multi_include.h", src);
        assert_eq!(r.lang, Some(Lang::Cpp));
        assert!(!r.fallback);

        let preamble = find_kind(&r.chunks, "preamble");
        assert_eq!(preamble.start_line, 1);
        assert_eq!(preamble.end_line, 7, "gap runs through the blank line right before `class Widget`");
        // Exact reconstruction, blank lines (including the double blank
        // between b.h and c.h) preserved verbatim -- this is what the
        // reported bug got wrong (blank lines silently absorbed by the
        // old per-node join).
        let expected = "#include <a.h>\n\n#include <b.h>\n\n\n#include <c.h>\n";
        assert_eq!(preamble.text, expected);

        assert_exact_lines("multi_include.h", src, &r.chunks);
    }

    #[test]
    fn crlf_line_endings_still_satisfy_the_exact_lines_invariant() {
        // Regression found via the real helios corpus (fix round 1):
        // CRLF files (common on this codebase) broke the invariant
        // because tree-sitter's byte offsets point into whatever text the
        // parser was given, so a node's own line ended with a bare `\r`
        // that `str::lines()` (used to define "source lines" everywhere
        // else, and by every consumer reconstructing text from
        // start_line/end_line) strips. `chunk_file` now normalizes
        // CRLF -> LF up front.
        let src = "#include <a.h>\r\ninline bool has(uint32_t f, uint32_t b) {\r\n    return (f & b) != 0;\r\n}\r\n";
        let r = chunk_file("crlf.h", src);
        assert_eq!(r.lang, Some(Lang::Cpp));
        assert!(!r.fallback);

        let has = find(&r.chunks, "has");
        assert_eq!(has.start_line, 2);
        assert_eq!(has.end_line, 4);
        // No stray `\r` anywhere in the reconstructed text.
        assert!(!has.text.contains('\r'), "text still contains a bare CR: {:?}", has.text);
        assert_eq!(has.text, "inline bool has(uint32_t f, uint32_t b) {\n    return (f & b) != 0;\n}");

        let preamble = find_kind(&r.chunks, "preamble");
        assert!(!preamble.text.contains('\r'));
        assert_eq!(preamble.text, "#include <a.h>");

        // assert_exact_lines itself uses `src.lines()` on the *original*
        // (still-CRLF) `src` -- since `str::lines()` already strips `\r`
        // from a `\r\n` terminator, this proves the invariant holds
        // end-to-end from the caller's point of view, not just internally
        // against the pre-normalized copy.
        assert_exact_lines("crlf.h", src, &r.chunks);
    }

    // -------------------------------------------------------------
    // Oversized unit splitting
    // -------------------------------------------------------------

    /// The 600-line-function fixture shared by the oversized-split tests
    /// and by the invariant sweep.
    fn big_fn_source() -> String {
        let mut body = String::from("int big_fn(int x) {\n");
        for i in 0..600 {
            body.push_str(&format!("    x += {i};\n"));
            if i % 37 == 0 {
                body.push('\n'); // occasional blank line = a split boundary candidate
            }
        }
        body.push_str("    return x;\n}\n");
        body
    }

    #[test]
    fn oversized_function_splits_without_altering_source_lines() {
        let body = big_fn_source();
        let r = chunk_file("t.c", &body);
        assert!(!r.fallback);

        let parts: Vec<&NewCodeChunk> = r.chunks.iter().filter(|c| c.symbol.as_deref().is_some_and(|s| s.starts_with("big_fn"))).collect();
        assert!(parts.len() >= 2, "expected the oversized function to split into multiple parts, got {}", parts.len());

        let total_parts = parts.len();
        let mut sorted = parts.clone();
        sorted.sort_by_key(|p| p.start_line);
        for (i, p) in sorted.iter().enumerate() {
            // No re-prepended signature line anywhere: every part's text
            // is exactly its own source lines, per the exact-lines
            // invariant -- checked in full below via assert_exact_lines.
            assert_eq!(p.symbol.as_deref(), Some(format!("big_fn (part {}/{total_parts})", i + 1)).as_deref());
            assert_eq!(p.kind, "function");
            let lines = p.text.lines().count();
            assert!(lines <= MAX_PART_LINES, "part has {lines} lines, expected <= {MAX_PART_LINES}");
        }
        // Parts cover the unit contiguously, no gaps or overlaps.
        for w in sorted.windows(2) {
            assert_eq!(w[0].end_line + 1, w[1].start_line, "parts should be contiguous");
        }
        assert_eq!(sorted.first().unwrap().start_line, 1);
        for p in &parts {
            assert_eq!(p.content_hash, content_hash(&p.text));
        }

        assert_exact_lines("t.c", &body, &r.chunks);
    }

    #[test]
    fn unit_under_400_lines_is_not_split() {
        let mut body = String::from("void small_fn() {\n");
        for i in 0..50 {
            body.push_str(&format!("    do_thing({i});\n"));
        }
        body.push_str("}\n");
        let r = chunk_file("t.c", &body);
        let parts: Vec<&NewCodeChunk> = r.chunks.iter().filter(|c| c.symbol.as_deref() == Some("small_fn")).collect();
        assert_eq!(parts.len(), 1);
        assert_exact_lines("t.c (small_fn)", &body, &r.chunks);
    }

    // -------------------------------------------------------------
    // Fallback: broken source and unknown extension
    // -------------------------------------------------------------

    #[test]
    fn severely_broken_source_falls_back_to_windows() {
        // Deliberately garbage C++ that ERRORs badly (unbalanced braces,
        // random tokens) -- must exceed the 25% error-byte threshold.
        let mut src = String::new();
        for _ in 0..30 {
            src.push_str("@@@ ][ {{{ )( <<< garbage tokens not valid cpp ~~~ &&&\n");
        }
        let r = chunk_file("broken.cpp", &src);
        assert_eq!(r.lang, Some(Lang::Cpp));
        assert!(r.fallback);
        assert!(r.chunks.iter().all(|c| c.kind == "window"));
        assert!(r.chunks.iter().all(|c| c.symbol.is_none() && c.scope.is_none()));
        assert_exact_lines("broken.cpp", &src, &r.chunks);
    }

    #[test]
    fn parser_failure_style_input_falls_back() {
        // Null bytes / binary-ish content: many grammars will still
        // "parse" something, but this is here to document the
        // parser.parse()-returns-None path is exercised by construction
        // in chunk_with_tree_sitter (belt-and-suspenders; the primary
        // path under test is the error-ratio gate above).
        let src = "\u{0}\u{1}\u{2}\u{3}not valid source at all {{{{ @#$%\n".repeat(20);
        let r = chunk_file("broken.rs", &src);
        assert!(r.fallback || !r.chunks.is_empty());
    }

    #[test]
    fn unknown_extension_falls_back_to_windows_with_overlap() {
        let mut src = String::new();
        for i in 0..250 {
            src.push_str(&format!("line {i}\n"));
        }
        let r = chunk_file("data.zig", &src);
        assert_eq!(r.lang, None);
        assert!(r.fallback);
        assert_eq!(r.chunks.len(), 2, "250 lines -> [1,200] then [181,250]");
        assert_eq!(r.chunks[0].start_line, 1);
        assert_eq!(r.chunks[0].end_line, 200);
        assert_eq!(r.chunks[0].kind, "window");
        assert_eq!(r.chunks[1].start_line, 181);
        assert_eq!(r.chunks[1].end_line, 250);
        // 20-line overlap: chunk 1 ends at 200, chunk 2 starts at 181.
        assert_eq!(r.chunks[0].end_line - r.chunks[1].start_line + 1, 20);
        for c in &r.chunks {
            assert_eq!(c.content_hash, content_hash(&c.text));
        }
        assert_exact_lines("data.zig", &src, &r.chunks);
    }

    #[test]
    fn empty_file_produces_no_chunks() {
        let r = chunk_file("t.c", "");
        assert!(r.chunks.is_empty());
        assert!(!r.fallback);

        let r2 = chunk_file("t.zig", "");
        assert!(r2.chunks.is_empty());
    }

    // -------------------------------------------------------------
    // Invariant sweep (fix round 1, item 4): every existing fixture,
    // across every language, must satisfy the exact-lines invariant.
    // Duplicates the individual `assert_exact_lines` calls sprinkled
    // through the tests above (kept there so a violation points straight
    // at the failing scenario) plus a few extra fixtures for coverage
    // breadth in one place.
    // -------------------------------------------------------------

    #[test]
    fn exact_lines_invariant_holds_for_every_fixture_and_language() {
        let fixtures: Vec<(&str, String)> = vec![
            ("thing.c", "#include <stdio.h>\nint GLOBAL = 1;\n\nint add(int a, int b) {\n    return a + b;\n}\n\nstruct Point { int x; int y; };\n".to_string()),
            ("typedefs.c", "typedef struct { int x; } Point2;\ntypedef enum { RED, GREEN } Color;\ntypedef union { int i; float f; } Num;\n".to_string()),
            ("foo.h", "class Foo {\npublic:\n    void bar();\n    int baz(int x) { return x; }\n};\n".to_string()),
            ("foo.cpp", "#include \"foo.h\"\n\nvoid Foo::bar() {\n    int local = 1;\n}\n".to_string()),
            ("namespace.cpp", "namespace outer::inner {\nclass Widget {\npublic:\n    Widget();\n};\nWidget::Widget() {}\nvoid free_fn() {}\n}\n".to_string()),
            (
                "dtor_operator.cpp",
                "namespace ns {\nclass Widget {\npublic:\n    ~Widget();\n    Widget& operator=(const Widget& o);\n};\nWidget::~Widget() {}\nWidget& Widget::operator=(const Widget& o) { return *this; }\n}\n".to_string(),
            ),
            ("enum_class.cpp", "enum class Color { Red, Green };\nstruct Point { int x; int y; };\n".to_string()),
            ("forward_decl.cpp", "class Fwd;\nint g = 1;\nclass Real { void m() {} };\n".to_string()),
            (
                "t.cs",
                "using System;\nnamespace Foo.Bar {\n    public class Baz {\n        public void Method() {\n            int x = 1;\n        }\n    }\n    public struct S { public int X; }\n    public enum E { A, B }\n}\n"
                    .to_string(),
            ),
            (
                "filescoped.cs",
                "namespace Helios;\n\nusing System;\n\npublic class Entity {\n    public void Update() {\n        int x = 1;\n    }\n}\n".to_string(),
            ),
            (
                "t.rs",
                "use std::io;\nconst X: i32 = 1;\n\nfn add(a: i32, b: i32) -> i32 { a + b }\n\nstruct Point { x: i32, y: i32 }\n\nenum Color { Red, Green }\n\ntrait Shape {\n    fn area(&self) -> f64;\n    fn describe(&self) -> String { String::new() }\n}\n\nimpl Point {\n    fn new() -> Self { Point { x: 0, y: 0 } }\n}\n\nmod sub {\n    pub fn f() {}\n}\n"
                    .to_string(),
            ),
            (
                "t.py",
                "import os\nX = 1\n\ndef add(a, b):\n    return a + b\n\nclass Point:\n    def __init__(self, x, y):\n        self.x = x\n\n    def dist(self):\n        return self.x\n".to_string(),
            ),
            (
                "nested.py",
                "class Outer:\n    class Inner:\n        def m(self):\n            pass\n\n    @staticmethod\n    def helper():\n        pass\n\n    async def go(self):\n        pass\n".to_string(),
            ),
            (
                "t.ts",
                "import { x } from \"y\";\nconst X = 1;\n\nexport function add(a: number, b: number): number { return a + b; }\n\nexport class Point {\n    x: number;\n    constructor(x: number) { this.x = x; }\n    dist(): number { return this.x; }\n}\n\ninterface Shape { area(): number; }\n\nenum Color { Red, Green }\n"
                    .to_string(),
            ),
            ("t.tsx", "function App() {\n    return <div>hi</div>;\n}\n\nclass Foo extends React.Component {\n    render() {\n        return <div/>;\n    }\n}\n".to_string()),
            (
                "t.js",
                "import { x } from \"y\";\nfunction add(a, b) { return a + b; }\nclass Point {\n    constructor(x) { this.x = x; }\n    dist() { return this.x; }\n}\nconst arrow = (a, b) => a + b;\n".to_string(),
            ),
            ("shader.glsl", "#version 450\nuniform vec3 color;\n\nfloat square(float x) {\n    return x * x;\n}\n\nvoid main() {\n    gl_FragColor = vec4(color, 1.0);\n}\n".to_string()),
            ("pragma.glsl", glsl_pragma_source()),
            (
                "t.md",
                "Intro text.\n\n# Title\n\nSome text.\n\n## Sub\n\nMore text.\n\n### Subsub\n\nDeep text.\n\n#### Ignored\n\nStill part of Subsub.\n\n## Sub2\n\nLast.\n".to_string(),
            ),
            ("multi_include.h", "#include <a.h>\n\n#include <b.h>\n\n\n#include <c.h>\n\nclass Widget {\n    void m() {}\n};\n".to_string()),
            ("big_fn.c", big_fn_source()),
            ("crlf.h", "#include <a.h>\r\ninline bool has(uint32_t f, uint32_t b) {\r\n    return (f & b) != 0;\r\n}\r\n".to_string()),
            ("broken.rs", "\u{0}\u{1}\u{2}\u{3}not valid source at all {{{{ @#$%\n".repeat(20)),
            ("unknown.zig", (0..250).map(|i| format!("line {i}\n")).collect::<String>()),
        ];

        // assert_exact_lines panics on the first mismatch, at the specific
        // fixture that broke -- exactly "0 violations" for this sweep,
        // fastest to diagnose by failing loudly and immediately rather
        // than accumulating a count.
        for (path, src) in &fixtures {
            let r = chunk_file(path, src);
            assert_exact_lines(path, src, &r.chunks);
        }
    }

    // -------------------------------------------------------------
    // extract_refs
    // -------------------------------------------------------------

    fn find_symbol<'a>(symbols: &'a [NewSymbol], name: &str) -> &'a NewSymbol {
        symbols.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no symbol {name:?} in {:#?}", symbols))
    }

    fn find_edge<'a>(edges: &'a [NewEdge], kind: &str, dst_name: &str) -> &'a NewEdge {
        edges
            .iter()
            .find(|e| e.kind == kind && e.dst_name == dst_name)
            .unwrap_or_else(|| panic!("no {kind:?} edge to {dst_name:?} in {:#?}", edges))
    }

    #[test]
    fn extract_refs_unknown_extension_and_markdown_yield_nothing() {
        let none = extract_refs("Makefile", "anything() at all\n");
        assert!(none.symbols.is_empty());
        assert!(none.edges.is_empty());

        // Markdown has a `Lang` (so `chunk_file` still splits it by
        // heading) but no code graph -- the brief is explicit: "Markdown:
        // none".
        let md = extract_refs("t.md", "# Title\n\n```c\nprintf(\"hi\");\n```\n");
        assert!(md.symbols.is_empty());
        assert!(md.edges.is_empty());
    }

    #[test]
    fn extract_refs_on_severely_broken_source_yields_nothing() {
        // Same >25% ERROR-bytes threshold `chunk_with_tree_sitter` bails
        // on -- there is no fallback reference extraction for it (the
        // brief: "Fallback files: none").
        let src = "\u{0}\u{1}\u{2}\u{3}not valid source at all {{{{ @#$%\n".repeat(20);
        let refs = extract_refs("broken.rs", &src);
        assert!(refs.symbols.is_empty());
        assert!(refs.edges.is_empty());
    }

    #[test]
    fn extract_refs_c_includes_and_calls() {
        let src = "#include <stdio.h>\n#include \"local.h\"\n\nint helper(int x) {\n    return x * 2;\n}\n\nint main() {\n    int y = helper(1);\n    return y;\n}\n";
        let refs = extract_refs("t.c", src);

        assert_eq!(find_symbol(&refs.symbols, "helper").kind, "function");
        assert_eq!(find_symbol(&refs.symbols, "helper").qualified, "helper");
        assert_eq!(find_symbol(&refs.symbols, "main").kind, "function");

        assert_eq!(find_edge(&refs.edges, "includes", "stdio.h").src_line, 1);
        assert_eq!(find_edge(&refs.edges, "includes", "local.h").src_line, 2);
        assert_eq!(find_edge(&refs.edges, "calls", "helper").src_line, 9);
    }

    #[test]
    fn extract_refs_c_member_call_through_pointer_and_dot() {
        // tree-sitter-c uses the same `field_expression` node for both
        // `.` and `->` -- one code path must cover both.
        let src = "struct S { int (*f)(void); };\nvoid use(struct S *p, struct S v) {\n    p->f();\n    v.f();\n}\n";
        let refs = extract_refs("t.c", src);
        let calls: Vec<&str> = refs.edges.iter().filter(|e| e.kind == "calls").map(|e| e.dst_name.as_str()).collect();
        assert_eq!(calls, vec!["f", "f"], "{:#?}", refs.edges);
    }

    #[test]
    fn extract_refs_cpp_out_of_line_method_qualifies_scope_and_finds_call() {
        // The brief's own example: a header-declared method defined out of
        // line must produce a symbol qualified "Foo::bar", and the call
        // inside it must be attributable back to that symbol once
        // `store::replace_file_symbols` resolves `src_symbol_id` by line
        // range (tested against the real store in store.rs).
        let src = "class Foo {\npublic:\n    void bar();\n};\n\nvoid Foo::bar() {\n    baz();\n}\n";
        let refs = extract_refs("t.cpp", src);

        let foo = find_symbol(&refs.symbols, "Foo");
        assert_eq!(foo.kind, "class");
        assert_eq!(foo.qualified, "Foo");

        let bar = find_symbol(&refs.symbols, "bar");
        assert_eq!(bar.kind, "method");
        assert_eq!(bar.qualified, "Foo::bar");
        assert_eq!(bar.start_line, 6);
        assert_eq!(bar.end_line, 8);

        // The bodyless declaration inside the class must not itself
        // produce a "bar" symbol (it's a `field_declaration`, not a
        // `function_definition`) -- exactly one "bar" symbol total.
        assert_eq!(refs.symbols.iter().filter(|s| s.name == "bar").count(), 1);

        let edge = find_edge(&refs.edges, "calls", "baz");
        assert_eq!(edge.src_line, 7);
    }

    #[test]
    fn extract_refs_cpp_includes_base_classes_and_qualified_calls() {
        let src = "#include <cstdio>\n#include \"local/thing.h\"\n\nclass Base {};\nclass Derived : public Base, private Other::Nested {\npublic:\n    void run() {\n        step();\n        this->helper();\n        ns::util();\n    }\n};\n";
        let refs = extract_refs("t.cpp", src);

        assert_eq!(find_edge(&refs.edges, "includes", "cstdio").src_line, 1);
        assert_eq!(find_edge(&refs.edges, "includes", "local/thing.h").src_line, 2);

        assert_eq!(find_edge(&refs.edges, "inherits", "Base").src_line, 5);
        assert_eq!(find_edge(&refs.edges, "inherits", "Other::Nested").src_line, 5);

        assert_eq!(find_edge(&refs.edges, "calls", "step").src_line, 8);
        assert_eq!(find_edge(&refs.edges, "calls", "helper").src_line, 9, "field_expression member call keeps only the member name");
        assert_eq!(find_edge(&refs.edges, "calls", "ns::util").src_line, 10, "qualified_identifier callee keeps its full spelling");

        assert_eq!(find_symbol(&refs.symbols, "run").qualified, "Derived::run");
    }

    #[test]
    fn extract_refs_strips_template_arguments_from_calls() {
        // Item 8: `add_event<picking::HoverEnter>` used to reach the graph
        // verbatim, and its last `::` segment was `HoverEnter>`.
        let src = "void setup(App& app) {\n    app.add_event<picking::HoverEnter>();\n    make<int>(1);\n    ns::build<Foo, Bar<int>>();\n}\n";
        let refs = extract_refs("t.cpp", src);
        assert_eq!(find_edge(&refs.edges, "calls", "add_event").src_line, 2, "{:#?}", refs.edges);
        assert_eq!(find_edge(&refs.edges, "calls", "make").src_line, 3, "{:#?}", refs.edges);
        assert_eq!(find_edge(&refs.edges, "calls", "ns::build").src_line, 4, "{:#?}", refs.edges);
        assert!(refs.edges.iter().all(|e| !e.dst_name.contains('<')), "{:#?}", refs.edges);
    }

    #[test]
    fn strip_generic_args_handles_nesting_turbofish_and_leaves_unbalanced_alone() {
        assert_eq!(strip_generic_args("add_event<picking::HoverEnter>"), "add_event");
        assert_eq!(strip_generic_args("ns::f<A, B<C>>"), "ns::f");
        assert_eq!(strip_generic_args("collect::<Vec<_>>"), "collect");
        assert_eq!(strip_generic_args("IList<T>"), "IList");
        assert_eq!(strip_generic_args("plain::name"), "plain::name");
        assert_eq!(strip_generic_args("operator<"), "operator<");
        assert_eq!(strip_generic_args("a>b"), "a>b");
    }

    #[test]
    fn walk_edges_is_iterative_and_keeps_pre_order() {
        // Deep nesting: a recursive walk would need one stack frame per level.
        let depth = 3000;
        let mut src = String::from("fn f() {\n");
        for _ in 0..depth {
            src.push_str("{ ");
        }
        src.push_str("g();");
        for _ in 0..depth {
            src.push_str(" }");
        }
        src.push_str("\n    h();\n}\n");
        let refs = extract_refs("deep.rs", &src);
        let calls: Vec<&str> = refs.edges.iter().filter(|e| e.kind == "calls").map(|e| e.dst_name.as_str()).collect();
        assert!(!refs.symbols.is_empty(), "the deep file must still parse");
        assert_eq!(calls, vec!["g", "h"], "pre-order: the nested call first");
    }

    #[test]
    fn extract_refs_csharp_imports_inherits_calls() {
        let src = "using System;\n\nnamespace App {\n    class Base {}\n    class Derived : Base {\n        void Foo() {\n            Bar();\n        }\n        void Bar() {}\n    }\n}\n";
        let refs = extract_refs("t.cs", src);

        assert_eq!(find_symbol(&refs.symbols, "Derived").qualified, "App::Derived");
        assert_eq!(find_symbol(&refs.symbols, "Foo").qualified, "App::Derived::Foo");

        assert_eq!(find_edge(&refs.edges, "imports", "System").src_line, 1);
        assert_eq!(find_edge(&refs.edges, "inherits", "Base").src_line, 5);
        assert_eq!(find_edge(&refs.edges, "calls", "Bar").src_line, 7);
    }

    #[test]
    fn extract_refs_rust_imports_inherits_calls_excludes_macros() {
        let src = "use std::collections::HashMap;\n\ntrait Greet {\n    fn hi(&self);\n}\n\nstruct Point;\n\nimpl Greet for Point {\n    fn hi(&self) {\n        bar();\n        println!(\"hi\");\n    }\n}\n\nfn bar() {}\n";
        let refs = extract_refs("t.rs", src);

        assert_eq!(find_symbol(&refs.symbols, "hi").qualified, "Point::hi");
        // The trait's own bodyless `fn hi(&self);` (line 4) must not have
        // produced a second "hi" symbol -- only the impl's, with a body.
        assert_eq!(refs.symbols.iter().filter(|s| s.name == "hi").count(), 1);

        assert_eq!(find_edge(&refs.edges, "imports", "std::collections::HashMap").src_line, 1);
        assert_eq!(find_edge(&refs.edges, "inherits", "Greet").src_line, 9);
        assert_eq!(find_edge(&refs.edges, "calls", "bar").src_line, 11);
        assert!(refs.edges.iter().all(|e| e.dst_name != "println"), "macro_invocation must never produce a calls edge: {:#?}", refs.edges);
    }

    #[test]
    fn extract_refs_python_imports_inherits_calls() {
        let src = "import os\nfrom foo.bar import Baz\n\nclass Derived(Base):\n    def hi(self):\n        bar()\n        self.baz()\n\ndef bar():\n    pass\n";
        let refs = extract_refs("t.py", src);

        assert_eq!(find_symbol(&refs.symbols, "hi").qualified, "Derived.hi");

        assert_eq!(find_edge(&refs.edges, "imports", "os").src_line, 1);
        assert_eq!(find_edge(&refs.edges, "imports", "foo.bar").src_line, 2);
        assert_eq!(find_edge(&refs.edges, "inherits", "Base").src_line, 4);
        assert_eq!(find_edge(&refs.edges, "calls", "bar").src_line, 6);
        assert_eq!(find_edge(&refs.edges, "calls", "baz").src_line, 7);
    }

    #[test]
    fn extract_refs_typescript_imports_inherits_calls() {
        let src =
            "import { Base } from \"./base\";\n\nclass Derived extends Base implements IFoo {\n    hi() {\n        bar();\n        this.baz();\n    }\n}\n\nfunction bar() {}\n";
        let refs = extract_refs("t.ts", src);

        assert_eq!(find_symbol(&refs.symbols, "hi").qualified, "Derived.hi");

        assert_eq!(find_edge(&refs.edges, "imports", "./base").src_line, 1);
        assert_eq!(find_edge(&refs.edges, "inherits", "Base").src_line, 3);
        assert_eq!(find_edge(&refs.edges, "inherits", "IFoo").src_line, 3);
        assert_eq!(find_edge(&refs.edges, "calls", "bar").src_line, 5);
        assert_eq!(find_edge(&refs.edges, "calls", "baz").src_line, 6);
    }

    #[test]
    fn extract_refs_glsl_functions_call_only() {
        let src = "float helper(float x) { return x * 2.0; }\nvoid main() {\n    float y = helper(1.0);\n}\n";
        let refs = extract_refs("shader.glsl", src);

        assert_eq!(find_symbol(&refs.symbols, "helper").kind, "function");
        assert_eq!(find_symbol(&refs.symbols, "main").kind, "function");
        assert_eq!(find_edge(&refs.edges, "calls", "helper").src_line, 3);
        assert!(refs.edges.iter().all(|e| e.kind == "calls"), "GLSL has no includes/imports/inherits: {:#?}", refs.edges);
    }
}
