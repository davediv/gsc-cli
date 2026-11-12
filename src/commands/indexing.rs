//! `gsc indexing` — notify Google about changed URLs through the Indexing API.

use std::path::PathBuf;

use clap::Subcommand;
use serde_json::{Value, json};

use crate::Context;
use crate::api::indexing::Notification;
use crate::error::{Error, ErrorKind, Result};
use crate::output::{Column, Kind, Report, count};
use crate::url_list;

#[derive(Debug, Subcommand)]
pub enum IndexingCommand {
    /// Tell Google that URLs were updated (or, with --deleted, removed)
    ///
    /// Google prioritises a fresh crawl of each URL. Limited to 200
    /// notifications per day by default.
    Publish {
        /// Full URLs to report
        urls: Vec<String>,

        /// Read more URLs from a file, one per line; `-` reads standard input
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,

        /// Report the URLs as removed instead of updated
        #[arg(long, requires = "yes")]
        deleted: bool,

        /// Confirm a --deleted notification, which asks Google to drop the URLs
        #[arg(long)]
        yes: bool,
    },

    /// Show the latest notifications Google has on record for a URL
    Status {
        /// Full URL to look up
        url: String,
    },
}

pub async fn run(command: IndexingCommand, ctx: &Context) -> Result<()> {
    let client = ctx.client()?;
    match command {
        IndexingCommand::Publish {
            mut urls,
            file,
            deleted,
            yes: _,
        } => {
            if let Some(path) = &file {
                urls.extend(url_list::read(path)?);
            }
            let mut seen = std::collections::HashSet::new();
            urls.retain(|url| seen.insert(url.clone()));
            if urls.is_empty() {
                return Err(Error::usage("no URLs to publish")
                    .with_hint("pass URLs as arguments or with --file <path>"));
            }
            for url in &urls {
                require_full_url(url)?;
            }
            let kind = if deleted {
                Notification::Deleted
            } else {
                Notification::Updated
            };

            let mut rows: Vec<Value> = Vec::new();
            let mut first_error: Option<Error> = None;
            let (mut published, mut failed, mut skipped) = (0, 0, 0);
            // Once the daily quota is gone, further requests cannot succeed.
            let mut quota_exhausted = false;
            for url in &urls {
                if quota_exhausted {
                    skipped += 1;
                    rows.push(json!({ "url": url, "published": false, "skipped": true }));
                    continue;
                }
                match client.publish_notification(url, kind).await {
                    Ok(metadata) => {
                        published += 1;
                        rows.push(json!({
                            "url": url,
                            "published": true,
                            "notifyTime": latest_time(&metadata, kind),
                        }));
                    }
                    // A rejected login will fail for every URL: stop here.
                    Err(error) if error.kind == ErrorKind::Auth => return Err(error),
                    Err(error) => {
                        failed += 1;
                        quota_exhausted = error.kind == ErrorKind::RateLimit;
                        rows.push(json!({
                            "url": url,
                            "published": false,
                            "error": error.message,
                        }));
                        first_error.get_or_insert(error);
                    }
                }
            }
            // If nothing went through, the failure itself is the result.
            if published == 0
                && let Some(error) = first_error
            {
                return Err(error);
            }

            let mut report = Report::new(json!({
                "type": kind.api_name(),
                "summary": {
                    "urls": urls.len(),
                    "published": published,
                    "failed": failed,
                    "skipped": skipped,
                },
                "results": rows,
            }))
            .field("Notification", kind.api_name())
            .field(
                "Published",
                format!("{published} of {}", count(urls.len(), "URL")),
            )
            .table(
                vec![
                    Column::text("url", "URL"),
                    Column::text("published", "Published"),
                    Column::new("notifyTime", "Notified", Kind::Date),
                    Column::text("error", "Error"),
                ],
                "/results",
            );
            if skipped > 0 {
                report = report.note(format!(
                    "The Indexing API quota ran out with {} left unsent. It resets daily.",
                    count(skipped, "URL")
                ));
            }
            ctx.emit(&report)
        }
        IndexingCommand::Status { url } => {
            require_full_url(&url)?;
            let metadata = client.notification_metadata(&url).await?;
            let time = |kind: Notification| {
                latest_time(&metadata, kind).unwrap_or_else(|| "never".to_owned())
            };
            let report = Report::new(metadata.clone())
                .field("URL", url)
                .field("Last update notice", time(Notification::Updated))
                .field("Last removal notice", time(Notification::Deleted));
            ctx.emit(&report)
        }
    }
}

fn require_full_url(url: &str) -> Result<()> {
    if url.starts_with("https://") || url.starts_with("http://") {
        Ok(())
    } else {
        Err(Error::usage(format!("`{url}` is not a full URL"))
            .with_hint("the Indexing API needs absolute URLs, starting with https://"))
    }
}

/// When Google last received a notification of this kind for the URL.
fn latest_time(metadata: &Value, kind: Notification) -> Option<String> {
    let field = match kind {
        Notification::Updated => "latestUpdate",
        Notification::Deleted => "latestRemove",
    };
    metadata
        .get(field)?
        .get("notifyTime")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_notification_time_for_each_kind() {
        let metadata = json!({
            "url": "https://example.com/job",
            "latestUpdate": {
                "url": "https://example.com/job",
                "type": "URL_UPDATED",
                "notifyTime": "2026-11-12T10:00:00Z",
            },
        });
        assert_eq!(
            latest_time(&metadata, Notification::Updated).as_deref(),
            Some("2026-11-12T10:00:00Z")
        );
        assert_eq!(latest_time(&metadata, Notification::Deleted), None);
        assert_eq!(latest_time(&json!({}), Notification::Updated), None);
    }

    #[test]
    fn only_absolute_urls_are_accepted() {
        assert!(require_full_url("https://example.com/a").is_ok());
        assert!(require_full_url("http://example.com/a").is_ok());
        let err = require_full_url("/a").unwrap_err();
        assert_eq!(err.kind, ErrorKind::Usage);
    }
}
