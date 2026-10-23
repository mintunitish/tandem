use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error("language server `{server}` is not installed; {hint}")]
    NotInstalled { server: String, hint: String },
    #[error("failed to spawn `{program}`: {source}")]
    Spawn { program: String, #[source] source: std::io::Error },
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("framing error: {0}")]
    Framing(#[from] crate::framing::FramingError),
    #[error("server error {code}: {message}")]
    Rpc { code: i64, message: String, data: Option<serde_json::Value> },
    #[error("request `{method}` timed out")]
    Timeout { method: String },
    #[error("language server exited")]
    ServerExited,
    #[error("path is not absolute or not representable as a file URI: {0}")]
    InvalidPath(PathBuf),
    #[error("invalid URI `{0}`")]
    InvalidUri(String),
}

impl LspError {
    /// LSP `RequestCancelled` / `ContentModified`: safe to ignore in the UI.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, LspError::Rpc { code: -32800 | -32801, .. })
    }
}
