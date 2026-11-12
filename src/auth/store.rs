//! On-disk credentials and the access-token cache.
//!
//! Two files live in the config directory, both readable only by the owner:
//!
//! * `credentials.json` — the long-lived credentials saved by `gsc auth login`.
//! * `token_cache.json` — short-lived access tokens, so that consecutive
//!   `gsc` invocations do not each pay for a token exchange.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::AuthorizedUser;
use crate::error::{Error, Result};
use crate::fsutil;

const CREDENTIALS_FILE: &str = "credentials.json";
const TOKEN_CACHE_FILE: &str = "token_cache.json";

/// Refuse to use a token that is about to expire mid-request.
const EXPIRY_MARGIN_SECS: i64 = 60;

/// What `gsc auth login` saved.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoredCredentials {
    /// A refresh token from the browser login flow.
    AuthorizedUser(AuthorizedUser),
}

pub fn credentials_path(config_dir: &Path) -> PathBuf {
    config_dir.join(CREDENTIALS_FILE)
}

pub fn load_credentials(config_dir: &Path) -> Result<Option<StoredCredentials>> {
    let path = credentials_path(config_dir);
    let Some(text) = fsutil::read_optional(&path)? else {
        return Ok(None);
    };
    serde_json::from_str(&text).map(Some).map_err(|e| {
        Error::auth(format!(
            "stored credentials at {} are unreadable: {e}",
            path.display()
        ))
        .with_hint("run `gsc auth login` to sign in again")
    })
}

pub fn save_credentials(config_dir: &Path, credentials: &StoredCredentials) -> Result<()> {
    let text = serde_json::to_string_pretty(credentials)?;
    fsutil::write_private(&credentials_path(config_dir), text.as_bytes())?;
    // Tokens cached for a previous identity must not outlive it.
    clear_token_cache(config_dir)
}

/// Delete stored credentials and cached tokens. Returns whether anything was stored.
pub fn clear(config_dir: &Path) -> Result<bool> {
    let had_credentials = fsutil::remove_if_exists(&credentials_path(config_dir))?;
    clear_token_cache(config_dir)?;
    Ok(had_credentials)
}

fn clear_token_cache(config_dir: &Path) -> Result<()> {
    fsutil::remove_if_exists(&config_dir.join(TOKEN_CACHE_FILE)).map(|_| ())
}

/// A bearer token and the Unix time at which it stops working.
#[derive(Clone, Serialize, Deserialize)]
pub struct CachedToken {
    pub access_token: String,
    pub expires_at: i64,
}

impl CachedToken {
    pub fn is_fresh(&self, now: i64) -> bool {
        self.expires_at - now > EXPIRY_MARGIN_SECS
    }
}

#[derive(Default, Serialize, Deserialize)]
struct TokenCacheFile {
    #[serde(default)]
    tokens: BTreeMap<String, CachedToken>,
}

/// Look up a still-valid token. The cache is an optimisation: any problem
/// reading it is treated as a miss.
pub fn cached_token(config_dir: &Path, key: &str, now: i64) -> Option<CachedToken> {
    read_cache(config_dir)
        .tokens
        .remove(key)
        .filter(|token| token.is_fresh(now))
}

/// Remember a token for later invocations, dropping any that have expired.
pub fn cache_token(config_dir: &Path, key: &str, token: &CachedToken, now: i64) -> Result<()> {
    let mut cache = read_cache(config_dir);
    cache.tokens.retain(|_, cached| cached.is_fresh(now));
    cache.tokens.insert(key.to_owned(), token.clone());
    let text = serde_json::to_string(&cache)?;
    fsutil::write_private(&config_dir.join(TOKEN_CACHE_FILE), text.as_bytes())
}

fn read_cache(config_dir: &Path) -> TokenCacheFile {
    fsutil::read_optional(&config_dir.join(TOKEN_CACHE_FILE))
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user() -> AuthorizedUser {
        AuthorizedUser {
            client_id: "id.apps.googleusercontent.com".into(),
            client_secret: "secret".into(),
            refresh_token: "refresh".into(),
            token_uri: super::super::DEFAULT_TOKEN_URI.into(),
            scopes: vec![super::super::SCOPE_WEBMASTERS.into()],
        }
    }

    fn token(expires_at: i64) -> CachedToken {
        CachedToken {
            access_token: format!("token-{expires_at}"),
            expires_at,
        }
    }

    #[test]
    fn credentials_round_trip_with_a_type_tag() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_credentials(tmp.path()).unwrap().is_none());

        save_credentials(tmp.path(), &StoredCredentials::AuthorizedUser(user())).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(credentials_path(tmp.path())).unwrap())
                .unwrap();
        assert_eq!(raw["type"], "authorized_user");

        let Some(StoredCredentials::AuthorizedUser(loaded)) = load_credentials(tmp.path()).unwrap()
        else {
            panic!("expected stored credentials");
        };
        assert_eq!(loaded.refresh_token, "refresh");
    }

    #[test]
    fn corrupt_credentials_are_an_auth_error() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(credentials_path(tmp.path()), "not json").unwrap();
        let err = load_credentials(tmp.path()).err().unwrap();
        assert_eq!(err.kind, crate::error::ErrorKind::Auth);
    }

    #[test]
    fn cache_returns_only_fresh_tokens() {
        let tmp = tempfile::tempdir().unwrap();
        let now = 1_000_000;
        cache_token(tmp.path(), "fresh", &token(now + 3600), now).unwrap();
        cache_token(tmp.path(), "stale", &token(now + 30), now).unwrap();

        assert_eq!(
            cached_token(tmp.path(), "fresh", now).unwrap().access_token,
            format!("token-{}", now + 3600)
        );
        assert!(
            cached_token(tmp.path(), "stale", now).is_none(),
            "tokens inside the expiry margin are not handed out"
        );
        assert!(cached_token(tmp.path(), "missing", now).is_none());
    }

    #[test]
    fn expired_entries_are_pruned_on_write() {
        let tmp = tempfile::tempdir().unwrap();
        cache_token(tmp.path(), "old", &token(2000), 1000).unwrap();
        cache_token(tmp.path(), "new", &token(9000), 5000).unwrap();
        let cache = read_cache(tmp.path());
        assert_eq!(cache.tokens.keys().collect::<Vec<_>>(), ["new"]);
    }

    #[test]
    fn saving_credentials_or_clearing_drops_cached_tokens() {
        let tmp = tempfile::tempdir().unwrap();
        let now = 1000;
        cache_token(tmp.path(), "k", &token(now + 3600), now).unwrap();
        save_credentials(tmp.path(), &StoredCredentials::AuthorizedUser(user())).unwrap();
        assert!(cached_token(tmp.path(), "k", now).is_none());

        cache_token(tmp.path(), "k", &token(now + 3600), now).unwrap();
        assert!(clear(tmp.path()).unwrap());
        assert!(cached_token(tmp.path(), "k", now).is_none());
        assert!(!clear(tmp.path()).unwrap(), "nothing left to remove");
    }

    #[test]
    fn unreadable_cache_is_a_miss_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(TOKEN_CACHE_FILE), "garbage").unwrap();
        assert!(cached_token(tmp.path(), "k", 0).is_none());
        cache_token(tmp.path(), "k", &token(5000), 0).unwrap();
        assert!(cached_token(tmp.path(), "k", 0).is_some());
    }
}
