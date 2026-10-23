use std::path::PathBuf;

/// Errors produced by `tandem-git`.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git: {0}")]
    Git(#[from] git2::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Bad input or unsupported situation (message is user-presentable).
    #[error("{0}")]
    Invalid(String),
    /// A hunk / file could not be applied because the target no longer matches.
    #[error("cannot apply to {}: {reason}", path.display())]
    Conflict { path: PathBuf, reason: String },
}

pub type Result<T> = std::result::Result<T, GitError>;

pub(crate) fn invalid<T>(msg: impl Into<String>) -> Result<T> {
    Err(GitError::Invalid(msg.into()))
}
