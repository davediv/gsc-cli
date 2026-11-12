//! `gsc sitemaps` against a mock API.

mod common;

use common::MockApi;
use mockito::Matcher;
use serde_json::json;

const DOMAIN: &str = "sc-domain:example.com";
const PREFIX: &str = "https://example.com/";
const DOMAIN_SITEMAPS: &str = "/webmasters/v3/sites/sc-domain%3Aexample.com/sitemaps";
const PREFIX_SITEMAPS: &str = "/webmasters/v3/sites/https%3A%2F%2Fexample.com%2F/sitemaps";
const SITEMAP_URL: &str = "https://example.com/sitemap.xml";
const ENCODED_SITEMAP: &str = "https%3A%2F%2Fexample.com%2Fsitemap.xml";

fn healthy() -> serde_json::Value {
    json!({
        "path": SITEMAP_URL,
        "lastSubmitted": "2026-10-01T08:00:00.000Z",
        "isPending": false,
        "isSitemapsIndex": false,
        "type": "sitemap",
        "lastDownloaded": "2026-11-10T03:12:45.000Z",
        "warnings": "0",
        "errors": "0",
        "contents": [
            { "type": "web", "submitted": "1200", "indexed": "0" },
            { "type": "image", "submitted": "40", "indexed": "0" },
        ],
    })
}

fn broken() -> serde_json::Value {
    json!({
        "path": "https://example.com/old-sitemap.xml",
        "lastSubmitted": "2025-01-15T10:00:00.000Z",
        "isPending": false,
        "isSitemapsIndex": false,
        "type": "sitemap",
        "warnings": "3",
        "errors": "2",
    })
}

#[test]
fn list_normalises_counts_and_flags_errors() {
    let mut api = MockApi::new();
    api.on("GET", DOMAIN_SITEMAPS)
        .with_body(json!({ "sitemap": [healthy(), broken()] }).to_string())
        .create();

    let doc = api.json(&["sitemaps", "list", "--site", DOMAIN]);
    assert_eq!(doc["site"], DOMAIN);
    assert_eq!(doc["sitemaps"][0]["submitted"], 1240);
    assert_eq!(doc["sitemaps"][0]["errors"], 0);
    assert_eq!(
        doc["sitemaps"][0]["contents"][0],
        json!({ "type": "web", "submitted": 1200 })
    );
    assert_eq!(doc["sitemaps"][1]["errors"], 2);
    assert_eq!(doc["sitemaps"][1]["warnings"], 3);
    assert_eq!(doc["sitemaps"][1]["lastDownloaded"], json!(null));
    assert!(
        doc["notes"][0]
            .as_str()
            .unwrap()
            .contains("only reports error counts"),
        "the caller is told the API cannot explain the errors"
    );

    api.gsc()
        .args(["sitemaps", "list", "--site", DOMAIN, "--format", "table"])
        .assert()
        .success()
        .stdout(predicates::str::starts_with(
            "\
Property:  sc-domain:example.com

SITEMAP                              TYPE      URLS  ERRORS  WARNINGS  LAST READ   PENDING
https://example.com/sitemap.xml      sitemap  1,240       0         0  2026-11-10  no
https://example.com/old-sitemap.xml  sitemap      0       2         3  -           no
",
        ));
}

#[test]
fn list_can_be_scoped_to_a_sitemap_index() {
    let mut api = MockApi::new();
    let scoped = api
        .on("GET", DOMAIN_SITEMAPS)
        .match_query(Matcher::UrlEncoded(
            "sitemapIndex".into(),
            "https://example.com/sitemap_index.xml".into(),
        ))
        .with_body("{}")
        .create();

    let doc = api.json(&[
        "sitemaps",
        "list",
        "--site",
        DOMAIN,
        "--index",
        "https://example.com/sitemap_index.xml",
    ]);
    scoped.assert();
    assert_eq!(doc["sitemaps"], json!([]));
    assert!(
        doc["notes"][0]
            .as_str()
            .unwrap()
            .contains("gsc sitemaps submit")
    );
}

#[test]
fn get_shows_status_and_contents() {
    let mut api = MockApi::new();
    let get = api
        .on(
            "GET",
            format!("{DOMAIN_SITEMAPS}/{ENCODED_SITEMAP}").as_str(),
        )
        .with_body(healthy().to_string())
        .create();

    let doc = api.json(&["sitemaps", "get", SITEMAP_URL, "--site", DOMAIN]);
    get.assert();
    assert_eq!(doc["path"], SITEMAP_URL);
    assert_eq!(doc["site"], DOMAIN);
    assert_eq!(doc["submitted"], 1240);

    api.gsc()
        .args([
            "sitemaps",
            "get",
            SITEMAP_URL,
            "--site",
            DOMAIN,
            "--format",
            "table",
        ])
        .assert()
        .success()
        .stdout(
            "\
Sitemap:         https://example.com/sitemap.xml
Property:        sc-domain:example.com
Status:          processed
Type:            sitemap
Errors:          0
Warnings:        0
Last submitted:  2026-10-01T08:00:00.000Z
Last read:       2026-11-10T03:12:45.000Z

CONTENT   URLS
web      1,200
image       40
",
        );
}

#[test]
fn relative_paths_resolve_against_a_url_prefix_property() {
    let mut api = MockApi::new();
    let put = api
        .on(
            "PUT",
            format!("{PREFIX_SITEMAPS}/{ENCODED_SITEMAP}").as_str(),
        )
        .match_header("content-length", "0")
        .with_status(204)
        .create();

    let doc = api.json(&["sitemaps", "submit", "/sitemap.xml", "--site", PREFIX]);
    put.assert();
    assert_eq!(
        doc["sitemap"], SITEMAP_URL,
        "the path was joined to the property URL"
    );
    assert_eq!(doc["submitted"], true);
    assert!(
        doc["notes"][0]
            .as_str()
            .unwrap()
            .contains("gsc sitemaps get")
    );
}

#[test]
fn relative_paths_are_rejected_for_a_domain_property() {
    let api = MockApi::new();
    let err = api.error(&["sitemaps", "submit", "/sitemap.xml", "--site", DOMAIN], 2);
    assert_eq!(
        err["message"],
        "`/sitemap.xml` is not a full URL, and sc-domain:example.com is a Domain property"
    );
}

#[test]
fn delete_requires_confirmation() {
    let mut api = MockApi::new();
    let delete = api
        .on(
            "DELETE",
            format!("{DOMAIN_SITEMAPS}/{ENCODED_SITEMAP}").as_str(),
        )
        .with_status(204)
        .expect(1)
        .create();

    let err = api.error(&["sitemaps", "delete", SITEMAP_URL, "--site", DOMAIN], 2);
    assert_eq!(
        err["message"],
        "refusing to remove https://example.com/sitemap.xml without confirmation"
    );
    assert!(!delete.matched());

    let doc = api.json(&["sitemaps", "delete", SITEMAP_URL, "--site", DOMAIN, "--yes"]);
    assert_eq!(doc["deleted"], true);
    delete.assert();
}

#[test]
fn unknown_sitemap_is_a_not_found_error() {
    let mut api = MockApi::new();
    api.on(
        "GET",
        format!("{DOMAIN_SITEMAPS}/{ENCODED_SITEMAP}").as_str(),
    )
    .with_status(404)
    .with_body(r#"{"error":{"code":404,"message":"Sitemap not found.","status":"NOT_FOUND"}}"#)
    .create();
    let err = api.error(&["sitemaps", "get", SITEMAP_URL, "--site", DOMAIN], 4);
    assert_eq!(err["message"], "Sitemap not found.");
    assert_eq!(err["status"], 404);
}
