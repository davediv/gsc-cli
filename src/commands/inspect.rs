//! `gsc inspect` — how Google indexes specific URLs, and what to fix.

use clap::Subcommand;
use serde_json::{Map, Value, json};

use super::inspect_batch::{self, BatchArgs};
use crate::Context;
use crate::api::inspection::Inspection;
use crate::diagnose::{self, Issue};
use crate::error::Result;
use crate::output::{Column, Report};
use crate::site;

#[derive(Debug, Subcommand)]
pub enum InspectCommand {
    /// Inspect one URL: index status, canonical, last crawl, and issues with fixes
    ///
    /// Reports what Google recorded the last time it crawled the URL, not
    /// the page as it is right now. Each problem found is listed with a
    /// stable code and a suggested fix.
    Url {
        /// Full URL, or a path such as `/pricing` for a URL-prefix property
        url: String,

        /// Also include Google's complete response under `raw`
        #[arg(long)]
        raw: bool,
    },

    /// Inspect many URLs and group the problems found
    ///
    /// The closest thing the API offers to the "Page indexing" report: URLs
    /// are inspected one by one and the findings grouped by issue, each with
    /// the affected URLs and a fix.
    ///
    ///   gsc inspect batch --submitted-sitemaps --only-issues
    ///   gsc inspect batch --sitemap https://example.com/sitemap.xml -n 500
    ///   gsc inspect batch --file urls.txt
    ///   gsc inspect batch https://example.com/a https://example.com/b
    #[command(verbatim_doc_comment)]
    Batch(BatchArgs),
}

pub async fn run(command: InspectCommand, ctx: &Context) -> Result<()> {
    let client = ctx.client()?;
    let site = ctx.site(&client).await?;

    match command {
        InspectCommand::Url { url, raw } => {
            let url = site::absolute_url(&site, &url, "URL")?;
            let result = client.inspect_url(&site, &url).await?;
            let inspected = Inspected::new(&url, &result, raw)?;
            ctx.emit(&inspected.into_report(&site))
        }
        InspectCommand::Batch(args) => inspect_batch::run(args, &site, &client, ctx).await,
    }
}

/// One URL's inspection, condensed to the fields worth reading.
pub struct Inspected {
    /// Flat summary: status fields, `issues`, and optionally `raw`.
    pub summary: Value,
    pub issues: Vec<Issue>,
    pub indexed: bool,
}

impl Inspected {
    pub fn new(url: &str, raw: &Value, include_raw: bool) -> Result<Self> {
        let inspection = Inspection::from_raw(raw)?;
        let issues = diagnose::diagnose(url, &inspection);
        let status = &inspection.index_status_result;

        let mut summary = Map::new();
        summary.insert("url".into(), json!(url));
        summary.insert("indexed".into(), json!(inspection.is_indexed()));
        let mut set = |key: &str, value: Value| {
            if is_informative(&value) {
                summary.insert(key.into(), value);
            }
        };
        set("verdict", json!(status.verdict));
        set("coverageState", json!(status.coverage_state));
        set("lastCrawlTime", json!(status.last_crawl_time));
        set("crawledAs", json!(status.crawled_as));
        set("robotsTxtState", json!(status.robots_txt_state));
        set("indexingState", json!(status.indexing_state));
        set("pageFetchState", json!(status.page_fetch_state));
        set("googleCanonical", json!(status.google_canonical));
        set("userCanonical", json!(status.user_canonical));
        set("sitemaps", json!(status.sitemap));
        set("referringUrls", json!(status.referring_urls));
        set("richResultTypes", json!(inspection.rich_result_types()));
        set("inspectionLink", json!(inspection.inspection_result_link));
        summary.insert("issues".into(), json!(issues));
        if include_raw {
            summary.insert("raw".into(), raw.clone());
        }

        Ok(Self {
            summary: Value::Object(summary),
            indexed: inspection.is_indexed(),
            issues,
        })
    }

