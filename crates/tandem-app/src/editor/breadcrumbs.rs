//! Breadcrumb bar above the editor: `project › dir › file`.

use crate::assets::{file_icon, icon};
use crate::theme::Palette;
use gpui_kit::assets::IconName;
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use std::path::Path;

pub const BREADCRUMB_H: f32 = 26.0;

pub fn segments(root: &Path, path: &Path) -> Vec<String> {
    let mut out = vec![root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()];
    if let Ok(rel) = path.strip_prefix(root) {
        out.extend(rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()));
    } else {
        out = path.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    }
    out
}

pub fn render(root: &Path, path: Option<&Path>, p: &Palette) -> impl IntoElement {
    let segs = path.map(|pth| segments(root, pth)).unwrap_or_default();
    let last = segs.len().saturating_sub(1);
    div()
        .h(px(BREADCRUMB_H))
        .flex_none()
        .w_full()
        .flex()
        .items_center()
        .px(px(12.))
        .gap(px(2.))
        .text_size(px(12.))
        .text_color(p.muted)
        .border_b_1()
        .border_color(p.border)
        .bg(p.editor_bg)
        .children(segs.into_iter().enumerate().map(move |(i, s)| {
            let (glyph, color) = if i == last { file_icon(&s) } else { (IconName::Folder, p.muted) };
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .when(i > 0, |d| d.child(icon(IconName::ChevronRight, 12., p.muted)))
                .child(icon(glyph, 14., color))
                .child(div().when(i == last, |d| d.text_color(p.text)).child(s))
        }))
}

#[cfg(test)]
mod tests {
    use super::segments;
    use std::path::Path;

    #[test]
    fn relative_segments() {
        assert_eq!(segments(Path::new("/a/proj"), Path::new("/a/proj/src/main.rs")), vec!["proj", "src", "main.rs"]);
    }
}
