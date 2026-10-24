//! Smaller tool windows: Problems (LSP diagnostics + the Find in Files and Usages result tabs),
//! Structure (LSP document symbols, else the tree-sitter `SymbolIndex`) and the placeholder for
//! tool windows not in v0.1.

use crate::assets::icon;
use crate::theme::{Palette, pal};
use gpui_kit::assets::IconName;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;

/// Tool windows that exist in the New UI layout but have no backend yet (Run, Bookmarks,
/// Notifications, Services).
pub struct NotAvailable;

impl Render for NotAvailable {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx);
        div().size_full().flex().items_center().justify_center().text_size(px(13.)).text_color(p.muted).child("Not available in v0.1")
    }
}

// ---------------------------------------------------------------------------------------------
// Problems (+ Find / Usages tabs)
// ---------------------------------------------------------------------------------------------

use crate::editor::EditorArea;
use crate::editor::find::{FindEvent, FindPanel};
use crate::editor::lsp::{LspHub, LspHubEvent};
use crate::editor::usages::{UsagesEvent, UsagesPanel};
use crate::model::project::{ProjectModel, ProjectModelEvent};
use std::path::PathBuf;
use std::sync::Arc;
use tandem_lsp::lsp_types::{self as lspt, DiagnosticSeverity};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultsTab {
    Problems,
    Find,
    Usages,
}

#[derive(Clone)]
enum ProblemRow {
    File(PathBuf, usize),
    Diag(PathBuf, Box<lspt::Diagnostic>),
}

/// Bottom "Problems" tool window: project diagnostics, plus the Find in Files and Usages
/// result tabs (they live here because tool-window ids are fixed in `stripes.rs`).
pub struct ProblemsPanel {
    root: PathBuf,
    editor: WeakEntity<EditorArea>,
    hub: Entity<LspHub>,
    pub find: Entity<FindPanel>,
    pub usages: Entity<UsagesPanel>,
    pub tab: ResultsTab,
    scroll: UniformListScrollHandle,
    /// Problem rows + (errors, warnings), rebuilt when diagnostics change (`rows_stale`).
    rows: Arc<Vec<ProblemRow>>,
    counts: (usize, usize),
    rows_stale: bool,
    _subs: Vec<Subscription>,
}

impl ProblemsPanel {
    pub fn new(editor: &Entity<EditorArea>, cx: &mut Context<Self>) -> Self {
        let e = editor.read(cx);
        let (root, hub, find, usages) = (e.root.clone(), e.lsp.clone(), e.find.clone(), e.usages.clone());
        let subs = vec![
            cx.subscribe(&hub, |this: &mut Self, _, ev: &LspHubEvent, cx| {
                if matches!(ev, LspHubEvent::Diagnostics(_)) {
                    this.rows_stale = true;
                }
                cx.notify();
            }),
            cx.subscribe(&find, |this: &mut Self, _, ev: &FindEvent, cx| {
                let FindEvent::Started = ev;
                this.tab = ResultsTab::Find;
                cx.notify();
            }),
            cx.subscribe(&usages, |this: &mut Self, _, ev: &UsagesEvent, cx| {
                let UsagesEvent::Updated = ev;
                this.tab = ResultsTab::Usages;
                cx.notify();
            }),
        ];
        Self {
            root,
            editor: editor.downgrade(),
            hub,
            find,
            usages,
            tab: ResultsTab::Problems,
            scroll: UniformListScrollHandle::new(),
            rows: Arc::default(),
            counts: (0, 0),
            rows_stale: true,
            _subs: subs,
        }
    }

