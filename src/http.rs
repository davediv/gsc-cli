//! Shared HTTP client configuration.

use std::time::Duration;

use crate::error::{Error, ErrorKind, Result};

const USER_AGENT: &str = concat!("gsc-cli/", env!("CARGO_PKG_VERSION"));

/// Build the HTTP client used for every request `gsc` makes.
pub fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        // URL inspection can legitimately take tens of seconds.
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| Error::internal(format!("could not initialise the HTTP client: {e}")))
}

impl From<reqwest::Error> for Error {
    fn from(source: reqwest::Error) -> Self {
        let what = if source.is_timeout() {
            "request timed out"
        } else if source.is_connect() {
            "could not connect"
        } else {
            "request failed"
        };
        // reqwest's Display includes the URL, which for Google APIs contains
        // no secrets (tokens travel in headers and request bodies).
        let mut message = format!("{what}: {source}");
        let mut cause = std::error::Error::source(&source);
        while let Some(inner) = cause {
            message.push_str(&format!(": {inner}"));
            cause = inner.source();
        }
        Error::new(ErrorKind::Network, message)
            .with_hint("check your internet connection and try again")
    }
}
