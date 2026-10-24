//! Status bar: breadcrumb path on the left; line:col, LF, UTF-8, indent, language, branch,
//! LSP and agent activity on the right. Its own entity so caret moves only repaint this row.

use crate::assets::{icon, language_display};
use crate::editor::actions::ShowBranches;
use crate::editor::lsp::LspHubEvent;
use crate::editor::{EditorArea, EditorAreaEvent};
use crate::keymap::{GoToLine, ToggleAgents, ToggleProblems};
use crate::theme::pal;
use gpui_kit::assets::IconName;
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;

pub const STATUS_BAR_H: f32 = 24.0;

pub struct StatusBarView {
    editor: Entity<EditorArea>,
    pub branch: String,
    pub agents_running: usize,
    _subs: Vec<Subscription>,
}

impl StatusBarView {
    pub fn new(editor: Entity<EditorArea>, branch: String, agents_running: usize, cx: &mut Context<Self>) -> Self {
        let hub = editor.read(cx).lsp.clone();
        let subs = vec![
            cx.subscribe(&editor, |_, _, ev: &EditorAreaEvent, cx| {
                if matches!(ev, EditorAreaEvent::CursorMoved | EditorAreaEvent::ActiveChanged(_)) {
                    cx.notify();
                }
            }),
            cx.subscribe(&hub, |_, _, _: &LspHubEvent, cx| cx.notify()),
        ];
        Self { editor, branch, agents_running, _subs: subs }
    }
}

fn item(id: impl Into<ElementId>, text: impl Into<SharedString>, hover: Hsla) -> Stateful<Div> {
    div()
        .id(id)
        .h_full()
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(4.))
        .rounded(px(3.))
        .hover(move |s| s.bg(hover))
        .child(text.into())
}

impl Render for StatusBarView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let ed = self.editor.read(cx);
        let cursor = ed.cursor(cx);
        let lang = ed.active_file().map(|f| f.language);
        let path = ed.active_file().map(|f| {
            f.path.strip_prefix(&ed.root).unwrap_or(&f.path).display().to_string().replace('/', " › ")
        });
        let project = ed.root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let has_file = cursor.is_some();
        let (eol, indent) = ed.active_file().map(|f| f.format(cx)).unwrap_or(("LF", "4 spaces".into()));
        let hub = ed.lsp.read(cx);
        let lsp_text = hub.status_text();
        let hint = hub.missing.clone();
        div()
            .h(px(STATUS_BAR_H))
            .flex_none()
            .w_full()
            .flex()
            .items_center()
            .px(px(6.))
            .bg(p.panel_bg)
            .border_t_1()
            .border_color(p.border)
            .text_size(px(12.))
            .text_color(p.text)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(6.))
                    .text_color(p.text)
                    .child(project.clone())
                    .children(path.map(|s| div().text_color(p.text).child(format!("› {s}")))),
            )
            .child(div().flex_1())
            .when(has_file, |d| {
                let (l, c) = cursor.unwrap_or((1, 1));
                d.child(item("sb-pos", format!("{l}:{c}"), p.hover).on_click(|_, w, cx| w.dispatch_action(Box::new(GoToLine), cx)))
                    .child(item("sb-eol", eol, p.hover))
                    .child(item("sb-enc", "UTF-8", p.hover))
                    .child(item("sb-indent", indent.clone(), p.hover))
                    .child(item("sb-lang", language_display(lang.unwrap_or("text")), p.hover))
            })
            .child(
                item("sb-branch", self.branch.clone(), p.hover)
                    .child(icon(IconName::GitBranch, 12., p.muted))
                    .on_click(|_, w, cx| w.dispatch_action(Box::new(ShowBranches), cx)),
            )
            .children(hint.map(|(server, hint)| {
                item("sb-lsp-hint", format!("Install {server}: {hint}"), p.hover)
                    .max_w(px(420.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(p.warning)
            }))
            .child(item("sb-lsp", lsp_text, p.hover).text_color(p.muted).on_click(|_, w, cx| w.dispatch_action(Box::new(ToggleProblems), cx)))
            .child(
                item("sb-agents", "", p.hover)
                    .child(div().size(px(7.)).rounded_full().bg(if self.agents_running > 0 { p.ok } else { p.muted }))
                    .child(format!("{} agents", self.agents_running))
                    .on_click(|_, w, cx| w.dispatch_action(Box::new(ToggleAgents), cx)),
            )
            .child(div().px(px(6.)).child(icon(IconName::Lock, 12., p.muted)))
    }
}

/// `(line separator, indent)` sniffed from the first lines of the buffer.
pub fn detect_format(text: &gpui_kit::component::input::Rope) -> (&'static str, SharedString) {
    use gpui_kit::component::input::RopeExt as _;
    let mut eol = "LF";
    let mut tabs = 0;
    let mut widths = Vec::new();
    for line in text.iter_lines().take(400) {
        let l = line.to_string();
        if l.ends_with('\r') {
            eol = "CRLF";
        }
        if l.starts_with('\t') {
            tabs += 1;
        } else {
            let n = l.len() - l.trim_start_matches(' ').len();
            if n > 0 && !l.trim().is_empty() && !l.trim_start().starts_with('*') {
                widths.push(n);
            }
        }
    }
    let indent = if tabs > widths.len() {
        "Tab".into()
    } else {
        let unit = widths.iter().copied().filter(|w| *w <= 8).min().unwrap_or(4);
        format!("{} spaces", if unit == 2 || unit == 4 || unit == 8 { unit } else { 4 }).into()
    };
    (eol, indent)
}
