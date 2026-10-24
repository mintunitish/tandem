//! The main window: New UI title bar, left/right tool-window stripes, a gpui-kit `DockArea`
//! (editor in the center; one tool window per side dock), status bar, Search Everywhere
//! overlay, and every workspace-level action handler.

use crate::editor::{EditorArea, EditorAreaEvent};
use crate::keymap::*;
use crate::model::project::{self, ProjectModel, ProjectModelEvent};
use crate::panels::agents::{AgentsEvent, AgentsPanel};
use crate::panels::database::DatabasePanel;
use crate::panels::git::{CommitPanel, GitLogPanel};
use crate::panels::harness::HarnessPanel;
use crate::panels::misc::{NotAvailable, ProblemsPanel, StructurePanel};
use crate::panels::project::{ProjectEvent, ProjectPanel};
use crate::panels::review::{ReviewEvent, ReviewPanel};
use crate::panels::terminal::TerminalPanel;
use crate::panels::{HideToolWindow, ToolWindow};
use crate::search_everywhere::{SearchEvent, SearchPopup, SearchTab};
use crate::state::{AppSettings, LayoutState, SideLayout};
use crate::statusbar::StatusBarView;
use crate::stripes::{STRIPE_WIDTH, Side, ToolWindowId, stripe_button};
use crate::theme::pal;
use crate::{settings, titlebar};
use gpui_kit::component::dock::{DockArea, DockEvent, DockLayout, DockPlacement, DockSkin, panel_handle};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{WindowExt as _, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const DOUBLE_SHIFT: Duration = Duration::from_millis(400);
/// Layout changes are written to `layout.json` this long after the last one.
const LAYOUT_SAVE_DEBOUNCE: Duration = Duration::from_millis(300);

fn placement(side: Side) -> DockPlacement {
    match side {
        Side::Left => DockPlacement::Left,
        Side::Right => DockPlacement::Right,
        Side::Bottom => DockPlacement::Bottom,
    }
}

pub struct Workspace {
    pub root: PathBuf,
    pub editor: Entity<EditorArea>,
    pub project: Entity<ProjectPanel>,
    pub agents: Entity<AgentsPanel>,
    pub review: Entity<ReviewPanel>,
    status: Entity<StatusBarView>,
    dock: Entity<DockArea>,
    tool_windows: HashMap<ToolWindowId, Entity<ToolWindow>>,
    pub layout: LayoutState,
    pub search: Option<Entity<SearchPopup>>,
    pub project_model: Entity<ProjectModel>,
    pub terminal: Entity<TerminalPanel>,
    pub problems: Entity<ProblemsPanel>,
    branch: String,
    focus: FocusHandle,
    last_shift: Option<Instant>,
    shift_down: bool,
    layout_save: Task<()>,
    window_save: Task<()>,
    /// Window bounds not yet written (debounced like the layout).
    pending_window: Option<crate::state::WindowGeometry>,
    _subs: Vec<Subscription>,
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Workspace {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = root.canonicalize().unwrap_or(root);
        let settings = cx.global::<AppSettings>().clone();
        let branch = project::current_branch(&root).unwrap_or_else(|| "main".into());

        let editor = cx.new(|cx| EditorArea::new(root.clone(), settings.editor_font_size, window, cx));
        let agents = cx.new(|cx| AgentsPanel::new(root.clone(), window, cx));
        let running = agents.read(cx).model.running_count();
        let status = cx.new(|cx| StatusBarView::new(editor.clone(), branch.clone(), running, cx));
        let project_model = cx.new(|cx| ProjectModel::new(root.clone(), &editor, cx));
        let project = cx.new(|cx| ProjectPanel::new(root.clone(), &project_model, window, cx));
        let review = cx.new(|cx| ReviewPanel::new(root.clone(), cx));
        let terminal = cx.new(|cx| TerminalPanel::new(&root, cx));
        let commit = cx.new(|cx| CommitPanel::new(root.clone(), &editor, &project_model, window, cx));
        let git_log = cx.new(|cx| GitLogPanel::new(root.clone(), cx));
        let problems = cx.new(|cx| ProblemsPanel::new(&editor, cx));
        let structure = cx.new(|cx| StructurePanel::new(&editor, &project_model, cx));
        let database = cx.new(|cx| DatabasePanel::new(root.clone(), window, cx));
        let harness = cx.new(|cx| HarnessPanel::new(root.clone(), window, cx));

        let mut views: Vec<(ToolWindowId, AnyView)> = vec![
            (ToolWindowId::Project, project.clone().into()),
            (ToolWindowId::Commit, commit.into()),
            (ToolWindowId::Structure, structure.into()),
            (ToolWindowId::Bookmarks, cx.new(|_| NotAvailable).into()),
            (ToolWindowId::Agents, agents.clone().into()),
            (ToolWindowId::Review, review.clone().into()),
            (ToolWindowId::Database, database.into()),
            (ToolWindowId::Notifications, cx.new(|_| NotAvailable).into()),
            (ToolWindowId::Terminal, terminal.clone().into()),
            (ToolWindowId::Run, cx.new(|_| NotAvailable).into()),
            (ToolWindowId::Problems, problems.clone().into()),
            (ToolWindowId::Git, git_log.into()),
            (ToolWindowId::Services, cx.new(|_| NotAvailable).into()),
            (ToolWindowId::Harness, harness.into()),
        ];
        let mut subs = Vec::new();
        let mut tool_windows = HashMap::new();
        for (id, view) in views.drain(..) {
            let tw = cx.new(|cx| ToolWindow::new(id, view, cx));
            subs.push(cx.subscribe_in(&tw, window, |this, _, ev: &HideToolWindow, window, cx| this.hide(ev.0.side(), window, cx)));
            tool_windows.insert(id, tw);
        }

        let (dock, skin) = DockSkin::dock_area("tandem-main", Some(1), window, cx);
        skin.set_toggle_button_visible(false, cx);
        let layout = LayoutState::load();
        dock.update(cx, |d, cx| {
            d.set_center(DockLayout::tabs().panel_view(panel_handle(editor.clone()), cx), window, cx);
            for side in Side::ALL {
                let SideLayout { id, open, size } = *layout.side(side);
                let pl = placement(side);
                d.set_dock(pl, DockLayout::tabs().panel_view(panel_handle(tool_windows[&id].clone()), cx), window, cx);
                d.set_dock_size(pl, px(size), window, cx);
                if d.is_dock_open(pl) != open {
                    d.toggle_dock(pl, window, cx);
                }
            }
        });

        subs.push(cx.subscribe(&dock, |this, dock, ev: &DockEvent, cx| {
            if matches!(ev, DockEvent::LayoutChanged) {
                let d = dock.read(cx);
                let mut l = this.layout.clone();
                for side in Side::ALL {
                    if let Some(s) = d.dock_size(placement(side)) {
                        l.side_mut(side).size = f32::from(s).round();
                    }
                }
                if l != this.layout {
                    this.layout = l;
                    this.save_layout(cx);
                }
            }
        }));
        subs.push(cx.subscribe_in(&editor, window, |this, _, ev: &EditorAreaEvent, window, cx| match ev {
            EditorAreaEvent::Toast(msg) => toast(msg.clone(), window, cx),
            EditorAreaEvent::ActiveChanged(Some(path)) => {
                let path = path.clone();
                this.project.update(cx, |p, cx| {
                    if p.autoscroll_from_source {
                        p.reveal(&path, cx)
                    }
                });
            }
            EditorAreaEvent::ShowToolWindow(id) if !this.is_open(*id) => this.show(*id, window, cx),
            _ => {}
        }));
        subs.push(cx.subscribe_in(&project_model, window, |this, _, ev: &ProjectModelEvent, window, cx| {
            if let ProjectModelEvent::Fs(batch) = ev {
                let paths: Vec<PathBuf> = batch.iter().flat_map(|e| e.paths.iter().cloned()).collect();
                this.editor.update(cx, |e, cx| e.reload_from_disk(&paths, window, cx));
            }
        }));
        subs.push(cx.subscribe_in(&project, window, |this, _, ev: &ProjectEvent, window, cx| match ev {
            ProjectEvent::OpenFile(path) => this.open_file(path.clone(), window, cx),
        }));
        subs.push(cx.subscribe_in(&agents, window, |this, agents, ev: &AgentsEvent, window, cx| {
            match ev {
                AgentsEvent::Toast(msg) => toast(msg.clone(), window, cx),
                AgentsEvent::Changed => {}
                AgentsEvent::ReviewReady(task) => {
                    // Refresh Review when it shows this task (or nothing yet); others get the new
                    // diff for intent-conflict detection.
                    let showing = this.review.read(cx).task().cloned();
                    if showing.is_none() || showing.as_ref() == Some(task) {
                        this.open_review(task, false, window, cx);
                    } else {
                        let others = agents.read(cx).review_inputs();
                        this.review.update(cx, |r, cx| r.refresh_conflicts(others, cx));
                    }
                }
                AgentsEvent::OpenReview(task) => this.open_review(task, true, window, cx),
            }
            let running = agents.read(cx).model.running_count();
            this.status.update(cx, |s, cx| {
                s.agents_running = running;
                cx.notify();
            });
            cx.notify();
        }));
        subs.push(cx.subscribe_in(&review, window, |this, _, ev: &ReviewEvent, window, cx| match ev {
            ReviewEvent::Toast(msg) => toast(msg.clone(), window, cx),
            ReviewEvent::OpenFile { path, line } => this.open_file_at(path.clone(), *line, window, cx),
            ReviewEvent::ShowTask(task) => {
                this.agents.update(cx, |a, cx| a.select_task(task, cx));
                this.open_review(task, true, window, cx);
            }
            ReviewEvent::Applied { .. } => {}
        }));
        subs.push(cx.observe_window_bounds(window, |this, window, cx| {
            let b = window.bounds();
            let geom = crate::state::WindowGeometry {
                x: f32::from(b.origin.x),
                y: f32::from(b.origin.y),
                width: f32::from(b.size.width),
                height: f32::from(b.size.height),
                maximized: window.is_maximized(),
            };
            this.save_window(geom, cx);
        }));
        // Debounced saves still pending when the app quits or the window closes are
        // written synchronously.
        subs.push(cx.on_app_quit(|this, cx| {
            this.flush_persistence(cx);
            async {}
        }));
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = weak.update(cx, |this, cx| this.flush_persistence(cx));
            true
        });

        Self {
            root: root.clone(),
            editor,
            project,
            agents,
            review,
            status,
            dock,
            tool_windows,
            layout,
            search: None,
            project_model,
            terminal,
            problems,
            branch,
            focus: cx.focus_handle(),
            last_shift: None,
            shift_down: false,
            layout_save: Task::ready(()),
            window_save: Task::ready(()),
            pending_window: None,
            _subs: subs,
        }
    }

    pub fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let path = if path.is_absolute() { path } else { self.root.join(path) };
        self.editor.update(cx, |e, cx| e.open(path, window, cx));
    }

    pub fn is_open(&self, id: ToolWindowId) -> bool {
        let s = self.layout.side(id.side());
        s.id == id && s.open
    }

    /// Persist the layout off the UI thread, `LAYOUT_SAVE_DEBOUNCE` after the last change.
    fn save_layout(&mut self, cx: &mut Context<Self>) {
        let layout = self.layout.clone();
        self.layout_save = cx.spawn(async move |_, cx| {
            cx.background_executor().timer(LAYOUT_SAVE_DEBOUNCE).await;
            cx.background_spawn(async move { layout.save() }).await;
        });
    }

    /// Persist the window bounds off the UI thread, `LAYOUT_SAVE_DEBOUNCE` after the last move.
    fn save_window(&mut self, geom: crate::state::WindowGeometry, cx: &mut Context<Self>) {
        self.pending_window = Some(geom);
        self.window_save = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LAYOUT_SAVE_DEBOUNCE).await;
            let Ok(Some(geom)) = this.update(cx, |this, _| this.pending_window.take()) else { return };
            cx.background_spawn(async move { geom.save() }).await;
        });
    }

    /// Write every debounced save now (layout, window bounds, recent files, queued task /
    /// policy writes). Called on quit and when the window closes.
    pub fn flush_persistence(&mut self, cx: &mut Context<Self>) {
        self.layout_save = Task::ready(());
        self.layout.save();
        self.window_save = Task::ready(());
        if let Some(geom) = self.pending_window.take() {
            geom.save();
        }
        self.project_model.update(cx, |m, _| m.flush_recent());
        crate::state::flush_writes();
    }

    /// Stripe click / ⌘N: open the tool window, or hide it when it is already showing.
    pub fn toggle_tool_window(&mut self, id: ToolWindowId, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_open(id) {
            self.hide(id.side(), window, cx);
        } else {
            self.show(id, window, cx);
        }
    }

    pub fn show(&mut self, id: ToolWindowId, window: &mut Window, cx: &mut Context<Self>) {
        let side = id.side();
        let pl = placement(side);
        let tw = self.tool_windows[&id].clone();
        let s = self.layout.side_mut(side);
        let replace = s.id != id;
        s.id = id;
        s.open = true;
        self.dock.update(cx, |d, cx| {
            if replace {
                d.set_dock(pl, DockLayout::tabs().panel_view(panel_handle(tw.clone()), cx), window, cx);
            }
            if !d.is_dock_open(pl) {
                d.toggle_dock(pl, window, cx);
            }
        });
        if id == ToolWindowId::Project {
            let f = self.project.read(cx).focus_handle(cx);
            window.focus(&f, cx);
        } else if id == ToolWindowId::Terminal {
            let f = self.terminal.read(cx).focus_handle(cx);
            window.focus(&f, cx);
        } else {
            let f = tw.read(cx).focus_handle(cx);
            window.focus(&f, cx);
        }
        self.save_layout(cx);
        cx.notify();
    }

    pub fn hide(&mut self, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        let pl = placement(side);
        self.layout.side_mut(side).open = false;
        self.dock.update(cx, |d, cx| {
            if d.is_dock_open(pl) {
                d.toggle_dock(pl, window, cx);
            }
        });
        self.focus_editor(window, cx);
        self.save_layout(cx);
        cx.notify();
    }

    fn hide_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for side in Side::ALL {
            self.hide(side, window, cx);
        }
    }

    fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let f = self.editor.read(cx).focus_handle(cx);
        window.focus(&f, cx);
    }

    pub fn open_search(&mut self, tab: SearchTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(s) = &self.search {
            s.update(cx, |s, cx| {
                s.set_tab(tab, cx);
                s.focus(window, cx);
            });
            return;
        }
        let current = self.editor.read(cx).active_path();
        let model = self.project_model.clone();
        let popup = cx.new(|cx| SearchPopup::new(tab, model, current, window, cx));
        let sub = cx.subscribe_in(&popup, window, |this, _, ev: &SearchEvent, window, cx| match ev {
            SearchEvent::OpenFile(path) => {
                this.close_search(window, cx);
                this.open_file(path.clone(), window, cx);
            }
            SearchEvent::RunAction(action) => {
                let action = action.boxed_clone();
                this.close_search(window, cx);
                window.dispatch_action(action, cx);
            }
            SearchEvent::OpenAt(path, line, col) => {
                this.close_search(window, cx);
                let path = if path.is_absolute() { path.clone() } else { this.root.join(path) };
                let pos = tandem_lsp::lsp_types::Position::new(*line, *col);
                this.editor.update(cx, |e, cx| e.open_at(path, pos, window, cx));
            }
            SearchEvent::OpenInFindWindow(q) => {
                this.close_search(window, cx);
                this.open_find_window(Some(q.clone()), window, cx);
            }
            SearchEvent::Dismiss => this.close_search(window, cx),
        });
        self._subs.push(sub);
        popup.update(cx, |s, cx| s.focus(window, cx));
        self.search = Some(popup);
        cx.notify();
    }

    /// ⌘⇧F: Search Everywhere's Text tab, seeded with the editor selection.
    pub fn find_in_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sel = self.editor.read(cx).selected_text(cx);
        self.open_search(SearchTab::Text, window, cx);
        if let (Some(sel), Some(s)) = (sel, self.search.clone()) {
            s.update(cx, |s, cx| s.set_query(sel, window, cx));
        }
    }

    /// The Find in Files results window (a tab of the bottom Problems tool window).
    pub fn open_find_window(&mut self, query: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_open(ToolWindowId::Problems) {
            self.show(ToolWindowId::Problems, window, cx);
        }
        let find = self.editor.read(cx).find.clone();
        find.update(cx, |f, cx| f.search_for(query, window, cx));
    }

    pub fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.take().is_some() {
            self.focus_editor(window, cx);
            cx.notify();
        }
    }

    fn stop_all_agents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Kill switch: SIGTERM every agent process group (tandem_acp::stop_all), SIGKILL after 2 s.
        self.agents.update(cx, |a, cx| a.stop_all(cx));
        self.status.update(cx, |s, cx| {
            s.agents_running = 0;
            cx.notify();
        });
        window.push_notification(Notification::warning("Kill switch: every agent was stopped.").title("Stop All Agents"), cx);
        cx.notify();
    }

    /// Show a task's diff in the Review panel (with conflicts against every other task).
    pub fn open_review(&mut self, task: &tandem_core::TaskId, show: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (input, others) = {
            let a = self.agents.read(cx);
            (a.review_input(task), a.review_inputs())
        };
        let Some(input) = input else { return };
        self.review.update(cx, |r, cx| r.open_task(input, others, cx));
        if show && !self.is_open(ToolWindowId::Review) {
            self.show(ToolWindowId::Review, window, cx);
        }
    }

    /// Open `path` and put the caret on 1-based `line`.
    pub fn open_file_at(&mut self, path: PathBuf, line: u32, window: &mut Window, cx: &mut Context<Self>) {
        let path = if path.is_absolute() { path } else { self.root.join(path) };
        let pos = gpui_kit::component::input::Position::new(line.saturating_sub(1), 0);
        self.editor.update(cx, |e, cx| e.open_at(path, pos, window, cx));
    }

    fn on_modifiers(&mut self, ev: &ModifiersChangedEvent, window: &mut Window, cx: &mut Context<Self>) {
        let only_shift = ev.modifiers == Modifiers::shift();
        if only_shift && !self.shift_down {
            let now = Instant::now();
            if self.last_shift.is_some_and(|t| now.duration_since(t) < DOUBLE_SHIFT) {
                self.last_shift = None;
                self.open_search(SearchTab::All, window, cx);
            } else {
                self.last_shift = Some(now);
            }
        } else if !only_shift && ev.modifiers.modified() {
            self.last_shift = None;
        }
        self.shift_down = only_shift;
    }

    fn render_stripe(&self, ids: &[&[ToolWindowId]], border_right: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let mut col = v_flex()
            .w(px(STRIPE_WIDTH))
            .flex_none()
            .h_full()
            .items_center()
            .py(px(6.))
            .gap(px(4.))
            .bg(p.panel_bg)
            .border_color(p.border)
            .when(border_right, |d| d.border_r_1())
            .when(!border_right, |d| d.border_l_1());
        for (gi, group) in ids.iter().enumerate() {
            if gi > 0 {
                col = col.child(div().flex_1());
            }
            for &id in group.iter() {
                let active = self.is_open(id);
                let listener = cx.listener(move |this, _: &ClickEvent, window, cx| this.toggle_tool_window(id, window, cx));
                col = col.child(stripe_button(id, active, move |e, w, cx| listener(e, w, cx), cx));
            }
        }
        col
    }
}

