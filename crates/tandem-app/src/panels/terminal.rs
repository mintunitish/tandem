//! Terminal tool window: `tandem_terminal::Terminal` sessions in tabs. The grid is painted with a
//! `canvas`: prepaint fits the PTY to the panel (resize on layout change) and takes one viewport
//! snapshot, paint draws background runs, styled text runs and the cursor. Wakeups only
//! `notify`, so output bursts coalesce into one repaint per frame.

use crate::editor::actions::NewTerminalTab;
use crate::theme::{MONO_FONT, Palette, ThemeKind, pal};
use gpui_kit::assets::IconName;
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use std::path::PathBuf;
use std::sync::Arc;
use tandem_terminal::{CellFlags, GridSnapshot, Key, KeyInput, Mods, Rgb, SpawnOptions, TermEvent, Terminal, palette};

const FONT_SIZE: f32 = 13.0;
const LINE_H: f32 = 17.0;
const PAD_X: f32 = 8.0;
const PAD_Y: f32 = 4.0;

struct TermTab {
    id: usize,
    term: Arc<Terminal>,
    title: String,
    _events: Task<()>,
}

pub struct TerminalPanel {
    root: PathBuf,
    tabs: Vec<TermTab>,
    active: usize,
    next_id: usize,
    started: bool,
    theme: Option<ThemeKind>,
    error: Option<String>,
    focus: FocusHandle,
    _intercept: Subscription,
}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn to_rgb(c: Hsla) -> Rgb {
    let r = c.to_rgb();
    let b = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    Rgb::new(b(r.r), b(r.g), b(r.b))
}

fn hsla(c: Rgb) -> Hsla {
    rgb(c.to_hex()).into()
}

/// Terminal palette matching the IDE theme (ANSI colors from JetBrains, background = editor).
fn term_palette(p: &Palette) -> palette::Palette {
    let mut t = match p.kind {
        ThemeKind::Darcula => palette::darcula(),
        ThemeKind::Light => palette::intellij_light(),
    };
    t.background = to_rgb(p.editor_bg);
    t.foreground = to_rgb(p.text);
    t
}

/// gpui keystroke → terminal key. `None` for keys the terminal should not see.
pub fn key_input(k: &Keystroke) -> Option<KeyInput> {
    let m = &k.modifiers;
    let mods = Mods { ctrl: m.control, alt: m.alt, shift: m.shift, cmd: m.platform };
    let key = match k.key.as_str() {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        "space" => Key::Char(' '),
        f if f.len() >= 2 && f.starts_with('f') && f[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)) => Key::F(f[1..].parse().unwrap_or(1)),
        other => {
            // Ctrl/Alt chords use the unshifted key; plain typing uses the composed character.
            let ch = if m.control || (m.alt && k.key_char.is_none()) {
                other.chars().next()?
            } else {
                let s = k.key_char.as_deref().unwrap_or(other);
                let mut it = s.chars();
                let c = it.next()?;
                if it.next().is_some() {
                    return None;
                }
                c
            };
            let mods = if m.control { mods } else { Mods { alt: false, shift: false, ..mods } };
            return Some(KeyInput::new(Key::Char(ch), mods));
        }
    };
    Some(KeyInput::new(key, mods))
}

impl TerminalPanel {
    pub fn new(root: &std::path::Path, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let weak = cx.weak_entity();
        let f = focus.clone();
        let intercept = cx.intercept_keystrokes(move |ev, window, cx| {
            if !f.contains_focused(window, cx) {
                return;
            }
            let k = &ev.keystroke;
            let _ = weak.update(cx, |this, cx| {
                if this.handle_keystroke(k, cx) {
                    cx.stop_propagation();
                }
            });
        });
        Self {
            root: root.to_path_buf(),
            tabs: vec![],
            active: 0,
            next_id: 0,
            started: false,
            theme: None,
            error: None,
            focus,
            _intercept: intercept,
        }
    }

