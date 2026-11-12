//! `gsc inspect batch` against a mock API that also serves sitemaps.

mod common;

use std::io::Write;

use common::MockApi;
use mockito::Matcher;
use serde_json::{Value, json};

const SITE: &str = "sc-domain:example.com";
const ENDPOINT: &str = "/v1/urlInspection/index:inspect";

/// The index status the mock reports, chosen by the URL's last path segment.
fn status_for(url: &str) -> Value {
    let common = json!({ "lastCrawlTime": "2026-11-08T10:22:33Z", "crawledAs": "MOBILE" });
    let specific = if url.ends_with("/noindex") {
        json!({
            "verdict": "NEUTRAL",
            "coverageState": "Excluded by 'noindex' tag",
            "indexingState": "BLOCKED_BY_META_TAG",
            "pageFetchState": "SUCCESSFUL",
        })
    } else if url.contains("/thin") {
        json!({
            "verdict": "NEUTRAL",
            "coverageState": "Crawled - currently not indexed",
            "indexingState": "INDEXING_ALLOWED",
            "pageFetchState": "SUCCESSFUL",
        })
    } else if url.ends_with("/moved") {
        json!({
            "verdict": "NEUTRAL",
            "coverageState": "Page with redirect",
            "pageFetchState": "SUCCESSFUL",
        })
    } else {
        json!({
            "verdict": "PASS",
            "coverageState": "Submitted and indexed",
            "indexingState": "INDEXING_ALLOWED",
            "pageFetchState": "SUCCESSFUL",
            "googleCanonical": url,
            "userCanonical": url,
        })
    };
    let mut merged = common;
    merged
        .as_object_mut()
        .unwrap()
        .extend(specific.as_object().unwrap().clone());
    merged
}

/// An inspection endpoint that answers according to [`status_for`].
fn inspection_endpoint(api: &mut MockApi) -> mockito::Mock {
    api.on("POST", ENDPOINT).with_body_from_request(|request| {
        let body: Value = serde_json::from_slice(request.body().unwrap()).unwrap();
        assert_eq!(body["siteUrl"], SITE);
        let url = body["inspectionUrl"].as_str().unwrap();
        json!({ "inspectionResult": { "indexStatusResult": status_for(url) } })
            .to_string()
            .into_bytes()
    })
}

fn url_set(urls: &[&str]) -> String {
    let entries: String = urls
        .iter()
        .map(|url| format!("<url><loc>{url}</loc></url>"))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">{entries}</urlset>"#
    )
}

fn sitemap_index(children: &[String]) -> String {
    let entries: String = children
        .iter()
        .map(|url| format!("<sitemap><loc>{url}</loc></sitemap>"))
        .collect();
    format!(
        r#"<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">{entries}</sitemapindex>"#
    )
}

fn gzip(text: &str) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(text.as_bytes()).unwrap();
    encoder.finish().unwrap()
}

fn urls_of(rows: &Value) -> Vec<&str> {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|row| row["url"].as_str().unwrap())
        .collect()
}

