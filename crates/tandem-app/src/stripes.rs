//! Tool-window identities and the New UI icon stripes on the window edges.

use crate::assets::icon;
use crate::theme::pal;
use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolWindowId {
    #[default]
    Project,
    Commit,
    Structure,
    Bookmarks,
    Agents,
    Review,
    Database,
    Notifications,
    Terminal,
    Run,
    Problems,
    Git,
    Services,
    Harness,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
    Bottom,
}

impl Side {
    pub const ALL: [Side; 3] = [Side::Left, Side::Right, Side::Bottom];

    /// Index into per-side arrays (`LayoutState::sides`).
    pub fn index(self) -> usize {
        self as usize
    }
}

impl ToolWindowId {
    pub const LEFT: &[ToolWindowId] = &[Self::Project, Self::Commit, Self::Structure, Self::Bookmarks];
    pub const RIGHT: &[ToolWindowId] = &[Self::Agents, Self::Review, Self::Database, Self::Notifications];
    pub const BOTTOM: &[ToolWindowId] = &[Self::Terminal, Self::Run, Self::Problems, Self::Git, Self::Services, Self::Harness];

    pub fn all() -> impl Iterator<Item = ToolWindowId> {
        Self::LEFT.iter().chain(Self::RIGHT).chain(Self::BOTTOM).copied()
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Project => "Project",
            Self::Commit => "Commit",
            Self::Structure => "Structure",
            Self::Bookmarks => "Bookmarks",
            Self::Agents => "Agents",
            Self::Review => "Review",
            Self::Database => "Database",
            Self::Notifications => "Notifications",
            Self::Terminal => "Terminal",
            Self::Run => "Run",
            Self::Problems => "Problems",
            Self::Git => "Git",
            Self::Services => "Services",
            Self::Harness => "Harness",
        }
    }

    /// Stable name for the dock's persisted layout.
    pub fn panel_name(self) -> &'static str {
        match self {
            Self::Project => "tandem.project",
            Self::Commit => "tandem.commit",
            Self::Structure => "tandem.structure",
            Self::Bookmarks => "tandem.bookmarks",
            Self::Agents => "tandem.agents",
            Self::Review => "tandem.review",
            Self::Database => "tandem.database",
            Self::Notifications => "tandem.notifications",
            Self::Terminal => "tandem.terminal",
            Self::Run => "tandem.run",
            Self::Problems => "tandem.problems",
            Self::Git => "tandem.git",
            Self::Services => "tandem.services",
            Self::Harness => "tandem.harness",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Self::Project => IconName::Folder,
            Self::Commit => IconName::GitCommitHorizontal,
            Self::Structure => IconName::ListTree,
            Self::Bookmarks => IconName::Bookmark,
            Self::Agents => IconName::Bot,
            Self::Review => IconName::GitCompare,
            Self::Database => IconName::Database,
            Self::Notifications => IconName::Bell,
            Self::Terminal => IconName::SquareTerminal,
            Self::Run => IconName::Play,
            Self::Problems => IconName::CircleAlert,
            Self::Git => IconName::GitBranch,
            Self::Services => IconName::Server,
            Self::Harness => IconName::FlaskConical,
        }
    }

    pub fn shortcut(self) -> &'static str {
        match self {
            Self::Project => "⌘1",
            Self::Commit => "⌘0",
            Self::Structure => "⌘7",
            Self::Bookmarks => "⌘2",
            Self::Agents => "⌘⌥A",
            Self::Terminal => "⌥F12",
            Self::Run => "⌘4",
            Self::Problems => "⌘6",
            Self::Git => "⌘9",
            Self::Services => "⌘8",
            _ => "",
        }
    }

    pub fn side(self) -> Side {
        if Self::LEFT.contains(&self) {
            Side::Left
        } else if Self::RIGHT.contains(&self) {
            Side::Right
        } else {
            Side::Bottom
        }
    }
}

pub const STRIPE_WIDTH: f32 = 40.0;

/// One stripe button: 28px rounded square with a 16px icon, filled when its window is visible.
pub fn stripe_button(
    id: ToolWindowId,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let p = pal(cx);
    let color = if active { p.text_bright } else { p.muted.opacity(1.0) };
    let tooltip = if id.shortcut().is_empty() { id.title().to_string() } else { format!("{}  {}", id.title(), id.shortcut()) };
    let _ = cx.theme();
    div()
        .id(SharedString::from(format!("stripe-{}", id.panel_name())))
        .test_support()
        .size(px(30.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .when(active, |d| d.bg(p.stripe_selected))
        .when(!active, |d| d.hover(|s| s.bg(p.hover)))
        .cursor_pointer()
        .child(icon(id.icon(), 18., if active { color } else { p.text }))
        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx))
        .on_click(on_click)
}
