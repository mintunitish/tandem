//! Search Everywhere (⇧⇧): one popup with All / Files / Symbols / Text / Actions tabs.
//! Files and symbols come from `tandem_project`'s background `FileIndex` / `SymbolIndex`
//! (kept live by the watcher, see `model::project`), text from streaming `find_in_files`,
//! actions from the keymap table. An empty Files query lists Recent Files (⌘E).

use crate::assets::{file_icon, icon};
use crate::editor::find;
use crate::keymap::{self, POPUP};
use crate::model::project::{ProjectModel, ProjectModelEvent};
use crate::theme::{MONO_FONT, Palette, pal};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use std::path::{Path, PathBuf};
use tandem_project::{CancelToken, FileIndex, SymbolIndex, find_in_files};

pub const MAX_RESULTS: usize = 60;
const MAX_TEXT_RESULTS: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchTab {
    All,
    Files,
    Symbols,
    Text,
    Actions,
}

impl SearchTab {
    const ALL: [SearchTab; 5] = [SearchTab::All, SearchTab::Files, SearchTab::Symbols, SearchTab::Text, SearchTab::Actions];

    fn label(self) -> &'static str {
        match self {
            SearchTab::All => "All",
            SearchTab::Files => "Files",
            SearchTab::Symbols => "Symbols",
            SearchTab::Text => "Text",
            SearchTab::Actions => "Actions",
        }
    }
}

#[derive(Clone)]
pub enum SearchItem {
    /// `path` is project-relative; `matched` are byte offsets into it.
    File { path: PathBuf, matched: Vec<usize> },
    Symbol { path: PathBuf, name: String, kind: &'static str, container: Option<String>, line: u32, col: u32 },
    Text { path: PathBuf, line: u32, col: u32, text: String, ranges: Vec<std::ops::Range<usize>> },
    Action { name: &'static str, keys: &'static str, action: fn() -> Box<dyn Action> },
}

pub enum SearchEvent {
    OpenFile(PathBuf),
    /// Open at a 0-based line / column.
    OpenAt(PathBuf, u32, u32),
    RunAction(Box<dyn Action>),
    /// "Open in Find Window" from the Text tab.
    OpenInFindWindow(String),
    Dismiss,
}

pub struct SearchPopup {
    pub tab: SearchTab,
    pub query: Entity<InputState>,
    model: Entity<ProjectModel>,
    root: PathBuf,
    /// Recent files (relative, most recent first, current file excluded).
    recent: Vec<PathBuf>,
    pub results: Vec<SearchItem>,
    pub selected: usize,
    text_results: Vec<SearchItem>,
    text_query: String,
    text_searching: bool,
    text_cancel: CancelToken,
    text_task: Task<()>,
    scroll: UniformListScrollHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SearchEvent> for SearchPopup {}

impl Focusable for SearchPopup {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }
}

fn rel(root: &Path, p: &Path) -> PathBuf {
    p.strip_prefix(root).map(Path::to_path_buf).unwrap_or_else(|_| p.to_path_buf())
}

impl SearchPopup {
    pub fn new(tab: SearchTab, model: Entity<ProjectModel>, current: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Type / to see commands"));
        let root = model.read(cx).root.clone();
        let recent: Vec<PathBuf> =
            model.read(cx).recent_files().into_iter().filter(|p| Some(p) != current.as_ref()).map(|p| rel(&root, &p)).collect();
        let subs = vec![
            cx.subscribe(&query, |this: &mut Self, _, ev: &InputEvent, cx| match ev {
                InputEvent::Change => {
                    this.selected = 0;
                    this.refresh(cx);
                }
                InputEvent::PressEnter { secondary, .. } => {
                    if *secondary && this.tab == SearchTab::Text {
                        cx.emit(SearchEvent::OpenInFindWindow(this.query_text(cx)));
                    } else {
                        this.confirm(cx);
                    }
                }
                _ => {}
            }),
            cx.subscribe(&model, |this: &mut Self, _, ev: &ProjectModelEvent, cx| {
                if matches!(ev, ProjectModelEvent::Indexed) {
                    this.refresh(cx);
                }
            }),
        ];
        let mut this = Self {
            tab,
            query,
            model,
            root,
            recent,
            results: vec![],
            selected: 0,
            text_results: vec![],
            text_query: String::new(),
            text_searching: false,
            text_cancel: CancelToken::new(),
            text_task: Task::ready(()),
            scroll: UniformListScrollHandle::new(),
            _subs: subs,
        };
        this.refresh(cx);
        this
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |s, cx| s.focus(window, cx));
    }

