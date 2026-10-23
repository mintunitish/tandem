//! Tandem IDE application crate: the gpui-kit shell (WebStorm New UI layout) and its panels.
#![recursion_limit = "512"]

pub mod app;
pub mod assets;
pub mod editor;
pub mod keymap;
pub mod model;
pub mod panels;
pub mod search_everywhere;
pub mod settings;
pub mod state;
pub mod statusbar;
pub mod stripes;
pub mod theme;
pub mod titlebar;
pub mod workspace;

pub use app::run;
pub use workspace::Workspace;
