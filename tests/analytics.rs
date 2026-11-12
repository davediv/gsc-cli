//! `gsc analytics` against a mock Search Analytics endpoint.

mod common;

use chrono::NaiveDate;
use common::MockApi;
use mockito::Matcher;
use serde_json::{Value, json};

const SITE: &str = "sc-domain:example.com";
const ENDPOINT: &str = "/webmasters/v3/sites/sc-domain%3Aexample.com/searchAnalytics/query";
const PERIOD: [&str; 4] = ["--start", "2026-10-01", "--end", "2026-10-28"];

fn api_row(keys: &[&str], clicks: u32, impressions: u32, position: f64) -> Value {
    json!({
        "keys": keys,
        "clicks": clicks,
        "impressions": impressions,
        "ctr": f64::from(clicks) / f64::from(impressions),
        "position": position,
    })
}

/// Run an analytics subcommand for the test property and period.
fn run(api: &MockApi, args: &[&str]) -> Value {
    let mut full = vec!["analytics"];
    full.extend_from_slice(args);
    full.extend_from_slice(&["--site", SITE]);
    api.json(&full)
}

#[test]
fn keywords_sends_the_expected_request_and_flattens_rows() {
    let mut api = MockApi::new();
    let query = api
        .on("POST", ENDPOINT)
        .match_body(Matcher::Json(json!({
            "startDate": "2026-10-01",
            "endDate": "2026-10-28",
            "type": "web",
            "dimensions": ["query"],
            // One more than the 100 shown, to learn whether more exist.
            "rowLimit": 101,
            "startRow": 0,
        })))
        .with_body(
            json!({
                "rows": [
                    api_row(&["rust cli"], 120, 3400, 4.238461),
                    api_row(&["search console api"], 7, 900, 11.5),
                ],
                "responseAggregationType": "byProperty",
            })
            .to_string(),
        )
        .create();

    let mut args = vec!["keywords"];
    args.extend_from_slice(&PERIOD);
    let doc = run(&api, &args);
    query.assert();

    assert_eq!(
        doc,
        json!({
            "site": SITE,
            "startDate": "2026-10-01",
            "endDate": "2026-10-28",
            "type": "web",
            "dimensions": ["query"],
            "aggregation": "byProperty",
            "rowCount": 2,
            "truncated": false,
            "rows": [
                { "query": "rust cli", "clicks": 120, "impressions": 3400,
                  "ctr": 0.0353, "position": 4.24 },
                { "query": "search console api", "clicks": 7, "impressions": 900,
                  "ctr": 0.0078, "position": 11.5 },
            ],
        })
    );
}

#[test]
fn table_and_csv_render_the_same_rows() {
    let mut api = MockApi::new();
    api.on("POST", ENDPOINT)
        .with_body(
            json!({ "rows": [
                api_row(&["https://example.com/"], 1500, 40000, 3.24),
                api_row(&["https://example.com/pricing"], 80, 2000, 12.0),
            ]})
            .to_string(),
        )
        .create();

    let base = ["analytics", "pages", "--site", SITE];
    api.gsc()
        .args(base)
        .args(PERIOD)
        .args(["--format", "table"])
        .assert()
        .success()
        .stdout(
            "\
Property:  sc-domain:example.com
Period:    2026-10-01 to 2026-10-28 (28 days)

PAGE                         CLICKS  IMPRESSIONS    CTR  POSITION
https://example.com/          1,500       40,000  3.75%       3.2
https://example.com/pricing      80        2,000  4.00%      12.0
",
        );

    api.gsc()
        .args(base)
        .args(PERIOD)
        .args(["--format", "csv"])
        .assert()
        .success()
        .stdout(
            "\
page,clicks,impressions,ctr,position
https://example.com/,1500,40000,0.0375,3.24
https://example.com/pricing,80,2000,0.04,12.0
",
        );
}