    pub fn set_tab(&mut self, tab: ResultsTab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    fn rebuild_rows(&mut self, cx: &App) {
        let (mut errors, mut warnings) = (0, 0);
        let mut rows = Vec::new();
        for (path, diags) in &self.hub.read(cx).diagnostics {
            let mut diags = diags.clone();
            diags.sort_by_key(|d| (d.severity.map(s_rank).unwrap_or(3), d.range.start.line));
            rows.push(ProblemRow::File(path.clone(), diags.len()));
            for d in diags {
                match d.severity {
                    Some(DiagnosticSeverity::ERROR) => errors += 1,
                    Some(DiagnosticSeverity::WARNING) => warnings += 1,
                    _ => {}
                }
                rows.push(ProblemRow::Diag(path.clone(), Box::new(d)));
            }
        }
        self.rows = Arc::new(rows);
        self.counts = (errors, warnings);
        self.rows_stale = false;
    }
}

fn s_rank(s: DiagnosticSeverity) -> u8 {
    match s {
        DiagnosticSeverity::ERROR => 0,
        DiagnosticSeverity::WARNING => 1,
        DiagnosticSeverity::INFORMATION => 2,
        _ => 3,
    }
}

impl Render for ProblemsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        if self.rows_stale {
            self.rebuild_rows(cx);
        }
        let (rows, (errors, warnings)) = (self.rows.clone(), self.counts);
        let n_find = self.find.read(cx).total;
        let n_usages = self.usages.read(cx).usages.len();
        let tab = |id: &'static str, label: String, t: ResultsTab, this: &Self, cx: &mut Context<Self>| {
            let active = this.tab == t;
            div()
                .id(id)
                .px(px(10.))
                .h(px(24.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .cursor_pointer()
                .when(active, |d| d.bg(p.stripe_selected).text_color(p.text_bright))
                .when(!active, |d| d.text_color(p.text).hover(|s| s.bg(p.hover)))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| this.set_tab(t, cx)))
        };
        let header = h_flex()
            .h(px(30.))
            .flex_none()
            .px(px(8.))
            .gap(px(4.))
            .child(tab("res-problems", format!("Project Errors  {}", errors + warnings), ResultsTab::Problems, self, cx))
            .child(tab("res-find", if n_find > 0 { format!("Find  {n_find}") } else { "Find".into() }, ResultsTab::Find, self, cx))
            .child(tab("res-usages", if n_usages > 0 { format!("Usages  {n_usages}") } else { "Usages".into() }, ResultsTab::Usages, self, cx));
        let body: AnyElement = match self.tab {
            ResultsTab::Find => self.find.clone().into_any_element(),
            ResultsTab::Usages => self.usages.clone().into_any_element(),
            ResultsTab::Problems if rows.is_empty() => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(p.muted)
                .child(if self.hub.read(cx).missing.is_some() { "No problems found (some language servers are not installed)" } else { "No problems found" })
                .into_any_element(),
            ResultsTab::Problems => {
                let root = self.root.clone();
                let list_p = p.clone();
                uniform_list("problems", rows.len(), cx.processor(move |_this, range: std::ops::Range<usize>, _, cx| {
                    range
                        .map(|ix| match &rows[ix] {
                            ProblemRow::File(path, n) => {
                                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                                let dir = path.parent().and_then(|d| d.strip_prefix(&root).ok()).map(|d| d.display().to_string()).unwrap_or_default();
                                let (g, c) = crate::assets::file_icon(&name);
                                h_flex()
                                    .id(("prob-file", ix))
                                    .h(px(24.))
                                    .px(px(8.))
                                    .gap(px(6.))
                                    .child(icon(IconName::ChevronDown, 12., list_p.muted))
                                    .child(icon(g, 16., c))
                                    .child(div().text_color(list_p.text).child(name))
                                    .child(div().text_color(list_p.muted).child(format!("{dir}  {n} problem{}", if *n == 1 { "" } else { "s" })))
                                    .into_any_element()
                            }
                            ProblemRow::Diag(path, d) => {
                                let (glyph, color) = match d.severity {
                                    Some(DiagnosticSeverity::ERROR) => (IconName::CircleAlert, list_p.error),
                                    Some(DiagnosticSeverity::WARNING) => (IconName::TriangleAlert, list_p.warning),
                                    _ => (IconName::CircleDot, list_p.muted),
                                };
                                let code = d.code.as_ref().map(|c| match c {
                                    lspt::NumberOrString::Number(n) => format!("TS{n}: "),
                                    lspt::NumberOrString::String(s) => format!("{s}: "),
                                });
                                let msg = format!("{}{}", code.unwrap_or_default(), d.message.lines().next().unwrap_or(""));
                                let (path, pos) = (path.clone(), d.range.start);
                                h_flex()
                                    .id(("prob", ix))
                                    .h(px(24.))
                                    .pl(px(42.))
                                    .pr(px(8.))
                                    .gap(px(6.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(list_p.hover))
                                    .child(icon(glyph, 14., color))
                                    .child(div().text_color(list_p.text).whitespace_nowrap().overflow_hidden().child(msg))
                                    .child(div().text_color(list_p.muted).child(format!(":{}", pos.line + 1)))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let path = path.clone();
                                        let _ = this.editor.update(cx, |e, cx| e.open_at(path, pos, window, cx));
                                    }))
                                    .into_any_element()
                            }
                        })
                        .collect::<Vec<_>>()
                }))
                .size_full()
                .track_scroll(&self.scroll)
                .into_any_element()
            }
        };
        v_flex().size_full().text_size(px(13.)).child(header).child(div().flex_1().min_h_0().child(body))
    }
}

