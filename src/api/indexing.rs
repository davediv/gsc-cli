//! The Indexing API: telling Google directly that a URL changed.

use clap::ValueEnum;
use reqwest::Method;
use serde_json::{Value, json};

use super::Client;
use crate::auth::Api;
use crate::error::{Error, Result};

/// What happened to the URL being reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Notification {
    /// The page is new or its content changed
    Updated,
    /// The page was removed
    Deleted,
}

impl Notification {
    pub fn api_name(self) -> &'static str {
        match self {
            Notification::Updated => "URL_UPDATED",
            Notification::Deleted => "URL_DELETED",
        }
    }
}

impl Client {
    /// Notify Google that `url` was updated or removed.
    ///
    /// Returns what Google now has on record for the URL.
    pub async fn publish_notification(&self, url: &str, kind: Notification) -> Result<Value> {
        let body = json!({ "url": url, "type": kind.api_name() });
        let mut response = self
            .request(
                Api::Indexing,
                Method::POST,
                "/v3/urlNotifications:publish",
                &[],
                Some(&body),
            )
            .await?;
        match response.get_mut("urlNotificationMetadata") {
            Some(metadata) => Ok(metadata.take()),
            None => Err(Error::internal(
                "Google's response did not contain notification metadata",
            )),
        }
    }

    /// The most recent notifications Google received for `url`.
    pub async fn notification_metadata(&self, url: &str) -> Result<Value> {
        self.request(
            Api::Indexing,
            Method::GET,
            "/v3/urlNotifications/metadata",
            &[("url", url)],
            None,
        )
        .await
    }
}