pub fn toast(msg: SharedString, window: &mut Window, cx: &mut App) {
    window.push_notification(Notification::info(msg), cx);
}

macro_rules! toggles {
    ($root:expr, $cx:expr, $( $action:ident => $id:ident ),* $(,)?) => {
        $root$(
            .on_action($cx.listener(|this, _: &$action, window, cx| this.toggle_tool_window(ToolWindowId::$id, window, cx)))
        )*
    };
}

macro_rules! editor_cmds {
    ($root:expr, $cx:expr, $( $action:ident => |$e:ident, $w:ident, $c:ident| $body:expr ),* $(,)?) => {
        $root$(
            .on_action($cx.listener(|this, _: &$action, window, cx| {
                this.editor.update(cx, |$e, $c| { let $w = window; $body });
            }))
        )*
    };
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let agents = self.agents.read(cx);
        let info = titlebar::TitleInfo {
            project: self.root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            branch: self.branch.clone(),
            agents_running: agents.model.running_count(),
            agents_waiting: agents.model.waiting_count(),
        };
        let _ = window;

        let root = div()
            .id("workspace")
            .key_context(WORKSPACE)
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(p.editor_bg)
            .text_color(p.text)
            .font_family(crate::theme::UI_FONT)
            .text_size(px(13.))
            .on_modifiers_changed(cx.listener(Self::on_modifiers))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.last_shift = None;
                if ev.keystroke.key == "escape" && this.search.is_none() {
                    this.focus_editor(window, cx);
                }
            }));
        let root = toggles!(root, cx,
            ToggleProject => Project, ToggleCommit => Commit, ToggleStructure => Structure,
            ToggleBookmarks => Bookmarks, ToggleAgents => Agents, ToggleReview => Review,
            ToggleDatabase => Database, ToggleNotifications => Notifications,
            ToggleTerminal => Terminal, ToggleRun => Run, ToggleProblems => Problems,
            ToggleGit => Git, ToggleServices => Services, ToggleHarness => Harness,
        );
        let root = editor_cmds!(root, cx,
            CloseTab => |e, w, c| e.close_active(w, c),
            NextTab => |e, w, c| e.cycle(1, w, c),
            PrevTab => |e, w, c| e.cycle(-1, w, c),
            GoToLine => |e, w, c| e.go_to_line(w, c),
            ToggleComment => |e, w, c| e.toggle_comment(w, c),
            DuplicateLine => |e, w, c| e.duplicate_line(w, c),
            DeleteLine => |e, w, c| e.delete_line(w, c),
            MoveLineUp => |e, w, c| e.move_line(true, w, c),
            MoveLineDown => |e, w, c| e.move_line(false, w, c),
        );
        let root = root
            .on_action(cx.listener(|this, _: &SaveFile, _, cx| this.editor.update(cx, |e, cx| e.save_all(cx))))
            .on_action(cx.listener(|this, _: &SearchEverywhere, window, cx| this.open_search(SearchTab::All, window, cx)))
            .on_action(cx.listener(|this, _: &GotoFile, window, cx| this.open_search(SearchTab::Files, window, cx)))
            .on_action(cx.listener(|this, _: &RecentFiles, window, cx| this.open_search(SearchTab::Files, window, cx)))
            .on_action(cx.listener(|this, _: &GotoAction, window, cx| this.open_search(SearchTab::Actions, window, cx)))
            .on_action(cx.listener(|this, _: &GotoSymbol, window, cx| this.open_search(SearchTab::Symbols, window, cx)))
            .on_action(cx.listener(|this, _: &FindInFiles, window, cx| this.find_in_files(window, cx)))
            .on_action(cx.listener(|this, _: &crate::editor::actions::ShowBranches, window, cx| {
                let me = cx.weak_entity();
                let on_checkout = std::rc::Rc::new(move |branch: String, window: &mut Window, cx: &mut App| {
                    let _ = me.update(cx, |this, cx| {
                        this.branch = branch.clone();
                        this.status.update(cx, |s, cx| {
                            s.branch = branch;
                            cx.notify();
                        });
                        let paths: Vec<PathBuf> = this.editor.read(cx).files.iter().map(|f| f.path.clone()).collect();
                        this.editor.update(cx, |e, cx| e.reload_from_disk(&paths, window, cx));
                        cx.notify();
                    });
                });
                crate::panels::git::branches_dialog(this.root.clone(), on_checkout, window, cx);
            }))
            .on_action(cx.listener(|_, _: &Find, window, cx| toast("Find: use ⌘F inside the editor".into(), window, cx)))
            .on_action(cx.listener(|this, _: &HideToolWindows, window, cx| this.hide_all(window, cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| settings::open(this.root.clone(), window, cx)))
            .on_action(cx.listener(|_, _: &ToggleTheme, _, cx| settings::toggle_theme(cx)))
            .on_action(cx.listener(|this, _: &StopAllAgents, window, cx| this.stop_all_agents(window, cx)))
            .on_action(cx.listener(|this, a: &crate::tasks::actions::OpenTerminalWith, window, cx| {
                let req = crate::tasks::terminal::TerminalRequest {
                    title: a.title.clone(),
                    program: a.program.clone(),
                    args: a.args.clone(),
                    cwd: a.cwd.clone().or_else(|| Some(this.root.clone())),
                };
                match crate::tasks::terminal::open_terminal_with(req) {
                    Ok(()) => {
                        this.show(ToolWindowId::Terminal, window, cx);
                        toast(format!("{}: finish signing in in the Terminal tab", a.title).into(), window, cx);
                    }
                    Err(e) => toast(format!("Cannot start {}: {e}", a.program).into(), window, cx),
                }
            }))
            .on_action(cx.listener(|this, a: &crate::tasks::actions::ShowAgentBlame, window, cx| {
                match crate::tasks::blame::lookup(&this.root, &a.path, a.line) {
                    Some(e) => {
                        let agent = this.agents.read(cx).badge(&e.agent).name;
                        toast(format!("Line {}: {agent} · task {} · “{}”", a.line, e.task, e.prompt_excerpt).into(), window, cx);
                        this.agents.update(cx, |ag, cx| ag.select_task(&e.task, cx));
                    }
                    None => toast(format!("Line {}: no agent wrote this line", a.line).into(), window, cx),
                }
            }))
            .on_action(cx.listener(|this, _: &SelectInProjectView, window, cx| {
                if let Some(path) = this.editor.read(cx).active_path() {
                    this.project.update(cx, |p, cx| p.reveal(&path, cx));
                }
                if !this.is_open(ToolWindowId::Project) {
                    this.show(ToolWindowId::Project, window, cx);
                }
            }))
            .on_action(cx.listener(|_, a: &NotImplemented, window, cx| {
                if a.0 == "Git Branches" {
                    return window.dispatch_action(Box::new(crate::editor::actions::ShowBranches), cx);
                }
                toast(format!("{}: not implemented yet", a.0).into(), window, cx)
            }))
            .on_action(cx.listener(|_, _: &Quit, _, cx| cx.quit()));

        let search_overlay = self.search.clone().map(|s| {
            deferred(
                div()
                    .absolute()
                    .top(px(titlebar::TITLE_BAR_H + 70.))
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(s),
            )
            .with_priority(1)
        });

        root.child(titlebar::render(&info, &p))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_stripe(&[ToolWindowId::LEFT, ToolWindowId::BOTTOM], true, cx))
                    .child(div().flex_1().min_w_0().h_full().child(self.dock.clone()))
                    .child(self.render_stripe(&[ToolWindowId::RIGHT], false, cx)),
            )
            .child(self.status.clone())
            .children(search_overlay)
    }
}
