//! Turning an inspection result into problems and fixes.
//!
//! Google reports a URL's index status as a handful of enums plus one line
//! of prose. This module translates them into [`Issue`]s with a stable
//! `code` (for programs to branch on) and a `fix` (for whoever, or whatever,
//! is going to repair the site).

use serde::Serialize;

use crate::api::inspection::{Inspection, ReportedIssue};

/// Ordered most serious first, so sorting by severity puts errors on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Keeps the page out of the index, or invalidates a rich result.
    Error,
    /// Likely to hurt, or a choice by Google that deserves a look.
    Warning,
    /// Expected behaviour worth knowing about.
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    /// Stable identifier, e.g. `noindex_meta_tag`.
    pub code: &'static str,
    pub severity: Severity,
    /// What is wrong, for this URL specifically.
    pub message: String,
    /// What to change to resolve it.
    pub fix: String,
}

/// Collects issues, keeping the first one reported under each code. Several
/// of Google's fields can describe the same underlying problem.
#[derive(Default)]
struct Findings(Vec<Issue>);

impl Findings {
    fn add(
        &mut self,
        code: &'static str,
        severity: Severity,
        message: impl Into<String>,
        fix: &str,
    ) {
        if !self.0.iter().any(|issue| issue.code == code) {
            self.0.push(Issue {
                code,
                severity,
                message: message.into(),
                fix: fix.to_owned(),
            });
        }
    }
}

const FIX_ROBOTS: &str = "Remove or narrow the Disallow rule in robots.txt that matches this \
    path. To keep a page out of Google, allow crawling and use a noindex tag instead: \
    robots.txt alone does not prevent indexing.";
const FIX_NOT_FOUND: &str = "Restore the page, 301-redirect it to the closest equivalent, or \
    remove it from sitemaps and internal links if it is gone for good.";
const FIX_SERVER_ERROR: &str = "Check server logs around the last crawl time. Fix crashes and \
    timeouts, and make sure Googlebot is not being rate-limited or blocked by a firewall.";
const FIX_STRUCTURED_DATA: &str = "Correct the structured data on the page and validate it \
    with the Rich Results Test (https://search.google.com/test/rich-results).";

