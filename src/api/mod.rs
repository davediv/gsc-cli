//! Authenticated HTTP client for Google's Search Console and Indexing APIs.
//!
//! [`Client::request`] is the single place where requests are sent. It adds
//! the bearer token, retries transient failures, and converts Google's error
//! responses into [`Error`]s with a hint about what to do next.

pub mod analytics;
pub mod indexing;
pub mod inspection;
pub mod sitemaps;
pub mod sites;

use std::time::{Duration, Instant};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::{CONTENT_LENGTH, RETRY_AFTER};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::auth::{Api, Credentials, TokenProvider};
use crate::error::{Error, ErrorKind, Result};

/// Override the Search Console API origin (used by tests and proxies).
pub const ENV_API_BASE_URL: &str = "GSC_API_BASE_URL";
/// Override the Indexing API origin.
pub const ENV_INDEXING_BASE_URL: &str = "GSC_INDEXING_BASE_URL";
/// How many times a transient failure is retried. `0` disables retries.
pub const ENV_MAX_RETRIES: &str = "GSC_MAX_RETRIES";

const DEFAULT_SEARCH_CONSOLE_URL: &str = "https://searchconsole.googleapis.com";
const DEFAULT_INDEXING_URL: &str = "https://indexing.googleapis.com";
const DEFAULT_MAX_RETRIES: u32 = 3;
const RETRY_BASE_DELAY: Duration = Duration::from_millis(500);
/// Never sleep longer than this for a single retry, whatever the server asks.
const RETRY_MAX_DELAY: Duration = Duration::from_secs(30);

/// Characters left as-is inside a URL path segment (RFC 3986 "unreserved").
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Percent-encode a value for use as one path segment.
///
/// Property identifiers are URLs themselves (`https://example.com/`), so
/// every `/` and `:` must be escaped when they appear in a request path.
pub fn encode_segment(value: &str) -> String {
    utf8_percent_encode(value, PATH_SEGMENT).to_string()
}