    /// Returns true when the keystroke was consumed by the terminal.
    fn handle_keystroke(&mut self, k: &Keystroke, cx: &mut Context<Self>) -> bool {
        let m = &k.modifiers;
        if m.platform {
            match k.key.as_str() {
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                        self.paste(&text);
                    }
                    return true;
                }
                "t" => {
                    self.new_tab(cx);
                    return true;
                }
                "backspace" | "left" | "right" => {}
                _ => return false,
            }
        }
        let Some(term) = self.active_term() else { return false };
        match key_input(k) {
            Some(input) => {
                let _ = term.send_key(input);
                true
            }
            None => false,
        }
    }

    fn active_term(&self) -> Option<Arc<Terminal>> {
        self.tabs.get(self.active).map(|t| t.term.clone())
    }

    /// Write raw text to the active session (tests, "Run in terminal").
    pub fn send_text(&self, text: &str) {
        if let Some(t) = self.active_term() {
            let _ = t.write(text.as_bytes());
        }
    }

    pub fn paste(&self, text: &str) {
        if let Some(t) = self.active_term() {
            let _ = t.paste(text);
        }
    }

    /// Visible text of the active session.
    pub fn screen_text(&self) -> String {
        self.active_term().map(|t| t.viewport_text()).unwrap_or_default()
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub fn new_tab(&mut self, cx: &mut Context<Self>) {
        self.started = true;
        let opts = SpawnOptions { cwd: Some(self.root.clone()), ..Default::default() };
        match Terminal::spawn(opts) {
            Ok(t) => {
                let n = self.tabs.len() + 1;
                self.adopt(t, if n == 1 { "Local".into() } else { format!("Local ({n})") }, cx);
            }
            Err(err) => {
                self.error = Some(format!("Cannot start shell: {err:#}"));
                cx.notify();
            }
        }
    }

    /// Show an already-running session as a new, active tab.
    pub fn adopt(&mut self, term: Terminal, title: String, cx: &mut Context<Self>) {
        let term = Arc::new(term);
        term.set_palette(term_palette(pal(cx)));
        let id = self.next_id;
        self.next_id += 1;
        let rx = term.events();
        let events = cx.spawn(async move |this, cx| {
            while let Ok(ev) = rx.recv().await {
                let alive = this.update(cx, |p, cx| p.on_event(id, ev, cx));
                if alive.is_err() {
                    break;
                }
            }
        });
        self.tabs.push(TermTab { id, term, title, _events: events });
        self.active = self.tabs.len() - 1;
        self.error = None;
        cx.notify();
    }

    fn on_event(&mut self, id: usize, ev: TermEvent, cx: &mut Context<Self>) {
        match ev {
            TermEvent::Wakeup | TermEvent::Bell => {}
            // Like JetBrains, local shell tabs keep their "Local" name (shells retitle on every
            // prompt); named sessions (agent sign-in) also keep theirs.
            TermEvent::Title(_) => {}
            TermEvent::Exit(_) => {
                if let Some(ix) = self.tabs.iter().position(|t| t.id == id) {
                    self.close_tab(ix, cx);
                }
            }
        }
        cx.notify();
    }

    fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(ix);
        let _ = tab.term.kill();
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        cx.notify();
    }

    fn render_grid(&self, term: Arc<Terminal>, p: &Palette, focused: bool) -> impl IntoElement {
        let cursor_color = p.text;
        canvas(
            move |bounds, window, _cx| {
                let font = font(MONO_FONT);
                let ts = window.text_system();
                let font_id = ts.resolve_font(&font);
                let cw = ts.advance(font_id, px(FONT_SIZE), 'M').map(|s| s.width).unwrap_or(px(0.));
                let cw = if cw <= px(0.) { px(FONT_SIZE * 0.6) } else { cw };
                let lh = px(LINE_H);
                let cols = (((bounds.size.width - px(2. * PAD_X)) / cw).floor() as i32).max(2) as u16;
                let rows = (((bounds.size.height - px(2. * PAD_Y)) / lh).floor() as i32).max(1) as u16;
                let (c, r) = term.size();
                if (c as u16, r as u16) != (cols, rows) {
                    let _ = term.resize(cols, rows);
                }
                (term.snapshot(), cw)
            },
            move |bounds, (snap, cw): (GridSnapshot, Pixels), window, cx| {
                paint_grid(bounds, &snap, cw, cursor_color, focused, window, cx);
            },
        )
        .size_full()
    }
}

