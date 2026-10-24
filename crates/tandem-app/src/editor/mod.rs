//! Editor area: tabs, breadcrumbs, gpui-kit `Editor`, line commands, LSP wiring (completion,
//! hover, definition, code actions, diagnostics, rename, usages, format), VCS gutter marks,
//! Find in Files and Usages results.

pub mod actions;
pub mod breadcrumbs;
pub mod find;
pub mod gutter;
pub mod lsp;
pub mod ops;
pub mod pane;
pub mod tabs;
pub mod usages;

pub use pane::{EditorArea, EditorAreaEvent};
