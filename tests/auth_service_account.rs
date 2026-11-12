//! Service-account and credential-file authentication, end to end.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use common::{TestEnv, TestServiceAccount, parse_json};
use predicates::prelude::*;
use serde_json::json;

const WEBMASTERS: &str = "https://www.googleapis.com/auth/webmasters";
const INDEXING: &str = "https://www.googleapis.com/auth/indexing";
const JWT_BEARER: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

/// A token endpoint that records the assertions it is sent.
struct TokenEndpoint {
    server: mockito::ServerGuard,
    mock: mockito::Mock,
    assertions: Arc<Mutex<Vec<String>>>,
}

impl TokenEndpoint {
    fn start() -> Self {
        let mut server = mockito::Server::new();
        let assertions = Arc::new(Mutex::new(Vec::new()));
        let sink = assertions.clone();
        let mock = server
            .mock("POST", "/token")
            .match_body(mockito::Matcher::UrlEncoded(
                "grant_type".into(),
                JWT_BEARER.into(),
            ))
            .with_body_from_request(move |request| {
                let form: HashMap<String, String> =
                    url::form_urlencoded::parse(request.body().unwrap())
                        .into_owned()
                        .collect();
                let mut seen = sink.lock().unwrap();
                seen.push(form["assertion"].clone());
                json!({
                    "access_token": format!("sa-token-{}", seen.len()),
                    "expires_in": 3599,
                    "token_type": "Bearer",
                })
                .to_string()
                .into_bytes()
            })
            .create();
        Self {
            server,
            mock,
            assertions,
        }
    }

    fn uri(&self) -> String {
        format!("{}/token", self.server.url())
    }

    fn assertions(&self) -> Vec<String> {
        self.assertions.lock().unwrap().clone()
    }
}

#[test]
fn service_account_login_validates_the_key_and_stores_only_a_pointer() {
    let env = TestEnv::new();
    let endpoint = TokenEndpoint::start();
    let keys = tempfile::tempdir().unwrap();
    let account = TestServiceAccount::create(keys.path(), &endpoint.uri());

    let output = env
        .gsc()
        .args(["auth", "login", "--service-account"])
        .arg(&account.key_file)
        .assert()
        .success()
        .get_output()
        .clone();
    let report = parse_json(&output.stdout);
    assert_eq!(report["method"], "service_account");
    assert_eq!(report["account"], account.client_email);
    assert!(
        report["notes"][0]
            .as_str()
            .unwrap()
            .contains(account.client_email),
        "the user is told which address to add in Search Console"
    );

    // The signed assertion is what Google expects.
    let assertions = endpoint.assertions();
    assert_eq!(assertions.len(), 1);
    let claims = account.verify_assertion(&assertions[0]);
    assert_eq!(claims["iss"], account.client_email);
    assert_eq!(claims["scope"], WEBMASTERS);
    assert_eq!(claims["aud"], endpoint.uri());
    assert_eq!(
        claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(),
        3600
    );

    // Only the path is saved; the private key is not copied.
    let saved = std::fs::read_to_string(env.config_dir.path().join("credentials.json")).unwrap();
    assert!(!saved.contains("PRIVATE KEY"));
    let saved = parse_json(saved.as_bytes());
    assert_eq!(saved["type"], "service_account_file");
    assert_eq!(
        saved["path"].as_str().unwrap(),
        account.key_file.canonicalize().unwrap().to_str().unwrap()
    );

    // The token from login is cached, so status needs no second exchange.
    let status = env.json(&["auth", "status"]);
    assert_eq!(status["method"], "service_account");
    assert_eq!(status["account"], account.client_email);
    assert!(
        status["source"]
            .as_str()
            .unwrap()
            .ends_with("(from gsc auth login)")
    );
    env.gsc()
        .args(["auth", "token"])
        .assert()
        .success()
        .stdout("sa-token-1\n");
    endpoint.mock.expect(1).assert();
}

