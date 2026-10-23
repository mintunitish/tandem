//! Tree-sitter symbol lookup for rs/ts/tsx/js/py/go.
//!
//! [`enclosing_symbols`] answers "which definitions does this line range touch":
//! it returns the *innermost* definitions overlapping the range. An outer symbol
//! (e.g. a class) is only reported when part of the range lies in it but outside
//! all of its nested definitions.

use crate::hunks;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tandem_core::{FileDiff, Hunk};
use tree_sitter::{Node, Parser, Tree};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lang { Rust, TypeScript, Tsx, JavaScript, Python, Go }

impl Lang {
    /// Detect from the file extension.
    pub fn from_path(path: impl AsRef<Path>) -> Option<Lang> {
        let ext = path.as_ref().extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "rs" => Lang::Rust,
            "ts" | "mts" | "cts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
            "py" | "pyi" => Lang::Python,
            "go" => Lang::Go,
            _ => return None,
        })
    }

    pub fn ts_language(self) -> tree_sitter::Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Fn,
    /// Classes and class-like types: struct/enum/trait/impl/interface/type spec.
    Class,
    Method,
    Const,
}

/// 1-based inclusive line range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LineRange { pub start: u32, pub end: u32 }

impl LineRange {
    pub fn new(start: u32, end: u32) -> Self { Self { start, end: end.max(start) } }
    pub fn overlaps(&self, o: &LineRange) -> bool { self.start <= o.end && o.start <= self.end }
    pub fn contains(&self, o: &LineRange) -> bool { self.start <= o.start && o.end <= self.end }
}

impl std::fmt::Display for LineRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.start == self.end { write!(f, "L{}", self.start) } else { write!(f, "L{}-{}", self.start, self.end) }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SymbolRef {
    pub name: String,
    pub kind: SymbolKind,
    pub range: LineRange,
    /// Name of the nearest enclosing symbol (class/impl/fn), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

impl SymbolRef {
    /// `Container.name` or `name`.
    pub fn qualified(&self) -> String {
        match &self.container { Some(c) => format!("{c}.{}", self.name), None => self.name.clone() }
    }
}

/// A parsed source file, reusable for many range queries.
pub struct ParsedSource {
    lang: Lang,
    symbols: Vec<SymbolRef>,
    /// For each symbol, indices of its direct nested symbols.
    children: Vec<Vec<usize>>,
}

impl ParsedSource {
    pub fn parse(lang: Lang, source: &str) -> Option<Self> {
        let mut parser = Parser::new();
        parser.set_language(&lang.ts_language()).ok()?;
        let tree: Tree = parser.parse(source, None)?;
        let mut out = ParsedSource { lang, symbols: Vec::new(), children: Vec::new() };
        out.collect(tree.root_node(), source.as_bytes(), None);
        Some(out)
    }

    pub fn lang(&self) -> Lang { self.lang }

    /// Every definition in the file, in document order (outer before inner).
    pub fn symbols(&self) -> &[SymbolRef] { &self.symbols }

    fn collect(&mut self, node: Node, src: &[u8], parent: Option<usize>) {
        let mut parent = parent;
        if let Some((name, kind)) = classify(self.lang, node, src) {
            let container = parent.map(|p| &self.symbols[p]);
            let kind = match (kind, container.map(|c| c.kind)) {
                (SymbolKind::Fn, Some(SymbolKind::Class)) => SymbolKind::Method,
                (k, _) => k,
            };
            let sym = SymbolRef {
                name,
                kind,
                range: LineRange::new(node.start_position().row as u32 + 1, end_line(node)),
                container: container.map(|c| c.name.clone()),
            };
            let idx = self.symbols.len();
            self.symbols.push(sym);
            self.children.push(Vec::new());
            if let Some(p) = parent { self.children[p].push(idx) }
            parent = Some(idx);
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.collect(child, src, parent);
        }
    }

