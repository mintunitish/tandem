//! Painted editor adornments gpui-kit's `Editor` has no slot for: VCS change marks in the
//! gutter (from `tandem_git::Repo::per_line_gutter_marks`) and the right-hand error stripe.

use crate::theme::Palette;
use gpui_kit::component::input::{EditorState, Position};
use gpui_kit::*;
use std::collections::BTreeMap;
use tandem_core::diff::LineKind;
use tandem_git::GutterMark;
use tandem_lsp::lsp_types::{Diagnostic, DiagnosticSeverity};

pub const STRIPE_W: f32 = 10.0;

/// A 3px bar at the gutter's left edge for every added/changed line, a small wedge where lines
/// were removed. Positions are read from the editor state at paint time, so they track scrolling.
pub fn change_marks(state: Entity<EditorState>, marks: Vec<GutterMark>, p: &Palette) -> impl IntoElement {
    let (added, removed) = (p.git_added, p.git_deleted);
    canvas(
        |_, _, _| (),
        move |bounds, _, window, cx| {
            if marks.is_empty() {
                return;
            }
            let s = state.read(cx);
            let (Some(lh), Some(tb)) = (s.line_height(), s.text_bounds()) else { return };
            let off = s.scroll_offset();
            let dm = s.display_map();
            let lines = dm.buffer_line_count();
            for m in &marks {
                let line = (m.line as usize).saturating_sub(1);
                if line > lines.max(1) || (line < lines && dm.is_buffer_line_hidden(line)) {
                    continue;
                }
                let row = if line < lines { dm.buffer_line_to_display_row(line) } else { line };
                let y = tb.origin.y + off.y + lh * row as f32;
                if y + lh < bounds.top() || y > bounds.bottom() {
                    continue;
                }
                let quad = match m.kind {
                    LineKind::Removed => Bounds::new(point(bounds.left() + px(1.), y - px(2.)), size(px(6.), px(4.))),
                    _ => Bounds::new(point(bounds.left() + px(1.), y), size(px(3.), lh)),
                };
                let color = if m.kind == LineKind::Removed { removed } else { added };
                window.paint_quad(fill(quad, color));
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

fn severity_rank(d: &Diagnostic) -> u8 {
    match d.severity {
        Some(DiagnosticSeverity::ERROR) => 0,
        Some(DiagnosticSeverity::WARNING) => 1,
        _ => 2,
    }
}

/// Error stripe right of the editor: one tick per problem line (red error / amber warning) and a
/// thin VCS tick; clicking a tick moves the caret there. The top square summarizes the file.
pub fn error_stripe(state: Entity<EditorState>, diags: &[Diagnostic], marks: &[GutterMark], lines: usize, p: &Palette) -> impl IntoElement {
    let mut by_line: BTreeMap<u32, u8> = BTreeMap::new();
    for d in diags {
        let e = by_line.entry(d.range.start.line).or_insert(3);
        *e = (*e).min(severity_rank(d));
    }
    let worst = by_line.values().copied().min();
    let summary = match worst {
        Some(0) => p.error,
        Some(1) => p.warning,
        Some(_) => p.muted,
        None => p.ok,
    };
    let frac = |line: u32| (line as f32 / lines.max(1) as f32).clamp(0.0, 0.995);
    let ticks = by_line.into_iter().take(400).map(|(line, rank)| {
        let color = match rank {
            0 => p.error,
            1 => p.warning,
            _ => p.muted,
        };
        let state = state.clone();
        div()
            .id(("stripe", line as usize))
            .absolute()
            .top(relative(frac(line)))
            .left(px(1.))
            .w(px(STRIPE_W - 2.))
            .h(px(3.))
            .bg(color)
            .cursor_pointer()
            .on_click(move |_, window, cx| {
                state.update(cx, |s, cx| {
                    s.set_cursor_position(Position::new(line, 0), window, cx);
                    s.focus(window, cx);
                })
            })
    });
    let vcs = marks.iter().take(400).map(|m| {
        div()
            .absolute()
            .top(relative(frac(m.line.saturating_sub(1))))
            .left(px(0.))
            .w(px(2.))
            .h(px(2.))
            .bg(if m.kind == LineKind::Removed { p.git_deleted } else { p.git_added })
    });
    div()
        .w(px(STRIPE_W))
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .bg(p.editor_bg)
        .child(div().h(px(14.)).flex_none().flex().items_center().justify_center().child(div().size(px(6.)).rounded(px(1.)).bg(summary)))
        .child(div().relative().flex_1().children(vcs).children(ticks))
}