// ---------------------------------------------------------------------------------------------
// Structure
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct OutlineItem {
    pub depth: usize,
    pub name: String,
    pub kind: String,
    pub pos: lspt::Position,
}

fn kind_glyph(kind: &str, p: &Palette) -> (IconName, Hsla) {
    match kind {
        "class" | "struct" | "interface" | "enum" | "trait" | "type" | "module" | "namespace" => (IconName::Braces, p.accent),
        "function" | "method" | "constructor" | "macro" => (IconName::Code, p.warning),
        _ => (IconName::CircleDot, p.muted),
    }
}

fn lsp_kind(k: lspt::SymbolKind) -> &'static str {
    match k {
        lspt::SymbolKind::CLASS => "class",
        lspt::SymbolKind::STRUCT => "struct",
        lspt::SymbolKind::INTERFACE => "interface",
        lspt::SymbolKind::ENUM => "enum",
        lspt::SymbolKind::MODULE | lspt::SymbolKind::NAMESPACE | lspt::SymbolKind::PACKAGE => "module",
        lspt::SymbolKind::FUNCTION => "function",
        lspt::SymbolKind::METHOD => "method",
        lspt::SymbolKind::CONSTRUCTOR => "constructor",
        lspt::SymbolKind::FIELD | lspt::SymbolKind::PROPERTY => "field",
        lspt::SymbolKind::CONSTANT => "const",
        lspt::SymbolKind::TYPE_PARAMETER => "type",
        _ => "variable",
    }
}

