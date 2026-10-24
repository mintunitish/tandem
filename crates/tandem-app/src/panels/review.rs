//! Review tool window: a task's `ReviewState` (DiffSet + per-hunk decisions, persisted in
//! `.tandem/tasks/<id>/review.json`), file list with +/− counts and risk badges, gpui-kit `Diff`
//! (unified / split), accept/reject per hunk and per file, "Apply accepted" onto the main
//! worktree with Agent Blame, and intent-conflict banners across every open task.

use crate::assets::{file_icon, icon};
use crate::model::agents::agent_badge;
use crate::model::review::{line_counts, to_unified_patch};
use crate::panels::agents::avatar;
use crate::panels::chip;
use crate::theme::{Palette, pal};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{Diff, DiffFile, DiffMode, DiffState};
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;
use tandem_core::{DiffSet, FileChange, TaskId};
use tandem_review::state::review_path;
use tandem_review::{BlameIndex, Decision, HunkRef, IntentConflict, ReviewState, ReviewSummary, SymbolRef, TaskMeta};

pub enum ReviewEvent {
    Toast(SharedString),
    /// Open a main-tree file at a 1-based line.
    OpenFile { path: PathBuf, line: u32 },
    /// "Show other task" on a conflict banner.
    ShowTask(TaskId),
    /// Accepted hunks were written to the main worktree.
    Applied { task: TaskId, hunks: usize },
}

/// What the Review panel needs for one task.
#[derive(Clone, Debug)]
pub struct ReviewInput {
    pub meta: TaskMeta,
    pub diff: DiffSet,
    pub worktree: Option<PathBuf>,
}

struct Current {
    input: ReviewInput,
    state: ReviewState,
    summary: ReviewSummary,
    conflicts: Vec<IntentConflict>,
    /// Symbols touched per hunk of the selected file.
    symbols: Vec<Vec<SymbolRef>>,
}

pub struct ReviewPanel {
    pub root: PathBuf,
    current: Option<Current>,
    selected: usize,
    diff: Entity<DiffState>,
    mode: DiffMode,
}

impl EventEmitter<ReviewEvent> for ReviewPanel {}

