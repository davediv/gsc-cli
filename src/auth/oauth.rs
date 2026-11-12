//! OAuth 2.0 for installed applications.
//!
//! `gsc auth login` runs the authorization-code flow with PKCE (RFC 7636)
//! and a loopback redirect (RFC 8252): it listens on `127.0.0.1`, sends the
//! user to Google's consent page, and receives the authorization code when
//! Google redirects the browser back.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use aws_lc_rs::{digest, rand};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::Instant;
use url::Url;

use super::store::CachedToken;
use super::{
    AuthorizedUser, DEFAULT_TOKEN_URI, SCOPE_WEBMASTERS, SCOPE_WEBMASTERS_READONLY, token_request,
};
use crate::error::{Error, ErrorKind, Result};

pub const DEFAULT_AUTH_URI: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const REVOKE_URI: &str = "https://oauth2.googleapis.com/revoke";

/// How long to wait for the user to finish in the browser.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);
/// How long one connection may take, in total, to send its request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REQUEST_BYTES: usize = 16 * 1024;

/// An OAuth client registered in a Google Cloud project.
#[derive(Clone)]
pub struct ClientConfig {
    pub client_id: String,
    pub client_secret: String,
    pub auth_uri: String,
    pub token_uri: String,
}

impl ClientConfig {
    pub fn new(client_id: String, client_secret: String) -> Self {
        Self {
            client_id,
            client_secret,
            auth_uri: DEFAULT_AUTH_URI.to_owned(),
            token_uri: DEFAULT_TOKEN_URI.to_owned(),
        }
    }

    /// Parse the JSON file downloaded from the Google Cloud Console.
    pub fn from_secret_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::io(format!("reading {}", path.display()), e))?;
        Self::from_secret_json(&text)
            .map_err(|message| Error::usage(format!("{}: {message}", path.display())))
    }

    fn from_secret_json(text: &str) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct SecretFile {
            installed: Option<SecretClient>,
            web: Option<SecretClient>,
            #[serde(rename = "type")]
            kind: Option<String>,
        }
        #[derive(Deserialize)]
        struct SecretClient {
            client_id: String,
            client_secret: String,
            auth_uri: Option<String>,
            token_uri: Option<String>,
        }

        let file: SecretFile =
            serde_json::from_str(text).map_err(|e| format!("not a valid JSON file ({e})"))?;
        let Some(client) = file.installed.or(file.web) else {
            return Err(match file.kind.as_deref() {
                Some("service_account") => {
                    "this is a service-account key, not an OAuth client file".to_owned()
                }
                _ => "not an OAuth client file; expected the JSON downloaded for a \
                      \"Desktop app\" OAuth client ID"
                    .to_owned(),
            });
        };
        Ok(Self {
            client_id: client.client_id,
            client_secret: client.client_secret,
            auth_uri: client
                .auth_uri
                .unwrap_or_else(|| DEFAULT_AUTH_URI.to_owned()),
            token_uri: client
                .token_uri
                .unwrap_or_else(|| DEFAULT_TOKEN_URI.to_owned()),
        })
    }
}

/// PKCE verifier and its S256 challenge.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn generate() -> Result<Self> {
        let verifier = random_token()?;
        let challenge = Self::challenge_for(&verifier);
        Ok(Self {
            verifier,
            challenge,
        })
    }

    /// `BASE64URL(SHA256(verifier))`, as defined by RFC 7636 §4.2.
    pub fn challenge_for(verifier: &str) -> String {
        URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, verifier.as_bytes()))
    }
}