#[test]
fn filters_types_and_data_state_reach_the_api() {
    let mut api = MockApi::new();
    let query = api
        .on("POST", ENDPOINT)
        .match_body(Matcher::Json(json!({
            "startDate": "2026-10-01",
            "endDate": "2026-10-28",
            "type": "image",
            "dimensions": ["query", "page"],
            "dimensionFilterGroups": [{ "groupType": "and", "filters": [
                { "dimension": "query", "operator": "includingRegex", "expression": "^how (to|do)" },
                { "dimension": "page", "operator": "contains", "expression": "/blog/" },
                { "dimension": "country", "operator": "equals", "expression": "usa" },
                { "dimension": "device", "operator": "equals", "expression": "MOBILE" },
            ]}],
            "aggregationType": "byPage",
            "dataState": "all",
            "rowLimit": 11,
            "startRow": 20,
        })))
        .with_body(r#"{"rows":[]}"#)
        .create();

    let mut args = vec![
        "query",
        "-d",
        "query,page",
        "--type",
        "image",
        "--filter",
        "query regex ^how (to|do)",
        "--page",
        "/blog/",
        "--country",
        "USA",
        "--device",
        "mobile",
        "--aggregation",
        "by-page",
        "--data-state",
        "all",
        "--limit",
        "10",
        "--offset",
        "20",
    ];
    args.extend_from_slice(&PERIOD);
    let doc = run(&api, &args);
    query.assert();

    assert_eq!(doc["dataState"], "all");
    assert_eq!(doc["filters"].as_array().unwrap().len(), 4);
    assert_eq!(doc["rows"], json!([]));
    assert!(
        doc["notes"][0]
            .as_str()
            .unwrap()
            .starts_with("No search data matched")
    );
}

#[test]
fn summary_returns_one_row_of_totals() {
    let mut api = MockApi::new();
    let query = api
        .on("POST", ENDPOINT)
        .match_body(Matcher::Json(json!({
            "startDate": "2026-10-01",
            "endDate": "2026-10-28",
            "type": "web",
            "rowLimit": 1,
            "startRow": 0,
        })))
        .with_body(json!({ "rows": [api_row(&[], 4321, 98765, 14.27)] }).to_string())
        .create();

    let mut args = vec!["summary"];
    args.extend_from_slice(&PERIOD);
    let doc = run(&api, &args);
    query.assert();
    assert_eq!(doc["dimensions"], json!([]));
    assert_eq!(
        doc["rows"],
        json!([{ "clicks": 4321, "impressions": 98765, "ctr": 0.0438, "position": 14.27 }])
    );
    assert_eq!(
        doc["truncated"], false,
        "totals are one row and never partial"
    );
    assert_eq!(doc.get("notes"), None);

    api.gsc()
        .args(["analytics", "totals", "--site", SITE, "--format", "table"])
        .args(PERIOD)
        .assert()
        .success()
        .stdout(predicates::str::ends_with(
            "\nCLICKS  IMPRESSIONS    CTR  POSITION\n 4,321       98,765  4.38%      14.3\n",
        ));
}

#[test]
fn default_period_is_28_days_ending_three_days_ago() {
    let mut api = MockApi::new();
    api.on("POST", ENDPOINT).with_body("{}").create();
    let doc = run(&api, &["dates"]);

    let date =
        |key: &str| NaiveDate::parse_from_str(doc[key].as_str().unwrap(), "%Y-%m-%d").unwrap();
    assert_eq!((date("endDate") - date("startDate")).num_days(), 27);
    // Search Console's "today" is Pacific time, up to a day behind UTC.
    let lag = (chrono::Utc::now().date_naive() - date("endDate")).num_days();
    assert!(
        (3..=4).contains(&lag),
        "end date should lag today by ~3 days, got {lag}"
    );
}

#[test]
fn hour_dimension_switches_to_hourly_data_ending_today() {
    let mut api = MockApi::new();
    let query = api
        .on("POST", ENDPOINT)
        .match_body(Matcher::PartialJson(json!({
            "dimensions": ["hour"],
            "dataState": "hourly_all",
        })))
        .with_body(
            json!({
                "rows": [api_row(&["2026-11-12T08:00:00-08:00"], 3, 50, 6.0)],
                "metadata": { "firstIncompleteHour": "2026-11-12T08:00:00-08:00" },
            })
            .to_string(),
        )
        .create();

    let doc = run(&api, &["query", "-d", "hour", "--days", "2"]);
    query.assert();
    assert_eq!(doc["dataState"], "hourly_all");
    assert_eq!(doc["rows"][0]["hour"], "2026-11-12T08:00:00-08:00");
    assert_eq!(
        doc["metadata"]["firstIncompleteHour"],
        "2026-11-12T08:00:00-08:00"
    );
    let lag = (chrono::Utc::now().date_naive()
        - NaiveDate::parse_from_str(doc["endDate"].as_str().unwrap(), "%Y-%m-%d").unwrap())
    .num_days();
    assert!(
        (0..=1).contains(&lag),
        "fresh data ends today, got a lag of {lag}"
    );
    assert!(
        doc["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().contains("still being processed"))
    );
}

#[test]
fn truncated_is_true_only_when_more_rows_really_exist() {
    let three = json!({ "rows": [
        api_row(&["a"], 9, 90, 1.0),
        api_row(&["b"], 8, 80, 2.0),
        api_row(&["c"], 7, 70, 3.0),
    ]})
    .to_string();
    let mut args = vec!["keywords", "-n", "2"];
    args.extend_from_slice(&PERIOD);

    // Asking for 2 requests 3. All three arrive, so a third row exists.
    let mut api = MockApi::new();
    api.on("POST", ENDPOINT)
        .match_body(Matcher::PartialJson(json!({ "rowLimit": 3 })))
        .with_body(&three)
        .create();
    let doc = run(&api, &args);
    assert_eq!(doc["rowCount"], 2);
    assert_eq!(doc["rows"][1]["query"], "b", "the probe row is not shown");
    assert_eq!(doc["truncated"], true);
    assert!(doc["notes"][0].as_str().unwrap().contains("--all"));

    // Exactly as many rows as the limit: nothing was cut off.
    let mut api = MockApi::new();
    api.on("POST", ENDPOINT)
        .with_body(
            json!({ "rows": [api_row(&["a"], 9, 90, 1.0), api_row(&["b"], 8, 80, 2.0)] })
                .to_string(),
        )
        .create();
    let doc = run(&api, &args);
    assert_eq!(doc["rowCount"], 2);
    assert_eq!(doc["truncated"], false);
    assert_eq!(doc.get("notes"), None);
}

#[test]
fn a_full_time_series_is_not_reported_as_truncated() {
    let mut api = MockApi::new();
    // 180 days requested, and Google has a row for every one of them.
    let days: Vec<Value> = (0..180)
        .map(|i| {
            let date = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap() + chrono::Days::new(i);
            api_row(&[&date.to_string()], 10, 100, 5.0)
        })
        .collect();
    let query = api
        .on("POST", ENDPOINT)
        .match_body(Matcher::PartialJson(
            json!({ "dimensions": ["date"], "rowLimit": 181 }),
        ))
        .with_body(json!({ "rows": days }).to_string())
        .create();

    let doc = api.json(&[
        "analytics",
        "dates",
        "--site",
        SITE,
        "--start",
        "2026-01-01",
        "--end",
        "2026-06-29",
    ]);
    query.assert();
    assert_eq!(doc["rowCount"], 180);
    assert_eq!(doc["truncated"], false);
    assert_eq!(doc.get("notes"), None);
}

#[test]
fn sort_ranks_a_full_page_then_cuts_to_the_limit() {
    let mut api = MockApi::new();
    let query = api
        .on("POST", ENDPOINT)
        // Google can only sort by clicks, so a whole page is fetched to rank.
        .match_body(Matcher::PartialJson(json!({ "rowLimit": 25000 })))
        .with_body(
            json!({ "rows": [
                api_row(&["most clicks"], 50, 100, 1.2),
                api_row(&["most impressions"], 10, 9000, 18.0),
                api_row(&["middle"], 20, 500, 7.0),
            ]})
            .to_string(),
        )
        .create();

    let mut args = vec!["keywords", "--sort", "impressions", "-n", "2"];
    args.extend_from_slice(&PERIOD);
    let doc = run(&api, &args);
    query.assert();
    let order: Vec<&str> = doc["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["query"].as_str().unwrap())
        .collect();
    assert_eq!(order, ["most impressions", "middle"]);
    assert_eq!(doc["truncated"], true);

    let mut args = vec!["keywords", "--sort", "position"];
    args.extend_from_slice(&PERIOD);
    let doc = run(&api, &args);
    assert_eq!(
        doc["rows"][0]["query"], "most clicks",
        "position sorts best-first"
    );
    assert_eq!(doc["truncated"], false);
}

#[test]
fn all_pages_through_every_row() {
    let mut api = MockApi::new();
    let full_page: Vec<Value> = (0..25_000)
        .map(|i| api_row(&[&format!("query {i}")], 1, 10, 5.0))
        .collect();
    let first = api
        .on("POST", ENDPOINT)
        .match_body(Matcher::PartialJson(
            json!({ "startRow": 0, "rowLimit": 25000 }),
        ))
        .with_body(json!({ "rows": full_page }).to_string())
        .expect(1)
        .create();
    let second = api
        .on("POST", ENDPOINT)
        .match_body(Matcher::PartialJson(
            json!({ "startRow": 25000, "rowLimit": 25000 }),
        ))
        .with_body(json!({ "rows": [api_row(&["the last one"], 1, 10, 5.0)] }).to_string())
        .expect(1)
        .create();

    let mut args = vec!["keywords", "--all"];
    args.extend_from_slice(&PERIOD);
    let doc = run(&api, &args);
    first.assert();
    second.assert();
    assert_eq!(doc["rowCount"], 25_001);
    assert_eq!(doc["truncated"], false);
    assert_eq!(doc["rows"][25_000]["query"], "the last one");
}

#[test]
fn a_limit_above_one_page_is_split_across_requests() {
    let mut api = MockApi::new();
    let full_page: Vec<Value> = (0..25_000)
        .map(|i| api_row(&[&format!("q{i}")], 1, 10, 5.0))
        .collect();
    api.on("POST", ENDPOINT)
        .match_body(Matcher::PartialJson(
            json!({ "startRow": 0, "rowLimit": 25000 }),
        ))
        .with_body(json!({ "rows": full_page }).to_string())
        .create();
    // The second request asks for the remaining 5 plus the probe row.
    let remainder = api
        .on("POST", ENDPOINT)
        .match_body(Matcher::PartialJson(json!({ "startRow": 25000, "rowLimit": 6 })))
        .with_body(
            json!({ "rows": (0..6).map(|i| api_row(&[&format!("tail{i}")], 1, 10, 5.0)).collect::<Vec<_>>() })
                .to_string(),
        )
        .create();

    let mut args = vec!["keywords", "--limit", "25005"];
    args.extend_from_slice(&PERIOD);
    let doc = run(&api, &args);
    remainder.assert();
    assert_eq!(doc["rowCount"], 25_005);
    assert_eq!(doc["rows"][25_004]["query"], "tail4");
    assert_eq!(
        doc["truncated"], true,
        "the probe row came back, so more exist"
    );
}

#[test]
fn offset_pages_through_a_sorted_ranking() {
    let mut api = MockApi::new();
    // In Google's order (by clicks): a, b, c, d. By impressions: d, c, b, a.
    let ranking = api
        .on("POST", ENDPOINT)
        .match_body(Matcher::PartialJson(
            json!({ "startRow": 0, "rowLimit": 25000 }),
        ))
        .with_body(
            json!({ "rows": [
                api_row(&["a"], 40, 100, 1.0),
                api_row(&["b"], 30, 200, 2.0),
                api_row(&["c"], 20, 300, 3.0),
                api_row(&["d"], 10, 400, 4.0),
            ]})
            .to_string(),
        )
        .expect(2)
        .create();
    let page = |offset: &'static str| {
        let mut args = vec![
            "keywords",
            "--sort",
            "impressions",
            "-n",
            "2",
            "--offset",
            offset,
        ];
        args.extend_from_slice(&PERIOD);
        run(&api, &args)
    };
    let names = |doc: &Value| -> Vec<String> {
        doc["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["query"].as_str().unwrap().to_owned())
            .collect()
    };

    let first = page("0");
    assert_eq!(names(&first), ["d", "c"]);
    assert_eq!(first["truncated"], true);

    // Page two continues the same ranking; both requests start at row 0.
    let second = page("2");
    assert_eq!(names(&second), ["b", "a"]);
    assert_eq!(second["truncated"], false);
    ranking.assert();
}

#[test]
fn bare_domain_is_resolved_before_querying() {
    let mut api = MockApi::new();
    api.on("GET", "/webmasters/v3/sites")
        .with_body(
            r#"{"siteEntry":[{"siteUrl":"sc-domain:example.com","permissionLevel":"siteOwner"}]}"#,
        )
        .create();
    let query = api.on("POST", ENDPOINT).with_body("{}").create();

    let doc = api.json(&["analytics", "summary", "--site", "example.com"]);
    query.assert();
    assert_eq!(doc["site"], SITE);
}

#[test]
fn usage_errors_are_caught_before_any_request() {
    let api = MockApi::new();
    let err = api.error(
        &[
            "analytics",
            "keywords",
            "--site",
            SITE,
            "--start",
            "2026-10-28",
            "--end",
            "2026-10-01",
        ],
        2,
    );
    assert_eq!(
        err["message"],
        "the start date 2026-10-28 is after the end date 2026-10-01"
    );

    let err = api.error(
        &[
            "analytics",
            "keywords",
            "--site",
            SITE,
            "--start",
            "last week",
        ],
        2,
    );
    assert_eq!(err["message"], "`last week` is not a valid date");

    api.gsc()
        .args([
            "analytics",
            "keywords",
            "--site",
            SITE,
            "--filter",
            "date equals 2026-01-01",
        ])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("cannot filter on `date`"));

    api.gsc()
        .args([
            "analytics",
            "keywords",
            "--site",
            SITE,
            "--start",
            "2026-01-01",
            "--days",
            "7",
        ])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("cannot be used with"));
}

#[test]
fn api_rejections_surface_googles_message() {
    let mut api = MockApi::new();
    api.on("POST", ENDPOINT)
        .with_status(400)
        .with_body(r#"{"error":{"code":400,"message":"Request contains an invalid argument.","status":"INVALID_ARGUMENT"}}"#)
        .create();
    let err = api.error(&["analytics", "keywords", "--site", SITE], 4);
    assert_eq!(err["message"], "Request contains an invalid argument.");
    assert_eq!(err["code"], "INVALID_ARGUMENT");
}
