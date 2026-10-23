//! The main window: New UI title bar, left/right tool-window stripes, a gpui-kit `DockArea`
//! (editor in the center; one tool window per side dock), status bar, Search Everywhere
//! overlay, and every workspace-level action handler.

use crate::editor::{EditorArea, EditorAreaEvent};
use crate::keymap::*;
use crate::model::{self, project};
use crate::panels::agents::{AgentsEvent, AgentsPanel};
use crate::panels::database::DatabasePanel;
use crate::panels::git::{CommitPanel, GitLogPanel};
use crate::panels::harness::HarnessPanel;
use crate::panels::misc::ListPanel;
use crate::panels::project::{ProjectEvent, ProjectPanel};
use crate::panels::review::{ReviewEvent, ReviewPanel};
use crate::panels::terminal::TerminalPanel;
use crate::panels::{HideToolWindow, ToolWindow, header_icon};
use crate::search_everywhere::{SearchEvent, SearchPopup, SearchTab};
use crate::state::{AppSettings, LayoutState};
use crate::statusbar::StatusBarView;
use crate::stripes::{STRIPE_WIDTH, Side, ToolWindowId, stripe_button};
use crate::theme::pal;
use crate::{settings, titlebar};
use gpui_kit::assets::IconName;
use gpui_kit::component::dock::{DockArea, DockEvent, DockLayout, DockPlacement, DockSkin, panel_handle};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{WindowExt as _, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const FILE_INDEX_LIMIT: usize = 100_000;
const DOUBLE_SHIFT: Duration = Duration::from_millis(400);

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
    files: Arc<Vec<PathBuf>>,
    indexing: bool,
    branch: String,
    focus: FocusHandle,
    last_shift: Option<Instant>,
    shift_down: bool,
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
        let git_changes = model::fake_git_changes();

        let editor = cx.new(|cx| EditorArea::new(root.clone(), settings.editor_font_size, window, cx));
        let status = cx.new(|cx| StatusBarView::new(editor.clone(), branch.clone(), 0, cx));
        let project = cx.new(|cx| ProjectPanel::new(root.clone(), &git_changes, cx));
        let agents = cx.new(|cx| AgentsPanel::new(window, cx));
        let review = cx.new(ReviewPanel::new);
        let terminal = cx.new(|cx| TerminalPanel::new(&root, cx));
        let commit = cx.new(|cx| CommitPanel::new(git_changes.clone(), window, cx));
        let git_log = cx.new(|_| GitLogPanel::new(commit.clone()));
        let database = cx.new(|cx| DatabasePanel::new(window, cx));
        let harness = cx.new(|cx| HarnessPanel::new(window, cx));
        let running = agents.read(cx).model.running_count();
        status.update(cx, |s, _| s.agents_running = running);

        let mut views: Vec<(ToolWindowId, AnyView)> = vec![
            (ToolWindowId::Project, project.clone().into()),
            (ToolWindowId::Commit, commit.into()),
            (ToolWindowId::Structure, cx.new(|_| ListPanel::structure()).into()),
            (ToolWindowId::Bookmarks, cx.new(|_| ListPanel::bookmarks()).into()),
            (ToolWindowId::Agents, agents.clone().into()),
            (ToolWindowId::Review, review.clone().into()),
            (ToolWindowId::Database, database.into()),
            (ToolWindowId::Notifications, cx.new(|_| ListPanel::notifications()).into()),
            (ToolWindowId::Terminal, terminal.into()),
            (ToolWindowId::Run, cx.new(|_| ListPanel::run()).into()),
            (ToolWindowId::Problems, cx.new(|_| ListPanel::problems()).into()),
            (ToolWindowId::Git, git_log.into()),
            (ToolWindowId::Services, cx.new(|_| ListPanel::services()).into()),
            (ToolWindowId::Harness, harness.into()),
        ];
        let mut subs = Vec::new();
        let mut tool_windows = HashMap::new();
        for (id, view) in views.drain(..) {
            let tw = cx.new(|cx| {
                let tw = ToolWindow::new(id, view, cx);
                if id == ToolWindowId::Project {
                    tw.with_header_extra(|_, cx| {
                        header_icon("tw-locate", IconName::Locate, pal(cx))
                            .on_click(|_, w, cx| w.dispatch_action(Box::new(SelectInProjectView), cx))
                            .into_any_element()
                    })
                } else {
                    tw
                }
            });
            subs.push(cx.subscribe_in(&tw, window, |this, _, ev: &HideToolWindow, window, cx| this.hide(ev.0.side(), window, cx)));
            tool_windows.insert(id, tw);
        }

        let (dock, skin) = DockSkin::dock_area("tandem-main", Some(1), window, cx);
        skin.set_toggle_button_visible(false, cx);
        let layout = LayoutState::load();
        dock.update(cx, |d, cx| {
            d.set_center(DockLayout::tabs().panel_view(panel_handle(editor.clone()), cx), window, cx);
            for (side, id, open, size) in [
                (Side::Left, layout.left, layout.left_open, layout.left_size),
                (Side::Right, layout.right, layout.right_open, layout.right_size),
                (Side::Bottom, layout.bottom, layout.bottom_open, layout.bottom_size),
            ] {
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
                if let Some(s) = d.dock_size(DockPlacement::Left) {
                    l.left_size = f32::from(s).round();
                }
                if let Some(s) = d.dock_size(DockPlacement::Right) {
                    l.right_size = f32::from(s).round();
                }
                if let Some(s) = d.dock_size(DockPlacement::Bottom) {
                    l.bottom_size = f32::from(s).round();
                }
                if l != this.layout {
                    this.layout = l;
                    this.layout.save();
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
            _ => {}
        }));
        subs.push(cx.subscribe_in(&project, window, |this, _, ev: &ProjectEvent, window, cx| match ev {
            ProjectEvent::OpenFile(path) => this.open_file(path.clone(), window, cx),
        }));
        subs.push(cx.subscribe_in(&agents, window, |this, agents, ev: &AgentsEvent, window, cx| {
            match ev {
                // Integration point: forward to `tandem_acp::Session::respond`.
                AgentsEvent::Decide(_) => {}
                AgentsEvent::Toast(msg) => toast(msg.clone(), window, cx),
            }
            let running = agents.read(cx).model.running_count();
            this.status.update(cx, |s, cx| {
                s.agents_running = running;
                cx.notify();
            });
            cx.notify();
        }));
        subs.push(cx.subscribe_in(&review, window, |_, _, ev: &ReviewEvent, window, cx| {
            // Integration point: AcceptHunk / RejectHunk → `tandem_review::apply_hunk`.
            if let ReviewEvent::Toast(msg) = ev {
                toast(msg.clone(), window, cx);
            }
        }));
        subs.push(cx.observe_window_bounds(window, |_, window, _| {
            let b = window.bounds();
            crate::state::WindowGeometry {
                x: f32::from(b.origin.x),
                y: f32::from(b.origin.y),
                width: f32::from(b.size.width),
                height: f32::from(b.size.height),
                maximized: window.is_maximized(),
            }
            .save();
        }));

        let mut this = Self {
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
            files: Arc::new(vec![]),
            indexing: true,
            branch,
            focus: cx.focus_handle(),
            last_shift: None,
            shift_down: false,
            _subs: subs,
        };
        this.start_indexing(cx);
        this
    }

    fn start_indexing(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let task = cx.background_executor().spawn(async move { project::index_files(&root, FILE_INDEX_LIMIT) });
        cx.spawn(async move |this, cx| {
            let files = Arc::new(task.await);
            let _ = this.update(cx, |this, cx| {
                this.files = files.clone();
                this.indexing = false;
                if let Some(s) = &this.search {
                    s.update(cx, |s, cx| s.set_files(files, cx));
                }
            });
        })
        .detach();
    }

    pub fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let path = if path.is_absolute() { path } else { self.root.join(path) };
        self.editor.update(cx, |e, cx| e.open(path, window, cx));
    }

    pub fn is_open(&self, id: ToolWindowId) -> bool {
        match id.side() {
            Side::Left => self.layout.left == id && self.layout.left_open,
            Side::Right => self.layout.right == id && self.layout.right_open,
            Side::Bottom => self.layout.bottom == id && self.layout.bottom_open,
        }
    }

    fn side_state(&mut self, side: Side) -> (&mut ToolWindowId, &mut bool) {
        match side {
            Side::Left => (&mut self.layout.left, &mut self.layout.left_open),
            Side::Right => (&mut self.layout.right, &mut self.layout.right_open),
            Side::Bottom => (&mut self.layout.bottom, &mut self.layout.bottom_open),
        }
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
        let (cur, open) = self.side_state(side);
        let replace = *cur != id;
        *cur = id;
        *open = true;
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
        } else {
            let f = tw.read(cx).focus_handle(cx);
            window.focus(&f, cx);
        }
        self.layout.save();
        cx.notify();
    }

    pub fn hide(&mut self, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        let pl = placement(side);
        *self.side_state(side).1 = false;
        self.dock.update(cx, |d, cx| {
            if d.is_dock_open(pl) {
                d.toggle_dock(pl, window, cx);
            }
        });
        self.focus_editor(window, cx);
        self.layout.save();
        cx.notify();
    }

    fn hide_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for side in [Side::Left, Side::Right, Side::Bottom] {
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
        let recent: Vec<PathBuf> = self
            .editor
            .read(cx)
            .recent
            .iter()
            .filter_map(|p| p.strip_prefix(&self.root).ok().map(Path::to_path_buf))
            .collect();
        let files = self.files.clone();
        let indexing = self.indexing;
        let popup = cx.new(|cx| SearchPopup::new(tab, files, indexing, recent, window, cx));
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
            SearchEvent::Dismiss => this.close_search(window, cx),
        });
        self._subs.push(sub);
        popup.update(cx, |s, cx| s.focus(window, cx));
        self.search = Some(popup);
        cx.notify();
    }

    pub fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.take().is_some() {
            self.focus_editor(window, cx);
            cx.notify();
        }
    }

    fn stop_all_agents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Integration point: `tandem_sandbox::kill_switch()` SIGTERMs every agent process group.
        self.agents.update(cx, |a, cx| {
            for t in &mut a.model.tasks {
                if matches!(t.status, model::agents::TaskStatus::Running | model::agents::TaskStatus::NeedsApproval) {
                    t.status = model::agents::TaskStatus::Failed;
                }
            }
            cx.notify();
        });
        self.status.update(cx, |s, cx| {
            s.agents_running = 0;
            cx.notify();
        });
        window.push_notification(Notification::warning("Kill switch: every agent was stopped.").title("Stop All Agents"), cx);
        cx.notify();
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
            agents_waiting: agents.model.tasks.iter().filter(|t| t.status == model::agents::TaskStatus::NeedsApproval).count(),
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
            .on_action(cx.listener(|_, _: &GotoSymbol, window, cx| toast("Go to Symbol needs tandem-lsp (not wired yet)".into(), window, cx)))
            .on_action(cx.listener(|_, _: &FindInFiles, window, cx| toast("Find in Files needs tandem-project search (not wired yet)".into(), window, cx)))
            .on_action(cx.listener(|_, _: &Find, window, cx| toast("Find: use ⌘F inside the editor".into(), window, cx)))
            .on_action(cx.listener(|this, _: &HideToolWindows, window, cx| this.hide_all(window, cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| settings::open(this.root.clone(), window, cx)))
            .on_action(cx.listener(|_, _: &ToggleTheme, _, cx| settings::toggle_theme(cx)))
            .on_action(cx.listener(|this, _: &StopAllAgents, window, cx| this.stop_all_agents(window, cx)))
            .on_action(cx.listener(|this, _: &SelectInProjectView, window, cx| {
                if let Some(path) = this.editor.read(cx).active_path() {
                    this.project.update(cx, |p, cx| p.reveal(&path, cx));
                }
                if !this.is_open(ToolWindowId::Project) {
                    this.show(ToolWindowId::Project, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &RunConfiguration, window, cx| {
                this.show(ToolWindowId::Run, window, cx);
                toast("Run 'dev': run configurations arrive in phase 2".into(), window, cx);
            }))
            .on_action(cx.listener(|_, _: &DebugConfiguration, window, cx| toast("Debugger (DAP) is a phase 2 feature".into(), window, cx)))
            .on_action(cx.listener(|_, _: &StopRun, window, cx| toast("Nothing is running".into(), window, cx)))
            .on_action(cx.listener(|_, a: &NotImplemented, window, cx| toast(format!("{}: not implemented yet", a.0).into(), window, cx)))
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
