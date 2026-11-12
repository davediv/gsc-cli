//! Sites: the properties a user has in Search Console.

use reqwest::Method;
use serde::{Deserialize, Serialize};

use super::{Client, encode_segment, lower_camel};
use crate::auth::Api;
use crate::error::Result;

const UNVERIFIED: &str = "siteUnverifiedUser";

/// A Search Console property and the caller's access to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Site {
    /// `sc-domain:example.com` for a Domain property, or a URL ending in `/`
    /// for a URL-prefix property.
    pub site_url: String,
    /// `siteOwner`, `siteFullUser`, `siteRestrictedUser`, or `siteUnverifiedUser`.
    #[serde(default)]
    pub permission_level: String,
}

impl Site {
    /// Whether the caller can actually read data for this property.
    pub fn is_verified(&self) -> bool {
        self.permission_level != UNVERIFIED
    }

    pub fn is_domain_property(&self) -> bool {
        self.site_url.starts_with("sc-domain:")
    }

    fn normalized(mut self) -> Self {
        self.permission_level = lower_camel(&self.permission_level);
        self
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SitesList {
    #[serde(default)]
    site_entry: Vec<Site>,
}

fn site_path(site: &str) -> String {
    format!("/webmasters/v3/sites/{}", encode_segment(site))
}

impl Client {
    pub async fn list_sites(&self) -> Result<Vec<Site>> {
        let list: SitesList = self
            .call(
                Api::SearchConsole,
                Method::GET,
                "/webmasters/v3/sites",
                &[],
                None,
            )
            .await?;
        Ok(list.site_entry.into_iter().map(Site::normalized).collect())
    }

    pub async fn get_site(&self, site: &str) -> Result<Site> {
        let found: Site = self
            .call(Api::SearchConsole, Method::GET, &site_path(site), &[], None)
            .await?;
        Ok(found.normalized())
    }

    pub async fn add_site(&self, site: &str) -> Result<()> {
        self.request(Api::SearchConsole, Method::PUT, &site_path(site), &[], None)
            .await
            .map(|_| ())
    }

    pub async fn delete_site(&self, site: &str) -> Result<()> {
        self.request(
            Api::SearchConsole,
            Method::DELETE,
            &site_path(site),
            &[],
            None,
        )
        .await
        .map(|_| ())
    }
}
