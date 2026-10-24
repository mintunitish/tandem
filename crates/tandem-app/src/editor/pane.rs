use super::actions::{FindUsages, ReformatCode, RenameSymbol};
use super::find::FindPanel;
use super::lsp::{self, LspHub, LspHubEvent, LspProviders};
use super::ops::{self, Edit};
use super::usages::{Usage, UsagesPanel};
use super::{breadcrumbs, gutter, tabs};
use crate::assets::{language_for, line_comment};
use crate::stripes::ToolWindowId;
use crate::theme::{MONO_FONT, pal};
use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_kit::component::highlighter::Diagnostic;
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState, Position, RopeExt as _, TabSize};
use gpui_kit::component::{WindowExt as _, h_flex, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use std::cell::OnceCell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use tandem_git::GutterMark;
use tandem_lsp::lsp_types as lspt;

pub struct OpenFile {
    pub path: PathBuf,
    pub state: Entity<EditorState>,
    pub dirty: bool,
    pub language: &'static str,
    /// Hash of the text last loaded from / written to disk (to skip no-op reloads).
    saved_hash: u64,
    /// Working-tree vs HEAD change marks (refreshed on open and save).
    pub git_marks: Arc<Vec<GutterMark>>,
    /// Error-stripe severity per line, rebuilt when the file's diagnostics change.
    stripe: Arc<gutter::StripeMap>,
    /// `(line separator, indent)` for the status bar; reset by every edit.
    format: OnceCell<(&'static str, SharedString)>,
    _subs: Vec<Subscription>,
}

impl OpenFile {
    /// `(line separator, indent)` sniffed from the buffer, cached until the next edit.
    pub fn format(&self, cx: &App) -> (&'static str, SharedString) {
        self.format.get_or_init(|| crate::statusbar::detect_format(self.state.read(cx).text())).clone()
    }
}

pub enum EditorAreaEvent {
    ActiveChanged(Option<PathBuf>),
    CursorMoved,
    Toast(SharedString),
    /// A file was written by ⌘S.
    Saved(PathBuf),
    /// Ask the workspace to show a tool window (Usages / Find results live in Problems).
    ShowToolWindow(ToolWindowId),
}

pub struct EditorArea {
    pub root: PathBuf,
    pub files: Vec<OpenFile>,
    pub active: Option<usize>,
    /// Most-recent first, for ⌘E.
    pub recent: Vec<PathBuf>,
    pub hovered_tab: Option<usize>,
    pub font_size: f32,
    pub lsp: Entity<LspHub>,
    pub find: Entity<FindPanel>,
    pub usages: Entity<UsagesPanel>,
    focus: FocusHandle,
    go_to_line: Entity<InputState>,
    rename_input: Entity<InputState>,
    tab_scroll: ScrollHandle,
    _subs: Vec<Subscription>,
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

fn hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

impl EditorArea {
    pub fn new(root: PathBuf, font_size: f32, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let go_to_line = cx.new(|cx| InputState::new(window, cx));
        let rename_input = cx.new(|cx| InputState::new(window, cx));
        let lsp = cx.new(|cx| LspHub::new(root.clone(), cx));
        let me = cx.weak_entity();
        let find = cx.new(|cx| FindPanel::new(root.clone(), me.clone(), window, cx));
        let usages = cx.new(|_| UsagesPanel::new(root.clone(), me));
        let subs = vec![cx.subscribe(&lsp, |this, _, ev: &LspHubEvent, cx| {
            if let LspHubEvent::Diagnostics(path) = ev {
                this.apply_diagnostics(path, cx);
            }
            cx.notify();
        })];
        Self {
            root,
            files: vec![],
            active: None,
            recent: vec![],
            hovered_tab: None,
            font_size,
            lsp,
            find,
            usages,
            focus: cx.focus_handle(),
            go_to_line,
            rename_input,
            tab_scroll: ScrollHandle::new(),
            _subs: subs,
        }
    }

    pub fn active_file(&self) -> Option<&OpenFile> {
        self.active.and_then(|i| self.files.get(i))
    }

    pub fn active_path(&self) -> Option<PathBuf> {
        self.active_file().map(|f| f.path.clone())
    }

    fn file(&self, path: &Path) -> Option<&OpenFile> {
        self.files.iter().find(|f| f.path == path)
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
        let saved_hash = hash(&content);
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
        if self.lsp.read(cx).manager.config_for(&path).is_some() {
            let providers = Rc::new(LspProviders { hub: self.lsp.clone(), path: path.clone(), area: cx.weak_entity() });
            lsp::install(&state, providers, cx);
            let weak = state.downgrade();
            self.lsp.update(cx, |hub, cx| hub.open(&path, weak, cx));
        }
        let path_for_sub = path.clone();
        let subs = vec![
            // Every edit marks the buffer modified until the next save (`set_value` reloads emit
            // no `Change`); LSP gets a rope snapshot, stringified when its debounce fires.
            cx.subscribe(&state, move |this, state, ev: &InputEvent, cx| {
                if !matches!(ev, InputEvent::Change) {
                    return;
                }
                let text = state.read(cx).text().clone();
                this.lsp.update(cx, |hub, cx| hub.change(&path_for_sub, text, cx));
                if let Some(f) = this.files.iter_mut().find(|f| f.path == path_for_sub) {
                    f.format = OnceCell::new();
                    if !f.dirty {
                        f.dirty = true;
                        cx.notify();
                    }
                }
            }),
            cx.observe(&state, |_, _, cx| cx.emit(EditorAreaEvent::CursorMoved)),
        ];
        let at = self.active.map(|a| a + 1).unwrap_or(self.files.len());
        let file = OpenFile {
            path: path.clone(),
            state,
            dirty: false,
            language,
            saved_hash,
            git_marks: Arc::default(),
            stripe: Arc::default(),
            format: OnceCell::new(),
            _subs: subs,
        };
        self.files.insert(at, file);
        self.refresh_git_marks(&path, cx);
        self.apply_diagnostics(&path, cx);
        self.activate(at, window, cx);
    }

    /// Open `path` and put the caret at `pos` (0-based line / UTF-16 column).
    pub fn open_at(&mut self, path: PathBuf, pos: lspt::Position, window: &mut Window, cx: &mut Context<Self>) {
        self.open(path.clone(), window, cx);
        if let Some(f) = self.file(&path) {
            let state = f.state.clone();
            state.update(cx, |s, cx| {
                s.set_cursor_position(Position::new(pos.line, pos.character), window, cx);
                s.focus(window, cx);
            });
        }
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
        let closed = self.files.remove(ix);
        self.lsp.update(cx, |hub, _| hub.close(&closed.path));
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

    /// ⌘S: write every modified buffer, `didSave` it and refresh its gutter marks.
    pub fn save_all(&mut self, cx: &mut Context<Self>) {
        let mut saved = Vec::new();
        for f in self.files.iter_mut().filter(|f| f.dirty) {
            let text = f.state.read(cx).value();
            match std::fs::write(&f.path, text.as_bytes()) {
                Ok(()) => {
                    f.dirty = false;
                    f.saved_hash = hash(&text);
                    saved.push(f.path.clone());
                }
                Err(err) => cx.emit(EditorAreaEvent::Toast(format!("Save failed: {err}").into())),
            }
        }
        for path in saved {
            self.lsp.update(cx, |hub, _| hub.save(&path));
            self.refresh_git_marks(&path, cx);
            cx.emit(EditorAreaEvent::Saved(path));
        }
        cx.notify();
    }

    /// Reload unmodified open buffers whose file changed on disk (external edits, Replace in
    /// Files, git checkout).
    pub fn reload_from_disk(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        for f in self.files.iter_mut().filter(|f| !f.dirty && paths.contains(&f.path)) {
            let Ok(text) = std::fs::read_to_string(&f.path) else { continue };
            let h = hash(&text);
            if h == f.saved_hash {
                continue;
            }
            f.saved_hash = h;
            f.format = OnceCell::new();
            f.state.update(cx, |s, cx| s.set_value(text, window, cx));
        }
        let paths = paths.to_vec();
        for p in paths {
            if self.file(&p).is_some() {
                self.refresh_git_marks(&p, cx);
            }
        }
    }

    pub fn refresh_git_marks(&mut self, path: &Path, cx: &mut Context<Self>) {
        let (root, p) = (self.root.clone(), path.to_path_buf());
        let task = cx.background_spawn(async move {
            tandem_git::Repo::open(&root).ok().and_then(|r| r.per_line_gutter_marks(&p).ok()).unwrap_or_default()
        });
        let p = path.to_path_buf();
        cx.spawn(async move |this, cx| {
            let marks = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(f) = this.files.iter_mut().find(|f| f.path == p) {
                    f.git_marks = Arc::new(marks);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn apply_diagnostics(&mut self, path: &Path, cx: &mut Context<Self>) {
        let mut diags = self.lsp.read(cx).diagnostics.get(path).cloned().unwrap_or_default();
        let Some(f) = self.files.iter_mut().find(|f| f.path == path) else { return };
        f.stripe = Arc::new(gutter::stripe_map(&diags));
        diags.sort_by_key(|d| (d.range.start.line, d.range.start.character));
        f.state.update(cx, |s, cx| {
            let text = s.text().clone();
            if let Some(set) = s.diagnostics_mut() {
                set.reset(&text);
                set.extend(diags.into_iter().map(Diagnostic::from));
            }
            cx.notify();
        });
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

    /// Selected text of the active editor (single line), to seed Find in Files.
    pub fn selected_text(&self, cx: &App) -> Option<String> {
        let f = self.active_file()?;
        let s = f.state.read(cx);
        let r = s.selected_range();
        let text = s.text().to_string();
        let sel = text.get(r.start.min(r.end)..r.start.max(r.end))?.to_string();
        (!sel.is_empty() && !sel.contains('\n')).then_some(sel)
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

    /// `(path, text, byte offset of caret)` of the active buffer.
    fn caret(&self, cx: &App) -> Option<(PathBuf, String, usize)> {
        let f = self.active_file()?;
        let s = f.state.read(cx);
        Some((f.path.clone(), s.text().to_string(), s.cursor()))
    }

    fn no_server(&self, path: &Path, cx: &mut Context<Self>) {
        let msg = match &self.lsp.read(cx).missing {
            Some((server, hint)) => format!("No language server: install {server}: {hint}"),
            None => format!("No language server for {}", path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()),
        };
        cx.emit(EditorAreaEvent::Toast(msg.into()));
    }

    /// ⇧F6: rename the symbol at the caret via LSP, applying the edit to every file.
    pub fn rename_symbol(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, text, offset)) = self.caret(cx) else { return };
        if !self.lsp.read(cx).is_open(&path) {
            return self.no_server(&path, cx);
        }
        let word = lsp::word_prefix(&text, offset).start..{
            let rest = &text[offset..];
            offset + rest.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')).unwrap_or(rest.len())
        };
        let old = text[word].to_string();
        let input = self.rename_input.clone();
        input.update(cx, |s, cx| {
            s.set_value(old.clone(), window, cx);
            s.select_all(window, cx);
        });
        let me = cx.weak_entity();
        window.open_dialog(cx, move |dialog, window, cx| {
            input.update(cx, |s, cx| s.focus(window, cx));
            let input_ok = input.clone();
            let (me, path, text) = (me.clone(), path.clone(), text.clone());
            dialog
                .title(format!("Rename '{old}'"))
                .w(px(380.))
                .child(v_flex().gap(px(6.)).child(div().text_size(px(12.)).text_color(pal(cx).muted).child("Rename symbol and its usages to:")).child(Input::new(&input)))
                .on_ok(move |_, window, cx| {
                    let new_name = input_ok.read(cx).value().trim().to_string();
                    if new_name.is_empty() {
                        return false;
                    }
                    let (path, text) = (path.clone(), text.clone());
                    let _ = me.update(cx, |this, cx| this.run_rename(path, text, offset, new_name, window, cx));
                    true
                })
        });
    }

    fn run_rename(&mut self, path: PathBuf, text: String, offset: usize, new_name: String, window: &mut Window, cx: &mut Context<Self>) {
        let pos = lsp::position(&text, offset);
        let rx = self.lsp.update(cx, |hub, _| hub.request(&path, Some(&text), move |c, p| async move { c.rename(&p, pos, &new_name).await }));
        let Some(rx) = rx else { return };
        cx.spawn_in(window, async move |this, cx| {
            let res = lsp::recv(rx).await;
            let _ = this.update_in(cx, |this, window, cx| match res {
                Ok(edit) => {
                    let (files, edits) = this.apply_workspace_edit(&edit, window, cx);
                    cx.emit(EditorAreaEvent::Toast(format!("Renamed {edits} occurrence(s) in {files} file(s)").into()));
                }
                Err(err) => cx.emit(EditorAreaEvent::Toast(format!("Rename failed: {err}").into())),
            });
        })
        .detach();
    }

    /// ⌥F7: LSP references → Usages tool window.
    pub fn find_usages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, text, offset)) = self.caret(cx) else { return };
        if !self.lsp.read(cx).is_open(&path) {
            return self.no_server(&path, cx);
        }
        let range = lsp::word_prefix(&text, offset);
        let rest = &text[offset..];
        let end = offset + rest.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')).unwrap_or(rest.len());
        let symbol = text[range.start..end].to_string();
        let pos = lsp::position(&text, offset);
        let rx = self.lsp.update(cx, |hub, _| hub.request(&path, Some(&text), move |c, p| async move { c.references(&p, pos, true).await }));
        let Some(rx) = rx else { return };
        cx.spawn_in(window, async move |this, cx| {
            let res = lsp::recv(rx).await;
            let _ = this.update(cx, |this, cx| match res {
                Ok(locs) => {
                    let usages = this.usages_from(locs, cx);
                    this.usages.update(cx, |u, cx| u.set(symbol, usages, cx));
                    cx.emit(EditorAreaEvent::ShowToolWindow(ToolWindowId::Problems));
                }
                Err(err) => cx.emit(EditorAreaEvent::Toast(format!("Find Usages failed: {err}").into())),
            });
        })
        .detach();
    }

    fn line_text(&self, path: &Path, line: u32, cx: &App) -> String {
        let text = match self.file(path) {
            Some(f) => f.state.read(cx).value().to_string(),
            None => std::fs::read_to_string(path).unwrap_or_default(),
        };
        text.lines().nth(line as usize).unwrap_or("").to_string()
    }

    fn usages_from(&self, locs: Vec<lspt::Location>, cx: &App) -> Vec<Usage> {
        let mut out: Vec<Usage> = locs
            .into_iter()
            .filter_map(|l| {
                let path = tandem_lsp::uri_to_path(&l.uri)?;
                let line_text = self.line_text(&path, l.range.start.line, cx);
                Some(Usage { path, pos: l.range.start, line_text })
            })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path).then(a.pos.line.cmp(&b.pos.line)));
        out
    }

    /// ⌘⌥L: LSP formatting of the active file.
    pub fn reformat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, text, _)) = self.caret(cx) else { return };
        if !self.lsp.read(cx).is_open(&path) {
            return self.no_server(&path, cx);
        }
        let rx = self.lsp.update(cx, |hub, _| hub.request(&path, Some(&text), |c, p| async move { c.format_document(&p, 4, true).await }));
        let Some(rx) = rx else { return };
        cx.spawn_in(window, async move |this, cx| {
            let res = lsp::recv(rx).await;
            let _ = this.update_in(cx, |this, window, cx| match res {
                Ok(edits) if !edits.is_empty() => {
                    let mut map = HashMap::new();
                    map.insert(path, edits);
                    this.apply_edits(map, window, cx);
                }
                Ok(_) => {}
                Err(err) => cx.emit(EditorAreaEvent::Toast(format!("Reformat failed: {err}").into())),
            });
        })
        .detach();
    }

    /// Apply an LSP `WorkspaceEdit`: open buffers are edited in place (undoable), other files
    /// are rewritten on disk. Returns `(files, edits)` touched.
    pub fn apply_workspace_edit(&mut self, edit: &lspt::WorkspaceEdit, window: &mut Window, cx: &mut Context<Self>) -> (usize, usize) {
        self.apply_edits(workspace_edit_by_file(edit), window, cx)
    }

    fn apply_edits(&mut self, by_file: HashMap<PathBuf, Vec<lspt::TextEdit>>, window: &mut Window, cx: &mut Context<Self>) -> (usize, usize) {
        let (mut files, mut count) = (0, 0);
        let mut written = Vec::new();
        for (path, mut edits) in by_file {
            // Apply back to front so earlier ranges stay valid.
            edits.sort_by_key(|e| std::cmp::Reverse((e.range.start.line, e.range.start.character)));
            count += edits.len();
            files += 1;
            if let Some(f) = self.file(&path) {
                f.state.update(cx, |s, cx| s.apply_lsp_edits(&edits, window, cx));
            } else if let Ok(mut text) = std::fs::read_to_string(&path) {
                for e in &edits {
                    let r = tandem_lsp::range_to_offsets(&text, e.range);
                    text.replace_range(r, &e.new_text);
                }
                if std::fs::write(&path, text).is_ok() {
                    written.push(path);
                }
            }
        }
        for p in written {
            cx.emit(EditorAreaEvent::Saved(p));
        }
        (files, count)
    }
}

