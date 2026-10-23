//! tandem-lsp: Language Server Protocol client manager (no UI).
//!
//! * [`config`]: built-in server registry (`builtin_servers`, `detect`, install hints).
//! * [`LspClient`]: one running server: JSON-RPC over stdio, handshake, document sync,
//!   typed requests, notification fan-out, crash detection with one auto-restart.
//! * [`LspManager`]: maps (root, server) to running clients and aggregates diagnostics.
//! * [`position`]: byte offset <-> UTF-16 LSP position conversion.
//! * [`framing`]: `Content-Length` framing codec.
//!
//! All async APIs require a tokio runtime (process I/O). Notification receivers are
//! `async-channel` receivers so the gpui app can poll them from its own executor.

pub mod client;
pub mod config;
pub mod debounce;
pub mod error;
pub mod framing;
pub mod manager;
pub mod position;
pub mod uri;

pub use client::{ClientOptions, LspClient, ServerNotification};
pub use config::{FileType, LanguageServerConfig, ServerCommand, builtin_servers, detect, server_for_path};
pub use debounce::ChangeDebouncer;
pub use error::LspError;
pub use lsp_types;
pub use manager::{ClientKey, LspManager, ManagerEvent};
pub use position::{offset_to_position, offsets_to_range, position_to_offset, range_to_offsets};
pub use uri::{path_to_uri, uri_to_path};

pub type Result<T, E = LspError> = std::result::Result<T, E>;
