//! Commit (left, ⌘0/⌘K) and Git (bottom, ⌘9) tool windows on `tandem_git::Repo`:
//! real status with stage/unstage checkboxes, commit (+ push via the `git` CLI, output shown),
//! history with the selected commit's diff in gpui-kit's `Diff`, and the branches popup.

use crate::assets::{file_icon, icon};
use crate::editor::{EditorArea, EditorAreaEvent};
use crate::model::project::{ProjectModel, ProjectModelEvent};
use crate::theme::{MONO_FONT, Palette, pal};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::diff::{Diff, DiffFile, DiffState};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;
use tandem_git::{BranchInfo, Change, CommitInfo, Repo, StatusEntry};

fn change_color(e: &StatusEntry, p: &Palette) -> Hsla {
    match (e.index, e.worktree) {
        (_, Change::Untracked) => p.git_untracked,
        (Change::Added, _) => p.git_added,
        (Change::Deleted, _) | (_, Change::Deleted) => p.git_deleted,
        (_, Change::Conflicted) => p.error,
        _ => p.git_modified,
    }
}

fn is_staged(e: &StatusEntry) -> bool {
    !matches!(e.index, Change::Unmodified | Change::Untracked)
}

/// Status is re-read this long after the last save / filesystem change.
const REFRESH_DEBOUNCE: Duration = Duration::from_millis(500);
/// Slow poll for changes the watcher does not see (git commands run outside Tandem touch
/// `.git/`, which is not watched).
const REFRESH_POLL: Duration = Duration::from_secs(15);

/// Run `git <args>` in `root`, returning combined stdout+stderr and success.
pub fn git_cli(root: &Path, args: &[&str]) -> (bool, String) {
    match std::process::Command::new("git").args(args).current_dir(root).output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).to_string();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.success(), s.trim_end().to_string())
        }
        Err(e) => (false, format!("cannot run git: {e}")),
    }
}

pub struct CommitPanel {
    root: PathBuf,
    pub entries: Vec<StatusEntry>,
    message: Entity<TextareaState>,
    amend: bool,
    busy: bool,
    output: Option<String>,
    error: Option<String>,
    editor: WeakEntity<EditorArea>,
    refresh_task: Task<()>,
    /// A status read is running; `refresh_again` asks for one more when it finishes.
    refreshing: bool,
    refresh_again: bool,
    _poll: Task<()>,
    _subs: Vec<Subscription>,
}