/// Flatten an LSP `documentSymbol` response into indented outline rows.
pub fn outline_from_lsp(resp: lspt::DocumentSymbolResponse) -> Vec<OutlineItem> {
    fn walk(syms: &[lspt::DocumentSymbol], depth: usize, out: &mut Vec<OutlineItem>) {
        for s in syms {
            out.push(OutlineItem { depth, name: s.name.clone(), kind: lsp_kind(s.kind).into(), pos: s.selection_range.start });
            if let Some(children) = &s.children {
                walk(children, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    match resp {
        lspt::DocumentSymbolResponse::Nested(syms) => walk(&syms, 0, &mut out),
        lspt::DocumentSymbolResponse::Flat(infos) => out.extend(infos.into_iter().map(|i| OutlineItem {
            depth: usize::from(i.container_name.is_some()),
            name: i.name,
            kind: lsp_kind(i.kind).into(),
            pos: i.location.range.start,
        })),
    }
    out
}

/// Structure tool window (⌘7 / ⌘F12): outline of the active file.
pub struct StructurePanel {
    editor: WeakEntity<EditorArea>,
    model: Entity<ProjectModel>,
    hub: Entity<LspHub>,
    pub path: Option<PathBuf>,
    pub items: Vec<OutlineItem>,
    source: &'static str,
    _task: Task<()>,
    _subs: Vec<Subscription>,
}

impl StructurePanel {
    pub fn new(editor: &Entity<EditorArea>, model: &Entity<ProjectModel>, cx: &mut Context<Self>) -> Self {
        let hub = editor.read(cx).lsp.clone();
        let subs = vec![
            cx.subscribe(editor, |this: &mut Self, editor, ev: &crate::editor::EditorAreaEvent, cx| {
                use crate::editor::EditorAreaEvent::*;
                if matches!(ev, ActiveChanged(_) | Saved(_)) {
                    this.path = editor.read(cx).active_path();
                    this.refresh(cx);
                }
            }),
            cx.subscribe(model, |this: &mut Self, _, ev: &ProjectModelEvent, cx| {
                if matches!(ev, ProjectModelEvent::Indexed | ProjectModelEvent::Fs(_)) {
                    this.refresh(cx);
                }
            }),
            cx.subscribe(&hub, |this: &mut Self, _, ev: &LspHubEvent, cx| {
                if matches!(ev, LspHubEvent::Status) && this.source != "LSP" {
                    this.refresh(cx);
                }
            }),
        ];
        let path = editor.read(cx).active_path();
        let mut this = Self { editor: editor.downgrade(), model: model.clone(), hub, path, items: vec![], source: "", _task: Task::ready(()), _subs: subs };
        this.refresh(cx);
        this
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            self.items.clear();
            cx.notify();
            return;
        };
        let syms = self.model.read(cx).symbols.read().file_structure(&path).to_vec();
        self.items = syms
            .into_iter()
            .map(|s| OutlineItem {
                depth: usize::from(s.container.is_some()),
                name: s.name,
                kind: s.kind.label().into(),
                pos: lspt::Position::new(s.selection.start_line as u32, s.selection.start_col as u32),
            })
            .collect();
        self.source = "tree-sitter";
        let rx = self.hub.update(cx, |hub, _| hub.request(&path, None, |c, p| async move { c.document_symbols(&p).await }));
        if let Some(rx) = rx {
            self._task = cx.spawn(async move |this, cx| {
                if let Ok(Some(resp)) = crate::editor::lsp::recv(rx).await {
                    let items = outline_from_lsp(resp);
                    let _ = this.update(cx, |s, cx| {
                        if !items.is_empty() {
                            s.items = items;
                            s.source = "LSP";
                        }
                        cx.notify();
                    });
                }
            });
        }
        cx.notify();
    }
}

impl Render for StructurePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let title = self.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string());
        let empty = match &title {
            None => "No file is open",
            Some(_) if self.items.is_empty() => "No symbols",
            _ => "",
        };
        v_flex()
            .size_full()
            .text_size(px(13.))
            .child(
                h_flex()
                    .h(px(26.))
                    .flex_none()
                    .px(px(10.))
                    .gap(px(6.))
                    .text_color(p.muted)
                    .children(title.map(|t| div().text_color(p.text).child(t)))
                    .child(div().flex_1())
                    .child(div().text_size(px(11.)).child(self.source)),
            )
            .when(!empty.is_empty(), |d| d.child(div().p(px(12.)).text_color(p.muted).child(empty)))
            .child(
                v_flex()
                    .id("structure-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(self.items.iter().enumerate().map(|(ix, it)| {
                        let (g, c) = kind_glyph(&it.kind, &p);
                        let (path, pos) = (self.path.clone(), it.pos);
                        h_flex()
                            .id(("outline", ix))
                            .h(px(24.))
                            .pl(px(10. + it.depth as f32 * 18.))
                            .pr(px(8.))
                            .gap(px(6.))
                            .cursor_pointer()
                            .hover(|s| s.bg(p.hover))
                            .child(icon(g, 14., c))
                            .child(div().text_color(p.text).whitespace_nowrap().child(it.name.clone()))
                            .child(div().text_color(p.muted).text_size(px(11.)).child(it.kind.clone()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(path) = path.clone() {
                                    let _ = this.editor.update(cx, |e, cx| e.open_at(path, pos, window, cx));
                                }
                            }))
                    })),
            )
    }
}
