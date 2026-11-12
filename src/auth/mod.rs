//! Authentication: finding credentials and turning them into access tokens.
//!
//! Credentials are looked up in a fixed order (see [`resolve`]). Whatever is
//! found is wrapped in a [`TokenProvider`], which hands out bearer tokens and
//! caches them in memory and on disk.

pub mod oauth;
pub mod service_account;
pub mod store;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aws_lc_rs::digest;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::{Error, ErrorKind, Result};
use service_account::ServiceAccountKey;
use store::{CachedToken, StoredCredentials};

/// Read and write access to Search Console.
pub const SCOPE_WEBMASTERS: &str = "https://www.googleapis.com/auth/webmasters";
/// Read-only access to Search Console.
pub const SCOPE_WEBMASTERS_READONLY: &str = "https://www.googleapis.com/auth/webmasters.readonly";
/// Access to the Indexing API.
pub const SCOPE_INDEXING: &str = "https://www.googleapis.com/auth/indexing";

pub const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

/// A ready-made bearer token. Takes precedence over everything else.
pub const ENV_ACCESS_TOKEN: &str = "GSC_ACCESS_TOKEN";
/// Path to a service-account key or `authorized_user` file, for `gsc` only.
pub const ENV_CREDENTIALS_FILE: &str = "GSC_CREDENTIALS_FILE";
/// Google's standard variable for the same thing, shared with other tools.
pub const ENV_GOOGLE_APPLICATION_CREDENTIALS: &str = "GOOGLE_APPLICATION_CREDENTIALS";

/// Lifetime assumed for a token when Google does not state one.
const DEFAULT_TOKEN_LIFETIME_SECS: i64 = 3600;

/// The Google API a token will be sent to. Determines the scope it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    SearchConsole,
    Indexing,
}

impl Api {
    /// The scope a service account requests for this API. Unlike a user
    /// login, a service account picks its scope on every token request.
    fn service_account_scope(self) -> &'static str {
        match self {
            Api::SearchConsole => SCOPE_WEBMASTERS,
            Api::Indexing => SCOPE_INDEXING,
        }
    }
}

/// OAuth credentials for an end user: a client plus a refresh token.
///
/// The field names match Google's `authorized_user` credential files, so a
/// file written by other Google tooling can be read as-is.
#[derive(Clone, Serialize, Deserialize)]
pub struct AuthorizedUser {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
    #[serde(default = "default_token_uri")]
    pub token_uri: String,
    /// Scopes the user granted. Empty when unknown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

fn default_token_uri() -> String {
    DEFAULT_TOKEN_URI.to_owned()
}

/// Secrets are never printed, even in debug output.
impl fmt::Debug for AuthorizedUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorizedUser")
            .field("client_id", &self.client_id)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

pub enum Credentials {
    /// A bearer token supplied directly; used as-is and never refreshed.
    AccessToken(String),
    AuthorizedUser(AuthorizedUser),
    ServiceAccount(ServiceAccountKey),
}

/// Where the active credentials were found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// An environment variable holding the credential itself.
    Env(&'static str),
    /// The file written by `gsc auth login`.
    Stored(PathBuf),
    /// A credential file that something else points at: an environment
    /// variable, or a saved `gsc auth login --service-account`.
    File { path: PathBuf, named_by: String },
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Env(name) => write!(f, "${name}"),
            Source::Stored(path) => write!(f, "{}", path.display()),
            Source::File { path, named_by } => write!(f, "{} (from {named_by})", path.display()),
        }
    }
}

/// The credentials `gsc` will use, and where they came from.
pub struct Identity {
    pub credentials: Credentials,
    pub source: Source,
}

impl Identity {
    /// Short machine-readable name of the authentication method.
    pub fn method(&self) -> &'static str {
        match self.credentials {
            Credentials::AccessToken(_) => "access_token",
            Credentials::AuthorizedUser(_) => "oauth",
            Credentials::ServiceAccount(_) => "service_account",
        }
    }

    /// The non-secret identifier of the credential, if it has one.
    pub fn account(&self) -> Option<&str> {
        match &self.credentials {
            Credentials::AccessToken(_) => None,
            Credentials::AuthorizedUser(user) => Some(&user.client_id),
            Credentials::ServiceAccount(account) => Some(&account.client_email),
        }
    }

    /// Scopes fixed at login time. Empty when unknown, and for service
    /// accounts, which request a scope per call instead.
    pub fn scopes(&self) -> Vec<String> {
        match &self.credentials {
            Credentials::AccessToken(_) | Credentials::ServiceAccount(_) => Vec::new(),
            Credentials::AuthorizedUser(user) => user.scopes.clone(),
        }
    }
}