    pub fn set_tab(&mut self, tab: SearchTab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.selected = 0;
        self.refresh(cx);
    }

    pub fn set_query(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |s, cx| s.set_value(text, window, cx));
        self.refresh(cx);
    }

    fn query_text(&self, cx: &App) -> String {
        self.query.read(cx).value().trim().to_string()
    }

    fn indexing(&self, cx: &App) -> bool {
        let m = self.model.read(cx);
        match self.tab {
            SearchTab::Files | SearchTab::All => !m.files_ready,
            SearchTab::Symbols => !m.symbols_ready,
            SearchTab::Text => self.text_searching,
            SearchTab::Actions => false,
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let q = self.query_text(cx);
        if self.tab == SearchTab::Text {
            self.start_text_search(&q, cx);
            self.results = self.text_results.clone();
        } else {
            let m = self.model.read(cx);
            let files = m.files.read();
            let symbols = m.symbols.read();
            self.results = rank(&q, self.tab, &files, &symbols, &self.recent);
        }
        self.selected = self.selected.min(self.results.len().saturating_sub(1));
        cx.notify();
    }

    fn start_text_search(&mut self, q: &str, cx: &mut Context<Self>) {
        if q == self.text_query {
            return;
        }
        self.text_cancel.cancel();
        self.text_cancel = CancelToken::new();
        self.text_query = q.to_string();
        self.text_results.clear();
        if q.chars().count() < 2 {
            self.text_searching = false;
            self.text_task = Task::ready(());
            return;
        }
        let opts = tandem_project::SearchOptions { max_results: MAX_TEXT_RESULTS, ..find::options(false, false, false) };
        let Ok(rx) = find_in_files(&self.root, q, &opts, self.text_cancel.clone()) else { return };
        self.text_searching = true;
        let root = self.root.clone();
        self.text_task = cx.spawn(async move |this, cx| {
            while let Ok(first) = rx.recv().await {
                let mut batch = vec![first];
                while let Ok(m) = rx.try_recv() {
                    batch.push(m);
                }
                let items: Vec<SearchItem> = batch
                    .into_iter()
                    .map(|m| SearchItem::Text { path: rel(&root, &m.path), line: m.line_no.saturating_sub(1) as u32, col: m.col as u32, text: m.line_text, ranges: m.ranges })
                    .collect();
                let alive = this.update(cx, |s, cx| {
                    s.text_results.extend(items);
                    if s.tab == SearchTab::Text {
                        s.results = s.text_results.clone();
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |s, cx| {
                s.text_searching = false;
                cx.notify();
            });
        });
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let n = self.results.len() as isize;
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
        self.scroll.scroll_to_item(self.selected, ScrollStrategy::Top);
        cx.notify();
    }

    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        match self.results.get(self.selected).cloned() {
            Some(SearchItem::File { path, .. }) => cx.emit(SearchEvent::OpenFile(path)),
            Some(SearchItem::Symbol { path, line, col, .. }) | Some(SearchItem::Text { path, line, col, .. }) => cx.emit(SearchEvent::OpenAt(path, line, col)),
            Some(SearchItem::Action { action, .. }) => cx.emit(SearchEvent::RunAction(action())),
            None => {}
        }
    }

    fn cycle_tab(&mut self, back: bool, cx: &mut Context<Self>) {
        let n = SearchTab::ALL.len();
        let ix = SearchTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        let next = if back { (ix + n - 1) % n } else { (ix + 1) % n };
        self.set_tab(SearchTab::ALL[next], cx);
    }
}

/// Pure ranking used by the popup (and unit-tested). Text results stream separately.
pub fn rank(q: &str, tab: SearchTab, files: &FileIndex, symbols: &SymbolIndex, recent: &[PathBuf]) -> Vec<SearchItem> {
    let mut out: Vec<SearchItem> = Vec::new();
    let file_items = |limit: usize| -> Vec<SearchItem> {
        if q.is_empty() {
            recent.iter().take(limit).map(|p| SearchItem::File { path: p.clone(), matched: vec![] }).collect()
        } else {
            files.fuzzy_files(q, limit).into_iter().map(|m| SearchItem::File { path: PathBuf::from(m.path), matched: m.positions }).collect()
        }
    };
    let symbol_items = |limit: usize| -> Vec<SearchItem> {
        symbols
            .fuzzy_symbols(q, limit)
            .into_iter()
            .map(|m| SearchItem::Symbol {
                path: m.path,
                name: m.symbol.name,
                kind: m.symbol.kind.label(),
                container: m.symbol.container,
                line: m.symbol.selection.start_line as u32,
                col: m.symbol.selection.start_col as u32,
            })
            .collect()
    };
    let action_items = |limit: usize| -> Vec<SearchItem> {
        let mut scored: Vec<(i64, SearchItem)> = keymap::entries()
            .into_iter()
            .filter_map(|e| {
                let score = if q.is_empty() { Some(0) } else { crate::model::fuzzy::score(q, e.name).map(|(s, _)| s) };
                score.map(|s| (s, SearchItem::Action { name: e.name, keys: e.keys, action: e.action }))
            })
            .collect();
        if !q.is_empty() {
            scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
        }
        scored.into_iter().take(limit).map(|(_, i)| i).collect()
    };
    match tab {
        SearchTab::Files => out = file_items(MAX_RESULTS),
        SearchTab::Symbols => out = symbol_items(MAX_RESULTS),
        SearchTab::Actions => out = action_items(MAX_RESULTS),
        SearchTab::Text => {}
        SearchTab::All => {
            out.extend(file_items(if q.is_empty() { MAX_RESULTS } else { 24 }));
            if !q.is_empty() {
                out.extend(symbol_items(16));
                out.extend(action_items(12));
            }
        }
    }
    out
}

fn highlighted_bytes(text: &str, matched: &[usize], hit: Hsla) -> StyledText {
    let highlights = matched
        .iter()
        .filter(|&&b| b < text.len() && text.is_char_boundary(b))
        .map(|&b| {
            let len = text[b..].chars().next().map(char::len_utf8).unwrap_or(1);
            (b..b + len, HighlightStyle { color: Some(hit), font_weight: Some(FontWeight::BOLD), ..Default::default() })
        })
        .collect::<Vec<_>>();
    StyledText::new(text.to_string()).with_highlights(highlights)
}

fn render_item(item: &SearchItem, selected: bool, p: &Palette) -> Div {
    let row = h_flex().h(px(28.)).px(px(10.)).gap(px(8.)).rounded(px(4.)).text_size(px(13.)).when(selected, |d| d.bg(p.row_selected));
    match item {
        SearchItem::File { path, matched } => {
            let full = path.to_string_lossy().to_string();
            let name_start = full.rfind('/').map(|i| i + 1).unwrap_or(0);
            let name = full[name_start..].to_string();
            let dir = full[..name_start].trim_end_matches('/').to_string();
            let name_hits: Vec<usize> = matched.iter().filter(|&&i| i >= name_start).map(|i| i - name_start).collect();
            let (glyph, color) = file_icon(&name);
            row.child(icon(glyph, 16., color))
                .child(div().text_color(p.text_bright).child(highlighted_bytes(&name, &name_hits, p.accent)))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_color(p.muted).child(dir))
        }
        SearchItem::Symbol { path, name, kind, container, line, .. } => {
            let glyph = match *kind {
                "class" | "struct" | "interface" | "enum" | "trait" | "type" => IconName::Braces,
                "function" | "method" | "macro" => IconName::Code,
                _ => IconName::CircleDot,
            };
            let file = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            row.child(icon(glyph, 16., if glyph == IconName::Braces { p.accent } else { p.warning }))
                .child(div().text_color(p.text_bright).child(name.clone()))
                .children(container.clone().map(|c| div().text_color(p.muted).child(format!("in {c}"))))
                .child(div().flex_1())
                .child(div().text_color(p.muted).whitespace_nowrap().child(format!("{file}:{}", line + 1)))
        }
        SearchItem::Text { path, line, text, ranges, .. } => {
            let lead = text.len() - text.trim_start().len();
            let shown = text.trim().to_string();
            let hits: Vec<usize> = ranges.iter().flat_map(|r| r.clone()).filter_map(|b| b.checked_sub(lead)).collect();
            let file = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            row.child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().font_family(MONO_FONT).text_size(px(12.)).text_color(p.text).child(highlighted_bytes(&shown, &hits, p.accent)))
                .child(div().flex_none().text_color(p.muted).child(format!("{file} {}", line + 1)))
        }
        SearchItem::Action { name, keys, .. } => row
            .child(icon(IconName::Zap, 16., p.muted))
            .child(div().flex_1().text_color(p.text_bright).child(*name))
            .child(div().text_color(p.muted).child(keymap::display_keys(keys))),
    }
}