/// 32 random bytes as 43 URL-safe characters.
fn random_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes)
        .map_err(|_| Error::internal("the system random number generator failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The consent-page URL the user must visit.
pub fn authorization_url(
    client: &ClientConfig,
    redirect_uri: &str,
    scopes: &[&str],
    state: &str,
    code_challenge: &str,
) -> Result<String> {
    let mut url = Url::parse(&client.auth_uri)
        .map_err(|e| Error::usage(format!("invalid auth_uri `{}`: {e}", client.auth_uri)))?;
    url.query_pairs_mut()
        .append_pair("client_id", &client.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", &scopes.join(" "))
        .append_pair("state", state)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256")
        // Ask for a refresh token, and force the consent page so that one is
        // issued even if the user has authorised this client before.
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent");
    Ok(url.into())
}

pub struct LoginRequest<'a> {
    pub client: &'a ClientConfig,
    pub scopes: &'a [&'a str],
    /// Loopback port to listen on; `0` picks a free one.
    pub port: u16,
    pub open_browser: bool,
}

/// The outcome of a successful login.
pub struct Grant {
    pub user: AuthorizedUser,
    pub access_token: CachedToken,
}

/// Run the browser login flow to completion.
///
/// Progress and the sign-in URL are written to stderr.
pub async fn login(http: &reqwest::Client, request: LoginRequest<'_>) -> Result<Grant> {
    let listener = TcpListener::bind(("127.0.0.1", request.port))
        .await
        .map_err(|e| {
            Error::io(
                format!(
                    "listening on 127.0.0.1:{} for the sign-in redirect",
                    request.port
                ),
                e,
            )
            .with_hint("pass a different --port, or omit it to pick a free one")
        })?;
    let port = listener
        .local_addr()
        .map_err(|e| Error::io("reading the listening port", e))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}");

    let pkce = Pkce::generate()?;
    let state = random_token()?;
    let url = authorization_url(
        request.client,
        &redirect_uri,
        request.scopes,
        &state,
        &pkce.challenge,
    )?;

    if request.open_browser {
        eprintln!("Opening your browser to sign in with Google.");
        eprintln!("If it does not open, visit this URL:\n");
    } else {
        eprintln!("Open this URL in a browser on this machine to sign in with Google:\n");
    }
    eprintln!("  {url}\n");
    if request.open_browser && open::that(&url).is_err() {
        eprintln!("(Could not launch a browser; open the URL above manually.)");
    }
    eprintln!("Waiting for you to approve access (Ctrl+C to cancel)...");

    let code = wait_for_callback(listener, &state).await?;
    let tokens = exchange_code(http, request.client, &code, &pkce.verifier, &redirect_uri).await?;

    let refresh_token = tokens.refresh_token.clone().ok_or_else(|| {
        Error::auth("Google did not return a refresh token").with_hint(
            "remove this app's access at https://myaccount.google.com/permissions, \
             then run `gsc auth login` again",
        )
    })?;

    // With granular consent the user can untick permissions, so trust what
    // Google says was granted rather than what was requested.
    let scopes: Vec<String> = match &tokens.scope {
        Some(granted) => granted.split_whitespace().map(str::to_owned).collect(),
        None => request.scopes.iter().map(|s| s.to_string()).collect(),
    };
    if !scopes
        .iter()
        .any(|s| s == SCOPE_WEBMASTERS || s == SCOPE_WEBMASTERS_READONLY)
    {
        return Err(Error::auth("Search Console access was not granted")
            .with_hint("run `gsc auth login` again and tick the Search Console permission"));
    }

    Ok(Grant {
        access_token: tokens.to_cached(),
        user: AuthorizedUser {
            client_id: request.client.client_id.clone(),
            client_secret: request.client.client_secret.clone(),
            refresh_token,
            token_uri: request.client.token_uri.clone(),
            scopes,
        },
    })
}

async fn exchange_code(
    http: &reqwest::Client,
    client: &ClientConfig,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<super::TokenResponse> {
    token_request(
        http,
        &client.token_uri,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", code_verifier),
            ("client_id", &client.client_id),
            ("client_secret", &client.client_secret),
            ("redirect_uri", redirect_uri),
        ],
    )
    .await
    .map_err(|e| match e.code.as_deref() {
        Some("invalid_client") => e.with_hint(
            "check the OAuth client ID and secret; the client must be of type \"Desktop app\"",
        ),
        _ => e,
    })
}

/// Trade the stored refresh token for a new access token.
pub async fn refresh(http: &reqwest::Client, user: &AuthorizedUser) -> Result<CachedToken> {
    let response = token_request(
        http,
        &user.token_uri,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &user.refresh_token),
            ("client_id", &user.client_id),
            ("client_secret", &user.client_secret),
        ],
    )
    .await
    .map_err(|e| match e.code.as_deref() {
        // Revoked, expired (7 days for apps in "Testing"), or password changed.
        Some("invalid_grant") => e.with_hint(
            "the saved login has expired or was revoked; run `gsc auth login` to sign in again",
        ),
        Some("invalid_client") => {
            e.with_hint("the OAuth client was deleted or its secret changed; run `gsc auth login` with the new client file")
        }
        _ => e,
    })?;
    Ok(response.to_cached())
}

/// Invalidate a refresh token at Google.
///
/// This ends the grant for every device that signed in with the same OAuth
/// client and Google account, which is why logout does not do it by default.
pub async fn revoke(http: &reqwest::Client, refresh_token: &str) -> Result<()> {
    let response = http
        .post(REVOKE_URI)
        .form(&[("token", refresh_token)])
        .send()
        .await?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::Auth,
            format!(
                "Google refused to revoke the token (HTTP {})",
                response.status()
            ),
        ))
    }
}

