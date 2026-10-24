//! Tree-sitter symbol index: Go to Symbol (⌘O) and File Structure (⌘F12).
//!
//! Languages: Rust, TypeScript, TSX, JavaScript/JSX, Python, Go. Definitions are found with
//! tag-style queries (`@def.<kind>` + `@name`); containers are resolved by walking ancestors.

use crate::fuzzy::{Query, Scratch, score_name};
use crate::watcher::{FsEvent, FsEventKind};
use crate::is_excluded_path;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Parser, Query as TsQuery, QueryCursor};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Trait,
    Interface,
    TypeAlias,
    Const,
    Variable,
    Module,
    Macro,
}

impl SymbolKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::TypeAlias => "type",
            Self::Const => "const",
            Self::Variable => "variable",
            Self::Module => "module",
            Self::Macro => "macro",
        }
    }
}

impl fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A source range: 0-based lines, byte columns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

impl From<Node<'_>> for Span {
    fn from(n: Node<'_>) -> Self {
        let (s, e) = (n.start_position(), n.end_position());
        Span { start_byte: n.start_byte(), end_byte: n.end_byte(), start_line: s.row, start_col: s.column, end_line: e.row, end_col: e.column }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// The whole definition.
    pub range: Span,
    /// Just the identifier (where the caret goes on navigation).
    pub selection: Span,
    /// Enclosing class / impl / trait / module / function name.
    pub container: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolMatch {
    pub path: PathBuf,
    pub symbol: Symbol,
    pub score: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lang {
    Rust,
    Ts,
    Tsx,
    Js,
    Python,
    Go,
}

impl Lang {
    fn from_path(p: &Path) -> Option<Self> {
        Some(match p.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "rs" => Self::Rust,
            "ts" | "mts" | "cts" => Self::Ts,
            "tsx" => Self::Tsx,
            "js" | "jsx" | "mjs" | "cjs" => Self::Js,
            "py" | "pyi" => Self::Python,
            "go" => Self::Go,
            _ => return None,
        })
    }

    fn language(self) -> Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Js => tree_sitter_javascript::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }

    fn query_src(self) -> &'static str {
        match self {
            Self::Rust => RUST_Q,
            Self::Ts | Self::Tsx => TS_Q,
            Self::Js => JS_Q,
            Self::Python => PY_Q,
            Self::Go => GO_Q,
        }
    }

    fn query(self) -> &'static TsQuery {
        static CELLS: [OnceLock<TsQuery>; 6] = [const { OnceLock::new() }; 6];
        CELLS[self as usize].get_or_init(|| {
            TsQuery::new(&self.language(), self.query_src()).unwrap_or_else(|e| panic!("bad {self:?} symbol query: {e}"))
        })
    }
}

const RUST_Q: &str = r#"
(function_item name: (identifier) @name) @def.function
(function_signature_item name: (identifier) @name) @def.function
(struct_item name: (type_identifier) @name) @def.struct
(union_item name: (type_identifier) @name) @def.struct
(enum_item name: (type_identifier) @name) @def.enum
(trait_item name: (type_identifier) @name) @def.trait
(type_item name: (type_identifier) @name) @def.type
(const_item name: (identifier) @name) @def.const
(static_item name: (identifier) @name) @def.const
(mod_item name: (identifier) @name) @def.module
(macro_definition name: (identifier) @name) @def.macro
"#;

const TS_Q: &str = r#"
(function_declaration name: (identifier) @name) @def.function
(generator_function_declaration name: (identifier) @name) @def.function
(function_signature name: (identifier) @name) @def.function
(class_declaration name: (type_identifier) @name) @def.class
(abstract_class_declaration name: (type_identifier) @name) @def.class
(method_definition name: (_) @name) @def.method
(abstract_method_signature name: (_) @name) @def.method
(method_signature name: (_) @name) @def.method
(public_field_definition name: (_) @name) @def.field
(interface_declaration name: (type_identifier) @name) @def.interface
(type_alias_declaration name: (type_identifier) @name) @def.type
(enum_declaration name: (identifier) @name) @def.enum
(internal_module name: (_) @name) @def.module
(variable_declarator name: (identifier) @name) @def.variable
"#;

