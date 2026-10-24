//! Project tool window: `tandem_project::FileTree` kept live by a `Watcher`, git status colors
//! from `tandem_git::Repo::status`, and the New/Rename/Delete/Reveal/Copy Path context menu.

use crate::assets::{file_icon, icon};
use crate::model::GitFileStatus;
use crate::theme::{Palette, pal};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::{WindowExt as _, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tandem_project::{FileTree, Watcher, icon_for_entry};

actions!(
    project_panel,
    [NewFile, NewDirectory, RenameEntry, DeleteEntry, RevealInFinder, CopyPath, CopyRelativePath]
);

pub enum ProjectEvent {
    OpenFile(PathBuf),
}

#[derive(Clone)]
struct Row {
    path: PathBuf,
    name: String,
    is_dir: bool,
    ignored: bool,
    expanded: bool,
    icon: &'static str,
    depth: usize,
}

pub struct ProjectPanel {
    root: PathBuf,
    tree: FileTree,
    rows: Vec<Row>,
    selected: Option<PathBuf>,
    pub autoscroll_from_source: bool,
    git: HashMap<PathBuf, GitFileStatus>,
    git_refreshing: bool,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    name_input: Entity<InputState>,
    _watcher: Option<Watcher>,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<ProjectEvent> for ProjectPanel {}

impl Focusable for ProjectPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Map a `tandem_project::icon_for_entry` key to a glyph + color.
pub fn glyph(key: &str, name: &str, p: &Palette) -> (IconName, Hsla) {
    match key {
        "folder" => (IconName::Folder, p.muted),
        "folder-open" => (IconName::FolderOpen, p.muted),
        "git" => (IconName::GitBranch, p.muted),
        "lock" => (IconName::FileLock, p.muted),
        "config" => (IconName::FileCog, p.muted),
        "image" => (IconName::FileImage, p.ok),
        _ => file_icon(name),
    }
}

/// Collapse `tandem_git` index/worktree changes into the tree's color classes.
pub fn git_status_map(root: &Path) -> HashMap<PathBuf, GitFileStatus> {
    use tandem_git::Change;
    let Ok(repo) = tandem_git::Repo::open(root) else { return HashMap::new() };
    let Ok(entries) = repo.status() else { return HashMap::new() };
    let base = repo.root().to_path_buf();
    entries
        .into_iter()
        .filter_map(|e| {
            let s = match (e.index, e.worktree) {
                (_, Change::Untracked) => GitFileStatus::Untracked,
                (Change::Added, _) => GitFileStatus::Added,
                (_, Change::Deleted) | (Change::Deleted, _) => GitFileStatus::Deleted,
                (Change::Unmodified, Change::Unmodified) => return None,
                _ => GitFileStatus::Modified,
            };
            Some((base.join(&e.path), s))
        })
        .collect()
}

impl ProjectPanel {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let tree = FileTree::new(&root);
        let watcher = Watcher::new(&root).ok();
        let mut tasks = Vec::new();
        if let Some(w) = &watcher {
            let rx = w.events();
            tasks.push(cx.spawn(async move |this, cx| {
                while let Ok(ev) = rx.recv().await {
                    let mut evs = vec![ev];
                    while let Ok(e) = rx.try_recv() {
                        evs.push(e);
                    }
                    let alive = this.update(cx, |p, cx| {
                        for ev in &evs {
                            p.tree.apply_event(ev);
                        }
                        p.rebuild();
                        p.refresh_git(cx);
                        cx.notify();
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            }));
        }
        let name_input = cx.new(|cx| InputState::new(window, cx));
        let mut this = Self {
            root,
            tree,
            rows: vec![],
            selected: None,
            autoscroll_from_source: true,
            git: HashMap::new(),
            git_refreshing: false,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            name_input,
            _watcher: watcher,
            _tasks: tasks,
        };
        this.rebuild();
        this.refresh_git(cx);
        this
    }

    pub fn refresh_git(&mut self, cx: &mut Context<Self>) {
        if self.git_refreshing {
            return;
        }
        self.git_refreshing = true;
        let root = self.root.clone();
        let task = cx.background_spawn(async move { git_status_map(&root) });
        cx.spawn(async move |this, cx| {
            let map = task.await;
            let _ = this.update(cx, |p, cx| {
                p.git = map;
                p.git_refreshing = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn rebuild(&mut self) {
        let root_name = self.root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| self.root.display().to_string());
        let mut rows = vec![Row { path: self.root.clone(), name: root_name, is_dir: true, ignored: false, expanded: true, icon: "folder-open", depth: 0 }];
        rows.extend(self.tree.visible_rows().into_iter().map(|r| Row {
            icon: icon_for_entry(&r.entry.name, r.entry.is_dir, r.expanded),
            path: r.entry.path,
            name: r.entry.name,
            is_dir: r.entry.is_dir,
            ignored: r.entry.ignored,
            expanded: r.expanded,
            depth: r.depth + 1,
        }));
        self.rows = rows;
    }

    pub fn toggle(&mut self, path: &Path, cx: &mut Context<Self>) {
        if path == self.root {
            return;
        }
        if self.tree.is_expanded(path) {
            self.tree.collapse(path);
        } else {
            self.tree.expand(path);
        }
        self.rebuild();
        cx.notify();
    }

    /// Expand ancestors of `path`, select it and scroll it into view ("Select Opened File").
    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Ok(rel) = path.strip_prefix(&self.root) else { return };
        let mut cur = self.root.clone();
        let comps: Vec<_> = rel.components().collect();
        for c in comps.iter().take(comps.len().saturating_sub(1)) {
            cur.push(c);
            self.tree.expand(&cur);
        }
        self.selected = Some(path.to_path_buf());
        self.rebuild();
        if let Some(ix) = self.rows.iter().position(|r| r.path == path) {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        cx.notify();
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn selected(&self) -> Option<&PathBuf> {
        self.selected.as_ref()
    }

    fn activate(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix).cloned() else { return };
        if row.is_dir {
            self.toggle(&row.path, cx);
        } else {
            cx.emit(ProjectEvent::OpenFile(row.path));
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let cur = self.selected.as_ref().and_then(|s| self.rows.iter().position(|r| &r.path == s)).unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, self.rows.len() as isize - 1) as usize;
        self.selected = Some(self.rows[next].path.clone());
        self.scroll.scroll_to_item(next, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// Directory new entries go into: the selection if it is a directory, else its parent.
    fn target_dir(&self) -> PathBuf {
        match &self.selected {
            Some(p) if p.is_dir() => p.clone(),
            Some(p) => p.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone()),
            None => self.root.clone(),
        }
    }

    fn after_fs_change(&mut self, dir: &Path, select: Option<PathBuf>, cx: &mut Context<Self>) {
        self.tree.refresh(dir);
        if dir != self.root {
            self.tree.expand(dir);
        }
        if let Some(s) = select {
            self.selected = Some(s);
        }
        self.rebuild();
        self.refresh_git(cx);
        cx.notify();
    }

    fn prompt_name(&mut self, title: String, initial: String, window: &mut Window, cx: &mut Context<Self>, on_ok: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) -> Result<(), String> + 'static) {
        let input = self.name_input.clone();
        input.update(cx, |s, cx| {
            s.set_value(initial, window, cx);
            s.select_all(window, cx);
        });
        let me = cx.weak_entity();
        let on_ok = std::rc::Rc::new(on_ok);
        window.open_dialog(cx, move |dialog, window, cx| {
            input.update(cx, |s, cx| s.focus(window, cx));
            let (input_ok, me, on_ok) = (input.clone(), me.clone(), on_ok.clone());
            dialog.title(title.clone()).w(px(360.)).child(v_flex().gap(px(6.)).child(Input::new(&input))).on_ok(move |_, window, cx| {
                let name = input_ok.read(cx).value().trim().to_string();
                if name.is_empty() || name.contains('/') && !name.split('/').all(|s| !s.is_empty() && s != "..") {
                    return false;
                }
                let res = me.update(cx, |this, cx| on_ok(this, name, window, cx));
                match res {
                    Ok(Ok(())) => true,
                    Ok(Err(msg)) => {
                        window.push_notification(gpui_kit::component::notification::Notification::error(msg), cx);
                        false
                    }
                    Err(_) => true,
                }
            })
        });
    }

    fn new_entry(&mut self, dir_kind: bool, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.target_dir();
        let title = if dir_kind { "New Directory" } else { "New File" }.to_string();
        self.prompt_name(title, String::new(), window, cx, move |this, name, window, cx| {
            let path = dir.join(&name);
            if path.exists() {
                return Err(format!("{name} already exists"));
            }
            let res = if dir_kind {
                std::fs::create_dir_all(&path)
            } else {
                path.parent().map(std::fs::create_dir_all).transpose().and_then(|_| std::fs::write(&path, ""))
            };
            res.map_err(|e| e.to_string())?;
            this.after_fs_change(&dir, Some(path.clone()), cx);
            if !dir_kind {
                cx.emit(ProjectEvent::OpenFile(path));
            }
            let _ = window;
            Ok(())
        });
    }

    fn rename_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone().filter(|p| *p != self.root) else { return };
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        self.prompt_name(format!("Rename '{name}'"), name, window, cx, move |this, new_name, _, cx| {
            let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
            let to = parent.join(&new_name);
            if to.exists() {
                return Err(format!("{new_name} already exists"));
            }
            std::fs::rename(&path, &to).map_err(|e| e.to_string())?;
            this.after_fs_change(&parent, Some(to), cx);
            Ok(())
        });
    }

    fn delete_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone().filter(|p| *p != self.root) else { return };
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let what = if path.is_dir() { "directory" } else { "file" };
        let me = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (path, me) = (path.clone(), me.clone());
            alert
                .title(format!("Delete {what}"))
                .description(format!("Delete '{name}'? This cannot be undone."))
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let res = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
                    if let Err(e) = res {
                        window.push_notification(gpui_kit::component::notification::Notification::error(e.to_string()), cx);
                    }
                    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
                    let _ = me.update(cx, |this, cx| this.after_fs_change(&parent, Some(parent.clone()), cx));
                    true
                })
        });
    }

    fn render_row(&self, ix: usize, focused: bool, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let e = &self.rows[ix];
        let selected = self.selected.as_ref() == Some(&e.path);
        let git = self.git.get(&e.path).copied();
        let name_color = match git {
            Some(GitFileStatus::Added) => p.git_added,
            Some(GitFileStatus::Modified) => p.git_modified,
            Some(GitFileStatus::Deleted) => p.git_deleted,
            Some(GitFileStatus::Untracked) => p.git_untracked,
            None if e.ignored => p.excluded,
            None => p.text,
        };
        let (glyph, glyph_color) = glyph(e.icon, &e.name, p);
        let glyph_color = if e.ignored && e.is_dir { p.excluded } else { glyph_color };
        let path = e.path.clone();
        let (is_dir, open) = (e.is_dir, e.expanded);
        h_flex_row()
            .id(("proj-row", ix))
            .h(px(24.))
            .w_full()
            .pl(px(8. + e.depth as f32 * 18.))
            .pr(px(8.))
            .gap(px(4.))
            .when(selected, |d| d.bg(if focused { p.row_selected } else { p.row_selected_inactive }))
            .when(!selected, |d| d.hover(|s| s.bg(p.hover)))
            .child(
                div()
                    .id(("proj-chevron", ix))
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(is_dir && ix > 0, |d| d.child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 14., p.muted)))
                    .when(ix == 0, |d| d.child(icon(IconName::ChevronDown, 14., p.muted)))
                    .on_click(cx.listener({
                        let path = path.clone();
                        move |this, _, _, cx| {
                            if is_dir {
                                this.toggle(&path, cx);
                            }
                        }
                    })),
            )
            .child(icon(glyph, 16., glyph_color))
            .child(div().text_color(name_color).whitespace_nowrap().child(e.name.clone()))
            .when(ix == 0, |d| d.child(div().pl(px(6.)).text_color(p.muted).whitespace_nowrap().child(tilde(&self.root))))
            .on_mouse_down(MouseButton::Right, cx.listener({
                let path = path.clone();
                move |this, _, window, cx| {
                    this.selected = Some(path.clone());
                    window.focus(&this.focus, cx);
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                this.selected = Some(path.clone());
                window.focus(&this.focus, cx);
                if ev.click_count() >= 2 {
                    this.activate(ix, cx);
                }
                cx.notify();
            }))
            .into_any_element()
    }
}

fn h_flex_row() -> Div {
    div().flex().flex_row().items_center()
}

fn tilde(path: &Path) -> String {
    let s = path.display().to_string();
    match dirs::home_dir() {
        Some(h) => s.replacen(&h.display().to_string(), "~", 1),
        None => s,
    }
}

impl Render for ProjectPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let focused = self.focus.contains_focused(window, cx);
        let count = self.rows.len();
        div()
            .id("project-panel")
            .key_context("ProjectPanel")
            .track_focus(&self.focus)
            .size_full()
            .text_size(px(13.))
            .on_action(cx.listener(|this, _: &NewFile, window, cx| this.new_entry(false, window, cx)))
            .on_action(cx.listener(|this, _: &NewDirectory, window, cx| this.new_entry(true, window, cx)))
            .on_action(cx.listener(|this, _: &RenameEntry, window, cx| this.rename_entry(window, cx)))
            .on_action(cx.listener(|this, _: &DeleteEntry, window, cx| this.delete_entry(window, cx)))
            .on_action(cx.listener(|this, _: &RevealInFinder, _, _| {
                if let Some(p) = &this.selected {
                    let _ = std::process::Command::new("open").arg("-R").arg(p).spawn();
                }
            }))
            .on_action(cx.listener(|this, _: &CopyPath, _, cx| {
                if let Some(p) = &this.selected {
                    cx.write_to_clipboard(ClipboardItem::new_string(p.display().to_string()));
                }
            }))
            .on_action(cx.listener(|this, _: &CopyRelativePath, _, cx| {
                if let Some(p) = &this.selected {
                    let rel = p.strip_prefix(&this.root).unwrap_or(p);
                    cx.write_to_clipboard(ClipboardItem::new_string(rel.display().to_string()));
                }
            }))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                match ev.keystroke.key.as_str() {
                    "up" => this.move_selection(-1, cx),
                    "down" => this.move_selection(1, cx),
                    "backspace" | "delete" if ev.keystroke.modifiers.platform || ev.keystroke.key == "delete" => this.delete_entry(window, cx),
                    "enter" | "right" | "left" => {
                        if let Some(ix) = this.selected.as_ref().and_then(|s| this.rows.iter().position(|r| &r.path == s)) {
                            let row = this.rows[ix].clone();
                            match ev.keystroke.key.as_str() {
                                "enter" => this.activate(ix, cx),
                                "right" if row.is_dir && !row.expanded => this.toggle(&row.path, cx),
                                "left" if row.is_dir && row.expanded => this.toggle(&row.path, cx),
                                _ => {}
                            }
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(
                uniform_list("project-tree", count, cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    range.map(|ix| this.render_row(ix, focused, &p, cx)).collect::<Vec<_>>()
                }))
                .size_full()
                .track_scroll(&self.scroll),
            )
            .context_menu(|menu, _, _| {
                menu.menu("New File…", Box::new(NewFile))
                    .menu("New Directory…", Box::new(NewDirectory))
                    .separator()
                    .menu("Rename…", Box::new(RenameEntry))
                    .menu("Delete…", Box::new(DeleteEntry))
                    .separator()
                    .menu("Copy Path", Box::new(CopyPath))
                    .menu("Copy Path From Repository Root", Box::new(CopyRelativePath))
                    .menu("Reveal in Finder", Box::new(RevealInFinder))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn git_map_is_empty_outside_a_repo() {
        let tmp = std::env::temp_dir().join(format!("tandem-proj-nogit-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        // Temp dirs are not inside a repository on CI or dev machines.
        if tandem_git::Repo::open(&tmp).is_err() {
            assert!(git_status_map(&tmp).is_empty());
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