/// List everything worth acting on for one inspected URL, most serious first.
pub fn diagnose(url: &str, inspection: &Inspection) -> Vec<Issue> {
    let status = &inspection.index_status_result;
    let is = |field: &Option<String>, value: &str| field.as_deref() == Some(value);
    let coverage = status
        .coverage_state
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    let mut findings = Findings::default();

    // Crawling blocked. Three different fields can each say so.
    if is(&status.robots_txt_state, "DISALLOWED")
        || is(&status.indexing_state, "BLOCKED_BY_ROBOTS_TXT")
        || is(&status.page_fetch_state, "BLOCKED_ROBOTS_TXT")
        || coverage.contains("blocked by robots.txt")
    {
        findings.add(
            "blocked_by_robots_txt",
            Severity::Error,
            "robots.txt blocks Googlebot from crawling this URL.",
            FIX_ROBOTS,
        );
    }

    // Indexing forbidden by the page itself.
    if is(&status.indexing_state, "BLOCKED_BY_META_TAG") {
        findings.add(
            "noindex_meta_tag",
            Severity::Error,
            "The page has a robots meta tag containing noindex.",
            "Remove `noindex` from the <meta name=\"robots\"> (or \"googlebot\") tag in the \
             page's <head>. Check templates and CMS or SEO-plugin settings that add it.",
        );
    } else if is(&status.indexing_state, "BLOCKED_BY_HTTP_HEADER") {
        findings.add(
            "noindex_http_header",
            Severity::Error,
            "The response carries an `X-Robots-Tag: noindex` header.",
            "Remove the X-Robots-Tag noindex header in the server, CDN, or framework \
             configuration for this path.",
        );
    } else if coverage.contains("noindex") {
        findings.add(
            "noindex",
            Severity::Error,
            "The page is excluded by a noindex directive.",
            "Remove noindex from the page's robots meta tag or its X-Robots-Tag response header.",
        );
    }

    // The last fetch failed.
    match status.page_fetch_state.as_deref() {
        Some("SOFT_404") => findings.add(
            "soft_404",
            Severity::Error,
            "Google treats this page as a soft 404: it returns 200 but looks empty or like \
             an error page.",
            "Serve real content in the initial HTML, or return a 404/410 status if the page \
             no longer exists. Look for thin content and client-side rendering that leaves \
             the HTML empty.",
        ),
        Some("NOT_FOUND") => findings.add(
            "not_found_404",
            Severity::Error,
            "The URL returned 404 when Google crawled it.",
            FIX_NOT_FOUND,
        ),
        Some("ACCESS_DENIED") => findings.add(
            "unauthorized_401",
            Severity::Error,
            "The URL asked Googlebot to authenticate (401).",
            "Serve the page without requiring a login, or drop it from sitemaps if it is \
             meant to be private.",
        ),
        Some("ACCESS_FORBIDDEN") => findings.add(
            "forbidden_403",
            Severity::Error,
            "The server refused Googlebot with 403.",
            "Allow Googlebot through the firewall, WAF, or bot protection. Verify real \
             Googlebot by reverse DNS rather than blocking by user agent or IP range.",
        ),
        Some("SERVER_ERROR") => findings.add(
            "server_error_5xx",
            Severity::Error,
            "The server returned a 5xx error when Google crawled the URL.",
            FIX_SERVER_ERROR,
        ),
        Some("REDIRECT_ERROR") => findings.add(
            "redirect_error",
            Severity::Error,
            "Google could not follow this URL's redirect (a loop, too long a chain, or a \
             bad target).",
            "Make the URL redirect in a single hop to a final URL that returns 200.",
        ),
        Some("BLOCKED_4XX") => findings.add(
            "blocked_4xx",
            Severity::Error,
            "The server returned a 4xx error other than 401, 403, or 404.",
            "Request the URL as Googlebot to see the exact status (often 410 or 429) and \
             fix its cause.",
        ),
        Some("INTERNAL_CRAWL_ERROR") => findings.add(
            "crawl_error",
            Severity::Warning,
            "Google hit an internal error while crawling this URL.",
            "Usually temporary. Inspect again later; if it persists, check that the page \
             loads quickly and reliably.",
        ),
        Some("INVALID_URL") => findings.add(
            "invalid_url",
            Severity::Error,
            "Google considers this URL malformed.",
            "Check the URL for unencoded or invalid characters and fix the links that use it.",
        ),
        _ => {}
    }

    // The same failures, when only the prose says so.
    if coverage.contains("soft 404") {
        findings.add(
            "soft_404",
            Severity::Error,
            "Google treats this page as a soft 404.",
            "Serve real content, or return a 404/410 status if the page no longer exists.",
        );
    } else if coverage.contains("(404)") {
        findings.add(
            "not_found_404",
            Severity::Error,
            "The URL returned 404 when Google crawled it.",
            FIX_NOT_FOUND,
        );
    } else if coverage.contains("(5xx)") {
        findings.add(
            "server_error_5xx",
            Severity::Error,
            "The server returned a 5xx error when Google crawled the URL.",
            FIX_SERVER_ERROR,
        );
    }

    // Google decided not to index, or has not got round to it.
    if coverage.contains("crawled") && coverage.contains("not indexed") {
        findings.add(
            "crawled_not_indexed",
            Severity::Warning,
            "Google crawled the page but chose not to index it.",
            "This is a quality or duplication signal rather than a technical block. Make \
             the content substantial and distinct from other pages, link to it from \
             relevant indexed pages, then request indexing in Search Console.",
        );
    } else if coverage.contains("discovered") && coverage.contains("not indexed") {
        findings.add(
            "discovered_not_indexed",
            Severity::Warning,
            "Google knows this URL exists but has not crawled it yet.",
            "Add internal links to the page and keep it in a sitemap. Check that the server \
             responds quickly, since slow sites are crawled less. New pages may only need time.",
        );
    } else if coverage.contains("unknown to google") {
        findings.add(
            "unknown_to_google",
            Severity::Warning,
            "Google has never seen this URL.",
            "Check the URL is correct and returns 200, list it in a sitemap \
             (`gsc sitemaps submit`), and link to it from pages that are already indexed.",
        );
    } else if coverage.contains("indexed without content") {
        findings.add(
            "indexed_without_content",
            Severity::Warning,
            "The page is indexed, but Google could not read its content.",
            "Check what the server returns to Googlebot: an empty response, a rendering \
             failure, or content that differs from what visitors get.",
        );
    }

    // Duplicates and canonical URLs.
    let google = status.google_canonical.as_deref();
    let declared = status.user_canonical.as_deref();
    match (google, declared) {
        (Some(google), Some(declared)) if google != declared => findings.add(
            "canonical_mismatch",
            Severity::Warning,
            "Google chose a different canonical URL than the one this page declares.",
            "If Google's choice is wrong, send consistent signals: the same URL in \
             rel=canonical, internal links, the sitemap, and redirects. Make sure the two \
             pages are not near-duplicates.",
        ),
        (Some(google), _) if google != url => findings.add(
            "canonicalised_elsewhere",
            Severity::Info,
            "Google indexes a different URL in place of this one.",
            "Nothing to do if that is the preferred URL. Otherwise point rel=canonical at \
             this URL and differentiate its content.",
        ),
        _ => {}
    }
    if coverage.contains("duplicate without user-selected canonical") {
        findings.add(
            "duplicate_no_canonical",
            Severity::Warning,
            "Google sees this page as a duplicate, and it does not declare a canonical URL.",
            "Add a rel=canonical link pointing at the preferred version of this content.",
        );
    }
    if coverage.contains("redirect") && !findings.0.iter().any(|i| i.code == "redirect_error") {
        findings.add(
            "redirected",
            Severity::Info,
            "This URL redirects, so its target is indexed instead.",
            "Expected for a redirect. Point internal links and sitemap entries at the final URL.",
        );
    }

    // Not indexed, yet nothing above says why.
    if !inspection.is_indexed() && findings.0.is_empty() {
        let reason = status
            .coverage_state
            .as_deref()
            .unwrap_or("no reason given");
        findings.add(
            "not_indexed",
            Severity::Warning,
            format!("The URL is not indexed: {reason}."),
            "Open the inspection link in Search Console for the full explanation.",
        );
    }

    if inspection.is_indexed() && coverage.contains("not submitted in sitemap") {
        findings.add(
            "not_in_sitemap",
            Severity::Info,
            "The page is indexed but is not in any submitted sitemap.",
            "Add it to a sitemap so Google notices changes to it sooner.",
        );
    }

    // Problems Google describes in its own words.
    for detected in inspection
        .rich_results_result
        .iter()
        .flat_map(|r| &r.detected_items)
    {
        let kind = detected
            .rich_result_type
            .as_deref()
            .unwrap_or("Rich result");
        for item in &detected.items {
            for issue in &item.issues {
                reported(
                    &mut findings.0,
                    "rich_result",
                    kind,
                    issue,
                    FIX_STRUCTURED_DATA,
                );
            }
        }
    }
    for issue in inspection.amp_result.iter().flat_map(|amp| &amp.issues) {
        reported(
            &mut findings.0,
            "amp",
            "AMP",
            issue,
            "Fix the AMP page and validate it with the AMP Test (https://search.google.com/test/amp).",
        );
    }
    for issue in inspection
        .mobile_usability_result
        .iter()
        .flat_map(|m| &m.issues)
    {
        reported(
            &mut findings.0,
            "mobile_usability",
            "Mobile usability",
            issue,
            "Adjust the page's responsive layout: viewport meta tag, font sizes, and tap-target spacing.",
        );
    }

    let mut issues = findings.0;
    issues.sort_by_key(|issue| issue.severity);
    issues
}

