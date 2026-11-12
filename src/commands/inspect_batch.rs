//! `gsc inspect batch` — inspect many URLs and group what is wrong.
//!
//! Search Console's "Page indexing" report is not available through the API.
//! Inspecting a set of URLs one by one and grouping the findings is the
//! closest equivalent, within the quota of 2,000 inspections per property
//! per day.

use std::collections::HashSet;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use clap::Args;
use futures_util::stream::{self, StreamExt};
use serde_json::{Map, Value, json};

use super::inspect::Inspected;
use crate::Context;
use crate::api::Client;
use crate::diagnose::Severity;
use crate::error::{Error, ErrorKind, Result};
use crate::output::{Column, Kind, Report, count};
use crate::site;
use crate::sitemap_xml;
use crate::url_list;

#[derive(Debug, Args)]
pub struct BatchArgs {
    /// URLs to inspect: full URLs, or paths for a URL-prefix property
    pub urls: Vec<String>,

    /// Read URLs from a file, one per line; `-` reads standard input
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,

    /// Take URLs from this sitemap or sitemap index (repeatable)
    #[arg(long, value_name = "URL")]
    pub sitemap: Vec<String>,

    /// Take URLs from every sitemap submitted for the property
    #[arg(long)]
    pub submitted_sitemaps: bool,

    /// Inspect at most this many URLs. Google allows 2,000 inspections per
    /// property per day
    #[arg(
        short = 'n',
        long,
        value_name = "N",
        default_value_t = 100,
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub limit: u32,

    /// How many inspections to run at the same time
    #[arg(
        long,
        value_name = "N",
        default_value_t = 5,
        value_parser = clap::value_parser!(u8).range(1..=20)
    )]
    pub concurrency: u8,

    /// List only the URLs that have errors or warnings
    #[arg(long)]
    pub only_issues: bool,
}

/// The URLs chosen for inspection and how they were arrived at.
struct Selection {
    urls: Vec<String>,
    /// More URLs were available than the limit allowed.
    truncated: bool,
    /// Sitemaps that could not be read.
    problems: Vec<String>,
}

/// What happened to one URL.
enum Outcome {
    Inspected(Inspected),
    Failed(Error),
    /// Not attempted, because an earlier request showed it would be futile.
    Skipped,
}

pub async fn run(args: BatchArgs, site: &str, client: &Client, ctx: &Context) -> Result<()> {
    let selection = select_urls(&args, site, client).await?;
    let outcomes = inspect_all(client, site, &selection.urls, args.concurrency.into()).await;

    // A rejected login fails every request identically; report it as itself.
    if let Some(error) = outcomes.iter().find_map(|outcome| match outcome {
        Outcome::Failed(error) if error.kind == ErrorKind::Auth => Some(error.clone()),
        _ => None,
    }) {
        return Err(error);
    }
    let inspected = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, Outcome::Inspected(_)))
        .count();
    if inspected == 0 {
        // Nothing to summarise: surface why, with its own exit code.
        return Err(outcomes
            .into_iter()
            .find_map(|outcome| match outcome {
                Outcome::Failed(error) => Some(error),
                _ => None,
            })
            .unwrap_or_else(|| Error::new(ErrorKind::RateLimit, "no URL could be inspected")));
    }

    ctx.emit(&build_report(site, &selection, &outcomes, args.only_issues))
}

/// Combine every URL source, de-duplicate, and apply the limit.
async fn select_urls(args: &BatchArgs, site: &str, client: &Client) -> Result<Selection> {
    let limit = args.limit as usize;
    let mut sitemaps = args
        .sitemap
        .iter()
        .map(|sitemap| site::absolute_url(site, sitemap, "sitemap"))
        .collect::<Result<Vec<_>>>()?;
    if args.submitted_sitemaps {
        let submitted = client.list_sitemaps(site, None).await?;
        sitemaps.extend(submitted.into_iter().map(|sitemap| sitemap.path));
    }

    let mut listed = args.urls.clone();
    if let Some(path) = &args.file {
        listed.extend(url_list::read(path)?);
    }
    if listed.is_empty() && sitemaps.is_empty() {
        return Err(Error::usage("no URLs to inspect").with_hint(
            "pass URLs as arguments, or use --file <path>, --sitemap <url>, \
             or --submitted-sitemaps",
        ));
    }

    let mut seen = HashSet::new();
    let mut urls = Vec::new();
    let mut truncated = false;
    for url in listed {
        let url = site::absolute_url(site, &url, "URL")?;
        if !seen.insert(url.clone()) {
            continue;
        }
        if urls.len() == limit {
            truncated = true;
            break;
        }
        urls.push(url);
    }

    let mut problems = Vec::new();
    if !sitemaps.is_empty() && !truncated {
        // Ask for one more new URL than there is room for: getting it proves
        // the selection was cut short.
        let room = limit - urls.len();
        let collected = sitemap_xml::collect_urls(client.http(), &sitemaps, room + 1, &seen).await;
        problems = collected.problems;
        truncated = collected.truncated || collected.urls.len() > room;
        urls.extend(collected.urls.into_iter().take(room));
    }

    if urls.is_empty() {
        return Err(match problems.first() {
            Some(problem) => Error::new(
                ErrorKind::Network,
                format!("could not read the sitemap {problem}"),
            )
            .with_hint("check the sitemap URL opens in a browser and is valid XML"),
            None => Error::usage("the sitemaps list no URLs"),
        });
    }
    Ok(Selection {
        urls,
        truncated,
        problems,
    })
}

