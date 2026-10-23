//! Tool windows. Each content view is wrapped in a `ToolWindow`, the dock panel that draws the
//! JetBrains-style header ("Project ▾ ……… ⋮ —") itself (gpui-kit's own title bar is disabled).

pub mod agents;
pub mod database;
pub mod git;
pub mod harness;
pub mod misc;
pub mod project;
pub mod review;
pub mod terminal;

use crate::assets::icon;
use crate::stripes::ToolWindowId;
use crate::theme::pal;
use gpui_kit::assets::IconName;
use gpui_kit::component::dock::{BasePanel, Panel, PanelControl, PanelEvent};
use gpui_kit::component::h_flex;
use gpui_kit::*;

/// Emitted when the header's hide button is pressed.
pub struct HideToolWindow(pub ToolWindowId);

type HeaderExtra = Box<dyn Fn(&mut Window, &mut App) -> AnyElement>;

pub struct ToolWindow {
    pub id: ToolWindowId,
    content: AnyView,
    focus: FocusHandle,
    /// Extra header controls (e.g. Project's "locate" button), rendered left of ⋮ —.
    header_extra: Option<HeaderExtra>,
}

impl ToolWindow {
    pub fn new(id: ToolWindowId, content: AnyView, cx: &mut Context<Self>) -> Self {
        Self { id, content, focus: cx.focus_handle(), header_extra: None }
    }

    pub fn with_header_extra(mut self, f: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) -> Self {
        self.header_extra = Some(Box::new(f));
        self
    }
}

impl EventEmitter<PanelEvent> for ToolWindow {}
impl EventEmitter<HideToolWindow> for ToolWindow {}

impl Focusable for ToolWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BasePanel for ToolWindow {
    fn panel_name(&self) -> &'static str {
        self.id.panel_name()
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for ToolWindow {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(self.id.title().into())
    }
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.id.title()
    }
    fn title_bar(&self, _: &App) -> bool {
        false
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }
}

impl Render for ToolWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal(cx).clone();
        let id = self.id;
        let extra = self.header_extra.as_ref().map(|f| f(window, cx));
        div()
            .id(SharedString::from(format!("tw-{}", id.panel_name())))
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(p.panel_bg)
            .text_color(p.text)
            .child(
                h_flex()
                    .h(px(36.))
                    .flex_none()
                    .pl(px(12.))
                    .pr(px(6.))
                    .gap(px(4.))
                    .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(id.title()))
                    .child(div().flex_1())
                    .children(extra)
                    .child(header_icon("tw-more", IconName::EllipsisVertical, &p))
                    .child(header_icon("tw-hide", IconName::Minus, &p).on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(HideToolWindow(id));
                    }))),
            )
            .child(div().flex_1().min_h_0().child(self.content.clone()))
    }
}

/// 24px square icon button with hover fill, JetBrains toolbar style.
pub fn header_icon(id: &'static str, name: IconName, p: &crate::theme::Palette) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .cursor_pointer()
        .hover(|s| s.bg(p.hover))
        .child(icon(name, 16., p.muted))
}

/// A uniform list row height for tool-window lists.
pub const ROW_H: f32 = 24.0;

/// Section label used inside tool windows ("Changes  3 files").
pub fn section_label(text: impl Into<SharedString>, p: &crate::theme::Palette) -> Div {
    div().px(px(12.)).py(px(6.)).text_size(px(12.)).text_color(p.muted).child(text.into())
}

/// Colored pill ("Running", "Needs approval").
pub fn chip(text: impl Into<SharedString>, fg: Hsla, bg: Hsla) -> Div {
    div()
        .px(px(6.))
        .h(px(18.))
        .flex()
        .items_center()
        .rounded(px(9.))
        .bg(bg)
        .text_color(fg)
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .child(text.into())
}
