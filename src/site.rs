//! Property identifiers.
//!
//! Search Console names a property either `sc-domain:example.com` (a Domain
//! property) or by a URL prefix ending in a slash (`https://example.com/`).
//! The API only accepts those exact strings. To spare users from remembering
//! which kind they have, `gsc` also accepts a bare `example.com` and matches
//! it against the properties the account can actually see.

use crate::api::sites::Site;
use crate::error::{Error, Result};

const DOMAIN_PREFIX: &str = "sc-domain:";

/// How a property was written on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteRef {
    /// Already in the form the API expects.
    Exact(String),
    /// A host or host/path that must be matched against the account's properties.
    Bare(String),
}

pub fn parse(input: &str) -> Result<SiteRef> {
    let input = input.trim();
    if input.is_empty() {
        return Err(Error::usage("the property cannot be empty"));
    }

    let lower = input.to_ascii_lowercase();
    if let Some(domain) = lower.strip_prefix(DOMAIN_PREFIX) {
        if domain.is_empty() || domain.contains('/') {
            return Err(
                Error::usage(format!("`{input}` is not a valid Domain property"))
                    .with_hint("a Domain property looks like `sc-domain:example.com`"),
            );
        }
        return Ok(SiteRef::Exact(format!("{DOMAIN_PREFIX}{domain}")));
    }

    for scheme in ["https://", "http://"] {
        if lower.starts_with(scheme) {
            let rest = &input[scheme.len()..];
            if rest.is_empty() {
                return Err(Error::usage(format!(
                    "`{input}` is not a valid property URL"
                )));
            }
            // A URL-prefix property for a whole origin always ends in a slash.
            return Ok(SiteRef::Exact(if rest.contains('/') {
                input.to_owned()
            } else {
                format!("{input}/")
            }));
        }
    }

    Ok(SiteRef::Bare(input.trim_end_matches('/').to_owned()))
}

/// Property identifiers a bare name could refer to, most likely first.
///
/// A Domain property is preferred because it covers every protocol and
/// subdomain, so it has the most complete data.
fn candidates(bare: &str) -> Vec<String> {
    let bare = bare.to_ascii_lowercase();
    let (host, path) = match bare.split_once('/') {
        Some((host, path)) => (host, format!("/{path}/")),
        None => (bare.as_str(), "/".to_owned()),
    };
    let apex = host.strip_prefix("www.").unwrap_or(host);

    let mut list = Vec::new();
    if path == "/" {
        list.push(format!("{DOMAIN_PREFIX}{apex}"));
    }
    let hosts = if apex == host {
        vec![host.to_owned(), format!("www.{host}")]
    } else {
        vec![host.to_owned()]
    };
    for host in &hosts {
        for scheme in ["https", "http"] {
            list.push(format!("{scheme}://{host}{path}"));
        }
    }
    list
}

