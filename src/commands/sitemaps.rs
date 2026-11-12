//! `gsc sitemaps` — list, inspect, submit, and remove sitemaps.

use clap::Subcommand;
use serde_json::{Value, json};

use crate::Context;
use crate::api::sitemaps::Sitemap;
use crate::error::{Error, Result};
use crate::output::{Column, Kind, Report};

#[derive(Debug, Subcommand)]
pub enum SitemapsCommand {
    /// List the sitemaps submitted for the property
    List {
        /// Only list the sitemaps inside this sitemap index
        #[arg(long, value_name = "URL")]
        index: Option<String>,
    },

    /// Show one sitemap's status, counts, and contents
    Get {
        /// Sitemap URL, or a path such as `/sitemap.xml` for a URL-prefix property
        sitemap: String,
    },

    /// Submit a sitemap, or ask Google to fetch it again
    Submit {
        /// Sitemap URL, or a path such as `/sitemap.xml` for a URL-prefix property
        sitemap: String,
    },

    /// Remove a sitemap from Search Console
    ///
    /// This only withdraws the submission. The file on your site is not
    /// touched, and URLs Google already knows stay indexed.
    #[command(alias = "remove")]
    Delete {
        /// Sitemap URL, or a path such as `/sitemap.xml` for a URL-prefix property
        sitemap: String,

        /// Confirm the removal
        #[arg(long)]
        yes: bool,
    },
}

pub async fn run(command: SitemapsCommand, ctx: &Context) -> Result<()> {
    let client = ctx.client()?;
    let site = ctx.site(&client).await?;

    match command {
        SitemapsCommand::List { index } => {
            let index = index.map(|i| sitemap_url(&site, &i)).transpose()?;
            let sitemaps = client.list_sitemaps(&site, index.as_deref()).await?;
            let rows: Vec<Value> = sitemaps.iter().map(Sitemap::to_json).collect();
            let mut report = Report::new(json!({ "site": site, "sitemaps": rows }))
                .field("Property", site)
                .table(
                    vec![
                        Column::text("path", "Sitemap"),
                        Column::text("type", "Type"),
                        Column::new("submitted", "URLs", Kind::Int),
                        Column::new("errors", "Errors", Kind::Int),
                        Column::new("warnings", "Warnings", Kind::Int),
                        Column::new("lastDownloaded", "Last read", Kind::Date),
                        Column::text("isPending", "Pending"),
                    ],
                    "/sitemaps",
                );
            if sitemaps.is_empty() {
                report =
                    report.note("No sitemaps submitted. Add one with `gsc sitemaps submit <url>`.");
            } else if sitemaps.iter().any(|s| s.errors > 0) {
                report = report.note(ERROR_DETAIL_NOTE);
            }
            ctx.emit(&report)
        }
        SitemapsCommand::Get { sitemap } => {
            let url = sitemap_url(&site, &sitemap)?;
            let found = client.get_sitemap(&site, &url).await?;
            ctx.emit(&detail(&site, &found))
        }
        SitemapsCommand::Submit { sitemap } => {
            let url = sitemap_url(&site, &sitemap)?;
            client.submit_sitemap(&site, &url).await?;
            let report = Report::new(json!({ "site": site, "sitemap": url, "submitted": true }))
                .field("Submitted", url.clone())
                .field("Property", site)
                .note(format!(
                    "Google fetches it in the background. Check progress with \
                         `gsc sitemaps get {url}`."
                ));
            ctx.emit(&report)
        }
        SitemapsCommand::Delete { sitemap, yes } => {
            let url = sitemap_url(&site, &sitemap)?;
            if !yes {
                return Err(
                    Error::usage(format!("refusing to remove {url} without confirmation"))
                        .with_hint("pass --yes to withdraw this sitemap from Search Console"),
                );
            }
            client.delete_sitemap(&site, &url).await?;
            ctx.emit(
                &Report::new(json!({ "site": site, "sitemap": url, "deleted": true }))
                    .field("Removed", url)
                    .field("Property", site),
            )
        }
    }
}

/// The API reports how many errors a sitemap has, but not what they are.
const ERROR_DETAIL_NOTE: &str = "The API only reports error counts. To see what is wrong, fetch \
     the sitemap and validate it, or open the Sitemaps report in Search Console.";

/// Turn what the user typed into the sitemap's full URL.
///
/// A path is only meaningful for a URL-prefix property, whose identifier is
/// itself the URL the path is relative to.
fn sitemap_url(site: &str, input: &str) -> Result<String> {
    let input = input.trim();
    if input.starts_with("http://") || input.starts_with("https://") {
        return Ok(input.to_owned());
    }
    if input.is_empty() {
        return Err(Error::usage("the sitemap cannot be empty"));
    }
    if site.starts_with("sc-domain:") {
        return Err(Error::usage(format!(
            "`{input}` is not a full URL, and {site} is a Domain property"
        ))
        .with_hint("give the sitemap's full URL, e.g. https://example.com/sitemap.xml"));
    }
    Ok(format!(
        "{}/{}",
        site.trim_end_matches('/'),
        input.trim_start_matches('/')
    ))
}

fn detail(site: &str, sitemap: &Sitemap) -> Report {
    let mut document = sitemap.to_json();
    document["site"] = json!(site);

    let status = if sitemap.is_pending {
        "pending (not processed yet)"
    } else if sitemap.errors > 0 {
        "has errors"
    } else {
        "processed"
    };
    let kind = match (&sitemap.kind, sitemap.is_sitemaps_index) {
        (_, true) => "sitemap index".to_owned(),
        (Some(kind), false) => crate::api::lower_camel(kind),
        (None, false) => "-".to_owned(),
    };
    let or_never = |value: &Option<String>| value.clone().unwrap_or_else(|| "never".to_owned());

    let mut report = Report::new(document)
        .field("Sitemap", sitemap.path.clone())
        .field("Property", site)
        .field("Status", status)
        .field("Type", kind)
        .field("Errors", sitemap.errors.to_string())
        .field("Warnings", sitemap.warnings.to_string())
        .field("Last submitted", or_never(&sitemap.last_submitted))
        .field("Last read", or_never(&sitemap.last_downloaded))
        .table(
            vec![
                Column::text("type", "Content"),
                Column::new("submitted", "URLs", Kind::Int),
            ],
            "/contents",
        );
    if sitemap.errors > 0 {
        report = report.note(ERROR_DETAIL_NOTE);
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_urls_pass_through_for_any_property() {
        for site in ["sc-domain:example.com", "https://example.com/"] {
            assert_eq!(
                sitemap_url(site, " https://cdn.example.com/sitemap.xml ").unwrap(),
                "https://cdn.example.com/sitemap.xml"
            );
        }
    }

    #[test]
    fn paths_are_joined_to_a_url_prefix_property() {
        assert_eq!(
            sitemap_url("https://example.com/", "/sitemap.xml").unwrap(),
            "https://example.com/sitemap.xml"
        );
        assert_eq!(
            sitemap_url("https://example.com/", "sitemap.xml").unwrap(),
            "https://example.com/sitemap.xml"
        );
        assert_eq!(
            sitemap_url("https://example.com/blog/", "/news/sitemap.xml").unwrap(),
            "https://example.com/blog/news/sitemap.xml"
        );
    }

    #[test]
    fn paths_need_a_full_url_on_a_domain_property() {
        let err = sitemap_url("sc-domain:example.com", "/sitemap.xml").unwrap_err();
        assert!(err.message.contains("Domain property"));
        assert!(err.hint.unwrap().contains("full URL"));
        assert!(sitemap_url("https://example.com/", "  ").is_err());
    }
}