const JS_Q: &str = r#"
(function_declaration name: (identifier) @name) @def.function
(generator_function_declaration name: (identifier) @name) @def.function
(class_declaration name: (identifier) @name) @def.class
(method_definition name: (_) @name) @def.method
(field_definition property: (_) @name) @def.field
(variable_declarator name: (identifier) @name) @def.variable
"#;

const PY_Q: &str = r#"
(function_definition name: (identifier) @name) @def.function
(class_definition name: (identifier) @name) @def.class
(module (expression_statement (assignment left: (identifier) @name)) @def.variable)
"#;

const GO_Q: &str = r#"
(function_declaration name: (identifier) @name) @def.function
(method_declaration name: (field_identifier) @name) @def.method
(type_spec name: (type_identifier) @name) @def.type
(const_spec name: (identifier) @name) @def.const
(var_spec name: (identifier) @name) @def.variable
"#;

fn text<'a>(n: Node<'_>, src: &'a [u8]) -> &'a str {
    std::str::from_utf8(&src[n.start_byte()..n.end_byte()]).unwrap_or("")
}

fn first_descendant<'t>(n: Node<'t>, kind: &str) -> Option<Node<'t>> {
    if n.kind() == kind {
        return Some(n);
    }
    let mut c = n.walk();
    n.children(&mut c).find_map(|ch| first_descendant(ch, kind))
}

/// `(name, is_type_like)` if `n` is a container node.
fn container_name(lang: Lang, n: Node<'_>, src: &[u8]) -> Option<(String, bool)> {
    let by_field = |f: &str| n.child_by_field_name(f).map(|x| text(x, src).to_string());
    match (lang, n.kind()) {
        (Lang::Rust, "impl_item") => {
            let t = n.child_by_field_name("type")?;
            let t = if t.kind() == "generic_type" { t.child_by_field_name("type").unwrap_or(t) } else { t };
            Some((text(t, src).to_string(), true))
        }
        (Lang::Rust, "trait_item") => by_field("name").map(|s| (s, true)),
        (Lang::Rust, "mod_item") => by_field("name").map(|s| (s, false)),
        (Lang::Ts | Lang::Tsx | Lang::Js, "class_declaration" | "abstract_class_declaration" | "interface_declaration") => {
            by_field("name").map(|s| (s, true))
        }
        (Lang::Ts | Lang::Tsx, "internal_module" | "enum_declaration") => by_field("name").map(|s| (s, false)),
        (Lang::Python, "class_definition") => by_field("name").map(|s| (s, true)),
        (Lang::Python, "function_definition") => by_field("name").map(|s| (s, false)),
        _ => None,
    }
}

fn go_receiver(n: Node<'_>, src: &[u8]) -> Option<String> {
    let recv = n.child_by_field_name("receiver")?;
    first_descendant(recv, "type_identifier").map(|t| text(t, src).to_string())
}

fn has_ancestor(n: Node<'_>, kinds: &[&str]) -> bool {
    let mut cur = n.parent();
    while let Some(p) = cur {
        if kinds.contains(&p.kind()) {
            return true;
        }
        cur = p.parent();
    }
    false
}

/// Extract the symbols of one source file, ordered by position. `None` for unsupported extensions.
pub fn extract_symbols(path: &Path, source: &str) -> Option<Vec<Symbol>> {
    let lang = Lang::from_path(path)?;
    let mut parser = Parser::new();
    parser.set_language(&lang.language()).ok()?;
    let tree = parser.parse(source, None)?;
    let src = source.as_bytes();
    let query = lang.query();
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut out: Vec<Symbol> = Vec::new();
    let mut matches = cursor.matches(query, tree.root_node(), src);
    while let Some(m) = matches.next() {
        let (mut def, mut name, mut kind_str) = (None, None, "");
        for c in m.captures {
            let cn = names[c.index as usize];
            if cn == "name" {
                name = Some(c.node);
            } else if let Some(k) = cn.strip_prefix("def.") {
                def = Some(c.node);
                kind_str = k;
            }
        }
        let (Some(def), Some(name)) = (def, name) else { continue };
        if let Some(sym) = build_symbol(lang, def, name, kind_str, src) {
            out.push(sym);
        }
    }
    out.sort_by_key(|s| (s.range.start_byte, s.selection.start_byte));
    out.dedup_by_key(|s| s.selection.start_byte);
    Some(out)
}

