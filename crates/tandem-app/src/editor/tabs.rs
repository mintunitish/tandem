//! Editor tab strip, New UI style: 38px tabs, accent underline on the active tab, close button
//! on hover/active, a dot for unsaved changes, middle-click closes.

use super::pane::EditorArea;
use crate::assets::{file_icon, icon};
use crate::theme::Palette;
use gpui_kit::assets::IconName;
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;

pub const TAB_BAR_H: f32 = 38.0;

pub fn render_tab_strip(
    area: &EditorArea,
    scroll: &ScrollHandle,
    p: &Palette,
    _window: &mut Window,
    cx: &mut Context<EditorArea>,
) -> impl IntoElement {
    let tabs = area.files.iter().enumerate().map(|(ix, f)| {
        let name = f.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let active = area.active == Some(ix);
        let hovered = area.hovered_tab == Some(ix);
        let (glyph, glyph_color) = file_icon(&name);
        let close_visible = active || hovered;
        let close = div()
            .id(("tab-close", ix))
            .size(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(3.))
            .when(close_visible, |d| d.hover(|s| s.bg(p.hover)).child(icon(IconName::X, 12., p.muted)))
            .when(!close_visible && f.dirty, |d| d.child(div().size(px(6.)).rounded_full().bg(p.muted)))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.close(ix, window, cx);
            }));
        div()
            .id(("editor-tab", ix))
            .h_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .pl(px(12.))
            .pr(px(8.))
            .relative()
            .cursor_pointer()
            .when(!active, |d| d.hover(|s| s.bg(p.hover)))
            .child(icon(glyph, 16., glyph_color))
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(if active { p.text_bright } else { p.text })
                    .whitespace_nowrap()
                    .child(name),
            )
            .when(f.dirty && close_visible, |d| d.child(div().size(px(6.)).rounded_full().bg(p.muted)))
            .child(close)
            .when(active, |d| {
                d.child(div().absolute().bottom_0().left_0().right_0().h(px(2.)).bg(p.accent))
            })
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.hovered_tab = Some(ix);
                } else if this.hovered_tab == Some(ix) {
                    this.hovered_tab = None;
                }
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Middle, cx.listener(move |this, _, window, cx| {
                this.close(ix, window, cx);
            }))
            .on_click(cx.listener(move |this, _, window, cx| this.activate(ix, window, cx)))
    });
    div()
        .id("editor-tabs")
        .h(px(TAB_BAR_H))
        .flex_none()
        .w_full()
        .flex()
        .border_b_1()
        .border_color(p.border)
        .bg(p.editor_bg)
        .overflow_x_scroll()
        .track_scroll(scroll)
        .children(tabs)
}
