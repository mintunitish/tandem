use thiserror::Error;

/// Errors from tandem-db. Messages never contain passwords: connection errors are
/// scrubbed of the secret before being wrapped.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DbError {
    #[error("database error: {0}")]
    Sql(String),
    #[error("query timed out")]
    Timeout,
    #[error("query cancelled")]
    Cancelled,
    #[error("configuration error: {0}")]
    Config(String),
    #[error("secret store error: {0}")]
    Secret(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("refused: {0}")]
    Refused(String),
}

impl From<sqlx::Error> for DbError {
    fn from(e: sqlx::Error) -> Self {
        DbError::Sql(e.to_string())
    }
}

impl DbError {
    /// Replaces any occurrence of `secret` in the message.
    pub(crate) fn scrub(self, secret: Option<&str>) -> Self {
        let Some(s) = secret.filter(|s| !s.is_empty()) else { return self };
        let fix = |m: String| m.replace(s, "***");
        match self {
            DbError::Sql(m) => DbError::Sql(fix(m)),
            DbError::Config(m) => DbError::Config(fix(m)),
            DbError::Io(m) => DbError::Io(fix(m)),
            other => other,
        }
    }
}