/// Find the credentials to use, or `None` if the user has not logged in.
///
/// Order of precedence:
///
/// 1. `$GSC_ACCESS_TOKEN` — a bearer token used as-is.
/// 2. `$GSC_CREDENTIALS_FILE` — a service-account key or `authorized_user` file.
/// 3. Credentials saved by `gsc auth login`.
/// 4. `$GOOGLE_APPLICATION_CREDENTIALS` — the same kind of file as (2).
///
/// The generic Google variable comes last on purpose: it is often set for
/// unrelated cloud work, and should not silently override an explicit
/// `gsc auth login`.
pub fn resolve(config_dir: &Path) -> Result<Option<Identity>> {
    if let Some(token) = non_empty_env(ENV_ACCESS_TOKEN) {
        return Ok(Some(Identity {
            credentials: Credentials::AccessToken(token),
            source: Source::Env(ENV_ACCESS_TOKEN),
        }));
    }

    if let Some(identity) = from_env_file(ENV_CREDENTIALS_FILE)? {
        return Ok(Some(identity));
    }

    if let Some(stored) = store::load_credentials(config_dir)? {
        return Ok(Some(match stored {
            StoredCredentials::AuthorizedUser(user) => Identity {
                credentials: Credentials::AuthorizedUser(user),
                source: Source::Stored(store::credentials_path(config_dir)),
            },
            StoredCredentials::ServiceAccountFile { path } => Identity {
                credentials: Credentials::ServiceAccount(ServiceAccountKey::from_file(&path)?),
                source: Source::File {
                    path,
                    named_by: "gsc auth login".to_owned(),
                },
            },
        }));
    }

    from_env_file(ENV_GOOGLE_APPLICATION_CREDENTIALS)
}

fn from_env_file(variable: &'static str) -> Result<Option<Identity>> {
    let Some(path) = non_empty_env(variable).map(PathBuf::from) else {
        return Ok(None);
    };
    Ok(Some(Identity {
        credentials: load_credential_file(&path)?,
        source: Source::File {
            path,
            named_by: format!("${variable}"),
        },
    }))
}

/// Read a Google credential file of either supported type.
pub fn load_credential_file(path: &Path) -> Result<Credentials> {
    let unusable = |reason: String| {
        Error::auth(format!(
            "cannot use {} as credentials: {reason}",
            path.display()
        ))
    };
    let text = std::fs::read_to_string(path).map_err(|e| unusable(e.to_string()))?;
    let document: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| unusable(format!("not valid JSON ({e})")))?;

    match document.get("type").and_then(|t| t.as_str()) {
        Some("service_account") => {
            ServiceAccountKey::from_file(path).map(Credentials::ServiceAccount)
        }
        Some("authorized_user") => serde_json::from_value(document)
            .map(Credentials::AuthorizedUser)
            .map_err(|e| unusable(e.to_string())),
        Some(other) => Err(
            unusable(format!("credential type `{other}` is not supported"))
                .with_hint("use a service-account key or an authorized_user file"),
        ),
        None if document.get("installed").is_some() || document.get("web").is_some() => Err(
            unusable("it is an OAuth client file, not a credential".to_owned())
                .with_hint("sign in with `gsc auth login --client-secret-file <path>` instead"),
        ),
        None => Err(unusable("it has no `type` field".to_owned())),
    }
}