impl Render for SearchPopup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let tabs = h_flex().gap(px(2.)).children(SearchTab::ALL.iter().map(|&t| {
            let active = t == self.tab;
            div()
                .id(SharedString::from(format!("se-tab-{}", t.label())))
                .px(px(10.))
                .h(px(26.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .cursor_pointer()
                .text_size(px(13.))
                .when(active, |d| d.bg(p.stripe_selected).text_color(p.text_bright))
                .when(!active, |d| d.text_color(p.text).hover(|s| s.bg(p.hover)))
                .child(t.label())
                .on_click(cx.listener(move |this, _, _, cx| this.set_tab(t, cx)))
        }));
        let results = self.results.clone();
        let selected = self.selected;
        let list_p = p.clone();
        let count = results.len();
        let empty_label = if self.indexing(cx) {
            if self.tab == SearchTab::Text { "Searching…" } else { "Indexing…" }
        } else if self.tab == SearchTab::Text && self.query_text(cx).chars().count() < 2 {
            "Type at least 2 characters"
        } else {
            "Nothing found"
        };
        let list = if count == 0 {
            div().h(px(80.)).flex().items_center().justify_center().text_color(p.muted).child(empty_label).into_any_element()
        } else {
            uniform_list("se-results", count, cx.processor(move |_this, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|ix| {
                        render_item(&results[ix], ix == selected, &list_p)
                            .id(("se-item", ix))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = ix;
                                this.confirm(cx);
                            }))
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }))
            .track_scroll(&self.scroll)
            .h(px((count.min(14) as f32) * 28.))
            .into_any_element()
        };
        let hint = if self.tab == SearchTab::Text { "⌘⏎ open in Find window · ⇥ switch tab" } else { "⇥ switch tab" };
        v_flex()
            .id("search-everywhere")
            .key_context(POPUP)
            .w(px(720.))
            .max_w_full()
            .bg(p.popup_bg)
            .border_1()
            .border_color(p.border)
            .rounded(px(10.))
            .shadow_lg()
            .p(px(8.))
            .gap(px(6.))
            .text_color(p.text)
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                match ev.keystroke.key.as_str() {
                    "up" => this.move_selection(-1, cx),
                    "down" => this.move_selection(1, cx),
                    "tab" => this.cycle_tab(ev.keystroke.modifiers.shift, cx),
                    "escape" => cx.emit(SearchEvent::Dismiss),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(SearchEvent::Dismiss)))
            .child(h_flex().child(tabs).child(div().flex_1()).child(div().text_size(px(12.)).text_color(p.muted).child(hint)))
            .child(Input::new(&self.query).id("se-query").prefix(icon(IconName::Search, 16., p.muted)).cleanable(false))
            .child(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn files() -> FileIndex {
        FileIndex::from_paths(["src/main.rs", "src/http/client.ts", "README.md"])
    }

    #[test]
    fn files_rank_by_name() {
        let r = rank("client", SearchTab::Files, &files(), &SymbolIndex::default(), &[]);
        assert!(matches!(&r[0], SearchItem::File { path, .. } if path.ends_with("client.ts")));
    }

    #[test]
    fn actions_tab_lists_actions_only() {
        let r = rank("", SearchTab::Actions, &files(), &SymbolIndex::default(), &[]);
        assert!(!r.is_empty());
        assert!(r.iter().all(|i| matches!(i, SearchItem::Action { .. })));
    }

    #[test]
    fn empty_query_shows_recent() {
        let recent = vec![PathBuf::from("README.md")];
        let r = rank("", SearchTab::All, &files(), &SymbolIndex::default(), &recent);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn symbols_tab_uses_symbol_index() {
        let dir = std::env::temp_dir().join(format!("tandem-se-sym-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("lib.rs"), "pub fn frobnicate() {}\n").unwrap();
        let idx = SymbolIndex::build(&dir);
        let r = rank("frob", SearchTab::Symbols, &FileIndex::default(), &idx, &[]);
        assert!(matches!(&r[0], SearchItem::Symbol { name, .. } if name == "frobnicate"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