    fn into_report(self, site: &str) -> Report {
        let mut document = self.summary;
        document["site"] = json!(site);

        let text = |key: &str| document.get(key).and_then(Value::as_str).map(str::to_owned);
        let list = |key: &str| {
            document.get(key).and_then(Value::as_array).map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
        };
        let fields: Vec<(&str, Option<String>)> = vec![
            ("URL", text("url")),
            ("Property", Some(site.to_owned())),
            (
                "Indexed",
                Some(if self.indexed { "yes" } else { "no" }.to_owned()),
            ),
            ("Coverage", text("coverageState")),
            ("Last crawl", text("lastCrawlTime")),
            ("Crawled as", text("crawledAs")),
            ("Robots.txt", text("robotsTxtState")),
            ("Indexing", text("indexingState")),
            ("Page fetch", text("pageFetchState")),
            ("Google canonical", text("googleCanonical")),
            ("Declared canonical", text("userCanonical")),
            ("In sitemaps", list("sitemaps")),
            ("Rich results", list("richResultTypes")),
            ("Details", text("inspectionLink")),
        ];

        let mut report = Report::new(document.clone());
        for (label, value) in fields {
            if let Some(value) = value {
                report = report.field(label, value);
            }
        }
        if self.issues.is_empty() {
            report.note("No issues found.")
        } else {
            report.table(issue_columns(), "/issues")
        }
    }
}

pub fn issue_columns() -> Vec<Column> {
    vec![
        Column::text("severity", "Severity"),
        Column::text("code", "Issue"),
        Column::text("message", "What is wrong"),
        Column::text("fix", "How to fix it"),
    ]
}

/// Whether a value says anything. Google pads results with `*_UNSPECIFIED`
/// placeholders; leaving those and empty values out keeps the output short.
fn is_informative(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(text) => !text.is_empty() && !text.ends_with("_UNSPECIFIED"),
        Value::Array(items) => !items.is_empty(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://example.com/page";

    #[test]
    fn summary_keeps_informative_fields_and_always_has_issues() {
        let raw = json!({
            "inspectionResultLink": "https://search.google.com/x",
            "indexStatusResult": {
                "verdict": "PASS",
                "coverageState": "Submitted and indexed",
                "robotsTxtState": "ALLOWED",
                "indexingState": "INDEXING_ALLOWED",
                "pageFetchState": "SUCCESSFUL",
                "lastCrawlTime": "2026-11-08T10:22:33Z",
                "crawledAs": "MOBILE",
                "googleCanonical": URL,
                "userCanonical": URL,
                "sitemap": ["https://example.com/sitemap.xml"],
            },
            "mobileUsabilityResult": { "verdict": "VERDICT_UNSPECIFIED" },
        });
        let inspected = Inspected::new(URL, &raw, false).unwrap();
        assert!(inspected.indexed);
        assert_eq!(
            inspected.summary,
            json!({
                "url": URL,
                "indexed": true,
                "verdict": "PASS",
                "coverageState": "Submitted and indexed",
                "lastCrawlTime": "2026-11-08T10:22:33Z",
                "crawledAs": "MOBILE",
                "robotsTxtState": "ALLOWED",
                "indexingState": "INDEXING_ALLOWED",
                "pageFetchState": "SUCCESSFUL",
                "googleCanonical": URL,
                "userCanonical": URL,
                "sitemaps": ["https://example.com/sitemap.xml"],
                "inspectionLink": "https://search.google.com/x",
                "issues": [],
            })
        );
    }

    #[test]
    fn placeholders_and_empty_values_are_dropped() {
        let raw = json!({ "indexStatusResult": {
            "verdict": "NEUTRAL",
            "coverageState": "URL is unknown to Google",
            "robotsTxtState": "ROBOTS_TXT_STATE_UNSPECIFIED",
            "indexingState": "INDEXING_STATE_UNSPECIFIED",
            "pageFetchState": "PAGE_FETCH_STATE_UNSPECIFIED",
            "crawledAs": "CRAWLING_USER_AGENT_UNSPECIFIED",
        }});
        let inspected = Inspected::new(URL, &raw, false).unwrap();
        assert!(!inspected.indexed);
        let keys: Vec<&String> = inspected.summary.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["url", "indexed", "verdict", "coverageState", "issues"]
        );
        assert_eq!(inspected.summary["issues"][0]["code"], "unknown_to_google");
    }

    #[test]
    fn raw_response_is_included_only_on_request() {
        let raw = json!({ "indexStatusResult": { "verdict": "PASS" }, "futureField": 1 });
        assert_eq!(
            Inspected::new(URL, &raw, false).unwrap().summary.get("raw"),
            None
        );
        assert_eq!(Inspected::new(URL, &raw, true).unwrap().summary["raw"], raw);
    }
}
