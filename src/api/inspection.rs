//! URL Inspection: how Google sees one URL in its index.

use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use super::Client;
use crate::auth::Api;
use crate::error::{Error, Result};

/// Language for the human-readable parts of the result. Fixed to English so
/// that `coverageState` can be interpreted reliably.
const LANGUAGE: &str = "en-US";

/// The parts of an inspection result that `gsc` interprets.
///
/// Every field is optional: Google omits whatever does not apply, and a URL
/// it has never seen has almost nothing set.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Inspection {
    /// Deep link to this inspection in the Search Console UI.
    pub inspection_result_link: Option<String>,
    pub index_status_result: IndexStatus,
    pub rich_results_result: Option<RichResults>,
    pub amp_result: Option<AmpResult>,
    pub mobile_usability_result: Option<MobileUsability>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct IndexStatus {
    /// `PASS` (indexed), `NEUTRAL` (excluded), or `FAIL` (error).
    pub verdict: Option<String>,
    /// Google's one-line explanation, e.g. "Crawled - currently not indexed".
    pub coverage_state: Option<String>,
    /// `ALLOWED` or `DISALLOWED`.
    pub robots_txt_state: Option<String>,
    /// `INDEXING_ALLOWED`, or which mechanism blocks indexing.
    pub indexing_state: Option<String>,
    /// `SUCCESSFUL`, or how the last fetch failed.
    pub page_fetch_state: Option<String>,
    pub last_crawl_time: Option<String>,
    /// `DESKTOP` or `MOBILE`.
    pub crawled_as: Option<String>,
    /// The URL Google chose to index for this content.
    pub google_canonical: Option<String>,
    /// The URL the page itself declares as canonical.
    pub user_canonical: Option<String>,
    /// Sitemaps Google found this URL in.
    pub sitemap: Vec<String>,
    pub referring_urls: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RichResults {
    pub verdict: Option<String>,
    pub detected_items: Vec<DetectedItems>,
}

/// All items of one structured-data type found on the page.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DetectedItems {
    /// e.g. "Breadcrumbs", "FAQ", "Product snippets".
    pub rich_result_type: Option<String>,
    pub items: Vec<RichResultItem>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RichResultItem {
    pub name: Option<String>,
    pub issues: Vec<ReportedIssue>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AmpResult {
    pub verdict: Option<String>,
    pub amp_url: Option<String>,
    pub issues: Vec<ReportedIssue>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MobileUsability {
    pub verdict: Option<String>,
    pub issues: Vec<ReportedIssue>,
}

/// A problem Google itself describes in words.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ReportedIssue {
    /// `ERROR` or `WARNING`.
    pub severity: Option<String>,
    /// Rich result and AMP issues use this field...
    pub issue_message: Option<String>,
    /// ...and mobile usability issues use this one.
    pub message: Option<String>,
}

impl ReportedIssue {
    pub fn text(&self) -> &str {
        self.issue_message
            .as_deref()
            .or(self.message.as_deref())
            .unwrap_or("unspecified issue")
    }

    pub fn is_error(&self) -> bool {
        self.severity.as_deref() == Some("ERROR")
    }
}

impl Inspection {
    pub fn from_raw(raw: &Value) -> Result<Self> {
        serde_json::from_value(raw.clone())
            .map_err(|e| Error::internal(format!("unexpected inspection result: {e}")))
    }

    /// Whether the URL is in Google's index.
    pub fn is_indexed(&self) -> bool {
        self.index_status_result.verdict.as_deref() == Some("PASS")
    }

    /// Names of the rich result types Google detected on the page.
    pub fn rich_result_types(&self) -> Vec<&str> {
        self.rich_results_result
            .iter()
            .flat_map(|rich| &rich.detected_items)
            .filter_map(|detected| detected.rich_result_type.as_deref())
            .collect()
    }
}

impl Client {
    /// Inspect `url`, which must belong to the property `site`.
    ///
    /// Returns Google's `inspectionResult` object unmodified. This reports
    /// the indexed version of the page; the API has no live-test equivalent.
    pub async fn inspect_url(&self, site: &str, url: &str) -> Result<Value> {
        let body = json!({
            "inspectionUrl": url,
            "siteUrl": site,
            "languageCode": LANGUAGE,
        });
        let mut response = self
            .request(
                Api::SearchConsole,
                Method::POST,
                "/v1/urlInspection/index:inspect",
                &[],
                Some(&body),
            )
            .await?;
        match response.get_mut("inspectionResult") {
            Some(result) => Ok(result.take()),
            None => Err(Error::internal(
                "Google's response did not contain an inspection result",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_result() {
        let raw = json!({
            "inspectionResultLink": "https://search.google.com/search-console/inspect?resource_id=x",
            "indexStatusResult": {
                "verdict": "PASS",
                "coverageState": "Submitted and indexed",
                "robotsTxtState": "ALLOWED",
                "indexingState": "INDEXING_ALLOWED",
                "lastCrawlTime": "2026-11-08T10:22:33Z",
                "pageFetchState": "SUCCESSFUL",
                "googleCanonical": "https://example.com/a",
                "userCanonical": "https://example.com/a",
                "sitemap": ["https://example.com/sitemap.xml"],
                "referringUrls": ["https://example.com/"],
                "crawledAs": "MOBILE",
            },
            "mobileUsabilityResult": { "verdict": "VERDICT_UNSPECIFIED" },
            "richResultsResult": {
                "verdict": "FAIL",
                "detectedItems": [
                    { "richResultType": "Breadcrumbs", "items": [{ "name": "Unnamed item" }] },
                    { "richResultType": "FAQ", "items": [{
                        "name": "FAQ",
                        "issues": [{ "issueMessage": "Missing field \"acceptedAnswer\"", "severity": "ERROR" }],
                    }]},
                ],
            },
        });
        let inspection = Inspection::from_raw(&raw).unwrap();
        assert!(inspection.is_indexed());
        assert_eq!(inspection.rich_result_types(), ["Breadcrumbs", "FAQ"]);
        assert_eq!(
            inspection.index_status_result.sitemap,
            ["https://example.com/sitemap.xml"]
        );
        let issue = &inspection.rich_results_result.unwrap().detected_items[1].items[0].issues[0];
        assert!(issue.is_error());
        assert_eq!(issue.text(), "Missing field \"acceptedAnswer\"");
    }

    #[test]
    fn a_url_unknown_to_google_has_almost_nothing_set() {
        let raw = json!({
            "inspectionResultLink": "https://search.google.com/x",
            "indexStatusResult": {
                "verdict": "NEUTRAL",
                "coverageState": "URL is unknown to Google",
                "robotsTxtState": "ROBOTS_TXT_STATE_UNSPECIFIED",
                "indexingState": "INDEXING_STATE_UNSPECIFIED",
                "pageFetchState": "PAGE_FETCH_STATE_UNSPECIFIED",
            },
        });
        let inspection = Inspection::from_raw(&raw).unwrap();
        assert!(!inspection.is_indexed());
        assert!(inspection.rich_result_types().is_empty());
        assert_eq!(inspection.index_status_result.last_crawl_time, None);
    }

    #[test]
    fn empty_and_unexpected_shapes() {
        assert_eq!(
            Inspection::from_raw(&json!({})).unwrap(),
            Inspection::default()
        );
        assert!(Inspection::from_raw(&json!({ "indexStatusResult": "nope" })).is_err());
    }

    #[test]
    fn reported_issue_text_falls_back_across_field_names() {
        let mobile = ReportedIssue {
            message: Some("Text too small to read".into()),
            severity: Some("WARNING".into()),
            ..Default::default()
        };
        assert_eq!(mobile.text(), "Text too small to read");
        assert!(!mobile.is_error());
        assert_eq!(ReportedIssue::default().text(), "unspecified issue");
    }
}
