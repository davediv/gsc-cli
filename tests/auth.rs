//! `gsc auth` end to end, with a local server standing in for Google's
//! token endpoint and the test acting as the user's browser.

mod common;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::{Arc, Mutex};

use common::{TestEnv, parse_json};
use gsc_cli::auth::oauth::Pkce;
use predicates::prelude::*;
use serde_json::json;
use url::Url;

const WEBMASTERS: &str = "https://www.googleapis.com/auth/webmasters";
const INDEXING: &str = "https://www.googleapis.com/auth/indexing";

/// A `gsc auth login --no-browser` process waiting for its redirect.
struct PendingLogin {
    child: Child,
    /// Kept open so the child can keep writing progress messages.
    _stderr: BufReader<ChildStderr>,
    /// Query parameters of the sign-in URL the CLI printed.
    params: HashMap<String, String>,
}

impl PendingLogin {
    fn start(env: &TestEnv, client_file: &std::path::Path, extra_args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gsc"))
            .env_clear()
            .env("GSC_CONFIG_DIR", env.config_dir.path())
            .args(["auth", "login", "--no-browser", "--format", "json"])
            .arg("--client-secret-file")
            .arg(client_file)
            .args(extra_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start gsc");

        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        let mut line = String::new();
        let url = loop {
            line.clear();
            let read = stderr.read_line(&mut line).expect("read stderr");
            assert!(read > 0, "gsc exited before printing a sign-in URL");
            if let Some(url) = line
                .trim()
                .strip_prefix("http")
                .map(|rest| format!("http{rest}"))
            {
                break url;
            }
        };
        let params = Url::parse(&url)
            .expect("sign-in URL")
            .query_pairs()
            .into_owned()
            .collect();
        Self {
            child,
            _stderr: stderr,
            params,
        }
    }

    /// Hit the loopback redirect the way a browser would and return the response.
    fn browser_get(&self, path_and_query: &str) -> String {
        let redirect = Url::parse(&self.params["redirect_uri"]).unwrap();
        assert_eq!(redirect.host_str(), Some("127.0.0.1"));
        let mut stream = TcpStream::connect(("127.0.0.1", redirect.port().unwrap())).unwrap();
        write!(
            stream,
            "GET {path_and_query} HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: text/html\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    fn finish(self) -> std::process::Output {
        self.child.wait_with_output().expect("wait for gsc")
    }
}

fn write_client_file(env: &TestEnv, server_url: &str) -> std::path::PathBuf {
    let path = env.config_dir.path().join("client_secret_test.json");
    let body = json!({ "installed": {
        "client_id": "test-client.apps.googleusercontent.com",
        "client_secret": "test-client-secret",
        "auth_uri": format!("{server_url}/auth"),
        "token_uri": format!("{server_url}/token"),
        "redirect_uris": ["http://localhost"],
    }});
    std::fs::write(&path, body.to_string()).unwrap();
    path
}

fn write_stored_login(env: &TestEnv, token_uri: &str, scopes: &[&str]) {
    let body = json!({
        "type": "authorized_user",
        "client_id": "test-client.apps.googleusercontent.com",
        "client_secret": "test-client-secret",
        "refresh_token": "stored-refresh-token",
        "token_uri": token_uri,
        "scopes": scopes,
    });
    std::fs::write(
        env.config_dir.path().join("credentials.json"),
        body.to_string(),
    )
    .unwrap();
}

fn form(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

#[test]
fn browser_login_exchanges_the_code_with_pkce_and_saves_a_refresh_token() {
    let env = TestEnv::new();
    let mut server = mockito::Server::new();
    let token_request = Arc::new(Mutex::new(String::new()));
    let sink = token_request.clone();
    let token_endpoint = server
        .mock("POST", "/token")
        .with_header("content-type", "application/json")
        .with_body_from_request(move |request| {
            *sink.lock().unwrap() = String::from_utf8_lossy(request.body().unwrap()).into_owned();
            json!({
                "access_token": "access-token-1",
                "expires_in": 3599,
                "refresh_token": "refresh-token-1",
                "scope": format!("{WEBMASTERS} {INDEXING}"),
                "token_type": "Bearer",
            })
            .to_string()
            .into_bytes()
        })
        .expect(1)
        .create();

    let client_file = write_client_file(&env, &server.url());
    let login = PendingLogin::start(&env, &client_file, &["--indexing"]);

    // The sign-in URL asks for the right things.
    assert_eq!(
        login.params["client_id"],
        "test-client.apps.googleusercontent.com"
    );
    assert_eq!(login.params["scope"], format!("{WEBMASTERS} {INDEXING}"));
    assert_eq!(login.params["code_challenge_method"], "S256");
    assert_eq!(login.params["access_type"], "offline");

    // Noise on the port is ignored; only the real redirect completes the login.
    assert!(
        login
            .browser_get("/favicon.ico")
            .starts_with("HTTP/1.1 404")
    );
    assert!(
        login
            .browser_get("/?state=forged&code=stolen-code")
            .starts_with("HTTP/1.1 404")
    );
    let state = login.params["state"].clone();
    let response = login.browser_get(&format!("/?state={state}&code=auth-code-1&scope=x"));
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.contains("You can close this tab"));

    let challenge = login.params["code_challenge"].clone();
    let redirect_uri = login.params["redirect_uri"].clone();
    let output = login.finish();
    assert!(output.status.success(), "{output:?}");
    let report = parse_json(&output.stdout);
    assert_eq!(report["authenticated"], true);
    assert_eq!(report["scopes"], json!([WEBMASTERS, INDEXING]));

    // The code was exchanged with the verifier matching the challenge.
    token_endpoint.assert();
    let sent = form(&token_request.lock().unwrap());
    assert_eq!(sent["grant_type"], "authorization_code");
    assert_eq!(sent["code"], "auth-code-1");
    assert_eq!(sent["client_secret"], "test-client-secret");
    assert_eq!(sent["redirect_uri"], redirect_uri);
    assert_eq!(Pkce::challenge_for(&sent["code_verifier"]), challenge);

    // The refresh token is on disk, readable only by the owner.
    let credentials_path = env.config_dir.path().join("credentials.json");
    let saved = parse_json(&std::fs::read(&credentials_path).unwrap());
    assert_eq!(saved["type"], "authorized_user");
    assert_eq!(saved["refresh_token"], "refresh-token-1");
    assert_eq!(saved["token_uri"], format!("{}/token", server.url()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&credentials_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // The access token from login is reused: no second trip to the endpoint.
    env.gsc()
        .args(["auth", "token"])
        .assert()
        .success()
        .stdout("access-token-1\n");
    let status = env.json(&["auth", "status"]);
    assert_eq!(status["method"], "oauth");
    assert_eq!(status["account"], "test-client.apps.googleusercontent.com");
    token_endpoint.assert();
}

#[test]
fn a_stalled_connection_does_not_hold_up_the_real_redirect() {
    let env = TestEnv::new();
    let mut server = mockito::Server::new();
    server
        .mock("POST", "/token")
        .with_body(
            json!({
                "access_token": "a",
                "expires_in": 3599,
                "refresh_token": "r",
                "scope": WEBMASTERS,
            })
            .to_string(),
        )
        .create();
    let client_file = write_client_file(&env, &server.url());
    let login = PendingLogin::start(&env, &client_file, &[]);

    // Connections that are opened and then say nothing, or only start a
    // request, as a browser preconnect or a hostile local process would.
    let port = Url::parse(&login.params["redirect_uri"])
        .unwrap()
        .port()
        .unwrap();
    let silent = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut partial = TcpStream::connect(("127.0.0.1", port)).unwrap();
    partial.write_all(b"GET /?state=").unwrap();

    let started = std::time::Instant::now();
    let state = login.params["state"].clone();
    let response = login.browser_get(&format!("/?state={state}&code=c"));
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    let output = login.finish();
    assert!(output.status.success(), "{output:?}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "the redirect was answered only after {:?}",
        started.elapsed()
    );
    drop((silent, partial));
}

#[test]
fn denying_consent_fails_the_login_without_saving_anything() {
    let env = TestEnv::new();
    let server = mockito::Server::new();
    let client_file = write_client_file(&env, &server.url());
    let login = PendingLogin::start(&env, &client_file, &[]);

    let state = login.params["state"].clone();
    let response = login.browser_get(&format!("/?error=access_denied&state={state}"));
    assert!(response.starts_with("HTTP/1.1 400"));

    let output = login.finish();
    assert_eq!(output.status.code(), Some(3));
    assert!(!env.config_dir.path().join("credentials.json").exists());
}

#[test]
fn login_fails_when_search_console_permission_is_unticked() {
    let env = TestEnv::new();
    let mut server = mockito::Server::new();
    server
        .mock("POST", "/token")
        .with_body(
            json!({
                "access_token": "a",
                "expires_in": 3599,
                "refresh_token": "r",
                "scope": INDEXING,
            })
            .to_string(),
        )
        .create();
    let client_file = write_client_file(&env, &server.url());
    let login = PendingLogin::start(&env, &client_file, &["--indexing"]);
    let state = login.params["state"].clone();
    login.browser_get(&format!("/?state={state}&code=c"));

    let output = login.finish();
    assert_eq!(output.status.code(), Some(3));
    assert!(!env.config_dir.path().join("credentials.json").exists());
}

#[test]
fn login_without_an_oauth_client_explains_how_to_get_one() {
    let output = TestEnv::new()
        .gsc()
        .args(["auth", "login", "--no-browser"])
        .assert()
        .code(2)
        .get_output()
        .clone();
    let err = parse_json(&output.stderr);
    assert_eq!(err["error"]["message"], "no OAuth client configured");
    assert!(
        err["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("--client-secret-file")
    );
}

#[test]
fn login_rejects_a_service_account_key_passed_as_client_file() {
    let env = TestEnv::new();
    let path = env.config_dir.path().join("key.json");
    std::fs::write(&path, r#"{"type":"service_account","client_email":"x@y"}"#).unwrap();
    env.gsc()
        .args(["auth", "login", "--no-browser", "--client-secret-file"])
        .arg(&path)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("service-account key"));
}

#[test]
fn stored_login_is_refreshed_once_and_then_served_from_the_cache() {
    let env = TestEnv::new();
    let mut server = mockito::Server::new();
    let refresh = server
        .mock("POST", "/token")
        .match_body(mockito::Matcher::AllOf(vec![
            mockito::Matcher::UrlEncoded("grant_type".into(), "refresh_token".into()),
            mockito::Matcher::UrlEncoded("refresh_token".into(), "stored-refresh-token".into()),
            mockito::Matcher::UrlEncoded("client_secret".into(), "test-client-secret".into()),
        ]))
        .with_body(r#"{"access_token":"refreshed-token","expires_in":3599}"#)
        .expect(1)
        .create();
    write_stored_login(&env, &format!("{}/token", server.url()), &[WEBMASTERS]);

    for _ in 0..2 {
        env.gsc()
            .args(["auth", "token"])
            .assert()
            .success()
            .stdout("refreshed-token\n");
    }
    refresh.assert();

    let doc = env.json(&["auth", "token", "--format", "json"]);
    assert_eq!(doc["accessToken"], "refreshed-token");
    assert!(doc["expiresAt"].as_i64().unwrap() > 0);
}

#[test]
fn revoked_login_is_an_auth_error_with_a_way_forward() {
    let env = TestEnv::new();
    let mut server = mockito::Server::new();
    server
        .mock("POST", "/token")
        .with_status(400)
        .with_body(
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
        )
        .create();
    write_stored_login(&env, &format!("{}/token", server.url()), &[WEBMASTERS]);

    let output = env
        .gsc()
        .args(["auth", "status"])
        .assert()
        .code(3)
        .stdout("")
        .get_output()
        .clone();
    let err = parse_json(&output.stderr);
    assert_eq!(err["error"]["kind"], "auth");
    assert_eq!(err["error"]["code"], "invalid_grant");
    assert_eq!(err["error"]["status"], 400);
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Token has been expired or revoked.")
    );
    assert!(
        err["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("gsc auth login")
    );
}

#[test]
fn indexing_token_requires_the_indexing_scope() {
    let env = TestEnv::new();
    // No server: the scope check must fail before any network call.
    write_stored_login(&env, "http://127.0.0.1:1/token", &[WEBMASTERS]);
    env.gsc()
        .args(["auth", "token", "--indexing"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("gsc auth login --indexing"));
}

#[test]
fn status_without_credentials_exits_3() {
    let output = TestEnv::new()
        .gsc()
        .args(["auth", "status"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let err = parse_json(&output.stderr);
    assert_eq!(err["error"]["message"], "not logged in");
}

#[test]
fn environment_token_takes_precedence_over_a_stored_login() {
    let env = TestEnv::new();
    write_stored_login(&env, "http://127.0.0.1:1/token", &[WEBMASTERS]);

    env.gsc()
        .env("GSC_ACCESS_TOKEN", "token-from-env")
        .args(["auth", "token"])
        .assert()
        .success()
        .stdout("token-from-env\n");

    let output = env
        .gsc()
        .env("GSC_ACCESS_TOKEN", "token-from-env")
        .args(["auth", "status"])
        .assert()
        .success()
        .get_output()
        .clone();
    let status = parse_json(&output.stdout);
    assert_eq!(status["method"], "access_token");
    assert_eq!(status["source"], "$GSC_ACCESS_TOKEN");
    assert_eq!(status["tokenExpiresAt"], json!(null));
}

#[test]
fn logout_removes_credentials_and_cached_tokens() {
    let env = TestEnv::new();
    write_stored_login(&env, "http://127.0.0.1:1/token", &[WEBMASTERS]);
    std::fs::write(env.config_dir.path().join("token_cache.json"), "{}").unwrap();

    let first = env.json(&["auth", "logout"]);
    assert_eq!(first, json!({ "loggedOut": true, "revoked": false }));
    assert!(!env.config_dir.path().join("credentials.json").exists());
    assert!(!env.config_dir.path().join("token_cache.json").exists());

    let second = env.json(&["auth", "logout"]);
    assert_eq!(second["loggedOut"], false);
}
