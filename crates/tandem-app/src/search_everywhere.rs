//! Search Everywhere (⇧⇧): one popup with All / Files / Actions tabs, fuzzy ranked.
//! Files come from the background index (`model::project::index_files`); actions come from the
//! keymap table, so every bound command is discoverable here with its shortcut.
//! Integration point: swap the file list for `tandem_project`'s index and add a Symbols tab
//! backed by `tandem_lsp` workspace symbols.

use crate::assets::{file_icon, icon};
use crate::keymap::{self, POPUP};
use crate::model::fuzzy;
use crate::theme::{Palette, pal};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use std::path::PathBuf;
use std::sync::Arc;

pub const MAX_RESULTS: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchTab {
    All,
    Files,
    Actions,
}

impl SearchTab {
    const ALL: [SearchTab; 3] = [SearchTab::All, SearchTab::Files, SearchTab::Actions];

    fn label(self) -> &'static str {
        match self {
            SearchTab::All => "All",
            SearchTab::Files => "Files",
            SearchTab::Actions => "Actions",
        }
    }
}

#[derive(Clone)]
pub enum SearchItem {
    File { path: PathBuf, matched: Vec<usize> },
    Action { name: &'static str, keys: &'static str, action: fn() -> Box<dyn Action> },
}

pub enum SearchEvent {
    OpenFile(PathBuf),
    RunAction(Box<dyn Action>),
    Dismiss,
}

pub struct SearchPopup {
    pub tab: SearchTab,
    pub query: Entity<InputState>,
    files: Arc<Vec<PathBuf>>,
    /// Recent files (relative), shown when the query is empty.
    recent: Vec<PathBuf>,
    pub results: Vec<SearchItem>,
    pub selected: usize,
    indexing: bool,
    scroll: UniformListScrollHandle,
    _sub: Subscription,
}

impl EventEmitter<SearchEvent> for SearchPopup {}

impl Focusable for SearchPopup {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }
}

impl SearchPopup {
    pub fn new(tab: SearchTab, files: Arc<Vec<PathBuf>>, indexing: bool, recent: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Type / to see commands"));
        let sub = cx.subscribe(&query, |this: &mut Self, _, ev: &InputEvent, cx| match ev {
            InputEvent::Change => {
                this.selected = 0;
                this.refresh(cx);
            }
            InputEvent::PressEnter { .. } => this.confirm(cx),
            _ => {}
        });
        let mut this = Self { tab, query, files, recent, results: vec![], selected: 0, indexing, scroll: UniformListScrollHandle::new(), _sub: sub };
        this.refresh(cx);
        this
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |s, cx| s.focus(window, cx));
    }

    pub fn set_files(&mut self, files: Arc<Vec<PathBuf>>, cx: &mut Context<Self>) {
        self.files = files;
        self.indexing = false;
        self.refresh(cx);
    }

    pub fn set_tab(&mut self, tab: SearchTab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.selected = 0;
        self.refresh(cx);
    }

    fn query_text(&self, cx: &App) -> String {
        self.query.read(cx).value().trim().to_string()
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let q = self.query_text(cx);
        self.results = rank(&q, self.tab, &self.files, &self.recent);
        self.selected = self.selected.min(self.results.len().saturating_sub(1));
        cx.notify();
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
            Some(SearchItem::Action { action, .. }) => cx.emit(SearchEvent::RunAction(action())),
            None => {}
        }
    }

    fn cycle_tab(&mut self, cx: &mut Context<Self>) {
        let ix = SearchTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.set_tab(SearchTab::ALL[(ix + 1) % SearchTab::ALL.len()], cx);
    }
}

