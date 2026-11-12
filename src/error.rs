//! Error type shared by every command.
//!
//! Errors carry a [`ErrorKind`] that maps to a stable process exit code and,
//! where possible, a `hint` telling the caller how to fix the problem. Both
//! are part of the CLI's contract with scripts and AI agents.

use std::fmt;

use serde::Serialize;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Broad failure category. Determines the process exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Invalid arguments or missing required input.
    Usage,
    /// Not logged in, or the credentials were rejected.
    Auth,
    /// Google rejected the request (permission, not found, bad request).
    Api,
    /// Quota or rate limit exhausted.
    RateLimit,
    /// The request never completed, or Google answered with a 5xx.
    Network,
    /// A local file could not be read or written.
    Io,
    /// A bug or an unexpected response shape.
    Internal,
}

impl ErrorKind {
    /// Process exit code for this kind of failure.
    ///
    /// `2` matches the code clap uses for argument errors, so every usage
    /// problem exits the same way regardless of who detected it.
    pub fn exit_code(self) -> u8 {
        match self {
            ErrorKind::Network | ErrorKind::Io | ErrorKind::Internal => 1,
            ErrorKind::Usage => 2,
            ErrorKind::Auth => 3,
            ErrorKind::Api => 4,
            ErrorKind::RateLimit => 5,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    /// What the caller can do about it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// HTTP status, when the error came from an API response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Google's canonical status string, e.g. `PERMISSION_DENIED`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            hint: None,
            status: None,
            code: None,
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Usage, message)
    }

    pub fn auth(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Auth, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }

    /// A filesystem failure, prefixed with what was being attempted.
    pub fn io(context: impl fmt::Display, source: std::io::Error) -> Self {
        Self::new(ErrorKind::Io, format!("{context}: {source}"))
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn exit_code(&self) -> u8 {
        self.kind.exit_code()
    }

    /// Multi-line rendering for a terminal.
    pub fn render_human(&self) -> String {
        let mut out = format!("error: {}", self.message);
        if let Some(hint) = &self.hint {
            out.push_str("\n  hint: ");
            out.push_str(hint);
        }
        out
    }

    /// Single-line `{"error": {...}}` rendering for machine consumers.
    pub fn render_json(&self) -> String {
        serde_json::json!({ "error": self }).to_string()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Self::new(ErrorKind::Io, source.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(source: serde_json::Error) -> Self {
        Self::internal(format!("invalid JSON: {source}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_stable() {
        assert_eq!(ErrorKind::Internal.exit_code(), 1);
        assert_eq!(ErrorKind::Network.exit_code(), 1);
        assert_eq!(ErrorKind::Io.exit_code(), 1);
        assert_eq!(ErrorKind::Usage.exit_code(), 2);
        assert_eq!(ErrorKind::Auth.exit_code(), 3);
        assert_eq!(ErrorKind::Api.exit_code(), 4);
        assert_eq!(ErrorKind::RateLimit.exit_code(), 5);
    }

    #[test]
    fn human_rendering_includes_hint() {
        let err = Error::auth("not logged in").with_hint("run `gsc auth login`");
        assert_eq!(
            err.render_human(),
            "error: not logged in\n  hint: run `gsc auth login`"
        );
        assert_eq!(
            Error::usage("bad flag").render_human(),
            "error: bad flag",
            "no hint line when there is no hint"
        );
    }

    #[test]
    fn json_rendering_omits_empty_fields() {
        let plain: serde_json::Value =
            serde_json::from_str(&Error::usage("bad flag").render_json()).unwrap();
        assert_eq!(
            plain,
            serde_json::json!({ "error": { "kind": "usage", "message": "bad flag" } })
        );

        let full = Error::new(ErrorKind::Api, "forbidden")
            .with_status(403)
            .with_code("PERMISSION_DENIED")
            .with_hint("check access");
        let full: serde_json::Value = serde_json::from_str(&full.render_json()).unwrap();
        assert_eq!(
            full,
            serde_json::json!({ "error": {
                "kind": "api",
                "message": "forbidden",
                "hint": "check access",
                "status": 403,
                "code": "PERMISSION_DENIED",
            }})
        );
    }

    #[test]
    fn io_errors_keep_their_context() {
        let source = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let err = Error::io("reading /tmp/x", source);
        assert_eq!(err.kind, ErrorKind::Io);
        assert_eq!(err.message, "reading /tmp/x: no such file");
    }
}
