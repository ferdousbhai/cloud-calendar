use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Missing or unreadable configuration.
    Config,
    NotFound,
    /// The request was invalid, or the provider refused it as invalid.
    BadRequest,
    /// An account needs signing in again (or its password was rejected).
    AccountAuth,
    /// An account's server or command-line tool is missing, failed, or answered something unexpected.
    AccountUnavailable,
}

impl ErrorKind {
    /// Stable machine-readable code, used in JSON error envelopes.
    pub fn code(self) -> &'static str {
        match self {
            ErrorKind::Config => "not_configured",
            ErrorKind::NotFound => "not_found",
            ErrorKind::BadRequest => "bad_request",
            ErrorKind::AccountAuth => "account_unauthorized",
            ErrorKind::AccountUnavailable => "account_unavailable",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::BadRequest, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
