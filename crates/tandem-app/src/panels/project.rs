//! Project tool window: lazily expanded filesystem tree.

use crate::assets::{file_icon, icon};
use crate::keymap::NotImplemented;
use crate::model::project::{DirEntry, IgnoreRules, list_dir};
use crate::model::{GitChange, GitFileStatus};
use crate::theme::{Palette, pal};
use gpui_kit::assets::IconName;
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub enum ProjectEvent {
    OpenFile(PathBuf),
}

#[derive(Clone)]
struct Row {
    entry: DirEntry,
    depth: usize,
}

pub struct ProjectPanel {
    root: PathBuf,
    rules: IgnoreRules,
    children: HashMap<PathBuf, Vec<DirEntry>>,
    expanded: HashSet<PathBuf>,
    rows: Vec<Row>,
    selected: Option<PathBuf>,
    pub autoscroll_from_source: bool,
    git: HashMap<PathBuf, GitFileStatus>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
}

impl EventEmitter<ProjectEvent> for ProjectPanel {}

impl Focusable for ProjectPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ProjectPanel {
    pub fn new(root: PathBuf, git_changes: &[GitChange], cx: &mut Context<Self>) -> Self {
        let rules = IgnoreRules::new(&root);
        let mut expanded = HashSet::new();
        expanded.insert(root.clone());
        let git = git_changes.iter().map(|c| (root.join(&c.path), c.status)).collect();
        let mut this = Self {
            root,
            rules,
            children: HashMap::new(),
            expanded,
            rows: vec![],
            selected: None,
            autoscroll_from_source: true,
            git,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
        };
        this.rebuild();
        this
    }

    fn children_of(&mut self, dir: &Path) -> Vec<DirEntry> {
        if !self.children.contains_key(dir) {
            let list = list_dir(dir, &self.rules);
            self.children.insert(dir.to_path_buf(), list);
        }
        self.children[dir].clone()
    }

    fn rebuild(&mut self) {
        let root_name = self.root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| self.root.display().to_string());
        let mut rows = vec![Row { entry: DirEntry { path: self.root.clone(), name: root_name, is_dir: true, excluded: false }, depth: 0 }];
        let root = self.root.clone();
        if self.expanded.contains(&root) {
            self.push_children(&root, 1, &mut rows);
        }
        self.rows = rows;
    }

    fn push_children(&mut self, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
        for entry in self.children_of(dir) {
            let open = entry.is_dir && self.expanded.contains(&entry.path);
            let path = entry.path.clone();
            rows.push(Row { entry, depth });
            if open {
                self.push_children(&path, depth + 1, rows);
            }
        }
    }

    pub fn toggle(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_path_buf());
        }
        self.rebuild();
        cx.notify();
    }

    /// Expand ancestors of `path`, select it and scroll it into view ("Select Opened File").
    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Ok(rel) = path.strip_prefix(&self.root) else { return };
        let mut cur = self.root.clone();
        self.expanded.insert(cur.clone());
        let comps: Vec<_> = rel.components().collect();
        for c in comps.iter().take(comps.len().saturating_sub(1)) {
            cur.push(c);
            self.expanded.insert(cur.clone());
        }
        self.selected = Some(path.to_path_buf());
        self.rebuild();
        if let Some(ix) = self.rows.iter().position(|r| r.entry.path == path) {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        cx.notify();
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    fn activate(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix).cloned() else { return };
        if row.entry.is_dir {
            self.toggle(&row.entry.path, cx);
        } else {
            cx.emit(ProjectEvent::OpenFile(row.entry.path));
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let cur = self.selected.as_ref().and_then(|s| self.rows.iter().position(|r| &r.entry.path == s)).unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, self.rows.len() as isize - 1) as usize;
        self.selected = Some(self.rows[next].entry.path.clone());
        self.scroll.scroll_to_item(next, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn render_row(&self, ix: usize, focused: bool, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let row = &self.rows[ix];
        let e = &row.entry;
        let selected = self.selected.as_ref() == Some(&e.path);
        let open = e.is_dir && self.expanded.contains(&e.path);
        let git = self.git.get(&e.path).copied();
        let name_color = match git {
            Some(GitFileStatus::Added) => p.git_added,
            Some(GitFileStatus::Modified) => p.git_modified,
            Some(GitFileStatus::Deleted) => p.git_deleted,
            Some(GitFileStatus::Untracked) => p.git_untracked,
            None if e.excluded => p.excluded,
            None => p.text,
        };
        let (glyph, glyph_color) = if e.is_dir {
            (if open { IconName::FolderOpen } else { IconName::Folder }, if e.excluded { p.excluded } else { p.muted })
        } else {
            file_icon(&e.name)
        };
        let path = e.path.clone();
        let is_dir = e.is_dir;
        h_flex_row()
            .id(("proj-row", ix))
            .h(px(24.))
            .w_full()
            .pl(px(8. + row.depth as f32 * 18.))
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
                    .when(is_dir, |d| d.child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 14., p.muted)))
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
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                match ev.keystroke.key.as_str() {
                    "up" => this.move_selection(-1, cx),
                    "down" => this.move_selection(1, cx),
                    "enter" | "right" | "left" => {
                        if let Some(ix) = this.selected.as_ref().and_then(|s| this.rows.iter().position(|r| &r.entry.path == s)) {
                            let row = this.rows[ix].clone();
                            let open = this.expanded.contains(&row.entry.path);
                            match ev.keystroke.key.as_str() {
                                "enter" => this.activate(ix, cx),
                                "right" if row.entry.is_dir && !open => this.toggle(&row.entry.path, cx),
                                "left" if row.entry.is_dir && open => this.toggle(&row.entry.path, cx),
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
                menu.menu("New File…", Box::new(NotImplemented("New File".into())))
                    .menu("New Directory…", Box::new(NotImplemented("New Directory".into())))
                    .separator()
                    .menu("Rename…", Box::new(NotImplemented("Rename".into())))
                    .menu("Delete…", Box::new(NotImplemented("Delete".into())))
                    .separator()
                    .menu("Copy Path", Box::new(NotImplemented("Copy Path".into())))
                    .menu("Reveal in Finder", Box::new(NotImplemented("Reveal in Finder".into())))
            })
    }
}
