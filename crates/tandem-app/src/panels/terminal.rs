//! Terminal tool window (stub grid). Integration point: `tandem_terminal::Term` renders its
//! alacritty grid into `TerminalPanel::lines` and receives keystrokes from `on_key_down`.

use crate::theme::{MONO_FONT, pal};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use std::time::Duration;

pub struct TerminalPanel {
    pub lines: Vec<(String, Option<Hsla>)>,
    cwd: String,
    input: String,
    cursor_on: bool,
    focus: FocusHandle,
    _blink: Task<()>,
}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TerminalPanel {
    pub fn new(root: &std::path::Path, cx: &mut Context<Self>) -> Self {
        let name = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let blink = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(530)).await;
                if this.update(cx, |t, cx| {
                    t.cursor_on = !t.cursor_on;
                    cx.notify();
                }).is_err() {
                    break;
                }
            }
        });
        let green: Hsla = rgb(0x5FB865).into();
        Self {
            lines: vec![
                ("Last login: Sat Oct 24 09:12:44 on ttys004".into(), None),
                (format!("{name} % git status --short"), None),
                (" M src/http/client.ts".into(), Some(rgb(0x6F9EF5).into())),
                ("?? src/http/retry.ts".into(), Some(rgb(0xD5756C).into())),
                (format!("{name} % cargo check -p tandem-app"), None),
                ("    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.31s".into(), Some(green)),
            ],
            cwd: name,
            input: String::new(),
            cursor_on: true,
            focus: cx.focus_handle(),
            _blink: blink,
        }
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let prompt = format!("{} % {}", self.cwd, self.input);
        div()
            .id("terminal")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(p.editor_bg)
            .on_click(cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                let k = &ev.keystroke;
                if k.modifiers.platform || k.modifiers.control {
                    return;
                }
                match k.key.as_str() {
                    "enter" => {
                        let cmd = std::mem::take(&mut this.input);
                        this.lines.push((format!("{} % {cmd}", this.cwd), None));
                        if !cmd.trim().is_empty() {
                            this.lines.push((format!("tandem: terminal backend not wired yet ({})", cmd.trim()), Some(rgb(0x6F737A).into())));
                        }
                    }
                    "backspace" => {
                        this.input.pop();
                    }
                    _ => {
                        if let Some(ch) = &k.key_char {
                            this.input.push_str(ch);
                        }
                    }
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .child(
                div()
                    .h(px(30.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(px(8.))
                    .gap(px(4.))
                    .bg(p.panel_bg)
                    .text_size(px(13.))
                    .child(div().px(px(10.)).h(px(24.)).flex().items_center().rounded(px(4.)).bg(p.stripe_selected).child("Local"))
                    .child(div().px(px(8.)).text_color(p.muted).child("+")),
            )
            .child(
                div()
                    .id("terminal-grid")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(px(10.))
                    .py(px(6.))
                    .font_family(MONO_FONT)
                    .text_size(px(13.))
                    .line_height(px(17.))
                    .children(self.lines.iter().map(|(l, c)| div().text_color(c.unwrap_or(p.text)).child(l.clone())))
                    .child(
                        div()
                            .flex()
                            .child(div().text_color(p.text).child(prompt))
                            .child(div().w(px(8.)).h(px(16.)).when(self.cursor_on, |d| d.bg(p.text))),
                    ),
            )
    }
}
