//! `gsc indexing` against a mock Indexing API.

mod common;

use common::MockApi;
use mockito::Matcher;
use predicates::prelude::*;
use serde_json::{Value, json};

const PUBLISH: &str = "/v3/urlNotifications:publish";
const METADATA: &str = "/v3/urlNotifications/metadata";
const JOB: &str = "https://example.com/jobs/rust-engineer";

/// A publish endpoint that echoes the notification back as Google does.
fn publish_endpoint(api: &mut MockApi) -> mockito::Mock {
    api.on("POST", PUBLISH).with_body_from_request(|request| {
        let body: Value = serde_json::from_slice(request.body().unwrap()).unwrap();
        let field = if body["type"] == "URL_DELETED" {
            "latestRemove"
        } else {
            "latestUpdate"
        };
        json!({ "urlNotificationMetadata": {
            "url": body["url"],
            field: { "url": body["url"], "type": body["type"], "notifyTime": "2026-11-12T10:00:00Z" },
        }})
        .to_string()
        .into_bytes()
    })
}

#[test]
fn publish_reports_each_url_as_updated() {
    let mut api = MockApi::new();
    let first = api
        .on("POST", PUBLISH)
        .match_body(Matcher::Json(json!({ "url": JOB, "type": "URL_UPDATED" })))
        .with_body(
            json!({ "urlNotificationMetadata": {
                "url": JOB,
                "latestUpdate": { "url": JOB, "type": "URL_UPDATED", "notifyTime": "2026-11-12T10:00:00Z" },
            }})
            .to_string(),
        )
        .expect(1)
        .create();
    let rest = publish_endpoint(&mut api).expect(1).create();

    // The duplicate is sent once.
    let doc = api.json(&[
        "indexing",
        "publish",
        JOB,
        "https://example.com/jobs/designer",
        JOB,
    ]);
    first.assert();
    rest.assert();
    assert_eq!(
        doc,
        json!({
            "type": "URL_UPDATED",
            "summary": { "urls": 2, "published": 2, "failed": 0, "skipped": 0 },
            "results": [
                { "url": JOB, "published": true, "notifyTime": "2026-11-12T10:00:00Z" },
                { "url": "https://example.com/jobs/designer", "published": true,
                  "notifyTime": "2026-11-12T10:00:00Z" },
            ],
        })
    );
}