impl CommitPanel {
    pub fn new(root: PathBuf, editor: &Entity<EditorArea>, model: &Entity<ProjectModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message = cx.new(|cx| TextareaState::new(window, cx).rows(4).placeholder("Commit Message"));
        let subs = vec![
            cx.subscribe(editor, |this: &mut Self, _, ev: &EditorAreaEvent, cx| {
                if matches!(ev, EditorAreaEvent::Saved(_)) {
                    this.refresh_soon(cx);
                }
            }),
            cx.subscribe(model, |this: &mut Self, _, ev: &ProjectModelEvent, cx| {
                if matches!(ev, ProjectModelEvent::Fs(_)) {
                    this.refresh_soon(cx);
                }
            }),
        ];
        let mut this = Self {
            root,
            entries: vec![],
            message,
            amend: false,
            busy: false,
            output: None,
            error: None,
            editor: editor.downgrade(),
            refresh_task: Task::ready(()),
            refreshing: false,
            refresh_again: false,
            _poll: cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(REFRESH_POLL).await;
                    if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                        break;
                    }
                }
            }),
            _subs: subs,
        };
        this.refresh(cx);
        this
    }

    /// Debounced [`refresh`](Self::refresh) for bursts of saves / watcher events.
    fn refresh_soon(&mut self, cx: &mut Context<Self>) {
        self.refresh_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REFRESH_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| this.refresh(cx));
        });
    }

    /// Re-read `git status` off the UI thread. While a read is in flight further calls
    /// coalesce into one re-run after it, so changes made meanwhile are not missed.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.refreshing {
            self.refresh_again = true;
            return;
        }
        self.refreshing = true;
        let root = self.root.clone();
        let task = cx.background_spawn(async move { Repo::open(&root).and_then(|r| r.status()).map_err(|e| e.to_string()) });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |p, cx| {
                p.refreshing = false;
                if std::mem::take(&mut p.refresh_again) {
                    p.refresh(cx);
                }
                match res {
                    Ok(mut entries) => {
                        entries.sort_by(|a, b| a.path.cmp(&b.path));
                        p.entries = entries;
                        p.error = None;
                    }
                    Err(e) => {
                        p.entries.clear();
                        p.error = Some(e);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Stage / unstage `paths` off the UI thread, then refresh.
    fn set_staged(&mut self, paths: Vec<PathBuf>, stage: bool, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let task = cx.background_spawn(async move {
            Repo::open(&root).and_then(|r| if stage { r.stage(&paths) } else { r.unstage(&paths) }).map_err(|e| e.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(e) = res {
                    window.push_notification(Notification::error(e).title("Git"), cx);
                }
                this.refresh(cx);
            });
        })
        .detach();
    }

    fn commit(&mut self, push: bool, window: &mut Window, cx: &mut Context<Self>) {
        let msg = self.message.read(cx).value().trim().to_string();
        if msg.is_empty() {
            window.push_notification(Notification::warning("Enter a commit message").title("Commit"), cx);
            return;
        }
        let (root, amend) = (self.root.clone(), self.amend);
        let task = cx.background_spawn(async move {
            if amend {
                let (ok, out) = git_cli(&root, &["commit", "--amend", "-m", &msg]);
                if ok { Ok(out) } else { Err(out) }
            } else {
                Repo::open(&root).and_then(|r| r.commit(&msg)).map(|id| id.chars().take(8).collect::<String>()).map_err(|e| e.to_string())
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let res = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match res {
                    Ok(id) => {
                        this.message.update(cx, |s, cx| s.set_value("", window, cx));
                        this.amend = false;
                        window.push_notification(Notification::success(format!("Committed {}", id.lines().next().unwrap_or(""))).title("Commit"), cx);
                        if push {
                            this.push(cx);
                        }
                    }
                    Err(e) => window.push_notification(Notification::error(e).title("Commit failed"), cx),
                }
                this.refresh(cx);
            });
        })
        .detach();
    }

    fn push(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        self.output = Some("$ git push\n".into());
        let root = self.root.clone();
        let task = cx.background_spawn(async move { git_cli(&root, &["push"]) });
        cx.spawn(async move |this, cx| {
            let (ok, out) = task.await;
            let _ = this.update(cx, |p, cx| {
                p.busy = false;
                p.output = Some(format!("$ git push\n{out}\n{}", if ok { "✓ pushed" } else { "✗ push failed" }));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for CommitPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let tracked: Vec<usize> = (0..self.entries.len()).filter(|i| self.entries[*i].worktree != Change::Untracked).collect();
        let untracked: Vec<usize> = (0..self.entries.len()).filter(|i| self.entries[*i].worktree == Change::Untracked).collect();
        let group = |title: &str, items: &[usize], this: &Self, cx: &mut Context<Self>| {
            v_flex()
                .child(
                    h_flex()
                        .h(px(24.))
                        .px(px(8.))
                        .gap(px(6.))
                        .child(icon(IconName::ChevronDown, 14., p.muted))
                        .child(div().text_color(p.text).child(title.to_string()))
                        .child(div().text_color(p.muted).child(format!("{} files", items.len()))),
                )
                .children(items.iter().map(|&ix| {
                    let e = &this.entries[ix];
                    let name = e.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let dir = e.path.parent().map(|d| d.display().to_string()).unwrap_or_default();
                    let (g, gc) = file_icon(&name);
                    let path = e.path.clone();
                    let abs = this.root.join(&e.path);
                    h_flex()
                        .id(("chg", ix))
                        .h(px(24.))
                        .pl(px(26.))
                        .gap(px(6.))
                        .hover(|s| s.bg(p.hover))
                        .child(
                            Checkbox::new(("chg-cb", ix))
                                .checked(is_staged(e))
                                .on_click(cx.listener(move |this, checked: &bool, window, cx| this.set_staged(vec![path.clone()], *checked, window, cx))),
                        )
                        .child(icon(g, 16., gc))
                        .child(div().text_color(change_color(e, &p)).child(name))
                        .child(div().text_color(p.muted).child(dir))
                        .on_click({
                            let editor = this.editor.clone();
                            move |ev, window, cx| {
                                if ev.click_count() >= 2 {
                                    let _ = editor.update(cx, |e, cx| e.open(abs.clone(), window, cx));
                                }
                            }
                        })
                }))
        };
        let tracked_el = group("Changes", &tracked, self, cx);
        let untracked_el = group("Unversioned Files", &untracked, self, cx);
        let n = self.entries.iter().filter(|e| is_staged(e)).count();
        v_flex()
            .size_full()
            .text_size(px(13.))
            .child(
                h_flex()
                    .h(px(30.))
                    .px(px(8.))
                    .gap(px(2.))
                    .child(crate::panels::header_icon("git-refresh", IconName::RefreshCw, &p).on_click(cx.listener(|this, _, _, cx| this.refresh(cx))))
                    .child(crate::panels::header_icon("git-stage-all", IconName::Plus, &p).on_click(cx.listener(|this, _, window, cx| {
                        let paths: Vec<PathBuf> = this.entries.iter().map(|e| e.path.clone()).collect();
                        this.set_staged(paths, true, window, cx);
                    }))),
            )
            .when_some(self.error.clone(), |d, e| d.child(div().px(px(12.)).py(px(6.)).text_color(p.muted).child(e)))
            .child(div().id("changes").flex_1().overflow_y_scroll().child(tracked_el).child(untracked_el))
            .when_some(self.output.clone(), |d, out| {
                d.child(
                    div()
                        .id("push-output")
                        .max_h(px(140.))
                        .overflow_y_scroll()
                        .border_t_1()
                        .border_color(p.border)
                        .bg(p.editor_bg)
                        .px(px(10.))
                        .py(px(6.))
                        .font_family(MONO_FONT)
                        .text_size(px(12.))
                        .text_color(p.text)
                        .children(out.lines().map(|l| div().child(l.to_string())).collect::<Vec<_>>()),
                )
            })
            .child(
                v_flex()
                    .flex_none()
                    .border_t_1()
                    .border_color(p.border)
                    .p(px(8.))
                    .gap(px(8.))
                    .child(
                        h_flex().gap(px(6.)).child(
                            Checkbox::new("amend").label("Amend").checked(self.amend).on_click(cx.listener(|this, c: &bool, _, cx| {
                                this.amend = *c;
                                cx.notify();
                            })),
                        ),
                    )
                    .child(Textarea::new(&self.message).h(px(84.)))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(
                                Button::new("commit")
                                    .primary()
                                    .small()
                                    .label(if n == 0 { "Commit".to_string() } else { format!("Commit {n} file{}", if n == 1 { "" } else { "s" }) })
                                    .on_click(cx.listener(|this, _, window, cx| this.commit(false, window, cx))),
                            )
                            .child(
                                Button::new("commit-push")
                                    .small()
                                    .label(if self.busy { "Pushing…" } else { "Commit and Push…" })
                                    .on_click(cx.listener(|this, _, window, cx| this.commit(true, window, cx))),
                            ),
                    ),
            )
    }
}

pub struct GitLogPanel {
    root: PathBuf,
    commits: Vec<CommitInfo>,
    branches: Vec<BranchInfo>,
    selected: Option<usize>,
    diff: Entity<DiffState>,
    changed_files: usize,
    error: Option<String>,
    scroll: UniformListScrollHandle,
}

impl GitLogPanel {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let diff = cx.new(|cx| DiffState::new(Vec::<DiffFile>::new(), cx));
        let mut this = Self { root, commits: vec![], branches: vec![], selected: None, diff, changed_files: 0, error: None, scroll: UniformListScrollHandle::new() };
        this.reload(cx);
        this
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let task = cx.background_spawn(async move {
            let repo = Repo::open(&root).map_err(|e| e.to_string())?;
            let log = repo.log(500).map_err(|e| e.to_string())?;
            let branches = repo.branches().unwrap_or_default();
            Ok::<_, String>((log, branches))
        });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            let _ = this.update(cx, |p, cx| {
                match res {
                    Ok((log, branches)) => {
                        p.commits = log;
                        p.branches = branches;
                        p.error = None;
                        if p.selected.is_none() && !p.commits.is_empty() {
                            p.select(0, cx);
                        }
                    }
                    Err(e) => p.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(c) = self.commits.get(ix) else { return };
        self.selected = Some(ix);
        let (root, id) = (self.root.clone(), c.id.clone());
        let task = cx.background_spawn(async move { Repo::open(&root).and_then(|r| r.diff_commit(&id)).unwrap_or_default() });
        cx.spawn(async move |this, cx| {
            let fds = task.await;
            let _ = this.update(cx, |p, cx| {
                let files: Vec<DiffFile> = fds.iter().filter(|f| !f.binary).flat_map(|f| DiffFile::parse(&f.to_patch()).unwrap_or_default()).collect();
                p.changed_files = fds.len();
                p.diff = cx.new(|cx| DiffState::new(files, cx));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

fn relative_time(t: i64) -> String {
    let dt = chrono::DateTime::from_timestamp(t, 0).map(|d| d.with_timezone(&chrono::Local));
    let Some(dt) = dt else { return String::new() };
    let now = chrono::Local::now();
    let days = (now.date_naive() - dt.date_naive()).num_days();
    match days {
        0 => format!("Today {}", dt.format("%H:%M")),
        1 => format!("Yesterday {}", dt.format("%H:%M")),
        _ => dt.format("%b %d, %Y").to_string(),
    }
}

impl Render for GitLogPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let commits = self.commits.clone();
        let selected = self.selected;
        let heads: Vec<(String, String)> = self.branches.iter().filter_map(|b| b.commit.clone().map(|c| (c, b.name.clone()))).collect();
        let list_p = p.clone();
        let locals: Vec<BranchInfo> = self.branches.iter().filter(|b| !b.is_remote).cloned().collect();
        let remotes: Vec<BranchInfo> = self.branches.iter().filter(|b| b.is_remote).cloned().collect();
        h_flex()
            .size_full()
            .items_start()
            .text_size(px(13.))
            .child(
                v_flex()
                    .id("git-branches")
                    .w(px(200.))
                    .h_full()
                    .flex_none()
                    .overflow_y_scroll()
                    .border_r_1()
                    .border_color(p.border)
                    .p(px(6.))
                    .child(h_flex().px(px(6.)).h(px(24.)).text_color(p.muted).child("Branches").child(div().flex_1()).child(
                        crate::panels::header_icon("git-log-refresh", IconName::RefreshCw, &p).on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ))
                    .child(branch_row("Local", false, &p))
                    .children(locals.iter().map(|b| div().pl(px(16.)).child(branch_row(&b.name, b.is_head, &p))))
                    .child(branch_row("Remote", false, &p))
                    .children(remotes.iter().map(|b| div().pl(px(16.)).child(branch_row(&b.name, false, &p)))),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .when_some(self.error.clone(), |d, e| d.child(div().p(px(12.)).text_color(p.muted).child(e)))
                    .child(
                        uniform_list("git-log", commits.len(), cx.processor(move |_this, range: std::ops::Range<usize>, _, cx| {
                            range
                                .map(|i| {
                                    let c = &commits[i];
                                    let tags: Vec<String> = heads.iter().filter(|(id, _)| *id == c.id).map(|(_, n)| n.clone()).collect();
                                    h_flex()
                                        .id(("commit", i))
                                        .h(px(24.))
                                        .px(px(8.))
                                        .gap(px(8.))
                                        .cursor_pointer()
                                        .when(selected == Some(i), |d| d.bg(list_p.row_selected))
                                        .when(selected != Some(i), |d| d.hover(|s| s.bg(list_p.hover)))
                                        .child(div().size(px(8.)).rounded_full().bg(list_p.accent))
                                        .child(div().flex_1().min_w_0().text_color(list_p.text).whitespace_nowrap().overflow_hidden().child(c.summary.clone()))
                                        .children(tags.into_iter().map(|t| div().px(px(5.)).rounded(px(3.)).bg(list_p.stripe_selected).text_size(px(11.)).text_color(list_p.warning).child(t)))
                                        .child(div().w(px(90.)).flex_none().overflow_hidden().whitespace_nowrap().text_color(list_p.muted).child(c.author.clone()))
                                        .child(div().w(px(120.)).flex_none().text_color(list_p.muted).child(relative_time(c.time)))
                                        .child(div().w(px(64.)).flex_none().font_family(MONO_FONT).text_size(px(12.)).text_color(list_p.muted).child(c.short_id.clone()))
                                        .on_click(cx.listener(move |this, _, _, cx| this.select(i, cx)))
                                        .into_any_element()
                                })
                                .collect::<Vec<_>>()
                        }))
                        .flex_1()
                        .track_scroll(&self.scroll),
                    ),
            )
            .child(
                v_flex()
                    .w(px(460.))
                    .h_full()
                    .flex_none()
                    .border_l_1()
                    .border_color(p.border)
                    .child(h_flex().h(px(26.)).px(px(10.)).text_color(p.muted).child(match selected.and_then(|i| self.commits.get(i)) {
                        Some(c) => format!("{}  ·  {} file(s)", c.short_id, self.changed_files),
                        None => "Select a commit".to_string(),
                    }))
                    .child(div().flex_1().min_h_0().child(Diff::new(&self.diff).line_number(true))),
            )
    }
}

fn branch_row(name: &str, current: bool, p: &Palette) -> impl IntoElement {
    h_flex()
        .h(px(24.))
        .px(px(6.))
        .gap(px(6.))
        .child(icon(if current { IconName::GitBranch } else { IconName::Folder }, 14., if current { p.warning } else { p.muted }))
        .child(div().text_color(p.text).whitespace_nowrap().child(name.to_string()))
}

/// Title-bar / status-bar branch popup: local branches (checkout), remote branches (checkout
/// as tracking branch). `on_checkout` receives the new branch name.
pub type OnCheckout = Rc<dyn Fn(String, &mut Window, &mut App)>;

pub fn branches_dialog(root: PathBuf, on_checkout: OnCheckout, window: &mut Window, cx: &mut App) {
    let r = root.clone();
    let task = cx.background_spawn(async move { Repo::open(&r).and_then(|r| r.branches()).unwrap_or_default() });
    window
        .spawn(cx, async move |cx| {
            let branches = task.await;
            let _ = cx.update(|window, cx| open_branches(root, branches, on_checkout, window, cx));
        })
        .detach();
}

fn open_branches(root: PathBuf, branches: Vec<BranchInfo>, on_checkout: OnCheckout, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, _, cx| {
        let p = pal(cx).clone();
        let row = |b: &BranchInfo, ix: usize| {
            let (root, name, remote, cb) = (root.clone(), b.name.clone(), b.is_remote, on_checkout.clone());
            h_flex()
                .id(("branch", ix))
                .h(px(26.))
                .px(px(8.))
                .gap(px(8.))
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|s| s.bg(p.hover))
                .child(icon(IconName::GitBranch, 14., if b.is_head { p.warning } else { p.muted }))
                .child(div().flex_1().text_color(if b.is_head { p.text_bright } else { p.text }).child(b.name.clone()))
                .children(b.upstream.clone().map(|u| div().text_size(px(11.)).text_color(p.muted).child(u)))
                .on_click(move |_, window, cx| {
                    let (root, name, cb) = (root.clone(), name.clone(), cb.clone());
                    let task = cx.background_spawn(async move {
                        if remote {
                            let (ok, out) = git_cli(&root, &["checkout", "--track", &name]);
                            if ok { Ok(name.split_once('/').map(|x| x.1.to_string()).unwrap_or(name.clone())) } else { Err(out) }
                        } else {
                            Repo::open(&root).and_then(|r| r.checkout_branch(&name)).map(|_| name.clone()).map_err(|e| e.to_string())
                        }
                    });
                    window
                        .spawn(cx, async move |cx| {
                            let res = task.await;
                            let _ = cx.update(|window, cx| match res {
                                Ok(b) => {
                                    window.close_dialog(cx);
                                    window.push_notification(Notification::success(format!("Checked out {b}")).title("Git"), cx);
                                    cb(b, window, cx);
                                }
                                Err(e) => window.push_notification(Notification::error(e).title("Checkout failed"), cx),
                            });
                        })
                        .detach();
                })
        };
        let local: Vec<_> = branches.iter().enumerate().filter(|(_, b)| !b.is_remote).map(|(i, b)| row(b, i).into_any_element()).collect();
        let remote: Vec<_> = branches.iter().enumerate().filter(|(_, b)| b.is_remote).map(|(i, b)| row(b, i).into_any_element()).collect();
        dialog
            .title("Git Branches")
            .w(px(420.))
            .child(
                v_flex()
                    .id("branches-list")
                    .max_h(px(420.))
                    .overflow_y_scroll()
                    .gap(px(2.))
                    .text_size(px(13.))
                    .child(div().px(px(8.)).py(px(4.)).text_size(px(12.)).text_color(p.muted).child("Local"))
                    .children(local)
                    .when(!remote.is_empty(), |d| d.child(div().px(px(8.)).py(px(4.)).text_size(px(12.)).text_color(p.muted).child("Remote")).children(remote))
                    .when(branches.is_empty(), |d| d.child(div().p(px(8.)).text_color(p.muted).child("Not a git repository"))),
            )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use tandem_core::{FileChange, FileDiff};

    #[test]
    fn patch_roundtrips_through_gpui_kit_parser() {
        let hunks = tandem_git::compute_hunks("a\nb\n", "a\nc\n");
        let fd = FileDiff { path: "x.txt".into(), old_path: None, change: FileChange::Modified, old_text: None, new_text: None, hunks, binary: false };
        let files = DiffFile::parse(&fd.to_patch()).expect("parses");
        assert_eq!(files.len(), 1);
    }
}