#[test]
fn follows_a_sitemap_index_and_groups_what_is_wrong() {
    let mut api = MockApi::new();
    let base = api.server.url();
    api.server
        .mock("GET", "/sitemap_index.xml")
        .with_body(sitemap_index(&[
            format!("{base}/pages.xml"),
            format!("{base}/posts.xml.gz"),
        ]))
        .create();
    api.server
        .mock("GET", "/pages.xml")
        .with_body(url_set(&[
            "https://example.com/",
            "https://example.com/noindex",
            "https://example.com/thin-1",
        ]))
        .create();
    api.server
        .mock("GET", "/posts.xml.gz")
        .with_header("content-type", "application/gzip")
        .with_body(gzip(&url_set(&[
            "https://example.com/thin-2",
            "https://example.com/moved",
            // Listed twice across sitemaps: inspected once.
            "https://example.com/",
        ])))
        .create();
    let inspections = inspection_endpoint(&mut api).expect(5).create();

    let doc = api.json(&[
        "inspect",
        "batch",
        "--site",
        SITE,
        "--sitemap",
        &format!("{base}/sitemap_index.xml"),
    ]);
    inspections.assert();

    assert_eq!(
        doc["summary"],
        json!({
            "urls": 5, "inspected": 5, "indexed": 1, "notIndexed": 4,
            "withIssues": 3, "failed": 0, "skipped": 0,
        }),
        "the redirect is informational, so it does not count as an issue"
    );
    assert_eq!(doc["truncated"], false);

    // Results keep sitemap order.
    assert_eq!(
        urls_of(&doc["results"]),
        [
            "https://example.com/",
            "https://example.com/noindex",
            "https://example.com/thin-1",
            "https://example.com/thin-2",
            "https://example.com/moved",
        ]
    );
    assert_eq!(
        doc["results"][0],
        json!({
            "url": "https://example.com/",
            "indexed": true,
            "coverageState": "Submitted and indexed",
            "lastCrawlTime": "2026-11-08T10:22:33Z",
            "issues": [],
        }),
        "canonicals equal to the URL are left out"
    );
    assert_eq!(doc["results"][1]["issues"], json!(["noindex_meta_tag"]));

    // Issues: errors first, then by how many URLs they affect.
    let groups: Vec<(&str, &str, u64)> = doc["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| {
            (
                g["code"].as_str().unwrap(),
                g["severity"].as_str().unwrap(),
                g["count"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        groups,
        [
            ("noindex_meta_tag", "error", 1),
            ("crawled_not_indexed", "warning", 2),
            ("redirected", "info", 1),
        ]
    );
    assert_eq!(
        doc["issues"][1]["urls"],
        json!(["https://example.com/thin-1", "https://example.com/thin-2"])
    );
    assert!(
        doc["issues"][1]["fix"]
            .as_str()
            .unwrap()
            .contains("request indexing")
    );
}

#[test]
fn only_issues_hides_healthy_and_informational_urls() {
    let mut api = MockApi::new();
    inspection_endpoint(&mut api).create();

    let doc = api.json(&[
        "inspect",
        "batch",
        "--site",
        SITE,
        "--only-issues",
        "https://example.com/",
        "https://example.com/noindex",
        "https://example.com/moved",
    ]);
    assert_eq!(urls_of(&doc["results"]), ["https://example.com/noindex"]);
    assert_eq!(
        doc["summary"]["inspected"], 3,
        "the summary still covers everything"
    );
}

#[test]
fn limit_caps_requests_and_reports_truncation() {
    let mut api = MockApi::new();
    let base = api.server.url();
    let many: Vec<String> = (0..10)
        .map(|i| format!("https://example.com/page-{i}"))
        .collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    api.server
        .mock("GET", "/sitemap.xml")
        .with_body(url_set(&many))
        .create();
    let inspections = inspection_endpoint(&mut api).expect(3).create();

    let doc = api.json(&[
        "inspect",
        "batch",
        "--site",
        SITE,
        "--sitemap",
        &format!("{base}/sitemap.xml"),
        "-n",
        "3",
    ]);
    inspections.assert();
    assert_eq!(doc["summary"]["urls"], 3);
    assert_eq!(doc["truncated"], true);
    assert_eq!(
        urls_of(&doc["results"]),
        [
            "https://example.com/page-0",
            "https://example.com/page-1",
            "https://example.com/page-2"
        ]
    );
    assert!(doc["notes"].as_array().unwrap().iter().any(|n| {
        n.as_str()
            .unwrap()
            .contains("Stopped selecting at the limit of 3 URLs")
    }));
}

#[test]
fn an_exact_fit_is_not_reported_as_truncated() {
    let mut api = MockApi::new();
    let base = api.server.url();
    api.server
        .mock("GET", "/sitemap.xml")
        .with_body(url_set(&["https://example.com/a", "https://example.com/b"]))
        .create();
    inspection_endpoint(&mut api).create();

    let doc = api.json(&[
        "inspect",
        "batch",
        "--site",
        SITE,
        "--sitemap",
        &format!("{base}/sitemap.xml"),
        "-n",
        "2",
    ]);
    assert_eq!(doc["summary"]["urls"], 2);
    assert_eq!(doc["truncated"], false);
    assert_eq!(
        doc["notes"],
        json!(["No issues found in the inspected URLs."])
    );
}

#[test]
fn urls_come_from_arguments_and_a_file_without_duplicates() {
    let mut api = MockApi::new();
    let inspections = inspection_endpoint(&mut api).expect(3).create();
    let file = api.env.config_dir.path().join("urls.txt");
    std::fs::write(
        &file,
        "# from the crawl\nhttps://example.com/b\n\nhttps://example.com/a\nhttps://example.com/c\n",
    )
    .unwrap();

    let doc = api.json(&[
        "inspect",
        "batch",
        "--site",
        SITE,
        "https://example.com/a",
        "--file",
        file.to_str().unwrap(),
    ]);
    inspections.assert();
    assert_eq!(
        urls_of(&doc["results"]),
        [
            "https://example.com/a",
            "https://example.com/b",
            "https://example.com/c"
        ]
    );
}

#[test]
fn urls_can_be_piped_on_stdin() {
    let mut api = MockApi::new();
    inspection_endpoint(&mut api).create();
    let output = api
        .gsc()
        .args(["inspect", "batch", "--site", SITE, "--file", "-"])
        .write_stdin("https://example.com/x\nhttps://example.com/noindex\n")
        .assert()
        .success()
        .get_output()
        .clone();
    let doc = common::parse_json(&output.stdout);
    assert_eq!(doc["summary"]["inspected"], 2);
    assert_eq!(doc["summary"]["withIssues"], 1);
}

#[test]
fn submitted_sitemaps_are_discovered_through_the_api() {
    let mut api = MockApi::new();
    let base = api.server.url();
    let listed = api
        .on(
            "GET",
            "/webmasters/v3/sites/sc-domain%3Aexample.com/sitemaps",
        )
        .with_body(json!({ "sitemap": [{ "path": format!("{base}/sitemap.xml") }] }).to_string())
        .create();
    api.server
        .mock("GET", "/sitemap.xml")
        .with_body(url_set(&["https://example.com/only"]))
        .create();
    inspection_endpoint(&mut api).create();

    let doc = api.json(&["inspect", "batch", "--site", SITE, "--submitted-sitemaps"]);
    listed.assert();
    assert_eq!(urls_of(&doc["results"]), ["https://example.com/only"]);
}

#[test]
fn quota_exhaustion_stops_early_and_says_what_was_left() {
    let mut api = MockApi::new();
    // Registered first, so it wins for the URL that trips the quota.
    api.on("POST", ENDPOINT)
        .match_body(Matcher::PartialJson(
            json!({ "inspectionUrl": "https://example.com/c" }),
        ))
        .with_status(429)
        .with_body(
            r#"{"error":{"code":429,"message":"Quota exceeded","status":"RESOURCE_EXHAUSTED"}}"#,
        )
        .expect_at_least(1)
        .create();
    let others = api
        .on("POST", ENDPOINT)
        .match_body(Matcher::Regex(
            r#""inspectionUrl":"https://example.com/[abde]""#.into(),
        ))
        .with_body_from_request(|request| {
            let body: Value = serde_json::from_slice(request.body().unwrap()).unwrap();
            json!({ "inspectionResult": {
                "indexStatusResult": status_for(body["inspectionUrl"].as_str().unwrap()),
            }})
            .to_string()
            .into_bytes()
        })
        .expect(2)
        .create();

    // One at a time, so the order of events is fixed.
    let doc = api.json(&[
        "inspect",
        "batch",
        "--site",
        SITE,
        "--concurrency",
        "1",
        "https://example.com/a",
        "https://example.com/b",
        "https://example.com/c",
        "https://example.com/d",
        "https://example.com/e",
    ]);
    others.assert();
    assert_eq!(doc["summary"]["inspected"], 2);
    assert_eq!(doc["summary"]["skipped"], 3);
    assert_eq!(
        doc["results"][2],
        json!({
            "url": "https://example.com/c", "skipped": true, "issues": ["not_inspected"],
        })
    );
    assert_eq!(
        doc["results"][4]["skipped"], true,
        "/d and /e were never requested"
    );
    assert!(doc["notes"].as_array().unwrap().iter().any(|n| {
        n.as_str()
            .unwrap()
            .contains("quota ran out with 3 URLs left uninspected")
    }));
}

#[test]
fn a_url_that_cannot_be_inspected_is_reported_alongside_the_rest() {
    let mut api = MockApi::new();
    api.on("POST", ENDPOINT)
        .match_body(Matcher::PartialJson(json!({ "inspectionUrl": "https://other.org/x" })))
        .with_status(403)
        .with_body(r#"{"error":{"code":403,"message":"The inspected URL is not part of this property.","status":"PERMISSION_DENIED"}}"#)
        .create();
    inspection_endpoint(&mut api).create();

    let doc = api.json(&[
        "inspect",
        "batch",
        "--site",
        SITE,
        "https://example.com/ok",
        "https://other.org/x",
    ]);
    assert_eq!(doc["summary"]["inspected"], 1);
    assert_eq!(doc["summary"]["failed"], 1);
    assert_eq!(
        doc["results"][1],
        json!({
            "url": "https://other.org/x",
            "error": "The inspected URL is not part of this property.",
            "issues": ["inspection_failed"],
        })
    );
    assert_eq!(doc["issues"][0]["code"], "inspection_failed");
    assert_eq!(doc["issues"][0]["urls"], json!(["https://other.org/x"]));
}

#[test]
fn when_nothing_can_be_inspected_the_cause_is_the_error() {
    let mut api = MockApi::new();
    api.on("POST", ENDPOINT)
        .with_status(403)
        .with_body(r#"{"error":{"code":403,"message":"You do not own this site.","status":"PERMISSION_DENIED"}}"#)
        .create();
    let err = api.error(
        &["inspect", "batch", "--site", SITE, "https://example.com/a"],
        4,
    );
    assert_eq!(err["message"], "You do not own this site.");

    let mut api = MockApi::new();
    api.on("POST", ENDPOINT)
        .with_status(429)
        .with_body(
            r#"{"error":{"code":429,"message":"Quota exceeded","status":"RESOURCE_EXHAUSTED"}}"#,
        )
        .create();
    let err = api.error(
        &["inspect", "batch", "--site", SITE, "https://example.com/a"],
        5,
    );
    assert_eq!(err["kind"], "rate_limit");
}

#[test]
fn an_unreadable_sitemap_is_an_error_but_a_partly_readable_index_is_not() {
    let mut api = MockApi::new();
    let base = api.server.url();
    api.server
        .mock("GET", "/missing.xml")
        .with_status(404)
        .create();
    let err = api.error(
        &[
            "inspect",
            "batch",
            "--site",
            SITE,
            "--sitemap",
            &format!("{base}/missing.xml"),
        ],
        1,
    );
    assert_eq!(
        err["message"],
        format!(
            "could not read the sitemap {base}/missing.xml: the server answered HTTP 404 Not Found"
        )
    );

    api.server
        .mock("GET", "/index.xml")
        .with_body(sitemap_index(&[
            format!("{base}/missing.xml"),
            format!("{base}/good.xml"),
        ]))
        .create();
    api.server
        .mock("GET", "/good.xml")
        .with_body(url_set(&["https://example.com/a"]))
        .create();
    inspection_endpoint(&mut api).create();
    let doc = api.json(&[
        "inspect",
        "batch",
        "--site",
        SITE,
        "--sitemap",
        &format!("{base}/index.xml"),
    ]);
    assert_eq!(doc["summary"]["inspected"], 1);
    assert!(doc["notes"].as_array().unwrap().iter().any(|n| {
        n.as_str()
            .unwrap()
            .contains("/missing.xml: the server answered HTTP 404")
    }));
}

#[test]
fn asking_for_nothing_is_a_usage_error() {
    let api = MockApi::new();
    let err = api.error(&["inspect", "batch", "--site", SITE], 2);
    assert_eq!(err["message"], "no URLs to inspect");
    assert!(
        err["hint"]
            .as_str()
            .unwrap()
            .contains("--submitted-sitemaps")
    );

    api.gsc()
        .args([
            "inspect",
            "batch",
            "--site",
            SITE,
            "-n",
            "0",
            "https://example.com/",
        ])
        .assert()
        .code(2);
}

#[test]
fn table_output_lists_urls_then_issues() {
    let mut api = MockApi::new();
    inspection_endpoint(&mut api).create();
    let output = api
        .gsc()
        .args([
            "inspect",
            "batch",
            "--site",
            SITE,
            "--format",
            "table",
            "https://example.com/",
            "https://example.com/noindex",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();
    assert!(
        text.starts_with(
            "\
Property:     sc-domain:example.com
Inspected:    2 of 2 URLs
Indexed:      1
Not indexed:  1
With issues:  1

URL                          INDEXED  COVERAGE                   LAST CRAWL  ISSUES
https://example.com/         yes      Submitted and indexed      2026-11-08  -
https://example.com/noindex  no       Excluded by 'noindex' tag  2026-11-08  noindex_meta_tag

Issues:
SEVERITY  ISSUE             URLS  WHAT IS WRONG"
        ),
        "{text}"
    );

    api.gsc()
        .args([
            "inspect",
            "batch",
            "--site",
            SITE,
            "--format",
            "csv",
            "https://example.com/",
            "https://example.com/noindex",
        ])
        .assert()
        .success()
        .stdout(
            "\
url,indexed,coverageState,lastCrawlTime,issues
https://example.com/,true,Submitted and indexed,2026-11-08T10:22:33Z,
https://example.com/noindex,false,Excluded by 'noindex' tag,2026-11-08T10:22:33Z,noindex_meta_tag
",
        );
}
