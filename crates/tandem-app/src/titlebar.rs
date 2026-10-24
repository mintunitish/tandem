//! New UI main toolbar: traffic lights inset, project ▾, branch ▾, search, settings, agent
//! status pill. Rendered inside gpui-kit's `TitleBar` (which owns window dragging).

use crate::assets::icon;
use crate::keymap::{NotImplemented, OpenSettings, SearchEverywhere, ToggleAgents};
use crate::theme::{Palette, hex};
use gpui_kit::assets::IconName;
use gpui_kit::component::TitleBar;
use gpui_kit::*;

pub const TITLE_BAR_H: f32 = 40.0;

pub struct TitleInfo {
    pub project: String,
    pub branch: String,
    pub agents_running: usize,
    pub agents_waiting: usize,
}

fn widget(id: &'static str, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(28.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(5.))
        .cursor_pointer()
        .hover(|s| s.bg(p.hover))
}

fn icon_btn(id: &'static str, name: IconName, color: Hsla, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .cursor_pointer()
        .hover(|s| s.bg(p.hover))
        .child(icon(name, 16., color))
}

/// Two-letter project badge on a colored square, like the New UI project widget.
fn project_badge(name: &str) -> impl IntoElement {
    let letters: String = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .take(2)
        .map(|s| s.chars().next().unwrap_or('?'))
        .collect::<String>()
        .to_uppercase();
    let letters = if letters.chars().count() < 2 { name.chars().take(2).collect::<String>().to_uppercase() } else { letters };
    let palette = [0x3D7A5A, 0x6B4FA0, 0x2F6FA8, 0xA35C3A, 0x8A6B2E];
    let ix = name.bytes().fold(0usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize)) % palette.len();
    div()
        .size(px(20.))
        .rounded(px(4.))
        .bg(hex(palette[ix]))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(10.))
        .font_weight(FontWeight::BOLD)
        .text_color(hex(0xFFFFFF))
        .child(letters)
}

pub fn render(info: &TitleInfo, p: &Palette) -> impl IntoElement {
    let fg = p.text_bright;
    let left = div()
        .flex()
        .items_center()
        .gap(px(2.))
        .child(
            widget("tb-project", p)
                .child(project_badge(&info.project))
                .child(div().text_color(fg).font_weight(FontWeight::SEMIBOLD).child(info.project.clone()))
                .child(icon(IconName::ChevronDown, 12., p.muted))
                .on_click(|_, w, cx| w.dispatch_action(Box::new(NotImplemented("Recent Projects".into())), cx)),
        )
        .child(
            widget("tb-branch", p)
                .child(icon(IconName::GitBranch, 16., p.text))
                .child(div().text_color(fg).child(info.branch.clone()))
                .child(icon(IconName::ChevronDown, 12., p.muted))
                .on_click(|_, w, cx| w.dispatch_action(Box::new(NotImplemented("Git Branches".into())), cx)),
        );

    let waiting = info.agents_waiting > 0;
    let pill_color = if waiting { p.error } else if info.agents_running > 0 { p.ok } else { p.muted };
    let agents = |n: usize| if n == 1 { "1 agent".to_string() } else { format!("{n} agents") };
    let pill_text = if waiting {
        format!("{} · {} need{} approval", agents(info.agents_running), info.agents_waiting, if info.agents_waiting == 1 { "s" } else { "" })
    } else if info.agents_running > 0 {
        format!("{} running", agents(info.agents_running))
    } else {
        "No agents running".to_string()
    };
    let pill = div()
        .id("tb-agents")
        .h(px(24.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(12.))
        .border_1()
        .border_color(pill_color.opacity(0.6))
        .bg(pill_color.opacity(0.14))
        .cursor_pointer()
        .child(div().size(px(7.)).rounded_full().bg(pill_color))
        .child(div().text_size(px(12.)).text_color(fg).child(pill_text))
        .on_click(|_, w, cx| w.dispatch_action(Box::new(ToggleAgents), cx));

    let right = div()
        .flex()
        .items_center()
        .gap(px(4.))
        .child(pill)
        .child(icon_btn("tb-search", IconName::Search, p.text, p).on_click(|_, w, cx| w.dispatch_action(Box::new(SearchEverywhere), cx)))
        .child(icon_btn("tb-settings", IconName::Settings, p.text, p).on_click(|_, w, cx| w.dispatch_action(Box::new(OpenSettings), cx)));

    TitleBar::new()
        .h(px(TITLE_BAR_H))
        .bg(p.title_bar_bg)
        .border_color(p.border)
        .child(
            div()
                .w_full()
                .h_full()
                .flex()
                .items_center()
                .justify_between()
                .pr(px(8.))
                .text_size(px(13.))
                .child(left)
                .child(right),
        )
}