/// What a request to the loopback listener turned out to be.
#[derive(Debug, PartialEq, Eq)]
enum Callback {
    /// Google redirected back with an authorization code.
    Code(String),
    /// Google redirected back with an error, e.g. `access_denied`.
    Denied(String),
    /// Something else: a favicon request, a stale tab, or a forged request.
    Unrelated,
}

/// Classify the target of a `GET` request received on the loopback port.
///
/// Requests whose `state` does not match are ignored rather than treated as
/// failures, so another local process cannot abort or hijack a login.
fn classify_callback(request_target: &str, expected_state: &str) -> Callback {
    let Ok(url) = Url::parse(&format!("http://127.0.0.1{request_target}")) else {
        return Callback::Unrelated;
    };
    if url.path() != "/" {
        return Callback::Unrelated;
    }
    let param = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    if param("state").as_deref() != Some(expected_state) {
        return Callback::Unrelated;
    }
    if let Some(error) = param("error") {
        return Callback::Denied(error);
    }
    match param("code") {
        Some(code) if !code.is_empty() => Callback::Code(code),
        _ => Callback::Unrelated,
    }
}

/// Wait for Google to redirect the browser back with an authorization code.
///
/// Each connection is served in its own task and reports back only if it
/// carried the real redirect. A connection that stalls, whether a browser
/// preconnect or another local process holding the port open, therefore
/// cannot delay the one that matters.
async fn wait_for_callback(listener: TcpListener, expected_state: &str) -> Result<String> {
    let deadline = Instant::now() + CALLBACK_TIMEOUT;
    let expected_state: Arc<str> = expected_state.into();
    let (outcome_tx, mut outcome_rx) = mpsc::channel::<Result<String>>(1);

    loop {
        tokio::select! {
            Some(outcome) = outcome_rx.recv() => return outcome,
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    tokio::spawn(serve_connection(
                        stream,
                        expected_state.clone(),
                        outcome_tx.clone(),
                    ));
                }
            }
            () = tokio::time::sleep_until(deadline) => {
                return Err(
                    Error::auth("timed out waiting for the browser sign-in to finish")
                        .with_hint("run `gsc auth login` again"),
                );
            }
        }
    }
}

/// Answer one connection to the loopback port, reporting the login's outcome
/// if this was the redirect from Google.
async fn serve_connection(
    mut stream: TcpStream,
    expected_state: Arc<str>,
    outcome: mpsc::Sender<Result<String>>,
) {
    let request = tokio::time::timeout(REQUEST_TIMEOUT, read_request_target(&mut stream)).await;
    let Ok(Some(target)) = request else {
        return;
    };
    let result = match classify_callback(&target, &expected_state) {
        Callback::Code(code) => {
            respond(
                &mut stream,
                "200 OK",
                "Signed in",
                "gsc is now connected to Google Search Console. You can close this tab.",
            )
            .await;
            Ok(code)
        }
        Callback::Denied(reason) => {
            respond(
                &mut stream,
                "400 Bad Request",
                "Sign-in was not completed",
                "Access was not granted. Return to the terminal to try again.",
            )
            .await;
            Err(Error::auth(format!("sign-in was not completed ({reason})"))
                .with_hint("run `gsc auth login` again and approve the request"))
        }
        Callback::Unrelated => {
            respond(&mut stream, "404 Not Found", "Not found", "").await;
            return;
        }
    };
    // Only the first outcome counts; a repeated redirect is ignored.
    let _ = outcome.try_send(result);
}

/// Read an HTTP request and return the target of its request line.
async fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    let mut buffer = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    // Read the whole header block: replying before the browser has finished
    // sending can make it report a connection reset instead of our page.
    while !buffer.windows(4).any(|w| w == b"\r\n\r\n") && buffer.len() < MAX_REQUEST_BYTES {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let line = buffer.split(|byte| *byte == b'\r').next()?;
    let mut parts = std::str::from_utf8(line).ok()?.split(' ');
    match (parts.next()?, parts.next()?) {
        ("GET", target) => Some(target.to_owned()),
        _ => None,
    }
}

