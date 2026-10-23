use super::ops::{self, Edit};
use super::{breadcrumbs, tabs};
use crate::assets::{language_for, line_comment};
use crate::theme::{MONO_FONT, pal};
use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState, Position, TabSize};
use gpui_kit::component::{WindowExt as _, h_flex, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use std::ops::Range;
use std::path::{Path, PathBuf};

pub struct OpenFile {
    pub path: PathBuf,
    pub state: Entity<EditorState>,
    pub dirty: bool,
    pub language: &'static str,
    _subs: Vec<Subscription>,
}

pub enum EditorAreaEvent {
    ActiveChanged(Option<PathBuf>),
    CursorMoved,
    Toast(SharedString),
}

pub struct EditorArea {
    pub root: PathBuf,
    pub files: Vec<OpenFile>,
    pub active: Option<usize>,
    /// Most-recent first, for ⌘E.
    pub recent: Vec<PathBuf>,
    pub hovered_tab: Option<usize>,
    pub font_size: f32,
    focus: FocusHandle,
    go_to_line: Entity<InputState>,
    tab_scroll: ScrollHandle,
}

impl EventEmitter<EditorAreaEvent> for EditorArea {}
impl EventEmitter<PanelEvent> for EditorArea {}

impl Focusable for EditorArea {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self.active_file() {
            Some(f) => f.state.read(cx).focus_handle(cx),
            None => self.focus.clone(),
        }
    }
}

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

impl EditorArea {
    pub fn new(root: PathBuf, font_size: f32, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let go_to_line = cx.new(|cx| InputState::new(window, cx));
        Self {
            root,
            files: vec![],
            active: None,
            recent: vec![],
            hovered_tab: None,
            font_size,
            focus: cx.focus_handle(),
            go_to_line,
            tab_scroll: ScrollHandle::new(),
        }
    }

    pub fn active_file(&self) -> Option<&OpenFile> {
        self.active.and_then(|i| self.files.get(i))
    }

    pub fn active_path(&self) -> Option<PathBuf> {
        self.active_file().map(|f| f.path.clone())
    }

    fn touch_recent(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(50);
    }

