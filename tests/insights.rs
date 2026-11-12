//! `gsc insights` against a mock Search Analytics endpoint.

mod common;

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

fn rows(rows: Vec<Value>) -> String {
    json!({ "rows": rows }).to_string()
}

fn run(api: &MockApi, args: &[&str]) -> Value {
    let mut full = vec!["insights"];
    full.extend_from_slice(args);
    full.extend_from_slice(&PERIOD);
    full.extend_from_slice(&["--site", SITE]);
    api.json(&full)
}

fn names<'a>(doc: &'a Value, key: &str) -> Vec<&'a str> {
    doc["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row[key].as_str().unwrap())
        .collect()
}

/// Register the four responses a comparison needs: rows and totals for the
/// current period, and the same for the earlier one.
fn comparison(
    api: &mut MockApi,
    dimension: &str,
    earlier: (&str, &str),
    current: Vec<Value>,
    previous: Vec<Value>,
) -> Vec<mockito::Mock> {
    let rows_for = |start: &str, end: &str| {
        Matcher::PartialJson(json!({
            "startDate": start, "endDate": end,
            "dimensions": [dimension], "rowLimit": 25000,
        }))
    };
    // A totals request has no dimensions and asks for a single row.
    let totals_for = |start: &str, end: &str| {
        Matcher::PartialJson(json!({ "startDate": start, "endDate": end, "rowLimit": 1 }))
    };
    vec![
        api.on("POST", ENDPOINT)
            .match_body(rows_for("2026-10-01", "2026-10-28"))
            .with_body(rows(current))
            .expect(1)
            .create(),
        api.on("POST", ENDPOINT)
            .match_body(rows_for(earlier.0, earlier.1))
            .with_body(rows(previous))
            .expect(1)
            .create(),
        api.on("POST", ENDPOINT)
            .match_body(totals_for("2026-10-01", "2026-10-28"))
            .with_body(rows(vec![api_row(&[], 1350, 43500, 12.4)]))
            .expect(1)
            .create(),
        api.on("POST", ENDPOINT)
            .match_body(totals_for(earlier.0, earlier.1))
            .with_body(rows(vec![api_row(&[], 1200, 40000, 14.0)]))
            .expect(1)
            .create(),
    ]
}

fn current_queries() -> Vec<Value> {
    vec![
        api_row(&["steady"], 100, 1000, 3.0),
        api_row(&["grew"], 80, 900, 4.0),
        api_row(&["shrank"], 20, 800, 9.0),
        api_row(&["brand new"], 15, 200, 6.0),
    ]
}

fn previous_queries() -> Vec<Value> {
    vec![
        api_row(&["steady"], 100, 1000, 3.0),
        api_row(&["shrank"], 90, 1500, 5.0),
        api_row(&["grew"], 30, 500, 7.0),
        api_row(&["vanished"], 40, 400, 8.0),
    ]
}

#[test]
fn compare_fetches_both_periods_and_ranks_by_size_of_change() {
    let mut api = MockApi::new();
    let mocks = comparison(
        &mut api,
        "query",
        ("2026-09-03", "2026-09-30"),
        current_queries(),
        previous_queries(),
    );

    let doc = run(&api, &["compare"]);
    for mock in &mocks {
        mock.assert();
    }

    assert_eq!(doc["by"], "query");
    assert_eq!(doc["sortedBy"], "clicks");
    assert_eq!(
        doc["period"],
        json!({ "startDate": "2026-10-01", "endDate": "2026-10-28" })
    );
    assert_eq!(
        doc["comparedWith"],
        json!({ "startDate": "2026-09-03", "endDate": "2026-09-30" }),
        "the 28 days immediately before"
    );
    assert_eq!(
        doc["totals"],
        json!({
            "clicks": 1350, "clicksPrevious": 1200, "clicksChange": 150,
            "impressions": 43500, "impressionsPrevious": 40000, "impressionsChange": 3500,
            "ctr": 0.031, "ctrPrevious": 0.03, "ctrChange": 0.001,
            "position": 12.4, "positionPrevious": 14.0, "positionChange": -1.6,
        })
    );
    assert_eq!(
        names(&doc, "query"),
        ["shrank", "grew", "vanished", "brand new", "steady"]
    );
    assert_eq!(
        doc["rows"][0],
        json!({
            "query": "shrank",
            "clicks": 20, "clicksPrevious": 90, "clicksChange": -70,
            "impressions": 800, "impressionsPrevious": 1500, "impressionsChange": -700,
            "ctr": 0.025, "ctrPrevious": 0.06, "ctrChange": -0.035,
            "position": 9.0, "positionPrevious": 5.0, "positionChange": 4.0,
        })
    );
    assert_eq!(doc["rows"][2]["position"], json!(null), "gone this period");
    assert_eq!(
        doc["rows"][3]["positionPrevious"],
        json!(null),
        "new this period"
    );
    assert_eq!(doc["truncated"], false);
}

