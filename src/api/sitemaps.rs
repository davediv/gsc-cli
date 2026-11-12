//! Sitemaps submitted for a property.

use reqwest::Method;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::{Client, encode_segment, lower_camel};
use crate::auth::Api;
use crate::error::Result;

/// A sitemap (or sitemap index) known to Search Console.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sitemap {
    /// The sitemap's URL.
    pub path: String,
    #[serde(default)]
    pub last_submitted: Option<String>,
    #[serde(default)]
    pub last_downloaded: Option<String>,
    /// True until Google has processed the sitemap.
    #[serde(default)]
    pub is_pending: bool,
    #[serde(default)]
    pub is_sitemaps_index: bool,
    /// `sitemap`, `rssFeed`, `atomFeed`, `urlList`, ...
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Problems with the sitemap file itself.
    #[serde(default, deserialize_with = "count")]
    pub errors: u64,
    /// Usually problems with individual URLs listed in it.
    #[serde(default, deserialize_with = "count")]
    pub warnings: u64,
    #[serde(default)]
    pub contents: Vec<SitemapContent>,
}

/// How many URLs of one kind a sitemap lists.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SitemapContent {
    /// `web`, `image`, `video`, `news`, ...
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, deserialize_with = "count")]
    pub submitted: u64,
}

impl Sitemap {
    /// URLs listed across every content type.
    pub fn submitted(&self) -> u64 {
        self.contents.iter().map(|content| content.submitted).sum()
    }

    /// The sitemap as shown to users, with counts as numbers and enum values
    /// in one consistent spelling.
    pub fn to_json(&self) -> Value {
        json!({
            "path": self.path,
            "type": self.kind.as_deref().map(lower_camel),
            "isSitemapsIndex": self.is_sitemaps_index,
            "isPending": self.is_pending,
            "submitted": self.submitted(),
            "errors": self.errors,
            "warnings": self.warnings,
            "lastSubmitted": self.last_submitted,
            "lastDownloaded": self.last_downloaded,
            "contents": self.contents.iter().map(|content| json!({
                "type": lower_camel(&content.kind),
                "submitted": content.submitted,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Google encodes 64-bit integers as JSON strings; accept either form.
fn count<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Count {
        Number(u64),
        Text(String),
    }
    match Count::deserialize(deserializer)? {
        Count::Number(n) => Ok(n),
        Count::Text(text) => text.trim().parse().map_err(serde::de::Error::custom),
    }
}

#[derive(Deserialize)]
struct SitemapList {
    #[serde(default)]
    sitemap: Vec<Sitemap>,
}

fn collection_path(site: &str) -> String {
    format!("/webmasters/v3/sites/{}/sitemaps", encode_segment(site))
}

fn sitemap_path(site: &str, sitemap: &str) -> String {
    format!("{}/{}", collection_path(site), encode_segment(sitemap))
}

impl Client {
    /// Sitemaps submitted for `site`, or those listed in one sitemap index.
    pub async fn list_sitemaps(&self, site: &str, index: Option<&str>) -> Result<Vec<Sitemap>> {
        let query: Vec<(&str, &str)> = index.iter().map(|url| ("sitemapIndex", *url)).collect();
        let list: SitemapList = self
            .call(
                Api::SearchConsole,
                Method::GET,
                &collection_path(site),
                &query,
                None,
            )
            .await?;
        Ok(list.sitemap)
    }

    pub async fn get_sitemap(&self, site: &str, sitemap: &str) -> Result<Sitemap> {
        self.call(
            Api::SearchConsole,
            Method::GET,
            &sitemap_path(site, sitemap),
            &[],
            None,
        )
        .await
    }

    pub async fn submit_sitemap(&self, site: &str, sitemap: &str) -> Result<()> {
        self.request(
            Api::SearchConsole,
            Method::PUT,
            &sitemap_path(site, sitemap),
            &[],
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn delete_sitemap(&self, site: &str, sitemap: &str) -> Result<()> {
        self.request(
            Api::SearchConsole,
            Method::DELETE,
            &sitemap_path(site, sitemap),
            &[],
            None,
        )
        .await
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_googles_response_with_string_encoded_counts() {
        let sitemap: Sitemap = serde_json::from_value(json!({
            "path": "https://example.com/sitemap.xml",
            "lastSubmitted": "2026-10-01T08:00:00.000Z",
            "isPending": false,
            "isSitemapsIndex": false,
            "type": "sitemap",
            "lastDownloaded": "2026-11-10T03:12:45.000Z",
            "warnings": "2",
            "errors": "0",
            "contents": [
                { "type": "web", "submitted": "120", "indexed": "0" },
                { "type": "image", "submitted": "35", "indexed": "0" },
            ],
        }))
        .unwrap();

        assert_eq!(sitemap.warnings, 2);
        assert_eq!(sitemap.errors, 0);
        assert_eq!(sitemap.submitted(), 155);
        assert_eq!(
            sitemap.to_json(),
            json!({
                "path": "https://example.com/sitemap.xml",
                "type": "sitemap",
                "isSitemapsIndex": false,
                "isPending": false,
                "submitted": 155,
                "errors": 0,
                "warnings": 2,
                "lastSubmitted": "2026-10-01T08:00:00.000Z",
                "lastDownloaded": "2026-11-10T03:12:45.000Z",
                "contents": [
                    { "type": "web", "submitted": 120 },
                    { "type": "image", "submitted": 35 },
                ],
            })
        );
    }

    #[test]
    fn tolerates_missing_fields_numeric_counts_and_uppercase_enums() {
        let sitemap: Sitemap = serde_json::from_value(json!({
            "path": "https://example.com/feed.xml",
            "isPending": true,
            "type": "RSS_FEED",
            "errors": 3,
            "contents": [{ "type": "WEB", "submitted": 7 }],
        }))
        .unwrap();
        assert!(sitemap.is_pending);
        assert_eq!(sitemap.errors, 3);
        assert_eq!(sitemap.warnings, 0);
        let shown = sitemap.to_json();
        assert_eq!(shown["type"], "rssFeed");
        assert_eq!(shown["contents"][0]["type"], "web");
        assert_eq!(shown["lastDownloaded"], Value::Null);
    }

    #[test]
    fn rejects_counts_that_are_not_numbers() {
        let result: Result<Sitemap, _> =
            serde_json::from_value(json!({ "path": "x", "errors": "many" }));
        assert!(result.is_err());
    }

    #[test]
    fn sitemap_urls_are_encoded_as_one_path_segment() {
        assert_eq!(
            sitemap_path(
                "sc-domain:example.com",
                "https://example.com/sitemap.xml?x=1"
            ),
            "/webmasters/v3/sites/sc-domain%3Aexample.com/sitemaps/https%3A%2F%2Fexample.com%2Fsitemap.xml%3Fx%3D1"
        );
    }
}