/// Pure ranking used by the popup (and unit-tested): files by fuzzy score, actions by name.
pub fn rank(q: &str, tab: SearchTab, files: &[PathBuf], recent: &[PathBuf]) -> Vec<SearchItem> {
    let mut out: Vec<(i64, SearchItem)> = Vec::new();
    if tab != SearchTab::Actions {
        if q.is_empty() {
            out.extend(recent.iter().map(|p| (0, SearchItem::File { path: p.clone(), matched: vec![] })));
        } else {
            for path in files {
                let s = path.to_string_lossy();
                if let Some((score, matched)) = fuzzy::score(q, &s) {
                    out.push((score, SearchItem::File { path: path.clone(), matched }));
                }
            }
        }
    }
    if tab != SearchTab::Files && (tab == SearchTab::Actions || !q.is_empty()) {
        for e in keymap::entries() {
            let score = if q.is_empty() { Some(0) } else { fuzzy::score(q, e.name).map(|(s, _)| s - 5) };
            if let Some(score) = score {
                out.push((score, SearchItem::Action { name: e.name, keys: e.keys, action: e.action }));
            }
        }
    }
    if !q.is_empty() {
        out.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    }
    out.truncate(MAX_RESULTS);
    out.into_iter().map(|(_, i)| i).collect()
}

fn highlighted(text: &str, matched: &[usize], base: Hsla, hit: Hsla) -> StyledText {
    let mut highlights = Vec::new();
    let mut byte = 0;
    for (ci, ch) in text.chars().enumerate() {
        let len = ch.len_utf8();
        if matched.contains(&ci) {
            highlights.push((byte..byte + len, HighlightStyle { color: Some(hit), font_weight: Some(FontWeight::BOLD), ..Default::default() }));
        }
        byte += len;
    }
    let _ = base;
    StyledText::new(text.to_string()).with_highlights(highlights)
}

fn render_item(item: &SearchItem, selected: bool, p: &Palette) -> Div {
    let row = h_flex()
        .h(px(28.))
        .px(px(10.))
        .gap(px(8.))
        .rounded(px(4.))
        .text_size(px(13.))
        .when(selected, |d| d.bg(p.row_selected));
    match item {
        SearchItem::File { path, matched } => {
            let full = path.to_string_lossy().to_string();
            let name_start = full.rfind('/').map(|i| i + 1).unwrap_or(0);
            let name = full[name_start..].to_string();
            let dir = full[..name_start].trim_end_matches('/').to_string();
            let name_offset = full[..name_start].chars().count();
            let name_hits: Vec<usize> = matched.iter().filter(|&&i| i >= name_offset).map(|i| i - name_offset).collect();
            let (glyph, color) = file_icon(&name);
            row.child(icon(glyph, 16., color))
                .child(div().text_color(p.text_bright).child(highlighted(&name, &name_hits, p.text_bright, p.accent)))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_color(p.muted).child(dir))
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
        let empty_label = if self.indexing && self.tab != SearchTab::Actions { "Indexing…" } else { "Nothing found" };
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
        v_flex()
            .id("search-everywhere")
            .key_context(POPUP)
            .w(px(680.))
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
                    "tab" => this.cycle_tab(cx),
                    "escape" => cx.emit(SearchEvent::Dismiss),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(SearchEvent::Dismiss)))
            .child(h_flex().child(tabs).child(div().flex_1()).child(div().text_size(px(12.)).text_color(p.muted).child("⇥ switch tab")))
            .child(
                Input::new(&self.query)
                    .id("se-query")
                    .prefix(icon(IconName::Search, 16., p.muted))
                    .cleanable(false),
            )
            .child(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn files() -> Vec<PathBuf> {
        ["src/main.rs", "src/http/client.ts", "README.md"].iter().map(PathBuf::from).collect()
    }

    #[test]
    fn files_rank_by_name() {
        let r = rank("client", SearchTab::Files, &files(), &[]);
        assert!(matches!(&r[0], SearchItem::File { path, .. } if path.ends_with("client.ts")));
    }

    #[test]
    fn actions_tab_lists_actions_only() {
        let r = rank("", SearchTab::Actions, &files(), &[]);
        assert!(!r.is_empty());
        assert!(r.iter().all(|i| matches!(i, SearchItem::Action { .. })));
    }

    #[test]
    fn empty_query_shows_recent() {
        let recent = vec![PathBuf::from("README.md")];
        let r = rank("", SearchTab::All, &files(), &recent);
        assert_eq!(r.len(), 1);
    }
}
