//! `gsc api` — call any endpoint directly.

use std::io::Read;

use clap::Args;
use reqwest::Method;
use serde_json::{Value, json};

use crate::Context;
use crate::api::encode_segment;
use crate::auth::Api;
use crate::error::{Error, Result};

/// Stands for the selected property in a request path.
const SITE_PLACEHOLDER: &str = "{site}";

#[derive(Debug, Args)]
pub struct ApiArgs {
    /// HTTP method: GET, POST, PUT, or DELETE
    pub method: String,

    /// Path within the API, e.g. /webmasters/v3/sites. A literal `{site}`
    /// is replaced with the URL-encoded property
    pub path: String,

    /// JSON request body: a literal, @FILE to read a file, or - for stdin
    #[arg(short, long, value_name = "JSON")]
    pub data: Option<String>,

    /// Query parameter as KEY=VALUE (repeatable)
    #[arg(short, long, value_name = "KEY=VALUE")]
    pub query: Vec<String>,

    /// Send the request to the Indexing API instead of Search Console
    #[arg(long)]
    pub indexing: bool,
}

pub async fn run(args: ApiArgs, ctx: &Context) -> Result<()> {
    let method = parse_method(&args.method)?;
    if !args.path.starts_with('/') {
        return Err(
            Error::usage(format!("path must start with `/`, got `{}`", args.path))
                .with_hint("example: gsc api GET /webmasters/v3/sites"),
        );
    }
    let query = args
        .query
        .iter()
        .map(|pair| parse_query_pair(pair))
        .collect::<Result<Vec<_>>>()?;
    let body = args.data.as_deref().map(read_body).transpose()?;
    let api = if args.indexing {
        Api::Indexing
    } else {
        Api::SearchConsole
    };

    let client = ctx.client()?;
    let path = if args.path.contains(SITE_PLACEHOLDER) {
        let site = ctx.site(&client).await?;
        args.path.replace(SITE_PLACEHOLDER, &encode_segment(&site))
    } else {
        args.path.clone()
    };
    let response = client
        .request(api, method, &path, &query, body.as_ref())
        .await?;
    // Successful writes come back empty; print an empty object so the output
    // is always a JSON document.
    ctx.emit_json(&match response {
        Value::Null => json!({}),
        other => other,
    })
}

fn parse_method(method: &str) -> Result<Method> {
    match method.to_ascii_uppercase().as_str() {
        "GET" => Ok(Method::GET),
        "POST" => Ok(Method::POST),
        "PUT" => Ok(Method::PUT),
        "DELETE" => Ok(Method::DELETE),
        other => Err(Error::usage(format!("unsupported HTTP method `{other}`"))
            .with_hint("use GET, POST, PUT, or DELETE")),
    }
}

fn parse_query_pair(pair: &str) -> Result<(&str, &str)> {
    pair.split_once('=')
        .filter(|(key, _)| !key.is_empty())
        .ok_or_else(|| Error::usage(format!("query parameter `{pair}` is not KEY=VALUE")))
}

/// Resolve `--data`: a JSON literal, `@path`, or `-` for stdin.
fn read_body(data: &str) -> Result<Value> {
    let text = if data == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|e| Error::io("reading the request body from stdin", e))?;
        buffer
    } else if let Some(path) = data.strip_prefix('@') {
        std::fs::read_to_string(path).map_err(|e| Error::io(format!("reading {path}"), e))?
    } else {
        data.to_owned()
    };
    serde_json::from_str(&text)
        .map_err(|e| Error::usage(format!("the request body is not valid JSON: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_are_case_insensitive_and_restricted() {
        assert_eq!(parse_method("get").unwrap(), Method::GET);
        assert_eq!(parse_method("Delete").unwrap(), Method::DELETE);
        assert!(parse_method("TRACE").is_err());
    }

    #[test]
    fn query_pairs_split_on_the_first_equals_sign() {
        assert_eq!(parse_query_pair("a=b").unwrap(), ("a", "b"));
        assert_eq!(
            parse_query_pair("url=https://x/?a=1").unwrap(),
            ("url", "https://x/?a=1")
        );
        assert_eq!(parse_query_pair("empty=").unwrap(), ("empty", ""));
        assert!(parse_query_pair("novalue").is_err());
        assert!(parse_query_pair("=x").is_err());
    }

    #[test]
    fn body_can_be_a_literal_or_a_file() {
        assert_eq!(read_body(r#"{"a":1}"#).unwrap(), json!({ "a": 1 }));
        assert!(read_body("{oops").is_err());

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("body.json");
        std::fs::write(&path, r#"{"from":"file"}"#).unwrap();
        assert_eq!(
            read_body(&format!("@{}", path.display())).unwrap(),
            json!({ "from": "file" })
        );
        assert!(read_body("@/no/such/file.json").is_err());
    }
}
