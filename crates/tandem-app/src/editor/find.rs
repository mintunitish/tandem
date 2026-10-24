//! Find in Files (⌘⇧F): streams `tandem_project::find_in_files` matches grouped by file;
//! click opens the match. Replace shows a `replace_preview` diff list, then
//! `apply_replacements` writes it and reloads unmodified open buffers.

use super::pane::EditorArea;
use crate::assets::{file_icon, icon};
use crate::theme::{MONO_FONT, Palette, pal};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use std::collections::HashMap;
use std::path::PathBuf;
use tandem_lsp::lsp_types::Position;
use tandem_project::{CancelToken, Match, ReplaceItem, SearchOptions, apply_replacements, find_in_files, replace_preview};

const MAX_MATCHES: usize = 5_000;

pub enum FindEvent {
    /// A search started from ⌘⇧F; the host shows the results tab.
    Started,
}

#[derive(Clone)]
enum Row {
    File(PathBuf, usize),
    Hit(Match),
    Replace(ReplaceItem),
}

pub struct FindPanel {
    root: PathBuf,
    area: WeakEntity<EditorArea>,
    pub query: Entity<InputState>,
    replace: Entity<InputState>,
    pub case: bool,
    pub whole_word: bool,
    pub regex: bool,
    groups: Vec<(PathBuf, Vec<Match>)>,
    group_ix: HashMap<PathBuf, usize>,
    pub total: usize,
    pub searching: bool,
    error: Option<String>,
    preview: Option<Vec<ReplaceItem>>,
    cancel: CancelToken,
    scroll: UniformListScrollHandle,
    _task: Task<()>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<FindEvent> for FindPanel {}

pub fn options(case: bool, whole_word: bool, regex: bool) -> SearchOptions {
    SearchOptions { case, whole_word, regex, max_results: MAX_MATCHES, ..Default::default() }
}

impl FindPanel {
    pub fn new(root: PathBuf, area: WeakEntity<EditorArea>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Text to find"));
        let replace = cx.new(|cx| InputState::new(window, cx).placeholder("Replace with"));
        let subs = vec![cx.subscribe(&query, |this: &mut Self, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change | InputEvent::PressEnter { .. }) {
                this.run(cx);
            }
        })];
        Self {
            root,
            area,
            query,
            replace,
            case: false,
            whole_word: false,
            regex: false,
            groups: vec![],
            group_ix: HashMap::new(),
            total: 0,
            searching: false,
            error: None,
            preview: None,
            cancel: CancelToken::new(),
            scroll: UniformListScrollHandle::new(),
            _task: Task::ready(()),
            _subs: subs,
        }
    }

    /// ⌘⇧F entry point: seed the query (e.g. from the editor selection) and search.
    pub fn search_for(&mut self, text: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = text {
            self.query.update(cx, |s, cx| s.set_value(t, window, cx));
        }
        self.query.update(cx, |s, cx| s.focus(window, cx));
        self.run(cx);
        cx.emit(FindEvent::Started);
    }

    fn query_text(&self, cx: &App) -> String {
        self.query.read(cx).value().to_string()
    }

    pub fn run(&mut self, cx: &mut Context<Self>) {
        self.cancel.cancel();
        self.cancel = CancelToken::new();
        self.groups.clear();
        self.group_ix.clear();
        self.total = 0;
        self.error = None;
        self.preview = None;
        let q = self.query_text(cx);
        if q.is_empty() {
            self.searching = false;
            self._task = Task::ready(());
            cx.notify();
            return;
        }
        let rx = match find_in_files(&self.root, &q, &options(self.case, self.whole_word, self.regex), self.cancel.clone()) {
            Ok(rx) => rx,
            Err(err) => {
                self.error = Some(err.to_string());
                self.searching = false;
                cx.notify();
                return;
            }
        };
        self.searching = true;
        self._task = cx.spawn(async move |this, cx| {
            while let Ok(first) = rx.recv().await {
                let mut batch = vec![first];
                while let Ok(m) = rx.try_recv() {
                    batch.push(m);
                    if batch.len() >= 512 {
                        break;
                    }
                }
                if this.update(cx, |p, cx| p.add(batch, cx)).is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |p, cx| {
                p.searching = false;
                p.groups.sort_by(|a, b| a.0.cmp(&b.0));
                p.group_ix = p.groups.iter().enumerate().map(|(i, g)| (g.0.clone(), i)).collect();
                cx.notify();
            });
        });
        cx.notify();
    }

    fn add(&mut self, batch: Vec<Match>, cx: &mut Context<Self>) {
        for m in batch {
            self.total += 1;
            let ix = match self.group_ix.get(&m.path) {
                Some(&i) => i,
                None => {
                    self.groups.push((m.path.clone(), vec![]));
                    self.group_ix.insert(m.path.clone(), self.groups.len() - 1);
                    self.groups.len() - 1
                }
            };
            let g = &mut self.groups[ix].1;
            g.push(m);
            g.sort_by_key(|m| m.line_no);
        }
        cx.notify();
    }

    /// All matches, file by file (for tests and the Search Everywhere Text tab).
    pub fn matches(&self) -> impl Iterator<Item = &Match> {
        self.groups.iter().flat_map(|g| g.1.iter())
    }

    fn preview_replace(&mut self, cx: &mut Context<Self>) {
        let (root, q, repl) = (self.root.clone(), self.query_text(cx), self.replace.read(cx).value().to_string());
        if q.is_empty() {
            return;
        }
        let opts = options(self.case, self.whole_word, self.regex);
        let task = cx.background_spawn(async move { replace_preview(&root, &q, &repl, &opts, CancelToken::new()) });
        self._task = cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |p, cx| {
                match res {
                    Ok(items) => p.preview = Some(items),
                    Err(err) => p.error = Some(err.to_string()),
                }
                cx.notify();
            });
        });
    }

    fn apply_replace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(items) = self.preview.take() else { return };
        let paths: Vec<PathBuf> = {
            let mut v: Vec<PathBuf> = items.iter().map(|i| i.path.clone()).collect();
            v.dedup();
            v
        };
        match apply_replacements(&items) {
            Ok(report) => {
                let mut msg = format!("Replaced {} line(s) in {} file(s)", report.lines_changed, report.files_changed);
                if !report.stale.is_empty() {
                    msg.push_str(&format!("; {} skipped (file changed)", report.stale.len()));
                }
                window.push_notification(Notification::info(msg).title("Replace in Files"), cx);
                let _ = self.area.update(cx, |a, cx| a.reload_from_disk(&paths, window, cx));
            }
            Err(err) => window.push_notification(Notification::error(err.to_string()).title("Replace in Files"), cx),
        }
        self.run(cx);
    }

    fn rows(&self) -> Vec<Row> {
        if let Some(items) = &self.preview {
            return items.iter().cloned().map(Row::Replace).collect();
        }
        let mut rows = Vec::with_capacity(self.total + self.groups.len());
        for (path, ms) in &self.groups {
            rows.push(Row::File(path.clone(), ms.len()));
            rows.extend(ms.iter().cloned().map(Row::Hit));
        }
        rows
    }
}

