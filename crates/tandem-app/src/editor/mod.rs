//! Editor area: tabs, breadcrumbs, gpui-kit `Editor`, and line commands.

pub mod breadcrumbs;
pub mod ops;
pub mod pane;
pub mod tabs;

pub use pane::{EditorArea, EditorAreaEvent};