/// Inspect every URL, a few at a time, keeping results in input order.
async fn inspect_all(
    client: &Client,
    site: &str,
    urls: &[String],
    concurrency: usize,
) -> Vec<Outcome> {
    // Set once the quota is exhausted or the login rejected: every later
    // request would fail the same way, so stop sending them.
    let stop = AtomicBool::new(false);
    let progress = Progress::new(urls.len());

    let outcomes = stream::iter(urls)
        .map(|url| {
            let (stop, progress) = (&stop, &progress);
            async move {
                if stop.load(Ordering::Relaxed) {
                    return Outcome::Skipped;
                }
                let outcome = match client.inspect_url(site, url).await {
                    Ok(raw) => match Inspected::new(url, &raw, false) {
                        Ok(inspected) => Outcome::Inspected(inspected),
                        Err(error) => Outcome::Failed(error),
                    },
                    Err(error) if error.kind == ErrorKind::RateLimit => {
                        stop.store(true, Ordering::Relaxed);
                        Outcome::Skipped
                    }
                    Err(error) => {
                        if error.kind == ErrorKind::Auth {
                            stop.store(true, Ordering::Relaxed);
                        }
                        Outcome::Failed(error)
                    }
                };
                progress.advance();
                outcome
            }
        })
        .buffered(concurrency)
        .collect()
        .await;
    progress.finish();
    outcomes
}

/// A one-line counter on stderr, shown only when a person is watching.
struct Progress {
    done: AtomicUsize,
    total: usize,
    enabled: bool,
}

impl Progress {
    fn new(total: usize) -> Self {
        Self {
            done: AtomicUsize::new(0),
            total,
            enabled: std::io::stderr().is_terminal(),
        }
    }

    fn advance(&self) {
        let done = self.done.fetch_add(1, Ordering::Relaxed) + 1;
        if self.enabled {
            eprint!("\rInspecting {done}/{}", self.total);
            let _ = std::io::stderr().flush();
        }
    }

    fn finish(&self) {
        if self.enabled {
            // Return to the start of the line and erase it.
            eprint!("\r\x1b[K");
            let _ = std::io::stderr().flush();
        }
    }
}

/// URLs sharing one problem.
struct Group {
    code: String,
    severity: Severity,
    message: String,
    fix: String,
    urls: Vec<String>,
}

fn add_to_group(
    groups: &mut Vec<Group>,
    code: &str,
    severity: Severity,
    message: &str,
    fix: &str,
    url: &str,
) {
    match groups
        .iter_mut()
        .find(|group| group.code == code && group.message == message)
    {
        Some(group) => group.urls.push(url.to_owned()),
        None => groups.push(Group {
            code: code.to_owned(),
            severity,
            message: message.to_owned(),
            fix: fix.to_owned(),
            urls: vec![url.to_owned()],
        }),
    }
}

