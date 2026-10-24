//! Darcula (New UI) and IntelliJ Light. Two layers:
//! * `Palette`: the exact JetBrains colors our own chrome paints with (global).
//! * a gpui-kit `ThemeConfig` built from the palette so stock components and the editor match.

use gpui_kit::component::{Theme, ThemeConfig};
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::rc::Rc;

pub const UI_FONT: &str = "Inter";
pub const MONO_FONT: &str = "JetBrains Mono";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeKind {
    #[default]
    Darcula,
    Light,
}

impl ThemeKind {
    pub fn name(self) -> &'static str {
        match self {
            ThemeKind::Darcula => "Darcula",
            ThemeKind::Light => "IntelliJ Light",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Palette {
    pub kind: ThemeKind,
    pub editor_bg: Hsla,
    pub panel_bg: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub text_bright: Hsla,
    pub muted: Hsla,
    pub accent: Hsla,
    pub selection: Hsla,
    pub error: Hsla,
    pub warning: Hsla,
    pub ok: Hsla,
    pub git_added: Hsla,
    pub git_modified: Hsla,
    /// Gutter bar of changed (not added) lines.
    pub gutter_modified: Hsla,
    pub git_deleted: Hsla,
    pub git_untracked: Hsla,
    pub hover: Hsla,
    pub row_selected: Hsla,
    pub row_selected_inactive: Hsla,
    pub stripe_selected: Hsla,
    pub current_line: Hsla,
    pub line_number: Hsla,
    pub line_number_active: Hsla,
    pub input_bg: Hsla,
    pub popup_bg: Hsla,
    pub excluded: Hsla,
    pub danger_bg: Hsla,
    pub amber_bg: Hsla,
    pub card_bg: Hsla,
    pub title_bar_bg: Hsla,
    // syntax
    pub syn_keyword: Hsla,
    pub syn_string: Hsla,
    pub syn_number: Hsla,
    pub syn_comment: Hsla,
    pub syn_function: Hsla,
    pub syn_type: Hsla,
    pub syn_constant: Hsla,
    pub syn_attribute: Hsla,
    pub syn_default: Hsla,
}

impl Global for Palette {}

pub fn hex(c: u32) -> Hsla {
    rgb(c).into()
}

fn hexa(c: u32, a: f32) -> Hsla {
    let mut h: Hsla = rgb(c).into();
    h.a = a;
    h
}

impl Palette {
    pub fn darcula() -> Self {
        Self {
            kind: ThemeKind::Darcula,
            editor_bg: hex(0x1E1F22),
            panel_bg: hex(0x2B2D30),
            border: hex(0x393B40),
            text: hex(0xBCBEC4),
            text_bright: hex(0xDFE1E5),
            muted: hex(0x6F737A),
            accent: hex(0x3574F0),
            selection: hex(0x214283),
            error: hex(0xF75464),
            warning: hex(0xF2C55C),
            ok: hex(0x5FB865),
            git_added: hex(0x5FAD65),
            git_modified: hex(0x6F9EF5),
            gutter_modified: hex(0x3574F0),
            git_deleted: hex(0x868A91),
            git_untracked: hex(0xD5756C),
            hover: hexa(0xFFFFFF, 0.06),
            row_selected: hex(0x2E436E),
            row_selected_inactive: hex(0x393B40),
            stripe_selected: hex(0x43454A),
            current_line: hex(0x26282E),
            line_number: hex(0x4B5059),
            line_number_active: hex(0xA1A3AB),
            input_bg: hex(0x1E1F22),
            popup_bg: hex(0x2B2D30),
            excluded: hex(0xC59B5A),
            danger_bg: hex(0x402929),
            amber_bg: hex(0x3D3223),
            card_bg: hex(0x2B2D30),
            title_bar_bg: hex(0x2B2D30),
            syn_keyword: hex(0xCF8E6D),
            syn_string: hex(0x6AAB73),
            syn_number: hex(0x2AACB8),
            syn_comment: hex(0x7A7E85),
            syn_function: hex(0x56A8F5),
            syn_type: hex(0xBCBEC4),
            syn_constant: hex(0xC77DBB),
            syn_attribute: hex(0xB3AE60),
            syn_default: hex(0xBCBEC4),
        }
    }

    pub fn light() -> Self {
        Self {
            kind: ThemeKind::Light,
            editor_bg: hex(0xFFFFFF),
            panel_bg: hex(0xF7F8FA),
            border: hex(0xEBECF0),
            text: hex(0x000000),
            text_bright: hex(0x000000),
            muted: hex(0x818594),
            accent: hex(0x3574F0),
            selection: hex(0xA6D2FF),
            error: hex(0xE55230),
            warning: hex(0xC27D04),
            ok: hex(0x208A3C),
            git_added: hex(0x067D17),
            git_modified: hex(0x0A4ED6),
            gutter_modified: hex(0x3574F0),
            git_deleted: hex(0x6C707E),
            git_untracked: hex(0xC7222D),
            hover: hexa(0x000000, 0.05),
            row_selected: hex(0xD5E1FF),
            row_selected_inactive: hex(0xDFE1E5),
            stripe_selected: hex(0xDFE1E5),
            current_line: hex(0xF5F8FE),
            line_number: hex(0xAEB3C2),
            line_number_active: hex(0x767A8A),
            input_bg: hex(0xFFFFFF),
            popup_bg: hex(0xFFFFFF),
            excluded: hex(0x9E7A3A),
            danger_bg: hex(0xFFF2F0),
            amber_bg: hex(0xFFF6DE),
            card_bg: hex(0xFFFFFF),
            title_bar_bg: hex(0xF7F8FA),
            syn_keyword: hex(0x0033B3),
            syn_string: hex(0x067D17),
            syn_number: hex(0x1750EB),
            syn_comment: hex(0x8C8C8C),
            syn_function: hex(0x00627A),
            syn_type: hex(0x000000),
            syn_constant: hex(0x871094),
            syn_attribute: hex(0x9E880D),
            syn_default: hex(0x080808),
        }
    }

    pub fn for_kind(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Darcula => Self::darcula(),
            ThemeKind::Light => Self::light(),
        }
    }

    pub fn is_dark(&self) -> bool {
        self.kind == ThemeKind::Darcula
    }
}

/// Shorthand: `pal(cx).accent`.
pub fn pal(cx: &App) -> &Palette {
    cx.global::<Palette>()
}

fn css(c: Hsla) -> String {
    let r = c.to_rgb();
    let to = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if r.a >= 0.999 {
        format!("#{:02x}{:02x}{:02x}", to(r.r), to(r.g), to(r.b))
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", to(r.r), to(r.g), to(r.b), to(r.a))
    }
}

/// The gpui-kit theme config generated from a palette.
pub fn theme_config(p: &Palette, editor_font_size: f32) -> serde_json::Value {
    let c = css;
    let syn = |color: Hsla| json!({ "color": c(color) });
    let syn_i = |color: Hsla| json!({ "color": c(color), "font_style": "italic" });
    let dark = p.is_dark();
    json!({
        "name": format!("Tandem {}", p.kind.name()),
        "mode": if dark { "dark" } else { "light" },
        "font.family": UI_FONT,
        "font.size": 13.0,
        "mono_font.family": MONO_FONT,
        "mono_font.size": editor_font_size,
        "radius": 4,
        "radius.lg": 8,
        "shadow": false,
        "colors": {
            "background": c(p.panel_bg),
            "foreground": c(p.text),
            "border": c(p.border),
            "input.border": c(p.border),
            "ring": c(p.accent),
            "caret": if dark { "#CED0D6" } else { "#000000" },
            "selection.background": c(p.selection),
            "accent.background": c(p.hover),
            "accent.foreground": c(p.text),
            "muted.background": c(p.panel_bg),
            "muted.foreground": c(p.muted),
            "popover.background": c(p.popup_bg),
            "popover.foreground": c(p.text),
            "primary.background": c(p.accent),
            "primary.hover.background": if dark { "#366ACE" } else { "#315FBD" },
            "primary.active.background": if dark { "#2F5EB9" } else { "#2E55A3" },
            "primary.foreground": "#FFFFFF",
            "secondary.background": if dark { "#393B40" } else { "#FFFFFF" },
            "secondary.hover.background": if dark { "#43454A" } else { "#F7F8FA" },
            "secondary.active.background": if dark { "#4E5157" } else { "#EBECF0" },
            "secondary.foreground": c(p.text),
            "danger.background": c(p.error),
            "danger.foreground": "#FFFFFF",
            "warning.background": c(p.warning),
            "warning.foreground": "#000000",
            "success.background": c(p.ok),
            "success.foreground": "#FFFFFF",
            "info.background": c(p.accent),
            "info.foreground": "#FFFFFF",
            "list.background": c(p.panel_bg),
            "list.hover.background": c(p.hover),
            "list.active.background": c(p.row_selected),
            "list.active.border": "#00000000",
            "list.even.background": c(p.panel_bg),
            "list.head.background": c(p.panel_bg),
            "table.background": c(p.editor_bg),
            "table.head.background": c(p.panel_bg),
            "table.head.foreground": c(p.muted),
            "table.hover.background": c(p.hover),
            "table.active.background": c(p.row_selected),
            "table.active.border": "#00000000",
            "table.even.background": c(p.editor_bg),
            "table.row.border": c(p.border),
            "tab.background": "#00000000",
            "tab.active.background": c(p.editor_bg),
            "tab.active.foreground": c(p.text),
            "tab.foreground": c(p.muted),
            "tab_bar.background": c(p.panel_bg),
            "title_bar.background": c(p.title_bar_bg),
            "title_bar.border": c(p.border),
            "status_bar.background": c(p.panel_bg),
            "status_bar.border": c(p.border),
            "sidebar.background": c(p.panel_bg),
            "sidebar.foreground": c(p.text),
            "sidebar.border": c(p.border),
            "scrollbar.background": "#00000000",
            "scrollbar.thumb.background": if dark { "#FFFFFF26" } else { "#00000026" },
            "scrollbar.thumb.hover.background": if dark { "#FFFFFF40" } else { "#00000040" },
            "switch.background": if dark { "#4E5157" } else { "#C9CCD6" },
            "overlay": if dark { "#00000066" } else { "#00000026" },
            "window.border": c(p.border),
            "drag.border": c(p.accent),
            "drop_target.background": "#3574F033",
            "link": c(p.accent),
            "link.hover": c(p.accent),
            "link.active": c(p.accent),
            "group_box.background": c(p.editor_bg),
            "base.red": c(p.error),
            "base.green": c(p.ok),
            "base.blue": c(p.accent),
            "base.yellow": c(p.warning)
        },
        "highlight": {
            "editor.background": c(p.editor_bg),
            "editor.foreground": c(p.syn_default),
            "editor.active_line.background": c(p.current_line),
            "editor.line_number": c(p.line_number),
            "editor.active_line_number": c(p.line_number_active),
            "editor.invisible": if dark { "#6F737A66" } else { "#AEB3C266" },
            "editor.gutter.background": c(p.editor_bg),
            "error": c(p.error),
            "warning": c(p.warning),
            "info": c(p.accent),
            "hint": c(p.muted),
            "created": c(p.git_added),
            "modified": c(p.git_modified),
            "deleted": c(p.git_deleted),
            "syntax": {
                "keyword": syn(p.syn_keyword),
                "string": syn(p.syn_string),
                "string.escape": syn(p.syn_keyword),
                "string.regex": syn(p.syn_string),
                "string.special": syn(p.syn_string),
                "number": syn(p.syn_number),
                "boolean": syn(p.syn_keyword),
                "comment": syn_i(p.syn_comment),
                "comment.doc": syn_i(if dark { hex(0x5F826B) } else { hex(0x8C8C8C) }),
                "function": syn(p.syn_function),
                "constructor": syn(p.syn_function),
                "type": syn(p.syn_type),
                "constant": syn(p.syn_constant),
                "enum": syn(p.syn_constant),
                "variant": syn(p.syn_constant),
                "attribute": syn(p.syn_attribute),
                "property": syn(p.syn_constant),
                "tag": syn(if dark { hex(0xD5B778) } else { hex(0x0033B3) }),
                "variable": syn(p.syn_default),
                "variable.special": syn(p.syn_keyword),
                "operator": syn(p.syn_default),
                "punctuation": syn(p.syn_default),
                "punctuation.bracket": syn(p.syn_default),
                "punctuation.delimiter": syn(p.syn_default),
                "label": syn(p.syn_constant),
                "title": json!({ "color": c(p.syn_keyword), "font_weight": 700 }),
                "preproc": syn(p.syn_attribute),
                "embedded": syn(p.syn_default)
            }
        }
    })
}

/// Install `kind` as the live theme: palette global + gpui-kit theme. Refreshes all windows.
pub fn apply(kind: ThemeKind, editor_font_size: f32, cx: &mut App) {
    let palette = Palette::for_kind(kind);
    let value = theme_config(&palette, editor_font_size);
    cx.set_global(palette);
    match serde_json::from_value::<ThemeConfig>(value) {
        Ok(config) => {
            let config = Rc::new(config);
            Theme::update(cx, |t| t.apply_config(&config));
        }
        Err(err) => eprintln!("tandem: theme config rejected: {err}"),
    }
    cx.refresh_windows();
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn theme_configs_parse() {
        for p in [Palette::darcula(), Palette::light()] {
            let v = theme_config(&p, 13.0);
            serde_json::from_value::<ThemeConfig>(v).expect("valid theme config");
        }
    }

    #[test]
    fn css_roundtrip() {
        assert_eq!(css(hex(0x3574F0)), "#3574f0");
    }
}
