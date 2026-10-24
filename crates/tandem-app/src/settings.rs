//! Settings dialog (⌘,): Appearance (theme, editor font size), Keymap (read-only table built
//! from `keymap::entries`) and Agents (the project's `.tandem/policy.json`).

use crate::keymap;
use crate::state::{AppSettings, load_policy, save_policy};
use crate::theme::{self, ThemeKind, pal};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Appearance,
    Keymap,
    Agents,
}

impl Page {
    const ALL: [Page; 3] = [Page::Appearance, Page::Keymap, Page::Agents];
    fn label(self) -> &'static str {
        match self {
            Page::Appearance => "Appearance",
            Page::Keymap => "Keymap",
            Page::Agents => "Agents & Policy",
        }
    }
}

pub struct SettingsView {
    page: Page,
    project: PathBuf,
    policy: tandem_core::Policy,
}

impl SettingsView {
    pub fn new(project: PathBuf) -> Self {
        let policy = load_policy(&project);
        Self { page: Page::Appearance, project, policy }
    }

    fn set_theme(&mut self, kind: ThemeKind, cx: &mut Context<Self>) {
        let mut s = cx.global::<AppSettings>().clone();
        s.theme = kind;
        s.save();
        theme::apply(kind, s.editor_font_size, cx);
        cx.set_global(s);
        cx.notify();
    }

    fn bump_font(&mut self, delta: f32, cx: &mut Context<Self>) {
        let mut s = cx.global::<AppSettings>().clone();
        s.editor_font_size = (s.editor_font_size + delta).clamp(9.0, 28.0);
        s.save();
        theme::apply(s.theme, s.editor_font_size, cx);
        cx.set_global(s);
        cx.notify();
    }

    fn save_policy(&mut self, cx: &mut Context<Self>) {
        save_policy(&self.project, &self.policy);
        cx.notify();
    }
}

/// Toggle a theme from anywhere (action handler).
pub fn toggle_theme(cx: &mut App) {
    let mut s = cx.global::<AppSettings>().clone();
    s.theme = match s.theme {
        ThemeKind::Darcula => ThemeKind::Light,
        ThemeKind::Light => ThemeKind::Darcula,
    };
    s.save();
    theme::apply(s.theme, s.editor_font_size, cx);
    cx.set_global(s);
}

pub fn open(project: PathBuf, window: &mut Window, cx: &mut App) {
    let view = cx.new(|_| SettingsView::new(project));
    window.open_dialog(cx, move |dialog, _, _| dialog.title("Settings").w(px(760.)).child(view.clone()));
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let settings = cx.global::<AppSettings>().clone();
        let nav = v_flex().w(px(170.)).flex_none().gap(px(2.)).children(Page::ALL.iter().map(|&pg| {
            let active = pg == self.page;
            div()
                .id(SharedString::from(format!("settings-nav-{}", pg.label())))
                .h(px(28.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .cursor_pointer()
                .when(active, |d| d.bg(p.row_selected).text_color(p.text_bright))
                .when(!active, |d| d.hover(|s| s.bg(p.hover)))
                .child(pg.label())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.page = pg;
                    cx.notify();
                }))
        }));

        let label = |t: &'static str| div().w(px(160.)).text_color(p.muted).child(t);
        let content: AnyElement = match self.page {
            Page::Appearance => v_flex()
                .gap(px(14.))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .child(label("Theme"))
                        .children([ThemeKind::Darcula, ThemeKind::Light].map(|k| {
                            let b = Button::new(SharedString::from(format!("theme-{}", k.name()))).small().label(k.name());
                            let b = if settings.theme == k { b.primary() } else { b.outline() };
                            b.on_click(cx.listener(move |this, _, _, cx| this.set_theme(k, cx)))
                        })),
                )
                .child(
                    h_flex()
                        .gap(px(8.))
                        .child(label("Editor font size"))
                        .child(Button::new("font-dec").small().outline().label("−").on_click(cx.listener(|this, _, _, cx| this.bump_font(-1.0, cx))))
                        .child(div().w(px(36.)).flex().justify_center().child(format!("{}", settings.editor_font_size)))
                        .child(Button::new("font-inc").small().outline().label("+").on_click(cx.listener(|this, _, _, cx| this.bump_font(1.0, cx)))),
                )
                .child(h_flex().gap(px(8.)).child(label("UI font")).child(format!("{} 13", theme::UI_FONT)))
                .child(h_flex().gap(px(8.)).child(label("Editor font")).child(format!("{} / line height 1.2", theme::MONO_FONT)))
                .into_any_element(),
            Page::Keymap => div()
                .id("settings-keymap")
                .h(px(380.))
                .overflow_y_scroll()
                .child(v_flex().children(keymap::entries().iter().map(|e| {
                    h_flex()
                        .h(px(26.))
                        .px(px(6.))
                        .border_b_1()
                        .border_color(p.border)
                        .child(div().flex_1().child(e.name))
                        .child(div().text_color(if e.keys.is_empty() { p.muted } else { p.accent }).child(if e.keys.is_empty() {
                            "—".to_string()
                        } else {
                            keymap::display_keys(e.keys)
                        }))
                })))
                .into_any_element(),
            Page::Agents => {
                let pol = &self.policy;
                v_flex()
                    .gap(px(14.))
                    .child(
                        h_flex().gap(px(8.)).child(label("Auto-allow safe reads")).child(
                            Switch::new("pol-safe").checked(pol.auto_allow_safe).on_click(cx.listener(|this, v: &bool, _, cx| {
                                this.policy.auto_allow_safe = *v;
                                this.save_policy(cx);
                            })),
                        ),
                    )
                    .child(
                        h_flex().gap(px(8.)).child(label("Seatbelt sandbox")).child(
                            Switch::new("pol-sandbox").checked(pol.sandbox).on_click(cx.listener(|this, v: &bool, _, cx| {
                                this.policy.sandbox = *v;
                                this.save_policy(cx);
                            })),
                        ),
                    )
                    .child(h_flex().gap(px(8.)).child(label("Trusted commands")).child(if pol.trusted_commands.is_empty() {
                        "none".to_string()
                    } else {
                        pol.trusted_commands.join(", ")
                    }))
                    .child(
                        div()
                            .p(px(10.))
                            .rounded(px(6.))
                            .bg(p.danger_bg)
                            .text_color(p.text)
                            .child("Destructive commands (rm -rf, git push --force, git reset --hard, DROP/TRUNCATE, curl | sh) always require a red-card approval. This cannot be turned off."),
                    )
                    .child(div().text_size(px(12.)).text_color(p.muted).child(format!("Stored in {}", self.project.join(".tandem/policy.json").display())))
                    .into_any_element()
            }
        };
        h_flex()
            .items_start()
            .gap(px(16.))
            .min_h(px(380.))
            .text_size(px(13.))
            .text_color(p.text)
            .child(nav)
            .child(div().flex_1().min_w_0().child(content))
    }
}