#[test]
fn service_account_requests_a_separate_token_for_the_indexing_api() {
    let env = TestEnv::new();
    let endpoint = TokenEndpoint::start();
    let keys = tempfile::tempdir().unwrap();
    let account = TestServiceAccount::create(keys.path(), &endpoint.uri());

    let run = |args: &[&str]| {
        env.gsc()
            .env("GSC_CREDENTIALS_FILE", &account.key_file)
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone()
    };
    assert_eq!(run(&["auth", "token"]), b"sa-token-1\n");
    assert_eq!(run(&["auth", "token", "--indexing"]), b"sa-token-2\n");
    // Each is cached under its own scope.
    assert_eq!(run(&["auth", "token"]), b"sa-token-1\n");
    assert_eq!(run(&["auth", "token", "--indexing"]), b"sa-token-2\n");

    let scopes: Vec<String> = endpoint
        .assertions()
        .iter()
        .map(|jwt| {
            account.verify_assertion(jwt)["scope"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(scopes, [WEBMASTERS, INDEXING]);
}

#[test]
fn credential_sources_are_tried_in_documented_order() {
    let env = TestEnv::new();
    let endpoint = TokenEndpoint::start();
    let keys = tempfile::tempdir().unwrap();
    let account = TestServiceAccount::create(keys.path(), &endpoint.uri());
    let key_file = account.key_file.to_str().unwrap();

    // Which credentials does `gsc` report using, given these variables?
    let source = |vars: &[(&str, &str)]| -> String {
        let mut cmd = env.gsc();
        for (name, value) in vars {
            cmd.env(name, value);
        }
        let output = cmd
            .args(["auth", "status"])
            .assert()
            .success()
            .get_output()
            .clone();
        parse_json(&output.stdout)["source"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    // 4. Not logged in: Google's standard variable is the fallback.
    assert_eq!(
        source(&[("GOOGLE_APPLICATION_CREDENTIALS", key_file)]),
        format!("{key_file} (from $GOOGLE_APPLICATION_CREDENTIALS)")
    );

    // 3. A saved login beats the generic variable.
    let mut user_endpoint = mockito::Server::new();
    user_endpoint
        .mock("POST", "/token")
        .with_body(r#"{"access_token":"user-token","expires_in":3599}"#)
        .create();
    let stored = env.config_dir.path().join("credentials.json");
    std::fs::write(
        &stored,
        json!({
            "type": "authorized_user",
            "client_id": "c", "client_secret": "s", "refresh_token": "r",
            "token_uri": format!("{}/token", user_endpoint.url()),
            "scopes": [WEBMASTERS],
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        source(&[("GOOGLE_APPLICATION_CREDENTIALS", key_file)]),
        stored.to_str().unwrap()
    );

    // 2. The gsc-specific variable beats the saved login.
    assert_eq!(
        source(&[("GSC_CREDENTIALS_FILE", key_file)]),
        format!("{key_file} (from $GSC_CREDENTIALS_FILE)")
    );

    // 1. An explicit token beats everything.
    assert_eq!(
        source(&[
            ("GSC_ACCESS_TOKEN", "raw"),
            ("GSC_CREDENTIALS_FILE", key_file),
            ("GOOGLE_APPLICATION_CREDENTIALS", key_file),
        ]),
        "$GSC_ACCESS_TOKEN"
    );
}

#[test]
fn authorized_user_credential_files_are_refreshed() {
    let env = TestEnv::new();
    let mut server = mockito::Server::new();
    server
        .mock("POST", "/token")
        .match_body(mockito::Matcher::UrlEncoded(
            "grant_type".into(),
            "refresh_token".into(),
        ))
        .with_body(r#"{"access_token":"adc-token","expires_in":3599}"#)
        .create();
    let file = env.config_dir.path().join("adc.json");
    std::fs::write(
        &file,
        json!({
            "type": "authorized_user",
            "client_id": "c", "client_secret": "s", "refresh_token": "r",
            "token_uri": format!("{}/token", server.url()),
        })
        .to_string(),
    )
    .unwrap();

    env.gsc()
        .env("GSC_CREDENTIALS_FILE", &file)
        .args(["auth", "token"])
        .assert()
        .success()
        .stdout("adc-token\n");
}

#[test]
fn rejected_service_account_is_an_auth_error_and_nothing_is_saved() {
    let env = TestEnv::new();
    let mut server = mockito::Server::new();
    server
        .mock("POST", "/token")
        .with_status(400)
        .with_body(r#"{"error":"invalid_grant","error_description":"Invalid JWT Signature."}"#)
        .create();
    let keys = tempfile::tempdir().unwrap();
    let account = TestServiceAccount::create(keys.path(), &format!("{}/token", server.url()));

    let output = env
        .gsc()
        .args(["auth", "login", "--service-account"])
        .arg(&account.key_file)
        .assert()
        .code(3)
        .get_output()
        .clone();
    let err = parse_json(&output.stderr);
    assert_eq!(err["error"]["code"], "invalid_grant");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Invalid JWT Signature.")
    );
    assert!(!env.config_dir.path().join("credentials.json").exists());
}

#[test]
fn unusable_credential_files_explain_what_is_wrong() {
    let env = TestEnv::new();
    let dir = env.config_dir.path();

    let client_file = dir.join("client.json");
    std::fs::write(
        &client_file,
        r#"{"installed":{"client_id":"a","client_secret":"b"}}"#,
    )
    .unwrap();
    env.gsc()
        .env("GSC_CREDENTIALS_FILE", &client_file)
        .args(["auth", "status", "--format", "table"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("it is an OAuth client file"))
        .stderr(predicate::str::contains(
            "gsc auth login --client-secret-file",
        ));

    let federated = dir.join("federated.json");
    std::fs::write(&federated, r#"{"type":"external_account"}"#).unwrap();
    env.gsc()
        .env("GSC_CREDENTIALS_FILE", &federated)
        .args(["auth", "status", "--format", "table"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "`external_account` is not supported",
        ));

    env.gsc()
        .env("GSC_CREDENTIALS_FILE", dir.join("missing.json"))
        .args(["auth", "status", "--format", "table"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("cannot use"));
}

#[test]
fn deleting_the_key_file_after_login_is_reported_clearly() {
    let env = TestEnv::new();
    let endpoint = TokenEndpoint::start();
    let keys = tempfile::tempdir().unwrap();
    let account = TestServiceAccount::create(keys.path(), &endpoint.uri());
    env.gsc()
        .args(["auth", "login", "--service-account"])
        .arg(&account.key_file)
        .assert()
        .success();

    std::fs::remove_file(&account.key_file).unwrap();
    env.gsc()
        .args(["auth", "status", "--format", "table"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "could not read the service-account key",
        ))
        .stderr(predicate::str::contains("gsc auth login --service-account"));
}

#[test]
fn service_account_flag_cannot_be_combined_with_browser_login_flags() {
    TestEnv::new()
        .gsc()
        .args([
            "auth",
            "login",
            "--service-account",
            "k.json",
            "--no-browser",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}