impl ReviewPanel {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let diff = cx.new(|cx| DiffState::new(Vec::<DiffFile>::new(), cx));
        Self { root, current: None, selected: 0, diff, mode: DiffMode::Unified }
    }

    pub fn task(&self) -> Option<&TaskId> {
        self.current.as_ref().map(|c| &c.input.meta.task)
    }

    pub fn state(&self) -> Option<&ReviewState> {
        self.current.as_ref().map(|c| &c.state)
    }

    /// Show `input`, restoring saved decisions when the diff is unchanged, and detect intent
    /// conflicts against every other open task (`others`).
    pub fn open_task(&mut self, input: ReviewInput, others: Vec<ReviewInput>, cx: &mut Context<Self>) {
        let path = review_path(&self.root, &input.meta.task);
        let state = ReviewState::load(&path).ok().filter(|s| s.diff() == &input.diff).unwrap_or_else(|| ReviewState::new(input.diff.clone()));
        let mut all: Vec<(TaskMeta, DiffSet)> = vec![(input.meta.clone(), input.diff.clone())];
        all.extend(others.into_iter().filter(|o| o.meta.task != input.meta.task && !o.diff.files.is_empty()).map(|o| (o.meta, o.diff)));
        let me = input.meta.task.clone();
        let conflicts = tandem_review::detect(&all).into_iter().filter(|c| c.a.task == me || c.b.task == me).collect();
        let summary = tandem_review::describe(&input.diff);
        let keep = self.task() == Some(&me);
        self.current = Some(Current { input, state, summary, conflicts, symbols: vec![] });
        let sel = if keep { self.selected } else { 0 };
        self.select(sel, cx);
    }

    /// Re-run conflict detection (e.g. when another task finished).
    pub fn refresh_conflicts(&mut self, others: Vec<ReviewInput>, cx: &mut Context<Self>) {
        if let Some(c) = &self.current {
            let input = c.input.clone();
            self.open_task(input, others, cx);
        }
    }

    pub fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = ix;
        let files = self.current.as_ref().map(|c| parse(&c.input.diff, ix)).unwrap_or_default();
        if let Some(c) = &mut self.current {
            c.symbols = c.input.diff.files.get(ix).map(tandem_review::symbols::hunk_symbols).unwrap_or_default();
        }
        let mode = self.mode;
        self.diff = cx.new(|cx| DiffState::new(files, cx).with_mode(mode));
        cx.notify();
    }

    pub fn set_mode(&mut self, mode: DiffMode, cx: &mut Context<Self>) {
        self.mode = mode;
        self.diff.update(cx, |d, cx| d.set_mode(mode, cx));
        cx.notify();
    }

    fn save(&self) {
        if let Some(c) = &self.current {
            let _ = c.state.save(review_path(&self.root, &c.input.meta.task));
        }
    }

    pub fn decide_hunk(&mut self, file: usize, hunk: usize, d: Decision, cx: &mut Context<Self>) {
        if let Some(c) = &mut self.current {
            c.state.set(HunkRef { file, hunk }, d);
        }
        self.save();
        cx.notify();
    }

    pub fn decide_file(&mut self, file: usize, d: Decision, cx: &mut Context<Self>) {
        if let Some(c) = &mut self.current {
            c.state.set_file(file, d);
        }
        self.save();
        cx.notify();
    }

    /// Write every accepted hunk to the main worktree and record Agent Blame.
    pub fn apply_accepted(&mut self, cx: &mut Context<Self>) {
        let Some(c) = &self.current else { return };
        let accepted = c.state.accepted_files();
        if accepted.is_empty() {
            cx.emit(ReviewEvent::Toast("Nothing accepted yet".into()));
            return;
        }
        let mut errors = vec![];
        let mut applied_files = vec![];
        let mut hunks = 0;
        for (fi, f) in c.state.diff().files.iter().enumerate() {
            let slots = f.hunks.len().max(1);
            let idx: Vec<usize> = (0..slots).filter(|&h| c.state.decision(HunkRef { file: fi, hunk: h }) == Some(Decision::Accepted)).collect();
            if idx.is_empty() {
                continue;
            }
            let r = if f.binary || f.hunks.is_empty() {
                match &c.input.worktree {
                    Some(wt) => tandem_git::copy_file_from_worktree(wt, &self.root, f),
                    None => Err(tandem_git::GitError::Invalid("worktree is gone".into())),
                }
            } else {
                tandem_git::apply_hunks(&self.root, f, &idx)
            };
            match r {
                Ok(()) => {
                    hunks += idx.len();
                    applied_files.push(f.path.clone());
                }
                Err(e) => errors.push(format!("{}: {e}", f.path.display())),
            }
        }
        // Blame only what actually landed.
        let landed: Vec<_> = accepted.into_iter().filter(|f| applied_files.contains(&f.path)).collect();
        if !landed.is_empty() {
            let applied = DiffSet { files: landed, ..c.state.diff().clone() };
            let path = tandem_review::blame::blame_path(&self.root);
            let mut idx = BlameIndex::load_or_default(&path).unwrap_or_default();
            idx.apply(&applied, &c.input.meta);
            if let Err(e) = idx.save(&path) {
                errors.push(format!("blame not saved: {e}"));
            }
            crate::tasks::blame::invalidate();
        }
        let task = c.input.meta.task.clone();
        if errors.is_empty() {
            cx.emit(ReviewEvent::Toast(format!("Applied {hunks} hunk(s) in {} file(s) to the main worktree", applied_files.len()).into()));
        } else {
            cx.emit(ReviewEvent::Toast(format!("Applied {hunks} hunk(s); {} failed: {}", errors.len(), errors.join("; ")).into()));
        }
        if hunks > 0 {
            cx.emit(ReviewEvent::Applied { task, hunks });
        }
        cx.notify();
    }

    /// Agent Blame entry for a main-tree line (see also `crate::tasks::blame::lookup`).
    pub fn blame_at(&self, path: &std::path::Path, line: u32) -> Option<tandem_core::BlameEntry> {
        crate::tasks::blame::lookup(&self.root, path, line)
    }

    fn file_row(&self, c: &Current, ix: usize, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let f = &c.input.diff.files[ix];
        let name = f.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let dir = f.path.parent().map(|d| d.display().to_string()).unwrap_or_default();
        let (g, gc) = file_icon(&name);
        let color = match f.change {
            FileChange::Added => p.git_added,
            FileChange::Deleted => p.git_deleted,
            _ => p.git_modified,
        };
        let (adds, dels) = line_counts(f);
        let selected = ix == self.selected;
        let shown = f.path.display().to_string();
        let risky = c.summary.risky.iter().any(|r| r.contains(&shown));
        let slots = f.hunks.len().max(1);
        let decided: Vec<Option<Decision>> = (0..slots).map(|h| c.state.decision(HunkRef { file: ix, hunk: h })).collect();
        let all_acc = decided.iter().all(|d| *d == Some(Decision::Accepted));
        let all_rej = decided.iter().all(|d| *d == Some(Decision::Rejected));
        h_flex()
            .id(("review-file", ix))
            .h(px(26.))
            .px(px(10.))
            .gap(px(6.))
            .when(selected, |d| d.bg(p.row_selected))
            .when(!selected, |d| d.hover(|s| s.bg(p.hover)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.select(ix, cx)))
            .child(icon(g, 16., gc))
            .child(div().flex_none().text_color(color).whitespace_nowrap().child(name))
            .child(div().flex_1().min_w_0().text_size(px(11.)).text_color(p.muted).whitespace_nowrap().overflow_hidden().text_ellipsis().child(dir))
            .when(risky, |d| d.child(icon(IconName::TriangleAlert, 12., p.warning)))
            .when(all_acc, |d| d.child(icon(IconName::Check, 12., p.ok)))
            .when(all_rej, |d| d.child(icon(IconName::X, 12., p.error)))
            .child(div().text_size(px(11.)).text_color(p.git_added).child(format!("+{adds}")))
            .child(div().text_size(px(11.)).text_color(p.error).child(format!("−{dels}")))
    }

    fn conflict_banner(&self, i: usize, c: &IntentConflict, me: &TaskId, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let (mine, other) = if &c.a.task == me { (&c.a, &c.b) } else { (&c.b, &c.a) };
        let other_name = agent_badge(&other.agent).2;
        let other_task = other.task.clone();
        h_flex()
            .flex_none()
            .mx(px(10.))
            .mt(px(6.))
            .px(px(10.))
            .py(px(7.))
            .gap(px(8.))
            .items_start()
            .rounded(px(6.))
            .border_1()
            .border_color(p.warning)
            .bg(p.amber_bg)
            .child(icon(IconName::TriangleAlert, 16., p.warning))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(p.warning).child("Intent conflict"))
                            .child(chip(format!("{:?}", c.severity).to_lowercase(), p.warning, p.warning.opacity(0.15)))
                            .children(c.symbol.clone().map(|s| div().text_size(px(11.)).text_color(p.muted).child(s))),
                    )
                    .child(div().text_size(px(12.)).text_color(p.text).child(c.explanation.clone()))
                    .child(div().text_size(px(12.)).text_color(p.muted).child(format!("This task: “{}”", mine.intent)))
                    .child(div().text_size(px(12.)).text_color(p.muted).child(format!("{other_name} ({}): “{}”", other.task, other.intent))),
            )
            .child(Button::new(("show-other", i)).xsmall().label("Show other task").on_click(cx.listener(move |_, _, _, cx| cx.emit(ReviewEvent::ShowTask(other_task.clone())))))
    }
}