    /// Innermost symbols overlapping `range` (1-based inclusive).
    pub fn enclosing(&self, range: LineRange) -> Vec<SymbolRef> {
        let mut out = Vec::new();
        for (i, s) in self.symbols.iter().enumerate() {
            if !s.range.overlaps(&range) { continue }
            let lo = s.range.start.max(range.start);
            let hi = s.range.end.min(range.end);
            let kids: Vec<&LineRange> = self.children[i].iter().map(|&c| &self.symbols[c].range).collect();
            let uncovered = (lo..=hi).any(|l| !kids.iter().any(|k| k.start <= l && l <= k.end));
            if uncovered { out.push(s.clone()) }
        }
        out
    }
}

fn end_line(node: Node) -> u32 {
    let end = node.end_position();
    // A node ending at column 0 really ends on the previous line.
    let row = if end.column == 0 && end.row > node.start_position().row { end.row - 1 } else { end.row };
    row as u32 + 1
}

/// Innermost definitions overlapping `line_range` (1-based inclusive) in `source`.
pub fn enclosing_symbols(lang: Lang, source: &str, line_range: LineRange) -> Vec<SymbolRef> {
    ParsedSource::parse(lang, source).map(|p| p.enclosing(line_range)).unwrap_or_default()
}

fn text(node: Node, src: &[u8]) -> Option<String> {
    node.utf8_text(src).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn field_text(node: Node, field: &str, src: &[u8]) -> Option<String> {
    node.child_by_field_name(field).and_then(|n| text(n, src))
}

fn is_upper_const(name: &str) -> bool {
    name.chars().any(|c| c.is_ascii_uppercase())
        && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn classify(lang: Lang, node: Node, src: &[u8]) -> Option<(String, SymbolKind)> {
    use SymbolKind::*;
    let kind = node.kind();
    match lang {
        Lang::Rust => match kind {
            "function_item" | "function_signature_item" => Some((field_text(node, "name", src)?, Fn)),
            "struct_item" | "enum_item" | "trait_item" | "union_item" => Some((field_text(node, "name", src)?, Class)),
            "impl_item" => {
                // `impl Trait for Type` and `impl Type` both group methods under the type.
                let ty = node.child_by_field_name("type")?;
                let name = ty.child_by_field_name("type").and_then(|n| text(n, src)).or_else(|| text(ty, src))?;
                Some((name, Class))
            }
            "const_item" | "static_item" => Some((field_text(node, "name", src)?, Const)),
            "macro_definition" => Some((field_text(node, "name", src)?, Fn)),
            _ => None,
        },
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => match kind {
            "function_declaration" | "generator_function_declaration" => Some((field_text(node, "name", src)?, Fn)),
            "class_declaration" | "abstract_class_declaration" | "interface_declaration" | "enum_declaration" => {
                Some((field_text(node, "name", src)?, Class))
            }
            "class" => Some((field_text(node, "name", src)?, Class)),
            "method_definition" => Some((field_text(node, "name", src)?, Method)),
            "public_field_definition" | "field_definition" => {
                let value = node.child_by_field_name("value")?;
                if !matches!(value.kind(), "arrow_function" | "function_expression" | "function") { return None }
                let name = field_text(node, "name", src).or_else(|| field_text(node, "property", src))?;
                Some((name, Method))
            }
            "variable_declarator" => {
                let name_node = node.child_by_field_name("name")?;
                if name_node.kind() != "identifier" { return None }
                let name = text(name_node, src)?;
                let value = node.child_by_field_name("value");
                if value.is_some_and(|v| matches!(v.kind(), "arrow_function" | "function_expression" | "function")) {
                    return Some((name, Fn));
                }
                // Top-level `const X = ...` only.
                let decl = node.parent()?;
                let is_const = decl.kind() == "lexical_declaration" && decl.child(0).is_some_and(|c| c.kind() == "const");
                let top = decl.parent().is_some_and(|p| matches!(p.kind(), "program" | "export_statement"));
                (is_const && top).then_some((name, Const))
            }
            _ => None,
        },
        Lang::Python => match kind {
            "function_definition" => Some((field_text(node, "name", src)?, Fn)),
            "class_definition" => Some((field_text(node, "name", src)?, Class)),
            "assignment" => {
                // Module-level UPPER_CASE = ...
                let stmt = node.parent().filter(|p| p.kind() == "expression_statement")?;
                stmt.parent().filter(|p| p.kind() == "module")?;
                let left = node.child_by_field_name("left").filter(|l| l.kind() == "identifier")?;
                let name = text(left, src)?;
                is_upper_const(&name).then_some((name, Const))
            }
            _ => None,
        },
        Lang::Go => match kind {
            "function_declaration" => Some((field_text(node, "name", src)?, Fn)),
            "method_declaration" => Some((field_text(node, "name", src)?, Method)),
            "type_spec" => Some((field_text(node, "name", src)?, Class)),
            "const_spec" => Some((field_text(node, "name", src)?, Const)),
            _ => None,
        },
    }
}

/// Symbols touched by each hunk of `file` (index-aligned with `file.hunks`).
/// Removed lines are resolved against `old_text`, added lines against `new_text`.
/// Returns empty lists for unsupported languages or missing texts.
pub fn hunk_symbols(file: &FileDiff) -> Vec<Vec<SymbolRef>> {
    let empty = || vec![Vec::new(); file.hunks.len()];
    let Some(lang) = Lang::from_path(&file.path).or_else(|| file.old_path.as_deref().and_then(Lang::from_path)) else {
        return empty();
    };
    if file.binary { return empty() }
    let old = file.old_text.as_deref().and_then(|t| ParsedSource::parse(lang, t));
    let new = file.new_text.as_deref().and_then(|t| ParsedSource::parse(lang, t));
    file.hunks.iter().map(|h| symbols_for_hunk(h, old.as_ref(), new.as_ref())).collect()
}

fn symbols_for_hunk(h: &Hunk, old: Option<&ParsedSource>, new: Option<&ParsedSource>) -> Vec<SymbolRef> {
    let (o, n) = hunks::changed_line_ranges(h);
    let mut out: Vec<SymbolRef> = Vec::new();
    let mut push_all = |syms: Vec<SymbolRef>| {
        for s in syms {
            if !out.iter().any(|e| e.name == s.name && e.kind == s.kind && e.container == s.container) {
                out.push(s);
            }
        }
    };
    // New-side first so ranges reflect the post-change file when both exist.
    if let (Some((a, b)), Some(p)) = (n, new) { push_all(p.enclosing(LineRange::new(a as u32 + 1, b as u32 + 1))) }
    if let (Some((a, b)), Some(p)) = (o, old) { push_all(p.enclosing(LineRange::new(a as u32 + 1, b as u32 + 1))) }
    out
}