/// Flatten `changes` and `documentChanges` into per-file text edits.
pub fn workspace_edit_by_file(edit: &lspt::WorkspaceEdit) -> HashMap<PathBuf, Vec<lspt::TextEdit>> {
    let mut out: HashMap<PathBuf, Vec<lspt::TextEdit>> = HashMap::new();
    if let Some(changes) = &edit.changes {
        for (uri, edits) in changes {
            if let Some(p) = tandem_lsp::uri_to_path(uri) {
                out.entry(p).or_default().extend(edits.iter().cloned());
            }
        }
    }
    let mut doc_edit = |e: &lspt::TextDocumentEdit| {
        if let Some(p) = tandem_lsp::uri_to_path(&e.text_document.uri) {
            out.entry(p).or_default().extend(e.edits.iter().map(|x| match x {
                lspt::OneOf::Left(t) => t.clone(),
                lspt::OneOf::Right(a) => a.text_edit.clone(),
            }));
        }
    };
    match &edit.document_changes {
        Some(lspt::DocumentChanges::Edits(edits)) => edits.iter().for_each(&mut doc_edit),
        Some(lspt::DocumentChanges::Operations(ops)) => {
            for op in ops {
                if let lspt::DocumentChangeOperation::Edit(e) = op {
                    doc_edit(e);
                }
            }
        }
        None => {}
    }
    out
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
            Some(f) => {
                let editor = Editor::new(&f.state)
                    .bordered(false)
                    .p_0()
                    .h_full()
                    .font_family(MONO_FONT)
                    .text_size(px(self.font_size))
                    .line_height(relative(1.2));
                let lines = f.state.read(cx).text().lines_len().max(1);
                h_flex()
                    .size_full()
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(editor)
                            .child(gutter::change_marks(f.state.clone(), f.git_marks.clone(), &p)),
                    )
                    .child(gutter::error_stripe(f.state.clone(), &f.stripe, &f.git_marks, lines, &p))
                    .into_any_element()
            }
            None => empty_state(&p).into_any_element(),
        };
        v_flex()
            .id("editor-area")
            .track_focus(&self.focus)
            .size_full()
            .bg(p.editor_bg)
            .on_action(cx.listener(|this, _: &RenameSymbol, window, cx| this.rename_symbol(window, cx)))
            .on_action(cx.listener(|this, _: &FindUsages, window, cx| this.find_usages(window, cx)))
            .on_action(cx.listener(|this, _: &ReformatCode, window, cx| this.reformat(window, cx)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use std::str::FromStr;

    #[test]
    #[allow(clippy::mutable_key_type)]
    fn workspace_edit_flattens_both_shapes() {
        let uri = lspt::Uri::from_str("file:///tmp/a.ts").unwrap();
        let te = lspt::TextEdit::new(lspt::Range::default(), "x".into());
        let mut changes = HashMap::new();
        changes.insert(uri.clone(), vec![te.clone()]);
        let edit = lspt::WorkspaceEdit {
            changes: Some(changes),
            document_changes: Some(lspt::DocumentChanges::Edits(vec![lspt::TextDocumentEdit {
                text_document: lspt::OptionalVersionedTextDocumentIdentifier { uri, version: None },
                edits: vec![lspt::OneOf::Left(te)],
            }])),
            change_annotations: None,
        };
        let m = workspace_edit_by_file(&edit);
        assert_eq!(m[&PathBuf::from("/tmp/a.ts")].len(), 2);
    }
}
