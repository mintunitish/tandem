//! tandem-terminal: headless terminal emulation (PTY + VT grid).
//!
//! A [`Terminal`] spawns a child process on a PTY (`portable-pty`), feeds its output into an
//! `alacritty_terminal::Term`, and exposes the result as plain-data snapshots
//! ([`GridSnapshot`]) that a UI (the gpui app) can render. No UI types appear here.
//!
//! * [`Terminal::spawn`] / [`SpawnOptions`]: start a shell or any command.
//! * [`Terminal::snapshot`], [`Terminal::snapshot_range`], [`Terminal::scroll`]: read the grid.
//! * [`Terminal::events`]: a [`TermEvent`] receiver telling the UI when to repaint.
//! * [`input::encode_key`], [`input::encode_paste`]: keyboard/paste to PTY bytes.
//! * [`palette`]: color palettes (Darcula, IntelliJ Light) used to resolve cell colors.
//!
//! Line coordinates: line `0` is the first row of the live screen, negative lines are scrollback
//! (`-scrollback_len` is the oldest line), and `rows - 1` is the last live row.

pub mod grid;
pub mod input;
pub mod palette;
mod terminal;

pub use grid::{Cell, CellFlags, Cursor, CursorShape, GridSnapshot, Rgb};
pub use input::{Key, KeyInput, Mods, encode_key, encode_paste};
pub use palette::Palette;
pub use terminal::{SpawnOptions, TermEvent, TermModes, Terminal};
