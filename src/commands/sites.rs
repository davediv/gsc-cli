//! `gsc sites` — list and manage Search Console properties.

use clap::Subcommand;
use serde_json::{Value, json};

use crate::Context;
use crate::api::sites::Site;
use crate::error::{Error, Result};
use crate::output::{Column, Report};
use crate::site::{self, SiteRef};

#[derive(Debug, Subcommand)]
pub enum SitesCommand {
    /// List every property this account can see
    List,

    /// Show one property and your permission level on it
    Get {
        /// Property to look up [default: --site, $GSC_SITE, or the saved default]
        property: Option<String>,
    },

    /// Add a property to your Search Console account
    ///
    /// The property is created unverified. Prove ownership in Search Console
    /// (or with a DNS record for a Domain property) before data is available.
    Add {
        /// `sc-domain:example.com` or a URL prefix such as `https://example.com/`
        property: String,
    },

    /// Remove a property from your Search Console account
    #[command(alias = "delete")]
    Remove {
        /// `sc-domain:example.com`, a URL prefix, or a bare domain
        property: String,

        /// Confirm the removal
        #[arg(long)]
        yes: bool,
    },
}

pub async fn run(command: SitesCommand, ctx: &Context) -> Result<()> {
    let client = ctx.client()?;
    match command {
        SitesCommand::List => {
            let mut sites = client.list_sites().await?;
            // Usable properties first, then alphabetically.
            sites.sort_by(|a, b| {
                (!a.is_verified(), &a.site_url).cmp(&(!b.is_verified(), &b.site_url))
            });
            let rows: Vec<Value> = sites.iter().map(describe).collect();
            let mut report = Report::new(json!({ "sites": rows })).table(
                vec![
                    Column::text("siteUrl", "Property"),
                    Column::text("type", "Type"),
                    Column::text("permissionLevel", "Permission"),
                ],
                "/sites",
            );
            if sites.is_empty() {
                report = report.note(
                    "This account has no properties. A service account must be added as a \
                     user of each property in Search Console.",
                );
            }
            ctx.emit(&report)
        }
        SitesCommand::Get { property } => {
            let site_url = match property {
                Some(property) => ctx.resolve_site(&client, &property).await?,
                None => ctx.site(&client).await?,
            };
            let found = client.get_site(&site_url).await?;
            ctx.emit(&detail(&found))
        }
        SitesCommand::Add { property } => {
            let SiteRef::Exact(site_url) = site::parse(&property)? else {
                return Err(Error::usage(format!(
                    "`{property}` does not say which kind of property to add"
                ))
                .with_hint(format!(
                    "use `sc-domain:{property}` for a Domain property, or \
                     `https://{property}/` for a URL-prefix property"
                )));
            };
            client.add_site(&site_url).await?;
            let report = Report::new(json!({ "siteUrl": site_url, "added": true }))
                .field("Added", site_url)
                .note(
                    "The property is unverified until you prove ownership at \
                     https://search.google.com/search-console.",
                );
            ctx.emit(&report)
        }
        SitesCommand::Remove { property, yes } => {
            let site_url = ctx.resolve_site(&client, &property).await?;
            if !yes {
                return Err(Error::usage(format!(
                    "refusing to remove {site_url} without confirmation"
                ))
                .with_hint("pass --yes to remove this property from your account"));
            }
            client.delete_site(&site_url).await?;
            ctx.emit(
                &Report::new(json!({ "siteUrl": site_url, "removed": true }))
                    .field("Removed", site_url),
            )
        }
    }
}

/// A site as shown to users: the API's fields plus what they imply.
fn describe(site: &Site) -> Value {
    json!({
        "siteUrl": site.site_url,
        "type": if site.is_domain_property() { "domain" } else { "urlPrefix" },
        "permissionLevel": site.permission_level,
        "verified": site.is_verified(),
    })
}

fn detail(site: &Site) -> Report {
    Report::new(describe(site))
        .field("Property", site.site_url.clone())
        .field(
            "Type",
            if site.is_domain_property() {
                "Domain property"
            } else {
                "URL-prefix property"
            },
        )
        .field("Permission", site.permission_level.clone())
        .field("Verified", if site.is_verified() { "yes" } else { "no" })
}