pub fn not_logged_in() -> Error {
    Error::auth("not logged in").with_hint(
        "run `gsc auth login --client-secret-file <path>` to sign in with a browser, \
         or `gsc auth login --service-account <key.json>`; see `gsc auth login --help`",
    )
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Hands out access tokens for an [`Identity`], refreshing and caching as needed.
pub struct TokenProvider {
    identity: Identity,
    http: reqwest::Client,
    config_dir: PathBuf,
    /// Tokens obtained by this process. Holding the lock across a refresh also
    /// stops concurrent requests from each starting their own.
    memo: Mutex<HashMap<String, CachedToken>>,
}

impl TokenProvider {
    pub fn new(identity: Identity, http: reqwest::Client, config_dir: PathBuf) -> Self {
        Self {
            identity,
            http,
            config_dir,
            memo: Mutex::new(HashMap::new()),
        }
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// A bearer token valid for `api`.
    pub async fn access_token(&self, api: Api) -> Result<String> {
        Ok(self.token(api).await?.access_token)
    }

    /// Like [`Self::access_token`], but also reports when the token expires.
    /// The expiry is `None` for tokens supplied through the environment.
    pub async fn token(&self, api: Api) -> Result<Token> {
        let renewable = match &self.identity.credentials {
            Credentials::AccessToken(token) => {
                return Ok(Token {
                    access_token: token.clone(),
                    expires_at: None,
                });
            }
            Credentials::AuthorizedUser(user) => {
                ensure_scope(user, api)?;
                Renewable::User(user)
            }
            Credentials::ServiceAccount(account) => Renewable::ServiceAccount(account),
        };
        let key = renewable.cache_key(api);

        let mut memo = self.memo.lock().await;
        let now = unix_now();
        if let Some(hit) = memo.get(&key).filter(|t| t.is_fresh(now)) {
            return Ok(hit.clone().into());
        }
        if let Some(hit) = store::cached_token(&self.config_dir, &key, now) {
            memo.insert(key, hit.clone());
            return Ok(hit.into());
        }

        let fresh = renewable.mint(&self.http, api).await?;
        // Failing to persist the cache only costs a token exchange next time.
        let _ = store::cache_token(&self.config_dir, &key, &fresh, now);
        memo.insert(key, fresh.clone());
        Ok(fresh.into())
    }

    /// Seed the on-disk cache with a token obtained during login.
    pub fn remember(config_dir: &Path, credentials: &Credentials, api: Api, token: &CachedToken) {
        let renewable = match credentials {
            Credentials::AccessToken(_) => return,
            Credentials::AuthorizedUser(user) => Renewable::User(user),
            Credentials::ServiceAccount(account) => Renewable::ServiceAccount(account),
        };
        let _ = store::cache_token(config_dir, &renewable.cache_key(api), token, unix_now());
    }
}

/// Credentials that can mint a new access token on demand.
enum Renewable<'a> {
    User(&'a AuthorizedUser),
    ServiceAccount(&'a ServiceAccountKey),
}

impl Renewable<'_> {
    /// A user's token covers every scope they granted, so one cache entry
    /// serves all APIs. A service-account token is minted per scope.
    fn cache_key(&self, api: Api) -> String {
        match self {
            Renewable::User(user) => cache_key(&[&user.client_id, &user.refresh_token]),
            Renewable::ServiceAccount(account) => cache_key(&[
                &account.client_email,
                account.private_key_id.as_deref().unwrap_or_default(),
                api.service_account_scope(),
            ]),
        }
    }

    async fn mint(&self, http: &reqwest::Client, api: Api) -> Result<CachedToken> {
        match self {
            Renewable::User(user) => oauth::refresh(http, user).await,
            Renewable::ServiceAccount(account) => {
                service_account::fetch_token(http, account, &[api.service_account_scope()]).await
            }
        }
    }
}

pub struct Token {
    pub access_token: String,
    /// Unix time at which the token stops working, when known.
    pub expires_at: Option<i64>,
}

impl From<CachedToken> for Token {
    fn from(cached: CachedToken) -> Self {
        Self {
            access_token: cached.access_token,
            expires_at: Some(cached.expires_at),
        }
    }
}

/// Fail early, with a useful hint, when the stored grant cannot reach `api`.
fn ensure_scope(user: &AuthorizedUser, api: Api) -> Result<()> {
    if user.scopes.is_empty() {
        // Unknown scopes (e.g. a credential file from other tooling): let Google decide.
        return Ok(());
    }
    let has = |scope: &str| user.scopes.iter().any(|s| s == scope);
    match api {
        Api::SearchConsole if has(SCOPE_WEBMASTERS) || has(SCOPE_WEBMASTERS_READONLY) => Ok(()),
        Api::SearchConsole => Err(Error::auth(
            "the stored login does not include Search Console access",
        )
        .with_hint("run `gsc auth login` and approve the Search Console permission")),
        Api::Indexing if has(SCOPE_INDEXING) => Ok(()),
        Api::Indexing => Err(
            Error::auth("the stored login does not include Indexing API access")
                .with_hint("run `gsc auth login --indexing` to add it"),
        ),
    }
}

/// Stable, non-reversible key for the token cache.
fn cache_key(parts: &[&str]) -> String {
    let hash = digest::digest(&digest::SHA256, parts.join("\n").as_bytes());
    hash.as_ref()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Successful response from an OAuth token endpoint.
#[derive(Deserialize)]
pub(crate) struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Space-separated scopes actually granted.
    #[serde(default)]
    pub scope: Option<String>,
}

impl TokenResponse {
    pub fn to_cached(&self) -> CachedToken {
        CachedToken {
            access_token: self.access_token.clone(),
            expires_at: unix_now() + self.expires_in.unwrap_or(DEFAULT_TOKEN_LIFETIME_SECS),
        }
    }
}

#[derive(Deserialize)]
struct TokenErrorBody {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// POST a form to an OAuth token endpoint.
///
/// On failure the returned error carries the OAuth error code (for example
/// `invalid_grant`) in [`Error::code`] so callers can attach a specific hint.
pub(crate) async fn token_request(
    http: &reqwest::Client,
    token_uri: &str,
    form: &[(&str, &str)],
) -> Result<TokenResponse> {
    let response = http.post(token_uri).form(form).send().await?;
    let status = response.status();
    let body = response.bytes().await?;

    if status.is_success() {
        return serde_json::from_slice(&body).map_err(|e| {
            Error::internal(format!("unexpected response from the token endpoint: {e}"))
        });
    }

    let error = match serde_json::from_slice::<TokenErrorBody>(&body) {
        Ok(parsed) => {
            let message = match parsed.error_description {
                Some(description) => format!(
                    "Google rejected the credentials: {description} ({})",
                    parsed.error
                ),
                None => format!("Google rejected the credentials: {}", parsed.error),
            };
            Error::auth(message).with_code(parsed.error)
        }
        Err(_) if status.is_server_error() => Error::new(
            ErrorKind::Network,
            format!("the token endpoint returned HTTP {status}"),
        )
        .with_hint("this is usually temporary; try again shortly"),
        Err(_) => Error::auth(format!("the token endpoint returned HTTP {status}")),
    };
    Err(error.with_status(status.as_u16()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(scopes: &[&str]) -> AuthorizedUser {
        AuthorizedUser {
            client_id: "client".into(),
            client_secret: "secret".into(),
            refresh_token: "refresh".into(),
            token_uri: DEFAULT_TOKEN_URI.into(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn debug_output_redacts_secrets() {
        let text = format!("{:?}", user(&[SCOPE_WEBMASTERS]));
        assert!(text.contains("client"));
        assert!(!text.contains("secret"));
        assert!(!text.contains("refresh"));
    }

    #[test]
    fn scope_checks_match_each_api() {
        let full = user(&[SCOPE_WEBMASTERS]);
        let readonly = user(&[SCOPE_WEBMASTERS_READONLY]);
        let both = user(&[SCOPE_WEBMASTERS, SCOPE_INDEXING]);
        let unknown = user(&[]);

        assert!(ensure_scope(&full, Api::SearchConsole).is_ok());
        assert!(ensure_scope(&readonly, Api::SearchConsole).is_ok());
        assert!(ensure_scope(&both, Api::Indexing).is_ok());
        assert!(ensure_scope(&unknown, Api::Indexing).is_ok());

        let err = ensure_scope(&full, Api::Indexing).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Auth);
        assert!(err.hint.unwrap().contains("--indexing"));

        let err = ensure_scope(&user(&[SCOPE_INDEXING]), Api::SearchConsole).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Auth);
    }

    #[test]
    fn cache_key_is_deterministic_and_hides_its_inputs() {
        let a = cache_key(&["client", "refresh-token"]);
        assert_eq!(a, cache_key(&["client", "refresh-token"]));
        assert_ne!(a, cache_key(&["client", "other-token"]));
        assert_eq!(a.len(), 32);
        assert!(!a.contains("refresh"));
    }

    #[test]
    fn authorized_user_files_default_the_token_uri() {
        let parsed: AuthorizedUser = serde_json::from_str(
            r#"{"client_id":"c","client_secret":"s","refresh_token":"r","type":"authorized_user"}"#,
        )
        .unwrap();
        assert_eq!(parsed.token_uri, DEFAULT_TOKEN_URI);
        assert!(parsed.scopes.is_empty());
    }
}