fn build_symbol(lang: Lang, def: Node<'_>, name: Node<'_>, kind_str: &str, src: &[u8]) -> Option<Symbol> {
    let mut def = def;
    let mut kind = match kind_str {
        "function" => SymbolKind::Function,
        "method" => SymbolKind::Method,
        "class" => SymbolKind::Class,
        "struct" => SymbolKind::Struct,
        "enum" => SymbolKind::Enum,
        "trait" => SymbolKind::Trait,
        "interface" => SymbolKind::Interface,
        "type" => SymbolKind::TypeAlias,
        "const" => SymbolKind::Const,
        "variable" | "field" => SymbolKind::Variable,
        "module" => SymbolKind::Module,
        "macro" => SymbolKind::Macro,
        _ => return None,
    };
    let mut container: Option<(String, bool)> = None;

    match (lang, def.kind()) {
        // JS/TS: only top-level variables; function-valued ones become functions.
        (Lang::Ts | Lang::Tsx | Lang::Js, "variable_declarator") => {
            let decl = def.parent()?;
            let top = decl.parent().is_some_and(|p| matches!(p.kind(), "program" | "export_statement"));
            if !top {
                return None;
            }
            let value = def.child_by_field_name("value").map(|v| v.kind());
            kind = if matches!(value, Some("arrow_function" | "function_expression" | "function" | "generator_function")) {
                SymbolKind::Function
            } else if text(decl, src).starts_with("const") {
                SymbolKind::Const
            } else {
                SymbolKind::Variable
            };
            def = decl;
        }
        // Class fields: keep only function-valued ones (arrow-function methods).
        (Lang::Ts | Lang::Tsx, "public_field_definition") | (Lang::Js, "field_definition") => {
            let v = def.child_by_field_name("value")?;
            if !matches!(v.kind(), "arrow_function" | "function_expression" | "function") {
                return None;
            }
            kind = SymbolKind::Method;
        }
        (Lang::Go, "method_declaration") => container = go_receiver(def, src).map(|s| (s, true)),
        (Lang::Go, "type_spec") => {
            kind = match def.child_by_field_name("type").map(|t| t.kind()) {
                Some("struct_type") => SymbolKind::Struct,
                Some("interface_type") => SymbolKind::Interface,
                _ => SymbolKind::TypeAlias,
            };
        }
        (Lang::Go, "const_spec" | "var_spec") => {
            if has_ancestor(def, &["function_declaration", "method_declaration", "func_literal"]) {
                return None;
            }
        }
        (Lang::Python, "expression_statement") => {
            let n = text(name, src);
            kind = if n.chars().any(char::is_alphabetic) && n == n.to_uppercase() { SymbolKind::Const } else { SymbolKind::Variable };
        }
        _ => {}
    }

    if container.is_none() {
        let mut cur = def.parent();
        while let Some(p) = cur {
            if let Some(c) = container_name(lang, p, src) {
                container = Some(c);
                break;
            }
            cur = p.parent();
        }
    }
    if kind == SymbolKind::Function && container.as_ref().is_some_and(|c| c.1) {
        kind = SymbolKind::Method;
    }
    Some(Symbol {
        name: text(name, src).trim_matches(['"', '\'']).to_string(),
        kind,
        range: def.into(),
        selection: name.into(),
        container: container.map(|c| c.0),
    })
}

/// Per-file symbol tables for a project.
#[derive(Default)]
pub struct SymbolIndex {
    root: PathBuf,
    files: HashMap<PathBuf, Vec<Symbol>>,
}