fn parse(ds: &DiffSet, ix: usize) -> Vec<DiffFile> {
    ds.files
        .get(ix)
        .and_then(|f| DiffFile::parse(&to_unified_patch(f)).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|f| {
            let lang = crate::assets::language_for(std::path::Path::new(f.path().as_ref()));
            f.with_language(lang)
        })
        .collect()
}

impl Render for ReviewPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let Some(c) = self.current.take() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .text_color(p.muted)
                .child(icon(IconName::GitCompare, 28., p.muted))
                .child("No task to review")
                .child(div().text_size(px(12.)).child("When an agent task finishes, its worktree diff shows up here."))
                .into_any_element();
        };
        let (letter, color, agent_name) = agent_badge(&c.input.meta.agent);
        let me = c.input.meta.task.clone();
        let files: Vec<AnyElement> = (0..c.input.diff.files.len()).map(|i| self.file_row(&c, i, &p, cx).into_any_element()).collect();
        let banners: Vec<AnyElement> = c.conflicts.iter().enumerate().map(|(i, k)| self.conflict_banner(i, k, &me, &p, cx).into_any_element()).collect();
        let counts = c.state.counts();
        let sel = self.selected;
        let file = c.input.diff.files.get(sel).cloned();
        let mut hunk_bar: Vec<AnyElement> = Vec::new();
        if let Some(f) = &file {
            let rel = f.path.clone();
            let slots = f.hunks.len().max(1);
            for h in 0..slots {
                let state = c.state.decision(HunkRef { file: sel, hunk: h }).unwrap_or_default();
                let (label, line) = match f.hunks.get(h) {
                    Some(hk) => (format!("@@ −{},{} +{},{}", hk.old_start, hk.old_lines, hk.new_start, hk.new_lines), hk.new_start.max(1)),
                    None => ("whole file".to_string(), 1),
                };
                let sym = c.symbols.get(h).and_then(|s| s.first()).map(|s| (s.qualified(), s.range.start));
                let open_line = sym.as_ref().map(|s| s.1).unwrap_or(line);
                let path = self.root.join(&rel);
                hunk_bar.push(
                    h_flex()
                        .gap(px(6.))
                        .px(px(8.))
                        .h(px(28.))
                        .rounded(px(4.))
                        .bg(p.panel_bg)
                        .child(div().text_size(px(12.)).text_color(p.muted).child(format!("Hunk {}", h + 1)))
                        .child(
                            div()
                                .id(("hunk-open", h))
                                .min_w_0()
                                .text_size(px(12.))
                                .text_color(p.accent)
                                .cursor_pointer()
                                .hover(|s| s.underline())
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(match &sym {
                                    Some((name, _)) => format!("{label}  {name}"),
                                    None => label,
                                })
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(ReviewEvent::OpenFile { path: path.clone(), line: open_line }))),
                        )
                        .child(div().flex_1())
                        .child(match state {
                            Decision::Accepted => div().text_size(px(12.)).text_color(p.ok).child("Accepted").into_any_element(),
                            Decision::Rejected => div().text_size(px(12.)).text_color(p.error).child("Rejected").into_any_element(),
                            Decision::Pending => div().into_any_element(),
                        })
                        .child(Button::new(("accept", h)).xsmall().primary().label("Accept").on_click(cx.listener(move |this, _, _, cx| this.decide_hunk(sel, h, Decision::Accepted, cx))))
                        .child(Button::new(("reject", h)).xsmall().label("Reject").on_click(cx.listener(move |this, _, _, cx| this.decide_hunk(sel, h, Decision::Rejected, cx))))
                        .into_any_element(),
                );
            }
        }
        let split = self.mode == DiffMode::Split;
        let root = self.root.clone();
        let risky = c.summary.risky.clone();
        let el = v_flex()
            .size_full()
            .text_size(px(13.))
            .child(
                h_flex()
                    .flex_none()
                    .px(px(12.))
                    .h(px(36.))
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(p.border)
                    .child(avatar(letter, color, 18.))
                    .child(div().flex_1().min_w_0().text_color(p.text).font_weight(FontWeight::MEDIUM).whitespace_nowrap().overflow_hidden().text_ellipsis().child(format!(
                        "{agent_name} · {}",
                        c.input.meta.intent_line()
                    )))
                    .child(div().flex_none().text_size(px(12.)).text_color(p.muted).child(format!(
                        "{} files · +{} −{} · {}/{} decided",
                        c.summary.files,
                        c.summary.additions,
                        c.summary.deletions,
                        counts.accepted + counts.rejected,
                        counts.total
                    )))
                    .child(
                        Button::new("diff-mode")
                            .xsmall()
                            .ghost()
                            .label(if split { "Unified" } else { "Split" })
                            .on_click(cx.listener(move |this, _, _, cx| this.set_mode(if split { DiffMode::Unified } else { DiffMode::Split }, cx))),
                    )
                    .child(Button::new("apply-accepted").xsmall().primary().label("Apply accepted").on_click(cx.listener(|this, _, _, cx| this.apply_accepted(cx)))),
            )
            .when(!risky.is_empty(), |d| {
                d.child(
                    h_flex()
                        .flex_none()
                        .flex_wrap()
                        .px(px(12.))
                        .py(px(4.))
                        .gap(px(6.))
                        .children(risky.into_iter().map(|r| chip(r, p.warning, p.warning.opacity(0.15)))),
                )
            })
            .children(banners)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .mt(px(6.))
                    .child(
                        v_flex()
                            .id("review-files")
                            .w(px(240.))
                            .h_full()
                            .flex_none()
                            .border_r_1()
                            .border_color(p.border)
                            .overflow_y_scroll()
                            .child(div().px(px(10.)).h(px(22.)).text_size(px(11.)).text_color(p.muted).child(format!("{} FILES", c.input.diff.files.len())))
                            .children(files),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .when_some(file.clone(), |d, f| {
                                let path = root.join(&f.path);
                                d.child(
                                    h_flex()
                                        .flex_none()
                                        .px(px(8.))
                                        .h(px(28.))
                                        .gap(px(6.))
                                        .child(div().flex_1().min_w_0().text_size(px(12.)).text_color(p.text).whitespace_nowrap().overflow_hidden().text_ellipsis().child(f.path.display().to_string()))
                                        .child(Button::new("open-file").xsmall().ghost().label("Open file").on_click(cx.listener(move |_, _, _, cx| {
                                            cx.emit(ReviewEvent::OpenFile { path: path.clone(), line: 1 })
                                        })))
                                        .child(Button::new("accept-file").xsmall().label("Accept file").on_click(cx.listener(move |this, _, _, cx| this.decide_file(sel, Decision::Accepted, cx))))
                                        .child(Button::new("reject-file").xsmall().label("Reject file").on_click(cx.listener(move |this, _, _, cx| this.decide_file(sel, Decision::Rejected, cx)))),
                                )
                            })
                            .child(v_flex().id("hunks").flex_none().max_h(px(160.)).overflow_y_scroll().p(px(6.)).gap(px(4.)).children(hunk_bar))
                            .child(div().flex_1().min_h_0().child(Diff::new(&self.diff).line_number(true).syntax_highlight(true))),
                    ),
            )
            .into_any_element();
        self.current = Some(c);
        el
    }
}