fn toggle(id: &'static str, label: &'static str, on: bool, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(22.))
        .px(px(6.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .cursor_pointer()
        .font_family(MONO_FONT)
        .text_size(px(12.))
        .when(on, |d| d.bg(p.stripe_selected).text_color(p.text_bright))
        .when(!on, |d| d.text_color(p.muted).hover(|s| s.bg(p.hover)))
        .child(label)
}

fn highlighted(line: &str, ranges: &[std::ops::Range<usize>], hit: Hsla) -> StyledText {
    let lead = line.len() - line.trim_start().len();
    let text = line.trim().to_string();
    let hl = ranges
        .iter()
        .filter_map(|r| {
            let s = r.start.checked_sub(lead)?;
            let e = (r.end - lead).min(text.len());
            (s < e && text.is_char_boundary(s) && text.is_char_boundary(e)).then(|| (s..e, HighlightStyle { background_color: Some(hit), ..Default::default() }))
        })
        .collect::<Vec<_>>();
    StyledText::new(text).with_highlights(hl)
}

impl Render for FindPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let rows = self.rows();
        let root = self.root.clone();
        let list_p = p.clone();
        let status = if let Some(e) = &self.error {
            e.clone()
        } else if let Some(items) = &self.preview {
            format!("Replace preview: {} line(s)", items.len())
        } else if self.searching {
            format!("Searching…  {} match(es)", self.total)
        } else if self.total > 0 {
            format!("{} match(es) in {} file(s){}", self.total, self.groups.len(), if self.total >= MAX_MATCHES { " (limit reached)" } else { "" })
        } else {
            String::new()
        };
        let hit_bg = p.accent.opacity(0.35);
        v_flex()
            .size_full()
            .text_size(px(13.))
            .child(
                h_flex()
                    .px(px(8.))
                    .py(px(4.))
                    .gap(px(4.))
                    .child(div().flex_1().child(Input::new(&self.query).small().prefix(icon(IconName::Search, 14., p.muted)).cleanable(true)))
                    .child(toggle("find-case", "Cc", self.case, &p).on_click(cx.listener(|this, _, _, cx| {
                        this.case = !this.case;
                        this.run(cx);
                    })))
                    .child(toggle("find-word", "W", self.whole_word, &p).on_click(cx.listener(|this, _, _, cx| {
                        this.whole_word = !this.whole_word;
                        this.run(cx);
                    })))
                    .child(toggle("find-regex", ".*", self.regex, &p).on_click(cx.listener(|this, _, _, cx| {
                        this.regex = !this.regex;
                        this.run(cx);
                    }))),
            )
            .child(
                h_flex()
                    .px(px(8.))
                    .pb(px(4.))
                    .gap(px(6.))
                    .child(div().flex_1().child(Input::new(&self.replace).small()))
                    .when(self.preview.is_none(), |d| {
                        d.child(Button::new("find-preview").small().label("Preview Replace").on_click(cx.listener(|this, _, _, cx| this.preview_replace(cx))))
                    })
                    .when(self.preview.is_some(), |d| {
                        d.child(Button::new("find-apply").small().primary().label("Replace All").on_click(cx.listener(|this, _, window, cx| this.apply_replace(window, cx))))
                            .child(Button::new("find-cancel").small().label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                                this.preview = None;
                                cx.notify();
                            })))
                    }),
            )
            .child(h_flex().h(px(22.)).flex_none().px(px(10.)).text_size(px(12.)).text_color(p.muted).child(status))
            .child(
                uniform_list("find-results", rows.len(), cx.processor(move |_this, range: std::ops::Range<usize>, _, cx| {
                    range
                        .map(|ix| match &rows[ix] {
                            Row::File(path, n) => {
                                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                                let dir = path.parent().and_then(|d| d.strip_prefix(&root).ok()).map(|d| d.display().to_string()).unwrap_or_default();
                                let (g, c) = file_icon(&name);
                                h_flex()
                                    .id(("find-file", ix))
                                    .h(px(22.))
                                    .px(px(8.))
                                    .gap(px(6.))
                                    .child(icon(g, 14., c))
                                    .child(div().text_color(list_p.text).child(name))
                                    .child(div().text_color(list_p.muted).child(format!("{dir}  {n}")))
                                    .into_any_element()
                            }
                            Row::Hit(m) => {
                                let (path, line, col) = (m.path.clone(), m.line_no.saturating_sub(1) as u32, m.col as u32);
                                h_flex()
                                    .id(("find-hit", ix))
                                    .h(px(22.))
                                    .pl(px(30.))
                                    .gap(px(8.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(list_p.hover))
                                    .child(div().w(px(44.)).flex_none().text_color(list_p.muted).child(m.line_no.to_string()))
                                    .child(div().font_family(MONO_FONT).text_size(px(12.)).text_color(list_p.text).whitespace_nowrap().child(highlighted(&m.line_text, &m.ranges, hit_bg)))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let path = path.clone();
                                        let _ = this.area.update(cx, |a, cx| a.open_at(path, Position::new(line, col), window, cx));
                                    }))
                                    .into_any_element()
                            }
                            Row::Replace(r) => {
                                let name = r.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                                v_flex()
                                    .id(("find-repl", ix))
                                    .h(px(22.))
                                    .px(px(8.))
                                    .child(
                                        h_flex()
                                            .gap(px(8.))
                                            .font_family(MONO_FONT)
                                            .text_size(px(12.))
                                            .child(div().w(px(140.)).flex_none().overflow_hidden().text_color(list_p.muted).child(format!("{name}:{}", r.line_no)))
                                            .child(div().text_color(list_p.git_deleted).line_through().whitespace_nowrap().child(r.old_line.trim().to_string()))
                                            .child(div().text_color(list_p.muted).child("→"))
                                            .child(div().text_color(list_p.git_added).whitespace_nowrap().child(r.new_line.trim().to_string())),
                                    )
                                    .into_any_element()
                            }
                        })
                        .collect::<Vec<_>>()
                }))
                .flex_1()
                .track_scroll(&self.scroll),
            )
    }
}