impl SymbolIndex {
    /// Parse every supported file under `root` (ignore-aware), in parallel.
    pub fn build(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let files = scan(&root);
        Self { root, files }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn symbol_count(&self) -> usize {
        self.files.values().map(Vec::len).sum()
    }

    fn key(&self, path: &Path) -> PathBuf {
        if path.is_absolute() { path.to_path_buf() } else { self.root.join(path) }
    }

    /// Re-read `path` from disk; removes it from the index if it vanished or is unsupported.
    pub fn update(&mut self, path: &Path) {
        let key = self.key(path);
        match std::fs::read_to_string(&key).ok().and_then(|s| extract_symbols(&key, &s)) {
            Some(syms) => {
                self.files.insert(key, syms);
            }
            None => {
                self.files.remove(&key);
            }
        }
    }

    /// Index unsaved editor content for `path`.
    pub fn update_source(&mut self, path: &Path, source: &str) {
        let key = self.key(path);
        if let Some(syms) = extract_symbols(&key, source) {
            self.files.insert(key, syms);
        }
    }

    /// Drop a file, or every file below a directory.
    pub fn remove(&mut self, path: &Path) {
        let key = self.key(path);
        self.files.retain(|k, _| !k.starts_with(&key));
    }

    /// Keep the index in sync with watcher events.
    pub fn apply_event(&mut self, ev: &FsEvent) {
        if ev.kind == FsEventKind::Removed {
            ev.paths.iter().for_each(|p| self.remove(p));
            return;
        }
        for p in &ev.paths {
            if is_excluded_path(&self.root, p) {
                continue;
            }
            if p.is_dir() {
                self.files.extend(scan(p));
            } else if Lang::from_path(p).is_some() {
                self.update(p);
            }
        }
    }

    /// Symbols of one file in source order (File Structure).
    pub fn file_structure(&self, path: &Path) -> &[Symbol] {
        self.files.get(&self.key(path)).map_or(&[], Vec::as_slice)
    }

    /// Go to Symbol: fuzzy match over every indexed symbol name.
    pub fn fuzzy_symbols(&self, query: &str, limit: usize) -> Vec<SymbolMatch> {
        let q = Query::new(query);
        if q.is_empty() || limit == 0 {
            return Vec::new();
        }
        let mut scratch = Scratch::default();
        let mut hits: Vec<(i32, &Path, &Symbol)> = Vec::new();
        for (path, syms) in &self.files {
            for s in syms {
                if let Some(sc) = score_name(&q.lower, s.name.as_bytes(), &mut scratch) {
                    let kind_boost = match s.kind {
                        SymbolKind::Class | SymbolKind::Struct | SymbolKind::Interface | SymbolKind::Trait | SymbolKind::Enum => 40,
                        SymbolKind::Function | SymbolKind::Method => 20,
                        _ => 0,
                    };
                    hits.push((sc + kind_boost, path, s));
                }
            }
        }
        hits.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.2.name.len().cmp(&b.2.name.len()))
                .then_with(|| a.1.cmp(b.1))
                .then_with(|| a.2.range.start_byte.cmp(&b.2.range.start_byte))
        });
        hits.truncate(limit);
        hits.into_iter().map(|(score, p, s)| SymbolMatch { path: p.to_path_buf(), symbol: s.clone(), score }).collect()
    }
}

fn scan(dir: &Path) -> HashMap<PathBuf, Vec<Symbol>> {
    let paths: Vec<PathBuf> = crate::walker(dir)
        .build()
        .flatten()
        .filter(|d| d.file_type().is_some_and(|t| t.is_file()) && Lang::from_path(d.path()).is_some())
        .map(|d| d.into_path())
        .collect();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
    let chunk = paths.len().div_ceil(threads).max(1);
    let mut files = HashMap::new();
    std::thread::scope(|s| {
        let handles: Vec<_> = paths
            .chunks(chunk)
            .map(|c| {
                s.spawn(move || {
                    c.iter()
                        .filter_map(|p| Some((p.clone(), extract_symbols(p, &std::fs::read_to_string(p).ok()?)?)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for h in handles {
            files.extend(h.join().unwrap_or_default());
        }
    });
    files
}