/// Normalise an enum value to the lowerCamelCase spelling used in Google's
/// reference documentation.
///
/// Depending on the endpoint, the API answers with either `siteOwner` or
/// `SITE_OWNER`. Emitting one spelling keeps `gsc` output predictable.
pub fn lower_camel(value: &str) -> String {
    if value.chars().any(|c| c.is_ascii_lowercase()) {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    for word in value.split('_').filter(|word| !word.is_empty()) {
        let mut chars = word.chars();
        match chars.next() {
            Some(first) if !out.is_empty() => out.push(first.to_ascii_uppercase()),
            Some(first) => out.push(first.to_ascii_lowercase()),
            None => {}
        }
        out.push_str(&chars.as_str().to_ascii_lowercase());
    }
    out
}

pub struct Client {
    http: reqwest::Client,
    tokens: TokenProvider,
    search_console_url: String,
    indexing_url: String,
    max_retries: u32,
    verbose: bool,
}

impl Client {
    pub fn new(http: reqwest::Client, tokens: TokenProvider, verbose: bool) -> Self {
        Self {
            http,
            tokens,
            search_console_url: base_url(ENV_API_BASE_URL, DEFAULT_SEARCH_CONSOLE_URL),
            indexing_url: base_url(ENV_INDEXING_BASE_URL, DEFAULT_INDEXING_URL),
            max_retries: std::env::var(ENV_MAX_RETRIES)
                .ok()
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(DEFAULT_MAX_RETRIES),
            verbose,
        }
    }

    /// The HTTP client, for unauthenticated requests such as fetching a sitemap.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Send a request and decode the JSON response into `T`.
    pub async fn call<T: DeserializeOwned>(
        &self,
        api: Api,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<T> {
        let value = self.request(api, method, path, query, body).await?;
        serde_json::from_value(value)
            .map_err(|e| Error::internal(format!("unexpected response from {path}: {e}")))
    }

    /// Send a request and return the JSON response.
    ///
    /// `path` is absolute within the API (`/webmasters/v3/sites`). An empty
    /// response body, which Google uses for successful writes, is returned as
    /// [`Value::Null`].
    pub async fn request(
        &self,
        api: Api,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value> {
        let base = match api {
            Api::SearchConsole => &self.search_console_url,
            Api::Indexing => &self.indexing_url,
        };
        let url = format!("{base}{path}");

        let mut attempt = 0;
        loop {
            let outcome = self.send_once(api, &method, &url, query, body).await;
            let (error, retry_after) = match outcome {
                Ok(value) => return Ok(value),
                Err(failure) => failure,
            };
            if attempt >= self.max_retries || !is_transient(&error) {
                return Err(error);
            }
            let delay = retry_after
                .unwrap_or(RETRY_BASE_DELAY * 2u32.pow(attempt))
                .min(RETRY_MAX_DELAY);
            if self.verbose {
                eprintln!("! {} — retrying in {delay:.1?}", error.message);
            }
            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }

    /// One attempt. On failure, also returns how long the server asked us to wait.
    async fn send_once(
        &self,
        api: Api,
        method: &Method,
        url: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value, (Error, Option<Duration>)> {
        let token = self.tokens.access_token(api).await.map_err(|e| (e, None))?;

        let mut request = self
            .http
            .request(method.clone(), url)
            .bearer_auth(token)
            .query(query);
        request = match body {
            Some(body) => request.json(body),
            // Google's front end answers 411 to a body-less PUT or POST that
            // does not declare its length.
            None if *method == Method::PUT || *method == Method::POST => {
                request.header(CONTENT_LENGTH, 0)
            }
            None => request,
        };

        if self.verbose {
            eprintln!("> {method} {url}");
        }
        let started = Instant::now();
        let response = request.send().await.map_err(|e| (Error::from(e), None))?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let bytes = response.bytes().await.map_err(|e| (Error::from(e), None))?;
        if self.verbose {
            eprintln!("< {status} ({} ms)", started.elapsed().as_millis());
        }

        if !status.is_success() {
            let uses_env_token = matches!(
                self.tokens.identity().credentials,
                Credentials::AccessToken(_)
            );
            return Err((api_error(api, status, &bytes, uses_env_token), retry_after));
        }
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|e| {
            (
                Error::internal(format!("Google returned a response that is not JSON: {e}")),
                None,
            )
        })
    }
}

fn base_url(variable: &str, default: &str) -> String {
    std::env::var(variable)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
        .trim_end_matches('/')
        .to_owned()
}

/// Worth retrying: rate limits, server errors, and connection problems.
fn is_transient(error: &Error) -> bool {
    matches!(error.kind, ErrorKind::RateLimit | ErrorKind::Network)
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: GoogleError,
}

#[derive(Deserialize)]
struct GoogleError {
    #[serde(default)]
    message: Option<String>,
    /// Canonical status, e.g. `PERMISSION_DENIED`.
    #[serde(default)]
    status: Option<String>,
    /// Legacy per-error detail; `reason` distinguishes quota from permission.
    #[serde(default)]
    errors: Vec<GoogleErrorDetail>,
}

#[derive(Deserialize)]
struct GoogleErrorDetail {
    #[serde(default)]
    reason: Option<String>,
}

/// Turn a non-2xx response into an [`Error`] with a hint where one is known.
fn api_error(api: Api, status: StatusCode, body: &[u8], uses_env_token: bool) -> Error {
    let parsed = serde_json::from_slice::<ErrorEnvelope>(body)
        .ok()
        .map(|envelope| envelope.error);
    let message = parsed
        .as_ref()
        .and_then(|e| e.message.clone())
        .unwrap_or_else(|| {
            let text = String::from_utf8_lossy(body);
            let snippet: String = text.trim().chars().take(200).collect();
            if snippet.is_empty() {
                format!("Google returned HTTP {status}")
            } else {
                format!("Google returned HTTP {status}: {snippet}")
            }
        });
    let code = parsed.as_ref().and_then(|e| e.status.clone());
    let reasons: Vec<&str> = parsed
        .iter()
        .flat_map(|e| &e.errors)
        .filter_map(|detail| detail.reason.as_deref())
        .collect();

    let quota_reason = reasons.iter().any(|reason| {
        matches!(
            *reason,
            "rateLimitExceeded" | "userRateLimitExceeded" | "quotaExceeded" | "dailyLimitExceeded"
        )
    });
    let kind = match status.as_u16() {
        401 => ErrorKind::Auth,
        429 => ErrorKind::RateLimit,
        403 if quota_reason || code.as_deref() == Some("RESOURCE_EXHAUSTED") => {
            ErrorKind::RateLimit
        }
        500..=599 => ErrorKind::Network,
        _ => ErrorKind::Api,
    };

    let lower = message.to_lowercase();
    let hint = match (kind, status.as_u16()) {
        (ErrorKind::Auth, _) if uses_env_token => Some(format!(
            "the token in ${} is invalid or expired; supply a fresh one",
            crate::auth::ENV_ACCESS_TOKEN
        )),
        (ErrorKind::Auth, _) => Some("run `gsc auth login` to sign in again".to_owned()),
        (ErrorKind::RateLimit, _) => Some(
            "a Google API quota was exhausted; wait before retrying. URL inspection allows \
             2,000 requests per day and 600 per minute for each property"
                .to_owned(),
        ),
        (ErrorKind::Network, _) => {
            Some("Google had a temporary problem; try again shortly".to_owned())
        }
        (_, 403)
            if lower.contains("has not been used in project") || lower.contains("is disabled") =>
        {
            let service = match api {
                Api::SearchConsole => "searchconsole.googleapis.com",
                Api::Indexing => "indexing.googleapis.com",
            };
            Some(format!(
                "enable the API for your Google Cloud project at \
                 https://console.cloud.google.com/apis/library/{service} and retry in a minute"
            ))
        }
        (_, 403) if lower.contains("insufficient authentication scopes") => Some(match api {
            Api::SearchConsole => {
                "the login is read-only; run `gsc auth login` without --readonly".to_owned()
            }
            Api::Indexing => {
                "run `gsc auth login --indexing` to grant Indexing API access".to_owned()
            }
        }),
        (_, 403) => Some(match api {
            Api::SearchConsole => "the signed-in account cannot access this property; run \
                 `gsc sites list` to see the ones it can. A service account must be added as a \
                 user of the property in Search Console"
                .to_owned(),
            Api::Indexing => "the signed-in account must be a verified owner of the site in \
                 Search Console to use the Indexing API"
                .to_owned(),
        }),
        (_, 404) => Some(match api {
            Api::SearchConsole => "check the property and URL are spelled exactly as in \
                 Search Console; run `gsc sites list`"
                .to_owned(),
            Api::Indexing => "Google has no notification on record for this URL; send one \
                 with `gsc indexing publish <url>`"
                .to_owned(),
        }),
        _ => None,
    };

    let mut error = Error::new(kind, message).with_status(status.as_u16());
    if let Some(code) = code {
        error = error.with_code(code);
    }
    if let Some(hint) = hint {
        error = error.with_hint(hint);
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error_for(api: Api, status: u16, body: &str) -> Error {
        api_error(
            api,
            StatusCode::from_u16(status).unwrap(),
            body.as_bytes(),
            false,
        )
    }

    #[test]
    fn encodes_property_identifiers_as_single_path_segments() {
        assert_eq!(
            encode_segment("https://example.com/"),
            "https%3A%2F%2Fexample.com%2F"
        );
        assert_eq!(
            encode_segment("sc-domain:example.com"),
            "sc-domain%3Aexample.com"
        );
        assert_eq!(
            encode_segment("https://example.com/a b?x=1&y=ü"),
            "https%3A%2F%2Fexample.com%2Fa%20b%3Fx%3D1%26y%3D%C3%BC"
        );
        assert_eq!(encode_segment("safe-._~chars"), "safe-._~chars");
    }

    #[test]
    fn enum_values_are_normalised_to_lower_camel_case() {
        assert_eq!(lower_camel("SITE_OWNER"), "siteOwner");
        assert_eq!(lower_camel("siteOwner"), "siteOwner");
        assert_eq!(lower_camel("BY_NEWS_SHOWCASE_PANEL"), "byNewsShowcasePanel");
        assert_eq!(lower_camel("WEB"), "web");
        assert_eq!(lower_camel("sitemap"), "sitemap");
        assert_eq!(lower_camel(""), "");
    }

    #[test]
    fn permission_errors_point_at_property_access() {
        let err = error_for(
            Api::SearchConsole,
            403,
            r#"{"error":{"code":403,"message":"User does not have sufficient permission for site 'https://example.com/'. See also: https://support.google.com/webmasters/answer/2451999.","errors":[{"message":"...","domain":"global","reason":"forbidden"}],"status":"PERMISSION_DENIED"}}"#,
        );
        assert_eq!(err.kind, ErrorKind::Api);
        assert_eq!(err.status, Some(403));
        assert_eq!(err.code.as_deref(), Some("PERMISSION_DENIED"));
        assert!(
            err.message
                .starts_with("User does not have sufficient permission")
        );
        assert!(err.hint.unwrap().contains("gsc sites list"));
    }

    #[test]
    fn disabled_api_points_at_the_console_page_for_that_api() {
        let body = r#"{"error":{"code":403,"message":"Google Search Console API has not been used in project 123 before or it is disabled.","status":"PERMISSION_DENIED"}}"#;
        let search = error_for(Api::SearchConsole, 403, body);
        assert!(
            search
                .hint
                .unwrap()
                .contains("searchconsole.googleapis.com")
        );
        let indexing = error_for(Api::Indexing, 403, body);
        assert!(indexing.hint.unwrap().contains("indexing.googleapis.com"));
    }

    #[test]
    fn insufficient_scope_suggests_the_right_login() {
        let body = r#"{"error":{"code":403,"message":"Request had insufficient authentication scopes.","status":"PERMISSION_DENIED"}}"#;
        assert!(
            error_for(Api::SearchConsole, 403, body)
                .hint
                .unwrap()
                .contains("without --readonly")
        );
        assert!(
            error_for(Api::Indexing, 403, body)
                .hint
                .unwrap()
                .contains("--indexing")
        );
    }

    #[test]
    fn quota_errors_are_rate_limits_whatever_the_status() {
        let too_many = error_for(
            Api::SearchConsole,
            429,
            r#"{"error":{"code":429,"message":"Quota exceeded for quota metric 'Queries'","status":"RESOURCE_EXHAUSTED"}}"#,
        );
        assert_eq!(too_many.kind, ErrorKind::RateLimit);
        assert_eq!(too_many.exit_code(), 5);

        let legacy = error_for(
            Api::SearchConsole,
            403,
            r#"{"error":{"code":403,"message":"Quota Exceeded","errors":[{"reason":"quotaExceeded"}]}}"#,
        );
        assert_eq!(legacy.kind, ErrorKind::RateLimit);
        assert!(is_transient(&legacy));
    }

    #[test]
    fn unauthorized_hint_depends_on_where_the_token_came_from() {
        let body =
            r#"{"error":{"code":401,"message":"Invalid Credentials","status":"UNAUTHENTICATED"}}"#;
        let stored = api_error(
            Api::SearchConsole,
            StatusCode::UNAUTHORIZED,
            body.as_bytes(),
            false,
        );
        assert_eq!(stored.kind, ErrorKind::Auth);
        assert!(stored.hint.unwrap().contains("gsc auth login"));

        let env = api_error(
            Api::SearchConsole,
            StatusCode::UNAUTHORIZED,
            body.as_bytes(),
            true,
        );
        assert!(env.hint.unwrap().contains("$GSC_ACCESS_TOKEN"));
    }

    #[test]
    fn server_errors_are_transient_and_bad_requests_are_not() {
        let unavailable = error_for(Api::SearchConsole, 503, "Service Unavailable");
        assert_eq!(unavailable.kind, ErrorKind::Network);
        assert!(is_transient(&unavailable));
        assert!(unavailable.message.contains("503"));
        assert!(unavailable.message.contains("Service Unavailable"));

        let bad = error_for(
            Api::SearchConsole,
            400,
            r#"{"error":{"code":400,"message":"Invalid value 'bogus'.","status":"INVALID_ARGUMENT"}}"#,
        );
        assert_eq!(bad.kind, ErrorKind::Api);
        assert!(!is_transient(&bad));
        assert_eq!(
            bad.hint, None,
            "Google's message already says what is wrong"
        );
    }

    #[test]
    fn non_json_error_bodies_are_truncated() {
        let err = error_for(Api::SearchConsole, 404, &"x".repeat(1000));
        assert!(err.message.len() < 300);
        assert_eq!(
            error_for(Api::SearchConsole, 404, "").message,
            "Google returned HTTP 404 Not Found"
        );
    }
}