#[test]
fn deleted_notifications_need_confirmation() {
    let mut api = MockApi::new();
    let removal = api
        .on("POST", PUBLISH)
        .match_body(Matcher::Json(json!({ "url": JOB, "type": "URL_DELETED" })))
        .with_body(
            json!({ "urlNotificationMetadata": {
                "url": JOB,
                "latestRemove": { "url": JOB, "type": "URL_DELETED", "notifyTime": "2026-11-12T11:00:00Z" },
            }})
            .to_string(),
        )
        .expect(1)
        .create();

    api.gsc()
        .args(["indexing", "publish", JOB, "--deleted"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--yes"));
    assert!(!removal.matched());

    let doc = api.json(&["indexing", "publish", JOB, "--deleted", "--yes"]);
    removal.assert();
    assert_eq!(doc["type"], "URL_DELETED");
    assert_eq!(doc["results"][0]["notifyTime"], "2026-11-12T11:00:00Z");
}

#[test]
fn urls_can_come_from_a_file() {
    let mut api = MockApi::new();
    let published = publish_endpoint(&mut api).expect(2).create();
    let file = api.env.config_dir.path().join("jobs.txt");
    std::fs::write(
        &file,
        "# new postings\nhttps://example.com/jobs/1\nhttps://example.com/jobs/2\n",
    )
    .unwrap();

    let doc = api.json(&["indexing", "publish", "--file", file.to_str().unwrap()]);
    published.assert();
    assert_eq!(doc["summary"]["published"], 2);
}

#[test]
fn a_failed_url_is_listed_and_quota_exhaustion_stops_the_run() {
    let mut api = MockApi::new();
    api.on("POST", PUBLISH)
        .match_body(Matcher::PartialJson(json!({ "url": "https://example.com/jobs/2" })))
        .with_status(429)
        .with_body(r#"{"error":{"code":429,"message":"Quota exceeded for quota metric 'Publish requests'","status":"RESOURCE_EXHAUSTED"}}"#)
        .create();
    let others = api
        .on("POST", PUBLISH)
        .match_body(Matcher::Regex(r#"jobs/[13]""#.into()))
        .with_body_from_request(|request| {
            let body: Value = serde_json::from_slice(request.body().unwrap()).unwrap();
            json!({ "urlNotificationMetadata": { "url": body["url"], "latestUpdate": {
                "notifyTime": "2026-11-12T10:00:00Z",
            }}})
            .to_string()
            .into_bytes()
        })
        .expect(1)
        .create();

    let doc = api.json(&[
        "indexing",
        "publish",
        "https://example.com/jobs/1",
        "https://example.com/jobs/2",
        "https://example.com/jobs/3",
    ]);
    others.assert();
    assert_eq!(
        doc["summary"],
        json!({ "urls": 3, "published": 1, "failed": 1, "skipped": 1 })
    );
    assert_eq!(doc["results"][1]["published"], false);
    assert!(
        doc["results"][1]["error"]
            .as_str()
            .unwrap()
            .starts_with("Quota exceeded")
    );
    assert_eq!(
        doc["results"][2],
        json!({
            "url": "https://example.com/jobs/3", "published": false, "skipped": true,
        })
    );
    assert!(
        doc["notes"][0]
            .as_str()
            .unwrap()
            .contains("ran out with 1 URL left unsent")
    );
}

#[test]
fn when_nothing_is_published_the_cause_is_the_error() {
    let mut api = MockApi::new();
    api.on("POST", PUBLISH)
        .with_status(403)
        .with_body(r#"{"error":{"code":403,"message":"Permission denied. Failed to verify the URL ownership.","status":"PERMISSION_DENIED"}}"#)
        .create();
    let err = api.error(&["indexing", "publish", JOB], 4);
    assert_eq!(
        err["message"],
        "Permission denied. Failed to verify the URL ownership."
    );
    assert!(err["hint"].as_str().unwrap().contains("verified owner"));
}

#[test]
fn a_disabled_api_points_at_the_right_console_page() {
    let mut api = MockApi::new();
    api.on("POST", PUBLISH)
        .with_status(403)
        .with_body(r#"{"error":{"code":403,"message":"Web Search Indexing API has not been used in project 123 before or it is disabled.","status":"PERMISSION_DENIED"}}"#)
        .create();
    let err = api.error(&["indexing", "publish", JOB], 4);
    assert!(
        err["hint"]
            .as_str()
            .unwrap()
            .contains("https://console.cloud.google.com/apis/library/indexing.googleapis.com")
    );
}

#[test]
fn status_shows_the_latest_notifications() {
    let mut api = MockApi::new();
    let lookup = api
        .on("GET", METADATA)
        .match_query(Matcher::UrlEncoded("url".into(), JOB.into()))
        .with_body(
            json!({
                "url": JOB,
                "latestUpdate": { "url": JOB, "type": "URL_UPDATED", "notifyTime": "2026-11-12T10:00:00Z" },
            })
            .to_string(),
        )
        .create();

    let doc = api.json(&["indexing", "status", JOB]);
    lookup.assert();
    assert_eq!(doc["latestUpdate"]["notifyTime"], "2026-11-12T10:00:00Z");

    api.gsc()
        .args(["indexing", "status", JOB, "--format", "table"])
        .assert()
        .success()
        .stdout(
            "\
URL:                  https://example.com/jobs/rust-engineer
Last update notice:   2026-11-12T10:00:00Z
Last removal notice:  never
",
        );
}

#[test]
fn status_of_a_never_notified_url_suggests_publishing() {
    let mut api = MockApi::new();
    api.on("GET", METADATA)
        .match_query(Matcher::Any)
        .with_status(404)
        .with_body(r#"{"error":{"code":404,"message":"Requested entity was not found.","status":"NOT_FOUND"}}"#)
        .create();
    let err = api.error(&["indexing", "status", JOB], 4);
    assert_eq!(err["message"], "Requested entity was not found.");
    assert!(
        err["hint"]
            .as_str()
            .unwrap()
            .contains("gsc indexing publish")
    );
}

#[test]
fn usage_errors_are_caught_before_any_request() {
    let api = MockApi::new();
    assert_eq!(
        api.error(&["indexing", "publish"], 2)["message"],
        "no URLs to publish"
    );
    assert_eq!(
        api.error(&["indexing", "publish", "/jobs/1"], 2)["message"],
        "`/jobs/1` is not a full URL"
    );
    assert_eq!(
        api.error(&["indexing", "status", "example.com/x"], 2)["message"],
        "`example.com/x` is not a full URL"
    );
}

#[test]
fn a_browser_login_without_the_indexing_scope_is_told_how_to_add_it() {
    let env = common::TestEnv::new();
    std::fs::write(
        env.config_dir.path().join("credentials.json"),
        json!({
            "type": "authorized_user",
            "client_id": "c", "client_secret": "s", "refresh_token": "r",
            "token_uri": "http://127.0.0.1:1/token",
            "scopes": ["https://www.googleapis.com/auth/webmasters"],
        })
        .to_string(),
    )
    .unwrap();
    env.gsc()
        .args(["indexing", "publish", JOB, "--format", "table"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("gsc auth login --indexing"));
}