    /// Open (or focus) `path` in a tab. Integration point for "go to definition" and review.
    pub fn open(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.files.iter().position(|f| f.path == path) {
            self.activate(ix, window, cx);
            return;
        }
        let meta = std::fs::metadata(&path);
        if meta.as_ref().map(|m| m.len() > MAX_FILE_BYTES).unwrap_or(false) {
            cx.emit(EditorAreaEvent::Toast(format!("{} is too large to open", path.display()).into()));
            return;
        }
        let content = match std::fs::read(&path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(_) => {
                    cx.emit(EditorAreaEvent::Toast("Binary files are not supported yet".into()));
                    return;
                }
            },
            Err(err) => {
                cx.emit(EditorAreaEvent::Toast(format!("Cannot open {}: {err}", path.display()).into()));
                return;
            }
        };
        let language = language_for(&path);
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .indent_guides(true)
                .folding(true)
                .soft_wrap(false)
                .tab_size(TabSize { tab_size: 4, hard_tabs: false })
                .default_value(content)
        });
        let path_for_sub = path.clone();
        let subs = vec![
            cx.subscribe(&state, move |this, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change)
                    && let Some(f) = this.files.iter_mut().find(|f| f.path == path_for_sub)
                    && !f.dirty
                {
                    f.dirty = true;
                    cx.notify();
                }
            }),
            cx.observe(&state, |_, _, cx| cx.emit(EditorAreaEvent::CursorMoved)),
        ];
        let at = self.active.map(|a| a + 1).unwrap_or(self.files.len());
        self.files.insert(at, OpenFile { path, state, dirty: false, language, _subs: subs });
        self.activate(at, window, cx);
    }

    pub fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.files.len() {
            return;
        }
        self.active = Some(ix);
        let path = self.files[ix].path.clone();
        self.touch_recent(&path);
        let state = self.files[ix].state.clone();
        state.update(cx, |s, cx| s.focus(window, cx));
        self.tab_scroll.scroll_to_item(ix);
        cx.emit(EditorAreaEvent::ActiveChanged(Some(path)));
        cx.notify();
    }

    pub fn close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.files.len() {
            return;
        }
        self.files.remove(ix);
        if self.files.is_empty() {
            self.active = None;
            window.focus(&self.focus, cx);
            cx.emit(EditorAreaEvent::ActiveChanged(None));
            cx.notify();
            return;
        }
        let next = match self.active {
            Some(a) if a > ix => a - 1,
            Some(a) if a == ix => ix.min(self.files.len() - 1),
            Some(a) => a,
            None => 0,
        };
        self.activate(next, window, cx);
    }

    pub fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(a) = self.active {
            self.close(a, window, cx);
        }
    }

    pub fn cycle(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.files.len() as isize;
        if n == 0 {
            return;
        }
        let cur = self.active.unwrap_or(0) as isize;
        self.activate(((cur + delta).rem_euclid(n)) as usize, window, cx);
    }

    pub fn save_all(&mut self, cx: &mut Context<Self>) {
        for f in self.files.iter_mut().filter(|f| f.dirty) {
            let text = f.state.read(cx).value();
            match std::fs::write(&f.path, text.as_bytes()) {
                Ok(()) => f.dirty = false,
                Err(err) => cx.emit(EditorAreaEvent::Toast(format!("Save failed: {err}").into())),
            }
        }
        cx.notify();
    }

    /// Run a pure line command against the active buffer as one undoable replacement.
    fn edit_with(&mut self, window: &mut Window, cx: &mut Context<Self>, f: impl FnOnce(&str, Range<usize>, &'static str) -> Option<Edit>) {
        let Some(file) = self.active_file() else { return };
        let lang = file.language;
        file.state.clone().update(cx, |st, cx| {
            let text = st.text().to_string();
            let sel = st.selected_range();
            let sel = sel.start.min(sel.end)..sel.start.max(sel.end);
            if let Some(e) = f(&text, sel, lang) {
                st.set_selected_range(e.range.clone(), cx);
                st.replace(e.text, window, cx);
                st.set_selected_range(e.selection, cx);
            }
        });
    }

    pub fn duplicate_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_with(window, cx, |t, s, _| Some(ops::duplicate(t, s)));
    }
    pub fn delete_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_with(window, cx, |t, s, _| Some(ops::delete_lines(t, s)));
    }
    pub fn move_line(&mut self, up: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_with(window, cx, |t, s, _| ops::move_lines(t, s, up));
    }
    pub fn toggle_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_with(window, cx, |t, s, lang| Some(ops::toggle_comment(t, s, line_comment(lang))));
    }

    /// 1-based (line, column) of the active caret, for the status bar.
    pub fn cursor(&self, cx: &App) -> Option<(u32, u32)> {
        self.active_file().map(|f| {
            let pos = f.state.read(cx).cursor_position();
            (pos.line + 1, pos.character + 1)
        })
    }

    pub fn go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = self.active_file() else { return };
        let editor = file.state.clone();
        let input = self.go_to_line.clone();
        let (line, col) = self.cursor(cx).unwrap_or((1, 1));
        input.update(cx, |s, cx| {
            s.set_value("", window, cx);
            s.set_placeholder(format!("{line}:{col}"), window, cx);
        });
        window.open_dialog(cx, move |dialog, window, cx| {
            input.update(cx, |s, cx| s.focus(window, cx));
            let editor = editor.clone();
            let input_ok = input.clone();
            dialog
                .title("Go to Line:Column")
                .w(px(340.))
                .child(v_flex().gap(px(6.)).child(Input::new(&input)).child(
                    div().text_size(px(12.)).text_color(pal(cx).muted).child("[Line] [:column]"),
                ))
                .on_ok(move |_, window, cx| {
                    let q = input_ok.read(cx).value().to_string();
                    let mut parts = q.split(':').map(|s| s.trim().parse::<u32>().ok());
                    let Some(Some(line)) = parts.next() else { return false };
                    let col = parts.next().flatten().unwrap_or(1);
                    editor.update(cx, |st, cx| {
                        st.set_cursor_position(Position::new(line.saturating_sub(1), col.saturating_sub(1)), window, cx);
                        st.focus(window, cx);
                    });
                    true
                })
        });
    }
}

impl BasePanel for EditorArea {
    fn panel_name(&self) -> &'static str {
        "tandem.editor"
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for EditorArea {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Editor"
    }
    fn title_bar(&self, _: &App) -> bool {
        false
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }
}

impl Render for EditorArea {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let body: AnyElement = match self.active_file() {
            Some(f) => Editor::new(&f.state)
                .bordered(false)
                .p_0()
                .h_full()
                .font_family(MONO_FONT)
                .text_size(px(self.font_size))
                .line_height(relative(1.2))
                .into_any_element(),
            None => empty_state(&p).into_any_element(),
        };
        v_flex()
            .id("editor-area")
            .track_focus(&self.focus)
            .size_full()
            .bg(p.editor_bg)
            .when(!self.files.is_empty(), |d| {
                d.child(tabs::render_tab_strip(self, &self.tab_scroll, &p, window, cx))
                    .child(breadcrumbs::render(&self.root, self.active_file().map(|f| f.path.as_path()), &p))
            })
            .child(div().flex_1().min_h_0().child(body))
    }
}

fn empty_state(p: &crate::theme::Palette) -> impl IntoElement {
    let row = |label: &'static str, keys: &'static str| {
        h_flex()
            .gap(px(10.))
            .child(div().w(px(150.)).flex().justify_end().text_color(p.muted).child(label))
            .child(div().w(px(150.)).text_color(p.accent).child(keys))
    };
    div().size_full().flex().items_center().justify_center().child(
        v_flex()
            .gap(px(10.))
            .text_size(px(13.))
            .child(row("Search Everywhere", "Double ⇧"))
            .child(row("Go to File", "⇧⌘O"))
            .child(row("Recent Files", "⌘E"))
            .child(row("Agents", "⌥⌘A"))
            .child(row("Drop files here to open them", "")),
    )
}