/// Record one issue that Google worded itself. Unlike the index findings,
/// every distinct message is kept: a page can have many structured-data errors.
fn reported(
    issues: &mut Vec<Issue>,
    family: &str,
    subject: &str,
    issue: &ReportedIssue,
    fix: &str,
) {
    let (code, severity) = match (family, issue.is_error()) {
        ("rich_result", true) => ("rich_result_error", Severity::Error),
        ("rich_result", false) => ("rich_result_warning", Severity::Warning),
        ("amp", true) => ("amp_error", Severity::Error),
        ("amp", false) => ("amp_warning", Severity::Warning),
        (_, true) => ("mobile_usability_error", Severity::Error),
        (_, false) => ("mobile_usability_warning", Severity::Warning),
    };
    let message = format!("{subject}: {}", issue.text());
    if !issues
        .iter()
        .any(|known| known.code == code && known.message == message)
    {
        issues.push(Issue {
            code,
            severity,
            message,
            fix: fix.to_owned(),
        });
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const URL: &str = "https://example.com/page";

    fn issues_for(index_status: Value) -> Vec<Issue> {
        let inspection =
            Inspection::from_raw(&json!({ "indexStatusResult": index_status })).unwrap();
        diagnose(URL, &inspection)
    }

    fn codes(issues: &[Issue]) -> Vec<&'static str> {
        issues.iter().map(|issue| issue.code).collect()
    }

    fn healthy() -> Value {
        json!({
            "verdict": "PASS",
            "coverageState": "Submitted and indexed",
            "robotsTxtState": "ALLOWED",
            "indexingState": "INDEXING_ALLOWED",
            "pageFetchState": "SUCCESSFUL",
            "googleCanonical": URL,
            "userCanonical": URL,
        })
    }

    #[test]
    fn a_healthy_indexed_page_has_no_issues() {
        assert_eq!(issues_for(healthy()), []);
    }

    #[test]
    fn robots_block_is_reported_once_however_many_fields_say_so() {
        let issues = issues_for(json!({
            "verdict": "NEUTRAL",
            "coverageState": "Blocked by robots.txt",
            "robotsTxtState": "DISALLOWED",
            "indexingState": "BLOCKED_BY_ROBOTS_TXT",
            "pageFetchState": "BLOCKED_ROBOTS_TXT",
        }));
        assert_eq!(codes(&issues), ["blocked_by_robots_txt"]);
        assert_eq!(issues[0].severity, Severity::Error);
        assert!(issues[0].fix.contains("Disallow"));
    }

    #[test]
    fn noindex_distinguishes_meta_tag_from_header() {
        let meta = issues_for(json!({
            "verdict": "NEUTRAL",
            "coverageState": "Excluded by 'noindex' tag",
            "indexingState": "BLOCKED_BY_META_TAG",
            "pageFetchState": "SUCCESSFUL",
        }));
        assert_eq!(codes(&meta), ["noindex_meta_tag"]);

        let header = issues_for(json!({
            "verdict": "NEUTRAL",
            "coverageState": "Excluded by 'noindex' tag",
            "indexingState": "BLOCKED_BY_HTTP_HEADER",
        }));
        assert_eq!(codes(&header), ["noindex_http_header"]);
        assert!(header[0].fix.contains("X-Robots-Tag"));

        let prose_only = issues_for(json!({
            "verdict": "NEUTRAL",
            "coverageState": "Excluded by 'noindex' tag",
        }));
        assert_eq!(codes(&prose_only), ["noindex"]);
    }

    #[test]
    fn each_fetch_failure_has_its_own_code() {
        let cases = [
            ("SOFT_404", "soft_404"),
            ("NOT_FOUND", "not_found_404"),
            ("ACCESS_DENIED", "unauthorized_401"),
            ("ACCESS_FORBIDDEN", "forbidden_403"),
            ("SERVER_ERROR", "server_error_5xx"),
            ("REDIRECT_ERROR", "redirect_error"),
            ("BLOCKED_4XX", "blocked_4xx"),
            ("INTERNAL_CRAWL_ERROR", "crawl_error"),
            ("INVALID_URL", "invalid_url"),
        ];
        for (state, code) in cases {
            let issues = issues_for(json!({ "verdict": "FAIL", "pageFetchState": state }));
            assert_eq!(codes(&issues), [code], "pageFetchState {state}");
        }
    }

    #[test]
    fn fetch_failures_are_recognised_from_prose_alone() {
        let cases = [
            ("Soft 404", "soft_404"),
            ("Not found (404)", "not_found_404"),
            ("Server error (5xx)", "server_error_5xx"),
        ];
        for (coverage, code) in cases {
            let issues = issues_for(json!({ "verdict": "FAIL", "coverageState": coverage }));
            assert_eq!(codes(&issues), [code], "{coverage}");
        }
        // Not doubled up when the enum says the same thing.
        let both = issues_for(json!({
            "verdict": "FAIL",
            "coverageState": "Not found (404)",
            "pageFetchState": "NOT_FOUND",
        }));
        assert_eq!(codes(&both), ["not_found_404"]);
    }

    #[test]
    fn googles_indexing_decisions_are_warnings_with_distinct_advice() {
        let crawled = issues_for(json!({
            "verdict": "NEUTRAL",
            "coverageState": "Crawled - currently not indexed",
            "pageFetchState": "SUCCESSFUL",
        }));
        assert_eq!(codes(&crawled), ["crawled_not_indexed"]);
        assert_eq!(crawled[0].severity, Severity::Warning);

        let discovered = issues_for(json!({
            "verdict": "NEUTRAL",
            "coverageState": "Discovered - currently not indexed",
        }));
        assert_eq!(codes(&discovered), ["discovered_not_indexed"]);
        assert_ne!(crawled[0].fix, discovered[0].fix);

        let unknown = issues_for(json!({
            "verdict": "NEUTRAL",
            "coverageState": "URL is unknown to Google",
        }));
        assert_eq!(codes(&unknown), ["unknown_to_google"]);
        assert!(unknown[0].fix.contains("gsc sitemaps submit"));

        let empty = issues_for(json!({
            "verdict": "PASS",
            "coverageState": "Page indexed without content",
        }));
        assert_eq!(codes(&empty), ["indexed_without_content"]);
    }

    #[test]
    fn canonical_disagreement_is_a_warning() {
        let issues = issues_for(json!({
            "verdict": "NEUTRAL",
            "coverageState": "Duplicate, Google chose different canonical than user",
            "googleCanonical": "https://example.com/other",
            "userCanonical": URL,
        }));
        assert_eq!(codes(&issues), ["canonical_mismatch"]);
        assert_eq!(
            issues[0].message,
            "Google chose a different canonical URL than the one this page declares."
        );
    }

    #[test]
    fn an_intentional_alternate_is_information_not_a_problem() {
        let issues = issues_for(json!({
            "verdict": "NEUTRAL",
            "coverageState": "Alternate page with proper canonical tag",
            "googleCanonical": "https://example.com/canonical",
            "userCanonical": "https://example.com/canonical",
        }));
        assert_eq!(codes(&issues), ["canonicalised_elsewhere"]);
        assert_eq!(issues[0].severity, Severity::Info);
    }

    #[test]
    fn duplicate_without_a_declared_canonical() {
        let issues = issues_for(json!({
            "verdict": "NEUTRAL",
            "coverageState": "Duplicate without user-selected canonical",
            "googleCanonical": "https://example.com/original",
        }));
        assert_eq!(
            codes(&issues),
            ["duplicate_no_canonical", "canonicalised_elsewhere"],
            "the warning sorts ahead of the informational note"
        );
    }

    #[test]
    fn redirects_are_informational_unless_they_fail() {
        let fine = issues_for(json!({
            "verdict": "NEUTRAL",
            "coverageState": "Page with redirect",
        }));
        assert_eq!(codes(&fine), ["redirected"]);
        assert_eq!(fine[0].severity, Severity::Info);

        let broken = issues_for(json!({
            "verdict": "FAIL",
            "coverageState": "Redirect error",
            "pageFetchState": "REDIRECT_ERROR",
        }));
        assert_eq!(codes(&broken), ["redirect_error"]);
    }

    #[test]
    fn an_unexplained_exclusion_still_produces_an_issue() {
        let issues = issues_for(json!({
            "verdict": "NEUTRAL",
            "coverageState": "Some state Google adds in future",
        }));
        assert_eq!(codes(&issues), ["not_indexed"]);
        assert_eq!(
            issues[0].message,
            "The URL is not indexed: Some state Google adds in future."
        );

        assert_eq!(codes(&issues_for(json!({}))), ["not_indexed"]);
    }

    #[test]
    fn indexed_but_missing_from_sitemaps_is_a_tip() {
        let mut status = healthy();
        status["coverageState"] = json!("Indexed, not submitted in sitemap");
        let issues = issues_for(status);
        assert_eq!(codes(&issues), ["not_in_sitemap"]);
        assert_eq!(issues[0].severity, Severity::Info);
    }

    #[test]
    fn structured_data_problems_keep_googles_wording_and_sort_by_severity() {
        let inspection = Inspection::from_raw(&json!({
            "indexStatusResult": healthy(),
            "richResultsResult": { "verdict": "FAIL", "detectedItems": [
                { "richResultType": "Product snippets", "items": [
                    { "name": "Widget", "issues": [
                        { "issueMessage": "Missing field \"review\" (optional)", "severity": "WARNING" },
                        { "issueMessage": "Missing field \"price\"", "severity": "ERROR" },
                    ]},
                    { "name": "Gadget", "issues": [
                        { "issueMessage": "Missing field \"price\"", "severity": "ERROR" },
                    ]},
                ]},
                { "richResultType": "Breadcrumbs", "items": [{ "name": "ok" }] },
            ]},
            "ampResult": { "verdict": "FAIL", "issues": [
                { "issueMessage": "Disallowed attribute", "severity": "ERROR" },
            ]},
            "mobileUsabilityResult": { "verdict": "FAIL", "issues": [
                { "issueType": "USE_LEGIBLE_FONT_SIZES", "message": "Text too small to read", "severity": "WARNING" },
            ]},
        }))
        .unwrap();
        let issues = diagnose(URL, &inspection);

        assert_eq!(
            issues
                .iter()
                .map(|i| (i.code, i.message.as_str()))
                .collect::<Vec<_>>(),
            [
                (
                    "rich_result_error",
                    "Product snippets: Missing field \"price\""
                ),
                ("amp_error", "AMP: Disallowed attribute"),
                (
                    "rich_result_warning",
                    "Product snippets: Missing field \"review\" (optional)"
                ),
                (
                    "mobile_usability_warning",
                    "Mobile usability: Text too small to read"
                ),
            ],
            "identical messages are merged; errors come before warnings"
        );
    }

    #[test]
    fn issues_serialise_with_lowercase_severity() {
        let issues = issues_for(json!({ "verdict": "FAIL", "pageFetchState": "NOT_FOUND" }));
        let value = serde_json::to_value(&issues[0]).unwrap();
        assert_eq!(value["code"], "not_found_404");
        assert_eq!(value["severity"], "error");
        assert!(value["message"].is_string());
        assert!(value["fix"].is_string());
    }
}