/// Find the property a bare name refers to among those the account can read.
pub fn pick<'a>(bare: &str, sites: &'a [Site]) -> Result<&'a Site> {
    let verified: Vec<&Site> = sites.iter().filter(|site| site.is_verified()).collect();
    for candidate in candidates(bare) {
        if let Some(site) = verified
            .iter()
            .find(|site| site.site_url.eq_ignore_ascii_case(&candidate))
        {
            return Ok(site);
        }
    }

    let hint = if verified.is_empty() {
        "this account has no verified properties; add one at \
         https://search.google.com/search-console, or check `gsc auth status` shows \
         the account you expect"
            .to_owned()
    } else {
        let mut names: Vec<&str> = verified.iter().map(|s| s.site_url.as_str()).collect();
        names.sort_unstable();
        let shown = names
            .iter()
            .take(10)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        let more = names.len().saturating_sub(10);
        if more > 0 {
            format!("available properties: {shown}, and {more} more (see `gsc sites list`)")
        } else {
            format!("available properties: {shown}")
        }
    };
    Err(Error::usage(format!("no property matches `{bare}`")).with_hint(hint))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(url: &str, permission: &str) -> Site {
        Site {
            site_url: url.into(),
            permission_level: permission.into(),
        }
    }

    #[test]
    fn exact_forms_are_normalised_but_not_looked_up() {
        assert_eq!(
            parse(" SC-Domain:Example.COM ").unwrap(),
            SiteRef::Exact("sc-domain:example.com".into())
        );
        assert_eq!(
            parse("https://example.com").unwrap(),
            SiteRef::Exact("https://example.com/".into())
        );
        assert_eq!(
            parse("https://example.com/").unwrap(),
            SiteRef::Exact("https://example.com/".into())
        );
        assert_eq!(
            parse("http://example.com/Blog/").unwrap(),
            SiteRef::Exact("http://example.com/Blog/".into()),
            "paths are case-sensitive and left untouched"
        );
    }

    #[test]
    fn anything_else_is_a_bare_name() {
        assert_eq!(
            parse("example.com").unwrap(),
            SiteRef::Bare("example.com".into())
        );
        assert_eq!(
            parse("example.com/").unwrap(),
            SiteRef::Bare("example.com".into())
        );
        assert_eq!(
            parse("www.example.com/blog").unwrap(),
            SiteRef::Bare("www.example.com/blog".into())
        );
    }

    #[test]
    fn malformed_properties_are_usage_errors() {
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
        assert!(parse("sc-domain:").is_err());
        assert!(parse("sc-domain:example.com/path").is_err());
        assert!(parse("https://").is_err());
    }

    #[test]
    fn candidates_prefer_the_domain_property_then_https() {
        assert_eq!(
            candidates("Example.com"),
            [
                "sc-domain:example.com",
                "https://example.com/",
                "http://example.com/",
                "https://www.example.com/",
                "http://www.example.com/",
            ]
        );
        assert_eq!(
            candidates("www.example.com"),
            [
                "sc-domain:example.com",
                "https://www.example.com/",
                "http://www.example.com/",
            ]
        );
        assert_eq!(
            candidates("example.com/blog"),
            [
                "https://example.com/blog/",
                "http://example.com/blog/",
                "https://www.example.com/blog/",
                "http://www.example.com/blog/",
            ],
            "a path can only be a URL-prefix property"
        );
    }

    #[test]
    fn pick_follows_candidate_order_and_skips_unverified_properties() {
        let sites = [
            site("http://example.com/", "siteOwner"),
            site("https://www.example.com/", "siteFullUser"),
            site("sc-domain:example.com", "siteUnverifiedUser"),
            site("sc-domain:other.org", "siteOwner"),
        ];
        assert_eq!(
            pick("example.com", &sites).unwrap().site_url,
            "http://example.com/",
            "the unverified Domain property is skipped; https://example.com/ does not exist"
        );
        assert_eq!(
            pick("www.example.com", &sites).unwrap().site_url,
            "https://www.example.com/"
        );
        assert_eq!(
            pick("other.org", &sites).unwrap().site_url,
            "sc-domain:other.org"
        );
    }

    #[test]
    fn no_match_lists_what_is_available() {
        let sites = [
            site("sc-domain:b.com", "siteOwner"),
            site("sc-domain:a.com", "siteOwner"),
            site("sc-domain:hidden.com", "siteUnverifiedUser"),
        ];
        let err = pick("nope.com", &sites).unwrap_err();
        assert_eq!(err.message, "no property matches `nope.com`");
        assert_eq!(
            err.hint.unwrap(),
            "available properties: sc-domain:a.com, sc-domain:b.com"
        );

        let err = pick("nope.com", &[]).unwrap_err();
        assert!(err.hint.unwrap().contains("no verified properties"));
    }

    #[test]
    fn long_property_lists_are_truncated_in_the_hint() {
        let sites: Vec<Site> = (0..13)
            .map(|i| site(&format!("sc-domain:site{i:02}.com"), "siteOwner"))
            .collect();
        let hint = pick("nope.com", &sites).unwrap_err().hint.unwrap();
        assert!(hint.contains("sc-domain:site09.com"));
        assert!(!hint.contains("sc-domain:site10.com"));
        assert!(hint.ends_with("and 3 more (see `gsc sites list`)"));
    }
}
