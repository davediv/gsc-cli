//! The API client, exercised through `gsc api` against a mock server.

mod common;

use common::{MockApi, TestEnv, parse_json};
use mockito::Matcher;
use predicates::prelude::*;
use serde_json::json;

const PERMISSION_DENIED: &str = r#"{"error":{"code":403,"message":"User does not have sufficient permission for site 'https://example.com/'.","errors":[{"message":"x","domain":"global","reason":"forbidden"}],"status":"PERMISSION_DENIED"}}"#;

#[test]
fn get_sends_the_bearer_token_and_prints_the_response() {
    let mut api = MockApi::new();
    let sites = api
        .on("GET", "/webmasters/v3/sites")
        .with_body(
            r#"{"siteEntry":[{"siteUrl":"sc-domain:example.com","permissionLevel":"siteOwner"}]}"#,
        )
        .create();

    let doc = api.json(&["api", "GET", "/webmasters/v3/sites"]);
    assert_eq!(doc["siteEntry"][0]["siteUrl"], "sc-domain:example.com");
    sites.assert();
}

#[test]
fn raw_json_is_printed_in_every_format() {
    let mut api = MockApi::new();
    api.on("GET", "/x").with_body(r#"{"ok":true}"#).create();
    for format in ["table", "csv", "json"] {
        api.gsc()
            .args(["api", "get", "/x", "--format", format])
            .assert()
            .success()
            .stdout("{\"ok\":true}\n");
    }
}

#[test]
fn post_sends_the_json_body_and_query_parameters() {
    let mut api = MockApi::new();
    let inspect = api
        .on("POST", "/v1/urlInspection/index:inspect")
        .match_query(Matcher::UrlEncoded("alt".into(), "json".into()))
        .match_header("content-type", "application/json")
        .match_body(Matcher::Json(json!({
            "inspectionUrl": "https://example.com/a",
            "siteUrl": "sc-domain:example.com",
        })))
        .with_body(r#"{"inspectionResult":{}}"#)
        .create();

    let doc = api.json(&[
        "api",
        "POST",
        "/v1/urlInspection/index:inspect",
        "--query",
        "alt=json",
        "--data",
        r#"{"inspectionUrl":"https://example.com/a","siteUrl":"sc-domain:example.com"}"#,
    ]);
    assert_eq!(doc, json!({ "inspectionResult": {} }));
    inspect.assert();
}

#[test]
fn body_can_come_from_stdin() {
    let mut api = MockApi::new();
    let mock = api
        .on("POST", "/echo")
        .match_body(Matcher::Json(json!({ "from": "stdin" })))
        .with_body("{}")
        .create();
    api.gsc()
        .args(["api", "POST", "/echo", "-d", "-"])
        .write_stdin(r#"{"from":"stdin"}"#)
        .assert()
        .success();
    mock.assert();
}

#[test]
fn bodyless_put_declares_a_zero_length_and_empty_response_prints_an_object() {
    let mut api = MockApi::new();
    let put = api
        .on("PUT", "/webmasters/v3/sites/https%3A%2F%2Fexample.com%2F")
        .match_header("content-length", "0")
        .with_status(204)
        .create();

    api.gsc()
        .args([
            "api",
            "PUT",
            "/webmasters/v3/sites/https%3A%2F%2Fexample.com%2F",
        ])
        .assert()
        .success()
        .stdout("{}\n");
    put.assert();
}

#[test]
fn indexing_flag_targets_the_indexing_api() {
    let api = MockApi::new();
    let mut indexing = mockito::Server::new();
    let metadata = indexing
        .mock("GET", "/v3/urlNotifications/metadata")
        .match_query(Matcher::UrlEncoded(
            "url".into(),
            "https://example.com/job".into(),
        ))
        .with_body(r#"{"url":"https://example.com/job"}"#)
        .create();

    let output = api
        .gsc()
        .env("GSC_INDEXING_BASE_URL", indexing.url())
        .args([
            "api",
            "GET",
            "/v3/urlNotifications/metadata",
            "-q",
            "url=https://example.com/job",
            "--indexing",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_eq!(parse_json(&output.stdout)["url"], "https://example.com/job");
    metadata.assert();
}

#[test]
fn permission_denied_exits_4_with_googles_message_and_a_hint() {
    let mut api = MockApi::new();
    api.on("GET", "/denied")
        .with_status(403)
        .with_body(PERMISSION_DENIED)
        .create();

    let err = api.error(&["api", "GET", "/denied"], 4);
    assert_eq!(err["kind"], "api");
    assert_eq!(err["status"], 403);
    assert_eq!(err["code"], "PERMISSION_DENIED");
    assert_eq!(
        err["message"],
        "User does not have sufficient permission for site 'https://example.com/'."
    );
    assert!(err["hint"].as_str().unwrap().contains("gsc sites list"));
}

#[test]
fn unauthorized_exits_3() {
    let mut api = MockApi::new();
    api.on("GET", "/x")
        .with_status(401)
        .with_body(
            r#"{"error":{"code":401,"message":"Invalid Credentials","status":"UNAUTHENTICATED"}}"#,
        )
        .create();
    let err = api.error(&["api", "GET", "/x"], 3);
    assert_eq!(err["kind"], "auth");
    assert!(err["hint"].as_str().unwrap().contains("$GSC_ACCESS_TOKEN"));
}

#[test]
fn quota_exhaustion_exits_5() {
    let mut api = MockApi::new();
    let mock = api
        .on("GET", "/x")
        .with_status(429)
        .with_body(
            r#"{"error":{"code":429,"message":"Quota exceeded","status":"RESOURCE_EXHAUSTED"}}"#,
        )
        .expect(1)
        .create();
    let err = api.error(&["api", "GET", "/x"], 5);
    assert_eq!(err["kind"], "rate_limit");
    mock.assert();
}

#[test]
fn transient_failures_are_retried_until_they_succeed() {
    let mut api = MockApi::new();
    // mockito serves the first mock that still expects a hit, so the 503 is
    // answered once and the retry reaches the success response.
    let unavailable = api
        .on("GET", "/flaky")
        .with_status(503)
        .with_header("retry-after", "0")
        .with_body("Service Unavailable")
        .expect(1)
        .create();
    let ok = api
        .on("GET", "/flaky")
        .with_body(r#"{"attempt":2}"#)
        .expect(1)
        .create();

    let output = api
        .gsc()
        .env("GSC_MAX_RETRIES", "2")
        .args(["api", "GET", "/flaky", "--verbose"])
        .assert()
        .success()
        .stdout("{\"attempt\":2}\n")
        .get_output()
        .clone();
    unavailable.assert();
    ok.assert();

    let log = String::from_utf8_lossy(&output.stderr);
    assert!(log.contains("> GET "), "{log}");
    assert!(log.contains("< 503"), "{log}");
    assert!(log.contains("retrying"), "{log}");
    assert!(log.contains("< 200"), "{log}");
}

#[test]
fn retries_stop_at_the_configured_limit() {
    let mut api = MockApi::new();
    let mock = api
        .on("GET", "/down")
        .with_status(500)
        .with_header("retry-after", "0")
        .with_body("boom")
        .expect(3)
        .create();
    let err = api
        .gsc()
        .env("GSC_MAX_RETRIES", "2")
        .args(["api", "GET", "/down"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    mock.assert();
    assert_eq!(parse_json(&err.stderr)["error"]["kind"], "network");
}

#[test]
fn client_errors_are_never_retried() {
    let mut api = MockApi::new();
    let mock = api
        .on("GET", "/bad")
        .with_status(400)
        .with_body(
            r#"{"error":{"code":400,"message":"Invalid value","status":"INVALID_ARGUMENT"}}"#,
        )
        .expect(1)
        .create();
    api.gsc()
        .env("GSC_MAX_RETRIES", "3")
        .args(["api", "GET", "/bad"])
        .assert()
        .code(4);
    mock.assert();
}

#[test]
fn unreachable_server_is_a_network_error() {
    let env = TestEnv::new();
    let output = env
        .gsc()
        .env("GSC_ACCESS_TOKEN", "t")
        .env("GSC_MAX_RETRIES", "0")
        // Port 1 is reserved and never listening.
        .env("GSC_API_BASE_URL", "http://127.0.0.1:1")
        .args(["api", "GET", "/x"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let err = parse_json(&output.stderr);
    assert_eq!(err["error"]["kind"], "network");
    assert!(
        err["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("internet connection")
    );
}

#[test]
fn calling_the_api_without_credentials_exits_3() {
    TestEnv::new()
        .gsc()
        .args(["api", "GET", "/webmasters/v3/sites", "--format", "table"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("error: not logged in"));
}

#[test]
fn usage_mistakes_exit_2_before_any_request() {
    let api = MockApi::new();
    assert_eq!(
        api.error(&["api", "GET", "webmasters/v3/sites"], 2)["message"],
        "path must start with `/`, got `webmasters/v3/sites`"
    );
    assert_eq!(
        api.error(&["api", "PATCH", "/x"], 2)["message"],
        "unsupported HTTP method `PATCH`"
    );
    assert!(
        api.error(&["api", "POST", "/x", "-d", "{not json"], 2)["message"]
            .as_str()
            .unwrap()
            .starts_with("the request body is not valid JSON")
    );
    assert_eq!(
        api.error(&["api", "GET", "/x", "-q", "novalue"], 2)["message"],
        "query parameter `novalue` is not KEY=VALUE"
    );
}
