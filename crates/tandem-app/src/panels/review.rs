//! Review tool window: per-task DiffSet with file list, gpui-kit `Diff`, hunk accept/reject and
//! the intent-conflict banner. Integration points: `ReviewPanel::set_diffset(DiffSet)` from
//! `tandem-review`; `ReviewEvent::{AcceptHunk, RejectHunk}` → `tandem_review::apply_hunk`.

use crate::assets::{file_icon, icon};
use crate::model::agents::agent_badge;
use crate::model::review::{fake_diffset, to_unified_patch};
use crate::panels::agents::avatar;
use crate::theme::{Palette, pal};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{Diff, DiffFile, DiffState};
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use tandem_core::{DiffSet, FileChange};

pub enum ReviewEvent {
    AcceptHunk { file: usize, hunk: usize },
    RejectHunk { file: usize, hunk: usize },
    Toast(SharedString),
}

pub struct ReviewPanel {
    pub diffset: DiffSet,
    selected: usize,
    diff: Entity<DiffState>,
    resolved: Vec<Vec<Option<bool>>>,
}

impl EventEmitter<ReviewEvent> for ReviewPanel {}

impl ReviewPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let diffset = fake_diffset();
        let diff = cx.new(|cx| DiffState::new(parse(&diffset, 0), cx));
        let resolved = diffset.files.iter().map(|f| vec![None; f.hunks.len()]).collect();
        Self { diffset, selected: 0, diff, resolved }
    }

    pub fn set_diffset(&mut self, diffset: DiffSet, cx: &mut Context<Self>) {
        self.resolved = diffset.files.iter().map(|f| vec![None; f.hunks.len()]).collect();
        self.diffset = diffset;
        self.select(0, cx);
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = ix;
        let files = parse(&self.diffset, ix);
        self.diff = cx.new(|cx| DiffState::new(files, cx));
        cx.notify();
    }

    fn resolve(&mut self, hunk: usize, accept: bool, cx: &mut Context<Self>) {
        let file = self.selected;
        if let Some(slot) = self.resolved.get_mut(file).and_then(|h| h.get_mut(hunk)) {
            *slot = Some(accept);
        }
        cx.emit(if accept { ReviewEvent::AcceptHunk { file, hunk } } else { ReviewEvent::RejectHunk { file, hunk } });
        cx.emit(ReviewEvent::Toast(format!("Hunk {} {}", hunk + 1, if accept { "accepted" } else { "rejected" }).into()));
        cx.notify();
    }

    fn file_row(&self, ix: usize, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let f = &self.diffset.files[ix];
        let name = f.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let (g, gc) = file_icon(&name);
        let color = match f.change {
            FileChange::Added => p.git_added,
            FileChange::Deleted => p.git_deleted,
            _ => p.git_modified,
        };
        let adds: usize = f.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == tandem_core::LineKind::Added).count();
        let dels: usize = f.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == tandem_core::LineKind::Removed).count();
        let selected = ix == self.selected;
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
            .child(div().flex_1().text_color(color).whitespace_nowrap().child(name))
            .child(div().text_size(px(11.)).text_color(p.git_added).child(format!("+{adds}")))
            .child(div().text_size(px(11.)).text_color(p.error).child(format!("−{dels}")))
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
        let (letter, color, agent_name) = agent_badge(&self.diffset.agent);
        let files: Vec<AnyElement> = (0..self.diffset.files.len()).map(|i| self.file_row(i, &p, cx).into_any_element()).collect();
        let hunks = self.diffset.files.get(self.selected).map(|f| f.hunks.len()).unwrap_or(0);
        let hunk_bar = (0..hunks).map(|h| {
            let state = self.resolved.get(self.selected).and_then(|r| r.get(h)).copied().flatten();
            let f = &self.diffset.files[self.selected].hunks[h];
            h_flex()
                .gap(px(6.))
                .px(px(8.))
                .h(px(28.))
                .rounded(px(4.))
                .bg(p.panel_bg)
                .child(div().text_size(px(12.)).text_color(p.muted).child(format!("Hunk {} @@ -{},{} +{},{}", h + 1, f.old_start, f.old_lines, f.new_start, f.new_lines)))
                .child(div().flex_1())
                .child(match state {
                    Some(true) => div().text_size(px(12.)).text_color(p.ok).child("Accepted").into_any_element(),
                    Some(false) => div().text_size(px(12.)).text_color(p.error).child("Rejected").into_any_element(),
                    None => h_flex()
                        .gap(px(6.))
                        .child(Button::new(("accept", h)).xsmall().primary().label("Accept").on_click(cx.listener(move |this, _, _, cx| this.resolve(h, true, cx))))
                        .child(Button::new(("reject", h)).xsmall().label("Reject").on_click(cx.listener(move |this, _, _, cx| this.resolve(h, false, cx))))
                        .into_any_element(),
                })
        });
        v_flex()
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
                    .child(div().text_color(p.text).font_weight(FontWeight::MEDIUM).child(format!("{agent_name} · Add retry/backoff to HTTP client")))
                    .child(div().flex_1())
                    .child(div().text_size(px(12.)).text_color(p.muted).child(format!("base {}", self.diffset.base_commit))),
            )
            .child(
                h_flex()
                    .flex_none()
                    .mx(px(10.))
                    .mt(px(8.))
                    .px(px(10.))
                    .py(px(7.))
                    .gap(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(p.warning)
                    .bg(p.amber_bg)
                    .child(icon(IconName::TriangleAlert, 16., p.warning))
                    .child(
                        v_flex()
                            .flex_1()
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(p.warning).child("Intent conflict"))
                            .child(div().text_size(px(12.)).text_color(p.text).child(
                                "Codex (“Migrate settings page to React Query”) also edits HttpClient.request() in src/http/client.ts.",
                            )),
                    )
                    .child(Button::new("compare-intents").xsmall().label("Compare")),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .mt(px(8.))
                    .child(
                        v_flex()
                            .w(px(220.))
                            .h_full()
                            .flex_none()
                            .border_r_1()
                            .border_color(p.border)
                            .child(div().px(px(10.)).h(px(22.)).text_size(px(11.)).text_color(p.muted).child(format!("{} FILES", self.diffset.files.len())))
                            .children(files),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(v_flex().flex_none().p(px(6.)).gap(px(4.)).children(hunk_bar))
                            .child(div().flex_1().min_h_0().child(Diff::new(&self.diff).line_number(true).syntax_highlight(true))),
                    ),
            )
    }
}