#[test]
fn compare_table_shows_totals_and_signed_changes() {
    let mut api = MockApi::new();
    comparison(
        &mut api,
        "query",
        ("2026-09-03", "2026-09-30"),
        current_queries(),
        previous_queries(),
    );

    api.gsc()
        .args([
            "insights", "compare", "--site", SITE, "--format", "table", "-n", "3",
        ])
        .args(PERIOD)
        .assert()
        .success()
        .stdout(
            "\
Property:       sc-domain:example.com
Period:         2026-10-01 to 2026-10-28 (28 days)
Compared with:  2026-09-03 to 2026-09-30 (28 days)
Clicks:         1,350 (was 1,200, +150)
Impressions:    43,500 (was 40,000, +3,500)
CTR:            3.10% (was 3.00%, +0.10%)
Position:       12.4 (was 14.0, -1.6)

QUERY     CLICKS  +/-  IMPR.   +/-    CTR      +/-  POS.   +/-
shrank        20  -70    800  -700  2.50%   -3.50%   9.0  +4.0
grew          80  +50    900  +400  8.89%   +2.89%   4.0  -3.0
vanished       0  -40      0  -400  0.00%  -10.00%     -     -
",
        );
}

#[test]
fn declining_and_rising_default_to_pages_and_keep_one_direction() {
    let mut api = MockApi::new();
    let current = vec![
        api_row(&["https://example.com/up"], 80, 900, 4.0),
        api_row(&["https://example.com/down"], 20, 800, 9.0),
        api_row(&["https://example.com/flat"], 50, 500, 2.0),
    ];
    let previous = vec![
        api_row(&["https://example.com/up"], 30, 500, 7.0),
        api_row(&["https://example.com/down"], 90, 1500, 5.0),
        api_row(&["https://example.com/flat"], 50, 500, 2.0),
        api_row(&["https://example.com/removed"], 10, 100, 9.0),
    ];
    comparison(
        &mut api,
        "page",
        ("2026-09-03", "2026-09-30"),
        current.clone(),
        previous.clone(),
    );
    let doc = run(&api, &["declining"]);
    assert_eq!(doc["by"], "page");
    assert_eq!(
        names(&doc, "page"),
        ["https://example.com/down", "https://example.com/removed"]
    );

    let mut api = MockApi::new();
    comparison(
        &mut api,
        "page",
        ("2026-09-03", "2026-09-30"),
        current,
        previous,
    );
    let doc = run(&api, &["rising"]);
    assert_eq!(names(&doc, "page"), ["https://example.com/up"]);
}

#[test]
fn sorting_by_position_treats_a_higher_ranking_as_rising() {
    let mut api = MockApi::new();
    comparison(
        &mut api,
        "query",
        ("2026-09-03", "2026-09-30"),
        current_queries(),
        previous_queries(),
    );
    let doc = run(&api, &["rising", "--by", "query", "--sort", "position"]);
    assert_eq!(doc["sortedBy"], "position");
    assert_eq!(names(&doc, "query"), ["grew"], "7 to 4 is the only move up");
    assert_eq!(doc["rows"][0]["positionChange"], -3.0);
}

#[test]
fn year_over_year_uses_the_same_dates_last_year_and_forwards_filters() {
    let mut api = MockApi::new();
    let filtered = api
        .on("POST", ENDPOINT)
        .match_body(Matcher::PartialJson(json!({
            "type": "image",
            "dimensionFilterGroups": [{ "groupType": "and", "filters": [
                { "dimension": "country", "operator": "equals", "expression": "usa" },
            ]}],
        })))
        .with_body("{}")
        .expect(4)
        .create();

    let doc = run(
        &api,
        &[
            "compare",
            "--vs",
            "year",
            "--country",
            "usa",
            "--type",
            "image",
        ],
    );
    filtered.assert();
    assert_eq!(
        doc["comparedWith"],
        json!({ "startDate": "2025-10-01", "endDate": "2025-10-28" })
    );
    assert_eq!(doc["rows"], json!([]));
    assert_eq!(doc["totals"]["clicks"], 0);
    assert_eq!(doc["totals"]["position"], json!(null));
    assert_eq!(doc["notes"], json!(["No data in either period."]));
}