fn build_report(
    site: &str,
    selection: &Selection,
    outcomes: &[Outcome],
    only_issues: bool,
) -> Report {
    let mut groups: Vec<Group> = Vec::new();
    let mut rows: Vec<Value> = Vec::new();
    let (mut inspected, mut indexed, mut with_issues, mut failed, mut skipped) = (0, 0, 0, 0, 0);

    for (url, outcome) in selection.urls.iter().zip(outcomes) {
        let mut row = Map::new();
        row.insert("url".into(), json!(url));
        let has_problem = match outcome {
            Outcome::Inspected(result) => {
                inspected += 1;
                if result.indexed {
                    indexed += 1;
                }
                row.insert("indexed".into(), json!(result.indexed));
                for key in ["coverageState", "lastCrawlTime"] {
                    if let Some(value) = result.summary.get(key) {
                        row.insert(key.into(), value.clone());
                    }
                }
                // A canonical is only worth a column when it points elsewhere.
                for key in ["googleCanonical", "userCanonical"] {
                    if let Some(value) = result.summary.get(key)
                        && value.as_str() != Some(url)
                    {
                        row.insert(key.into(), value.clone());
                    }
                }
                let codes: Vec<&str> = result.issues.iter().map(|issue| issue.code).collect();
                row.insert("issues".into(), json!(codes));
                for issue in &result.issues {
                    add_to_group(
                        &mut groups,
                        issue.code,
                        issue.severity,
                        &issue.message,
                        &issue.fix,
                        url,
                    );
                }
                let actionable = result
                    .issues
                    .iter()
                    .any(|issue| issue.severity != Severity::Info);
                if actionable {
                    with_issues += 1;
                }
                actionable
            }
            Outcome::Failed(error) => {
                failed += 1;
                row.insert("error".into(), json!(error.message));
                row.insert("issues".into(), json!(["inspection_failed"]));
                add_to_group(
                    &mut groups,
                    "inspection_failed",
                    Severity::Error,
                    &error.message,
                    error.hint.as_deref().unwrap_or(
                        "Check that the URL belongs to this property, then inspect it again.",
                    ),
                    url,
                );
                true
            }
            Outcome::Skipped => {
                skipped += 1;
                row.insert("skipped".into(), json!(true));
                row.insert("issues".into(), json!(["not_inspected"]));
                false
            }
        };
        if has_problem || !only_issues {
            rows.push(Value::Object(row));
        }
    }

    // Most serious first, then the problems affecting the most URLs.
    groups.sort_by(|a, b| (a.severity, b.urls.len()).cmp(&(b.severity, a.urls.len())));
    let issues: Vec<Value> = groups
        .iter()
        .map(|group| {
            json!({
                "code": group.code,
                "severity": group.severity,
                "count": group.urls.len(),
                "message": group.message,
                "fix": group.fix,
                "urls": group.urls,
            })
        })
        .collect();

    let document = json!({
        "site": site,
        "summary": {
            "urls": selection.urls.len(),
            "inspected": inspected,
            "indexed": indexed,
            "notIndexed": inspected - indexed,
            "withIssues": with_issues,
            "failed": failed,
            "skipped": skipped,
        },
        "truncated": selection.truncated,
        "issues": issues,
        "results": rows,
    });

    let mut report = Report::new(document)
        .field("Property", site)
        .field(
            "Inspected",
            format!("{inspected} of {}", count(selection.urls.len(), "URL")),
        )
        .field("Indexed", indexed.to_string())
        .field("Not indexed", (inspected - indexed).to_string())
        .field("With issues", with_issues.to_string());
    if failed > 0 {
        report = report.field("Failed", failed.to_string());
    }
    if skipped > 0 {
        report = report.field("Not inspected", skipped.to_string());
    }
    report = report
        .table(
            vec![
                Column::text("url", "URL"),
                Column::text("indexed", "Indexed"),
                Column::text("coverageState", "Coverage"),
                Column::new("lastCrawlTime", "Last crawl", Kind::Date),
                Column::text("issues", "Issues"),
            ],
            "/results",
        )
        .section(
            "Issues",
            vec![
                Column::text("severity", "Severity"),
                Column::text("code", "Issue"),
                Column::new("count", "URLs", Kind::Int),
                Column::text("message", "What is wrong"),
                Column::text("fix", "How to fix it"),
            ],
            "/issues",
        );

    if groups.is_empty() && skipped == 0 {
        report = report.note("No issues found in the inspected URLs.");
    }
    if skipped > 0 {
        report = report.note(format!(
            "The URL Inspection quota ran out with {} left uninspected. It resets daily; \
             rerun later or with a lower --limit.",
            count(skipped, "URL")
        ));
    }
    if selection.truncated {
        report = report.note(format!(
            "More URLs exist than the {} selected. Raise --limit to cover them (Google \
             allows 2,000 inspections per property per day).",
            selection.urls.len()
        ));
    }
    for problem in &selection.problems {
        report = report.note(format!("Could not read the sitemap {problem}"));
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_merge_identical_problems_and_keep_distinct_messages_apart() {
        let mut groups = Vec::new();
        let fix = "fix it";
        add_to_group(
            &mut groups,
            "soft_404",
            Severity::Error,
            "Soft 404.",
            fix,
            "https://x/a",
        );
        add_to_group(
            &mut groups,
            "soft_404",
            Severity::Error,
            "Soft 404.",
            fix,
            "https://x/b",
        );
        add_to_group(
            &mut groups,
            "rich_result_error",
            Severity::Error,
            "Product: Missing field \"price\"",
            fix,
            "https://x/a",
        );
        add_to_group(
            &mut groups,
            "rich_result_error",
            Severity::Error,
            "Product: Missing field \"name\"",
            fix,
            "https://x/a",
        );

        let shape: Vec<(&str, usize)> = groups
            .iter()
            .map(|group| (group.code.as_str(), group.urls.len()))
            .collect();
        assert_eq!(
            shape,
            [
                ("soft_404", 2),
                ("rich_result_error", 1),
                ("rich_result_error", 1)
            ]
        );
    }
}
