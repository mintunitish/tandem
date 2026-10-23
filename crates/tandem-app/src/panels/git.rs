//! Commit (left) and Git (bottom) tool windows. Integration points:
//! `tandem_git::status(root)` → `CommitPanel::changes`, `tandem_git::commit(root, msg, paths)`,
//! `tandem_git::log(root, n)` → `GitLogPanel::commits`.

use crate::assets::{file_icon, icon};
use crate::keymap::NotImplemented;
use crate::model::{GitChange, GitFileStatus};
use crate::theme::{Palette, pal};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use std::collections::HashSet;

fn status_color(s: GitFileStatus, p: &Palette) -> Hsla {
    match s {
        GitFileStatus::Added => p.git_added,
        GitFileStatus::Modified => p.git_modified,
        GitFileStatus::Deleted => p.git_deleted,
        GitFileStatus::Untracked => p.git_untracked,
    }
}

pub struct CommitPanel {
    pub changes: Vec<GitChange>,
    included: HashSet<usize>,
    message: Entity<TextareaState>,
    amend: bool,
}

impl CommitPanel {
    pub fn new(changes: Vec<GitChange>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(4)
                .placeholder("Commit Message")
        });
        let included = (0..changes.len()).filter(|i| changes[*i].status != GitFileStatus::Untracked).collect();
        Self { changes, included, message, amend: false }
    }
}

impl Render for CommitPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let tracked: Vec<usize> = (0..self.changes.len()).filter(|i| self.changes[*i].status != GitFileStatus::Untracked).collect();
        let untracked: Vec<usize> = (0..self.changes.len()).filter(|i| self.changes[*i].status == GitFileStatus::Untracked).collect();
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
                    let c = &this.changes[ix];
                    let name = c.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let dir = c.path.parent().map(|d| d.display().to_string()).unwrap_or_default();
                    let (g, gc) = file_icon(&name);
                    h_flex()
                        .id(("chg", ix))
                        .h(px(24.))
                        .pl(px(26.))
                        .gap(px(6.))
                        .hover(|s| s.bg(p.hover))
                        .child(
                            Checkbox::new(("chg-cb", ix))
                                .checked(this.included.contains(&ix))
                                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                    if *checked {
                                        this.included.insert(ix);
                                    } else {
                                        this.included.remove(&ix);
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(icon(g, 16., gc))
                        .child(div().text_color(status_color(c.status, &p)).child(name))
                        .child(div().text_color(p.muted).child(dir))
                }))
        };
        let tracked_el = group("Changes", &tracked, self, cx);
        let untracked_el = group("Unversioned Files", &untracked, self, cx);
        let n = self.included.len();
        v_flex()
            .size_full()
            .text_size(px(13.))
            .child(
                h_flex()
                    .h(px(30.))
                    .px(px(8.))
                    .gap(px(2.))
                    .child(crate::panels::header_icon("git-refresh", IconName::RefreshCw, &p))
                    .child(crate::panels::header_icon("git-rollback", IconName::RotateCcw, &p))
                    .child(crate::panels::header_icon("git-diff", IconName::GitCompare, &p)),
            )
            .child(div().id("changes").flex_1().overflow_y_scroll().child(tracked_el).child(untracked_el))
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
                                    .on_click(|_, window, cx| window.dispatch_action(Box::new(NotImplemented("Commit".into())), cx)),
                            )
                            .child(
                                Button::new("commit-push")
                                    .small()
                                    .label("Commit and Push…")
                                    .on_click(|_, window, cx| window.dispatch_action(Box::new(NotImplemented("Commit and Push".into())), cx)),
                            ),
                    ),
            )
    }
}

pub struct GitLogPanel {
    commits: Vec<(&'static str, &'static str, &'static str, &'static str, Option<&'static str>)>,
    changes: Entity<CommitPanel>,
}

impl GitLogPanel {
    pub fn new(changes: Entity<CommitPanel>) -> Self {
        Self {
            commits: vec![
                ("4f2a9c1", "Wire quota pool header into cockpit", "Nik", "Today 10:42", Some("main")),
                ("b81e07d", "tandem-review: intent conflict detection", "Nik", "Today 09:15", None),
                ("1c9f3aa", "Darcula New UI palette + JetBrains Mono", "Nik", "Yesterday 22:08", None),
                ("77d0e52", "ACP client: permission request routing", "Nik", "Yesterday 18:31", None),
                ("e3b4c90", "Initial workspace skeleton", "Nik", "Oct 23", Some("origin/main")),
            ],
            changes,
        }
    }
}

impl Render for GitLogPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        h_flex()
            .size_full()
            .items_start()
            .text_size(px(13.))
            .child(
                v_flex()
                    .w(px(220.))
                    .h_full()
                    .flex_none()
                    .border_r_1()
                    .border_color(p.border)
                    .p(px(6.))
                    .child(div().px(px(6.)).h(px(24.)).text_color(p.muted).child("Branches"))
                    .child(branch_row("HEAD (Current Branch)", true, &p))
                    .child(branch_row("Local", false, &p))
                    .child(div().pl(px(22.)).child(branch_row("main", true, &p)))
                    .child(branch_row("Remote", false, &p))
                    .child(div().pl(px(22.)).child(branch_row("origin/main", false, &p))),
            )
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .children(self.commits.iter().enumerate().map(|(i, (hash, msg, author, date, tag))| {
                        h_flex()
                            .id(("commit", i))
                            .h(px(24.))
                            .px(px(8.))
                            .gap(px(8.))
                            .when(i == 0, |d| d.bg(p.row_selected_inactive))
                            .hover(|s| s.bg(p.hover))
                            .child(div().size(px(8.)).rounded_full().bg(p.accent))
                            .child(div().flex_1().text_color(p.text).whitespace_nowrap().overflow_hidden().child(*msg))
                            .children(tag.map(|t| div().px(px(5.)).rounded(px(3.)).bg(p.stripe_selected).text_size(px(11.)).text_color(p.warning).child(t)))
                            .child(div().w(px(60.)).text_color(p.muted).child(*author))
                            .child(div().w(px(110.)).text_color(p.muted).child(*date))
                            .child(div().w(px(60.)).font_family(crate::theme::MONO_FONT).text_size(px(12.)).text_color(p.muted).child(*hash))
                    })),
            )
            .child(div().w(px(380.)).h_full().flex_none().border_l_1().border_color(p.border).child(self.changes.clone()))
    }
}

fn branch_row(name: &'static str, current: bool, p: &Palette) -> impl IntoElement {
    h_flex()
        .h(px(24.))
        .px(px(6.))
        .gap(px(6.))
        .child(icon(if current { IconName::GitBranch } else { IconName::Folder }, 14., if current { p.warning } else { p.muted }))
        .child(div().text_color(p.text).child(name))
}