#[derive(PartialEq, Clone, Copy)]
struct Style {
    fg: Rgb,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

fn paint_grid(bounds: Bounds<Pixels>, snap: &GridSnapshot, cw: Pixels, cursor: Hsla, focused: bool, window: &mut Window, cx: &mut App) {
    let lh = px(LINE_H);
    let origin = bounds.origin + point(px(PAD_X), px(PAD_Y));
    window.paint_quad(fill(bounds, hsla(snap.default_bg)));
    for row in 0..snap.rows {
        let y = origin.y + lh * row as f32;
        let cells = snap.row(row);
        // Backgrounds: merge equal neighbors.
        let mut col = 0;
        while col < cells.len() {
            let (_, bg) = cells[col].effective_colors();
            let start = col;
            while col < cells.len() && cells[col].effective_colors().1 == bg {
                col += 1;
            }
            if bg != snap.default_bg {
                let b = Bounds::new(point(origin.x + cw * start as f32, y), size(cw * (col - start) as f32, lh));
                window.paint_quad(fill(b, hsla(bg)));
            }
        }
        // Text: one shaped run per style span, positioned on the cell grid.
        let mut col = 0;
        while col < cells.len() {
            let c = &cells[col];
            if c.flags.contains(CellFlags::WIDE_SPACER) || c.flags.contains(CellFlags::HIDDEN) || c.ch == ' ' || c.ch == '\0' {
                col += 1;
                continue;
            }
            let style = |c: &tandem_terminal::Cell| Style {
                fg: c.effective_colors().0,
                bold: c.flags.bold(),
                italic: c.flags.italic(),
                underline: c.flags.underline(),
                strike: c.flags.contains(CellFlags::STRIKEOUT),
            };
            let st = style(c);
            let start = col;
            let mut text = String::new();
            while col < cells.len() {
                let cc = &cells[col];
                if cc.flags.contains(CellFlags::WIDE_SPACER) {
                    col += 1;
                    continue;
                }
                if style(cc) != st || cc.flags.contains(CellFlags::HIDDEN) {
                    break;
                }
                text.push(if cc.ch == '\0' { ' ' } else { cc.ch });
                col += 1;
            }
            let trimmed = text.trim_end().to_string();
            if trimmed.is_empty() {
                continue;
            }
            let mut f = font(MONO_FONT);
            if st.bold {
                f.weight = FontWeight::BOLD;
            }
            if st.italic {
                f.style = FontStyle::Italic;
            }
            let color = if c.flags.contains(CellFlags::DIM) { hsla(st.fg).opacity(0.66) } else { hsla(st.fg) };
            let run = TextRun {
                len: trimmed.len(),
                font: f,
                color,
                background_color: None,
                underline: st.underline.then(|| UnderlineStyle { thickness: px(1.), color: Some(color), wavy: false }),
                strikethrough: st.strike.then(|| StrikethroughStyle { thickness: px(1.), color: Some(color) }),
            };
            let line = window.text_system().shape_line(trimmed.into(), px(FONT_SIZE), &[run], None);
            let _ = line.paint(point(origin.x + cw * start as f32, y), lh, TextAlign::Left, None, window, cx);
        }
    }
    let cur = snap.cursor;
    if cur.visible && cur.row < snap.rows {
        let b = Bounds::new(point(origin.x + cw * cur.col as f32, origin.y + lh * cur.row as f32), size(cw, lh));
        if focused {
            window.paint_quad(fill(b, cursor.opacity(0.75)));
        } else {
            window.paint_quad(outline(b, cursor.opacity(0.75), BorderStyle::Solid));
        }
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        // Sessions the cockpit spawned (e.g. agent sign-in) become tabs here.
        for (req, term) in crate::tasks::terminal::take_pending() {
            self.started = true;
            self.adopt(term, req.title, cx);
        }
        if !self.started {
            self.new_tab(cx);
        }
        if self.theme != Some(p.kind) {
            self.theme = Some(p.kind);
            for t in &self.tabs {
                t.term.set_palette(term_palette(&p));
            }
        }
        let focused = self.focus.contains_focused(window, cx);
        let tabs = self.tabs.iter().enumerate().map(|(ix, t)| {
            let active = ix == self.active;
            div()
                .id(("term-tab", ix))
                .px(px(10.))
                .h(px(24.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(4.))
                .cursor_pointer()
                .when(active, |d| d.bg(p.stripe_selected).text_color(p.text_bright))
                .when(!active, |d| d.text_color(p.text).hover(|s| s.bg(p.hover)))
                .child(t.title.clone())
                .child(
                    div()
                        .id(("term-tab-close", ix))
                        .rounded(px(3.))
                        .hover(|s| s.bg(p.hover))
                        .child(crate::assets::icon(IconName::X, 11., p.muted))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.close_tab(ix, cx);
                        })),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.active = ix;
                    window.focus(&this.focus, cx);
                    cx.notify();
                }))
        });
        let body: AnyElement = match self.active_term() {
            Some(term) => self.render_grid(term, &p, focused).into_any_element(),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(p.muted)
                .child(self.error.clone().unwrap_or_else(|| "Session ended. Press + (⌘T) for a new one.".into()))
                .into_any_element(),
        };
        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(p.editor_bg)
            .on_action(cx.listener(|this, _: &NewTerminalTab, _, cx| this.new_tab(cx)))
            .on_click(cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)))
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                if let Some(t) = this.active_term() {
                    let dy = ev.delta.pixel_delta(px(LINE_H)).y;
                    let lines = (f32::from(dy) / LINE_H).round() as i32;
                    if lines != 0 {
                        t.scroll(lines);
                        cx.notify();
                    }
                }
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
                    .children(tabs)
                    .child(
                        div()
                            .id("term-new")
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(p.hover))
                            .child(crate::assets::icon(IconName::Plus, 14., p.muted))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.new_tab(cx);
                                window.focus(&this.focus, cx);
                            })),
                    ),
            )
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn ks(s: &str) -> Keystroke {
        Keystroke::parse(s).unwrap()
    }

    #[test]
    fn maps_keys() {
        assert_eq!(key_input(&ks("enter")), Some(KeyInput::plain(Key::Enter)));
        assert_eq!(key_input(&ks("ctrl-c")).map(|k| k.key), Some(Key::Char('c')));
        assert_eq!(key_input(&ks("f5")).map(|k| k.key), Some(Key::F(5)));
        let mut a = ks("a");
        a.key_char = Some("a".into());
        assert_eq!(key_input(&a), Some(KeyInput::plain(Key::Char('a'))));
    }
}
