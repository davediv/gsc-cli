//! `gsc inspect` against a mock URL Inspection endpoint.

mod common;

use common::MockApi;
use mockito::Matcher;
use serde_json::{Value, json};

const SITE: &str = "sc-domain:example.com";
const ENDPOINT: &str = "/v1/urlInspection/index:inspect";
const PAGE: &str = "https://example.com/pricing";

fn response(index_status: Value) -> String {
    json!({ "inspectionResult": {
        "inspectionResultLink": "https://search.google.com/search-console/inspect?resource_id=sc-domain:example.com",
        "indexStatusResult": index_status,
    }})
    .to_string()
}

fn indexed() -> Value {
    json!({
        "verdict": "PASS",
        "coverageState": "Submitted and indexed",
        "robotsTxtState": "ALLOWED",
        "indexingState": "INDEXING_ALLOWED",
        "lastCrawlTime": "2026-11-08T10:22:33Z",
        "pageFetchState": "SUCCESSFUL",
        "googleCanonical": PAGE,
        "userCanonical": PAGE,
        "sitemap": ["https://example.com/sitemap.xml"],
        "crawledAs": "MOBILE",
    })
}

fn noindex() -> Value {
    json!({
        "verdict": "NEUTRAL",
        "coverageState": "Excluded by 'noindex' tag",
        "robotsTxtState": "ALLOWED",
        "indexingState": "BLOCKED_BY_META_TAG",
        "lastCrawlTime": "2026-11-01T00:00:00Z",
        "pageFetchState": "SUCCESSFUL",
        "crawledAs": "MOBILE",
    })
}

#[test]
fn inspect_sends_the_url_and_property_and_summarises_a_healthy_page() {
    let mut api = MockApi::new();
    let inspect = api
        .on("POST", ENDPOINT)
        .match_body(Matcher::Json(json!({
            "inspectionUrl": PAGE,
            "siteUrl": SITE,
            "languageCode": "en-US",
        })))
        .with_body(response(indexed()))
        .create();

    let doc = api.json(&["inspect", "url", PAGE, "--site", SITE]);
    inspect.assert();
    assert_eq!(
        doc,
        json!({
            "url": PAGE,
            "indexed": true,
            "verdict": "PASS",
            "coverageState": "Submitted and indexed",
            "lastCrawlTime": "2026-11-08T10:22:33Z",
            "crawledAs": "MOBILE",
            "robotsTxtState": "ALLOWED",
            "indexingState": "INDEXING_ALLOWED",
            "pageFetchState": "SUCCESSFUL",
            "googleCanonical": PAGE,
            "userCanonical": PAGE,
            "sitemaps": ["https://example.com/sitemap.xml"],
            "inspectionLink": "https://search.google.com/search-console/inspect?resource_id=sc-domain:example.com",
            "issues": [],
            "site": SITE,
            "notes": ["No issues found."],
        })
    );
}

#[test]
fn a_blocked_page_reports_the_issue_with_a_fix() {
    let mut api = MockApi::new();
    api.on("POST", ENDPOINT)
        .with_body(response(noindex()))
        .create();

    let doc = api.json(&["inspect", "url", PAGE, "--site", SITE]);
    assert_eq!(doc["indexed"], false);
    assert_eq!(doc["issues"].as_array().unwrap().len(), 1);
    let issue = &doc["issues"][0];
    assert_eq!(issue["code"], "noindex_meta_tag");
    assert_eq!(issue["severity"], "error");
    assert_eq!(
        issue["message"],
        "The page has a robots meta tag containing noindex."
    );
    assert!(issue["fix"].as_str().unwrap().contains("Remove `noindex`"));
    assert_eq!(
        doc.get("notes"),
        None,
        "no 'no issues' note when there are issues"
    );

    let table = api
        .gsc()
        .args(["inspect", "url", PAGE, "--site", SITE, "--format", "table"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let table = String::from_utf8(table).unwrap();
    assert!(table.starts_with(
        "\
URL:         https://example.com/pricing
Property:    sc-domain:example.com
Indexed:     no
Coverage:    Excluded by 'noindex' tag
Last crawl:  2026-11-01T00:00:00Z
"
    ));
    assert!(table.contains("\nSEVERITY  ISSUE             WHAT IS WRONG"));
    assert!(table.contains(
        "\nerror     noindex_meta_tag  The page has a robots meta tag containing noindex."
    ));

    api.gsc()
        .args(["inspect", "url", PAGE, "--site", SITE, "--format", "csv"])
        .assert()
        .success()
        .stdout(predicates::str::starts_with(
            "severity,code,message,fix\nerror,noindex_meta_tag,The page has a robots meta tag containing noindex.,",
        ));
}

#[test]
fn raw_flag_includes_googles_full_response() {
    let mut api = MockApi::new();
    api.on("POST", ENDPOINT)
        .with_body(response(indexed()))
        .create();

    let without = api.json(&["inspect", "url", PAGE, "--site", SITE]);
    assert_eq!(without.get("raw"), None);

    let with = api.json(&["inspect", "url", PAGE, "--site", SITE, "--raw"]);
    assert_eq!(with["raw"]["indexStatusResult"], indexed());
}

#[test]
fn a_path_is_resolved_against_a_url_prefix_property() {
    let mut api = MockApi::new();
    let inspect = api
        .on("POST", ENDPOINT)
        .match_body(Matcher::PartialJson(json!({
            "inspectionUrl": "https://example.com/pricing",
            "siteUrl": "https://example.com/",
        })))
        .with_body(response(indexed()))
        .create();

    let doc = api.json(&[
        "inspect",
        "url",
        "/pricing",
        "--site",
        "https://example.com/",
    ]);
    inspect.assert();
    assert_eq!(doc["url"], PAGE);
}

#[test]
fn a_path_needs_a_full_url_on_a_domain_property() {
    let api = MockApi::new();
    let err = api.error(&["inspect", "url", "/pricing", "--site", SITE], 2);
    assert_eq!(
        err["message"],
        "`/pricing` is not a full URL, and sc-domain:example.com is a Domain property"
    );
    assert_eq!(err["hint"], "give the full URL, starting with https://");
}

#[test]
fn a_url_outside_the_property_surfaces_googles_error() {
    let mut api = MockApi::new();
    api.on("POST", ENDPOINT)
        .with_status(403)
        .with_body(r#"{"error":{"code":403,"message":"You do not own this site, or the inspected URL is not part of this property.","status":"PERMISSION_DENIED"}}"#)
        .create();
    let err = api.error(&["inspect", "url", "https://other.com/", "--site", SITE], 4);
    assert_eq!(
        err["message"],
        "You do not own this site, or the inspected URL is not part of this property."
    );
}

#[test]
fn inspection_quota_exhaustion_exits_5() {
    let mut api = MockApi::new();
    api.on("POST", ENDPOINT)
        .with_status(429)
        .with_body(r#"{"error":{"code":429,"message":"Quota exceeded for sc-domain:example.com.","status":"RESOURCE_EXHAUSTED"}}"#)
        .create();
    let err = api.error(&["inspect", "url", PAGE, "--site", SITE], 5);
    assert!(
        err["hint"]
            .as_str()
            .unwrap()
            .contains("2,000 requests per day")
    );
}