async fn respond(stream: &mut TcpStream, status: &str, title: &str, message: &str) {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>gsc</title></head>\
         <body style=\"font-family:system-ui,sans-serif;max-width:32rem;margin:4rem auto;padding:0 1rem\">\
         <h1>{title}</h1><p>{message}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // The browser may already have gone away; there is nothing useful to do then.
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> ClientConfig {
        ClientConfig::new("my-client.apps.googleusercontent.com".into(), "shh".into())
    }

    #[test]
    fn pkce_matches_the_rfc_7636_test_vector() {
        // RFC 7636, Appendix B.
        assert_eq!(
            Pkce::challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_pkce_is_consistent_and_unique() {
        let a = Pkce::generate().unwrap();
        let b = Pkce::generate().unwrap();
        assert_eq!(a.verifier.len(), 43, "32 bytes, base64url without padding");
        assert_eq!(a.challenge, Pkce::challenge_for(&a.verifier));
        assert_ne!(a.verifier, b.verifier);
    }

    #[test]
    fn authorization_url_carries_every_required_parameter() {
        let url = authorization_url(
            &client(),
            "http://127.0.0.1:4242",
            &[SCOPE_WEBMASTERS, super::super::SCOPE_INDEXING],
            "state-123",
            "challenge-abc",
        )
        .unwrap();
        let url = Url::parse(&url).unwrap();
        assert_eq!(url.host_str(), Some("accounts.google.com"));
        assert_eq!(url.path(), "/o/oauth2/v2/auth");

        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(params["client_id"], "my-client.apps.googleusercontent.com");
        assert_eq!(params["redirect_uri"], "http://127.0.0.1:4242");
        assert_eq!(params["response_type"], "code");
        assert_eq!(
            params["scope"],
            "https://www.googleapis.com/auth/webmasters https://www.googleapis.com/auth/indexing"
        );
        assert_eq!(params["state"], "state-123");
        assert_eq!(params["code_challenge"], "challenge-abc");
        assert_eq!(params["code_challenge_method"], "S256");
        assert_eq!(params["access_type"], "offline");
        assert_eq!(params["prompt"], "consent");
        assert!(
            !url.as_str().contains("shh"),
            "the secret never goes in the URL"
        );
    }

    #[test]
    fn parses_a_desktop_client_secret_file() {
        let parsed = ClientConfig::from_secret_json(
            r#"{"installed":{"client_id":"abc","project_id":"p","client_secret":"xyz",
                "auth_uri":"https://accounts.google.com/o/oauth2/auth",
                "token_uri":"https://oauth2.googleapis.com/token",
                "redirect_uris":["http://localhost"]}}"#,
        )
        .unwrap();
        assert_eq!(parsed.client_id, "abc");
        assert_eq!(parsed.client_secret, "xyz");
        assert_eq!(parsed.auth_uri, "https://accounts.google.com/o/oauth2/auth");
        assert_eq!(parsed.token_uri, DEFAULT_TOKEN_URI);
    }

    #[test]
    fn secret_file_defaults_missing_endpoints() {
        let parsed =
            ClientConfig::from_secret_json(r#"{"web":{"client_id":"a","client_secret":"b"}}"#)
                .unwrap();
        assert_eq!(parsed.auth_uri, DEFAULT_AUTH_URI);
        assert_eq!(parsed.token_uri, DEFAULT_TOKEN_URI);
    }

    #[test]
    fn rejects_files_that_are_not_oauth_clients() {
        let service_account =
            ClientConfig::from_secret_json(r#"{"type":"service_account","client_email":"x"}"#)
                .err()
                .unwrap();
        assert!(service_account.contains("service-account key"));

        let other = ClientConfig::from_secret_json(r#"{"hello":"world"}"#)
            .err()
            .unwrap();
        assert!(other.contains("Desktop app"));

        assert!(ClientConfig::from_secret_json("nope").is_err());
    }

    #[test]
    fn classifies_loopback_requests() {
        let state = "expected";
        assert_eq!(
            classify_callback("/?state=expected&code=4%2F0Abc&scope=x", state),
            Callback::Code("4/0Abc".into())
        );
        assert_eq!(
            classify_callback("/?error=access_denied&state=expected", state),
            Callback::Denied("access_denied".into())
        );
        assert_eq!(
            classify_callback("/favicon.ico", state),
            Callback::Unrelated
        );
        assert_eq!(classify_callback("/", state), Callback::Unrelated);
        assert_eq!(
            classify_callback("/?state=expected&code=", state),
            Callback::Unrelated
        );
    }

    #[test]
    fn requests_with_the_wrong_state_are_ignored() {
        assert_eq!(
            classify_callback("/?state=forged&code=stolen", "expected"),
            Callback::Unrelated
        );
        assert_eq!(
            classify_callback("/?state=forged&error=access_denied", "expected"),
            Callback::Unrelated,
            "a forged error must not be able to abort the login"
        );
        assert_eq!(
            classify_callback("/?code=no-state", "expected"),
            Callback::Unrelated
        );
    }
}