#[test]
fn striking_distance_keeps_keywords_just_off_the_top() {
    let mut api = MockApi::new();
    let query = api
        .on("POST", ENDPOINT)
        .match_body(Matcher::PartialJson(json!({
            "startDate": "2026-10-01", "dimensions": ["query"], "rowLimit": 25000,
        })))
        .with_body(rows(vec![
            api_row(&["already winning"], 500, 5000, 2.1),
            api_row(&["edge of page one"], 30, 900, 8.0),
            api_row(&["page two, popular"], 12, 3000, 11.4),
            api_row(&["page two, rare"], 0, 4, 14.0),
            api_row(&["buried"], 1, 2000, 35.0),
        ]))
        .create();

    let doc = run(&api, &["striking-distance"]);
    query.assert();
    assert_eq!(
        names(&doc, "query"),
        ["page two, popular", "edge of page one"]
    );
    assert_eq!(doc["rowsScanned"], 5);
    assert_eq!(doc["minPosition"], 8.0);
    assert_eq!(doc["maxPosition"], 20.0);
    assert_eq!(
        doc["rows"][0],
        json!({ "query": "page two, popular", "clicks": 12, "impressions": 3000,
                "ctr": 0.004, "position": 11.4 })
    );

    // The alias and custom bounds.
    let doc = run(
        &api,
        &[
            "opportunities",
            "--min-position",
            "20",
            "--max-position",
            "40",
            "--min-impressions",
            "1",
        ],
    );
    assert_eq!(names(&doc, "query"), ["buried"]);
}

#[test]
fn striking_distance_can_include_the_ranking_page() {
    let mut api = MockApi::new();
    let query = api
        .on("POST", ENDPOINT)
        .match_body(Matcher::PartialJson(
            json!({ "dimensions": ["query", "page"] }),
        ))
        .with_body(rows(vec![api_row(
            &["rust cli tutorial", "https://example.com/blog/cli"],
            4,
            600,
            12.0,
        )]))
        .create();

    let doc = run(&api, &["striking-distance", "--with-page"]);
    query.assert();
    assert_eq!(doc["rows"][0]["query"], "rust cli tutorial");
    assert_eq!(doc["rows"][0]["page"], "https://example.com/blog/cli");
}

#[test]
fn striking_distance_rejects_an_inverted_range() {
    let api = MockApi::new();
    let err = api.error(
        &[
            "insights",
            "striking-distance",
            "--site",
            SITE,
            "--min-position",
            "20",
            "--max-position",
            "10",
        ],
        2,
    );
    assert_eq!(
        err["message"],
        "--min-position 20 is greater than --max-position 10"
    );
}

#[test]
fn low_ctr_flags_items_below_their_position_average() {
    let mut api = MockApi::new();
    let query = api
        .on("POST", ENDPOINT)
        .match_body(Matcher::PartialJson(
            json!({ "dimensions": ["page"], "rowLimit": 25000 }),
        ))
        .with_body(rows(vec![
            api_row(&["https://example.com/strong-a"], 100, 1000, 3.2),
            api_row(&["https://example.com/strong-b"], 90, 1000, 2.8),
            api_row(&["https://example.com/weak"], 10, 1000, 3.0),
            api_row(&["https://example.com/deep"], 1, 5000, 30.0),
        ]))
        .create();

    let doc = run(&api, &["low-ctr"]);
    query.assert();
    assert_eq!(doc["by"], "page");
    assert_eq!(
        doc["rows"],
        json!([{
            "page": "https://example.com/weak",
            "clicks": 10, "impressions": 1000, "ctr": 0.01, "position": 3.0,
            "expectedCtr": 0.0667, "missedClicks": 57,
        }])
    );
    assert!(
        doc["notes"][0]
            .as_str()
            .unwrap()
            .contains("not an industry benchmark")
    );

    api.gsc()
        .args(["insights", "low-ctr", "--site", SITE, "--format", "table"])
        .args(PERIOD)
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "\
PAGE                      CLICKS  IMPRESSIONS    CTR  POSITION  EXPECTED  MISSED CLICKS
https://example.com/weak      10        1,000  1.00%       3.0     6.67%             57
",
        ));
}

#[test]
fn low_ctr_by_query_with_nothing_to_report() {
    let mut api = MockApi::new();
    api.on("POST", ENDPOINT)
        .match_body(Matcher::PartialJson(json!({ "dimensions": ["query"] })))
        .with_body(rows(vec![
            api_row(&["a"], 100, 1000, 3.0),
            api_row(&["b"], 100, 1000, 3.0),
        ]))
        .create();
    let doc = run(&api, &["low-ctr", "--by", "query"]);
    assert_eq!(doc["rows"], json!([]));
    assert!(
        doc["notes"][0]
            .as_str()
            .unwrap()
            .starts_with("Nothing stands out")
    );
}
