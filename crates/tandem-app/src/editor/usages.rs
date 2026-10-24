//! Usages results (⌥F7): LSP references grouped by file; click opens the location.

use super::pane::EditorArea;
use crate::assets::{file_icon, icon};
use crate::theme::{MONO_FONT, pal};
use gpui_kit::assets::IconName;
use gpui_kit::component::h_flex;
use gpui_kit::*;
use std::path::PathBuf;
use tandem_lsp::lsp_types::Position;

#[derive(Clone, Debug)]
pub struct Usage {
    pub path: PathBuf,
    pub pos: Position,
    pub line_text: String,
}

pub enum UsagesEvent {
    Updated,
}

#[derive(Clone)]
enum Row {
    File(PathBuf, usize),
    Hit(Usage),
}

pub struct UsagesPanel {
    root: PathBuf,
    area: WeakEntity<EditorArea>,
    pub symbol: String,
    pub usages: Vec<Usage>,
    rows: Vec<Row>,
    scroll: UniformListScrollHandle,
}

impl EventEmitter<UsagesEvent> for UsagesPanel {}

impl UsagesPanel {
    pub fn new(root: PathBuf, area: WeakEntity<EditorArea>) -> Self {
        Self { root, area, symbol: String::new(), usages: vec![], rows: vec![], scroll: UniformListScrollHandle::new() }
    }

    pub fn set(&mut self, symbol: String, usages: Vec<Usage>, cx: &mut Context<Self>) {
        let mut rows = Vec::new();
        let mut i = 0;
        while i < usages.len() {
            let path = usages[i].path.clone();
            let n = usages[i..].iter().take_while(|u| u.path == path).count();
            rows.push(Row::File(path, n));
            rows.extend(usages[i..i + n].iter().cloned().map(Row::Hit));
            i += n;
        }
        self.symbol = symbol;
        self.usages = usages;
        self.rows = rows;
        cx.emit(UsagesEvent::Updated);
        cx.notify();
    }
}

impl Render for UsagesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let header = if self.symbol.is_empty() {
            "Place the caret on a symbol and press ⌥F7".to_string()
        } else {
            format!("Usages of '{}'  ·  {} result(s)", self.symbol, self.usages.len())
        };
        let rows = self.rows.clone();
        let root = self.root.clone();
        let list_p = p.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(13.))
            .child(h_flex().h(px(26.)).flex_none().px(px(10.)).text_color(p.muted).child(header))
            .child(
                uniform_list("usages", rows.len(), cx.processor(move |_this, range: std::ops::Range<usize>, _, cx| {
                    range
                        .map(|ix| match &rows[ix] {
                            Row::File(path, n) => {
                                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                                let dir = path.parent().and_then(|d| d.strip_prefix(&root).ok()).map(|d| d.display().to_string()).unwrap_or_default();
                                let (g, c) = file_icon(&name);
                                h_flex()
                                    .id(("usage-file", ix))
                                    .h(px(22.))
                                    .px(px(8.))
                                    .gap(px(6.))
                                    .child(icon(IconName::ChevronDown, 12., list_p.muted))
                                    .child(icon(g, 14., c))
                                    .child(div().text_color(list_p.text).child(name))
                                    .child(div().text_color(list_p.muted).child(format!("{dir}  {n}")))
                                    .into_any_element()
                            }
                            Row::Hit(u) => {
                                let u2 = u.clone();
                                h_flex()
                                    .id(("usage-hit", ix))
                                    .h(px(22.))
                                    .pl(px(40.))
                                    .gap(px(8.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(list_p.hover))
                                    .child(div().w(px(40.)).text_color(list_p.muted).child(format!("{}", u.pos.line + 1)))
                                    .child(div().font_family(MONO_FONT).text_size(px(12.)).text_color(list_p.text).whitespace_nowrap().child(u.line_text.trim().to_string()))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let u = u2.clone();
                                        let _ = this.area.update(cx, |a, cx| a.open_at(u.path, u.pos, window, cx));
                                    }))
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
