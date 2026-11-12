//! Reading sitemap files to find the URLs they list.
//!
//! Supports XML sitemaps, sitemap indexes (followed recursively), plain-text
//! sitemaps with one URL per line, and gzip-compressed versions of each.

use std::collections::{HashSet, VecDeque};
use std::io::Read;

use flate2::read::MultiGzDecoder;
use quick_xml::Reader;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::Event;

/// The sitemap protocol caps a file at 50 MB uncompressed; allow some slack.
const MAX_SITEMAP_BYTES: u64 = 64 * 1024 * 1024;
/// Upper bound on files fetched while following sitemap indexes.
const MAX_SITEMAPS: usize = 500;
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// What a sitemap file turned out to contain.
#[derive(Debug, PartialEq, Eq)]
pub enum Document {
    /// Page URLs.
    UrlSet(Vec<String>),
    /// URLs of further sitemaps.
    Index(Vec<String>),
}

/// Parse the bytes of a sitemap file, decompressing them first if needed.
pub fn parse(bytes: &[u8]) -> Result<Document, String> {
    let decompressed;
    let bytes = if bytes.starts_with(&GZIP_MAGIC) {
        decompressed = gunzip(bytes)?;
        &decompressed[..]
    } else {
        bytes
    };
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}').trim();

    if text.is_empty() {
        return Err("the file is empty".to_owned());
    }
    if !text.starts_with('<') {
        return parse_text(text);
    }
    parse_xml(text)
}

fn gunzip(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    // Read one byte past the cap so that hitting it can be told apart from
    // a file that is exactly that size.
    MultiGzDecoder::new(bytes)
        .take(MAX_SITEMAP_BYTES + 1)
        .read_to_end(&mut out)
        .map_err(|e| format!("invalid gzip data: {e}"))?;
    if out.len() as u64 > MAX_SITEMAP_BYTES {
        return Err("the decompressed file is larger than 64 MB".to_owned());
    }
    Ok(out)
}

/// A plain-text sitemap: one absolute URL per line.
fn parse_text(text: &str) -> Result<Document, String> {
    let urls: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("http://") || line.starts_with("https://"))
        .map(str::to_owned)
        .collect();
    if urls.is_empty() {
        return Err("it is neither XML nor a list of URLs".to_owned());
    }
    Ok(Document::UrlSet(urls))
}

fn parse_xml(text: &str) -> Result<Document, String> {
    let mut reader = Reader::from_str(text);
    let mut root: Option<String> = None;
    // Local names of the elements currently open.
    let mut open: Vec<String> = Vec::new();
    let mut locations = Vec::new();
    // Text of the <loc> being read, if we are inside one that counts.
    let mut current: Option<String> = None;

    loop {
        let event = reader
            .read_event()
            .map_err(|e| format!("invalid XML: {e}"))?;
        match event {
            Event::Start(element) => {
                let name = element.local_name().into_inner().to_owned();
                if root.is_none() {
                    root = Some(name.clone());
                }
                // Only a <loc> directly inside <url> or <sitemap> names an
                // entry. Extensions such as <image:loc> sit one level deeper.
                let parent = open.last().map(String::as_str);
                if name == "loc" && matches!(parent, Some("url" | "sitemap")) {
                    current = Some(String::new());
                }
                open.push(name);
            }
            Event::End(_) => {
                if open.pop().as_deref() == Some("loc")
                    && let Some(location) = current.take()
                {
                    let location = location.trim();
                    if !location.is_empty() {
                        locations.push(location.to_owned());
                    }
                }
            }
            Event::Text(text) => {
                if let Some(buffer) = current.as_mut() {
                    buffer.push_str(&text.xml10_content());
                }
            }
            Event::CData(data) => {
                if let Some(buffer) = current.as_mut() {
                    buffer.push_str(&data.into_inner());
                }
            }
            // `&amp;` and friends arrive as separate events between text runs.
            Event::GeneralRef(reference) => {
                if let Some(buffer) = current.as_mut() {
                    let resolved = reference
                        .resolve_char_ref()
                        .map_err(|e| format!("invalid XML: {e}"))?;
                    match resolved {
                        Some(character) => buffer.push(character),
                        None => {
                            let name = reference.into_inner();
                            let value = resolve_predefined_entity(&name)
                                .ok_or_else(|| format!("invalid XML: unknown entity &{name};"))?;
                            buffer.push_str(value);
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    match root.as_deref() {
        Some("urlset") => Ok(Document::UrlSet(locations)),
        Some("sitemapindex") => Ok(Document::Index(locations)),
        Some(other) => Err(format!(
            "it is not a sitemap: the root element is <{other}>"
        )),
        None => Err("it contains no XML elements".to_owned()),
    }
}

/// The page URLs gathered from one or more sitemaps.
#[derive(Debug, Default)]
pub struct Collected {
    /// Unique page URLs, in the order they were found.
    pub urls: Vec<String>,
    /// True when collection stopped at the limit with more left to read.
    pub truncated: bool,
    /// Sitemaps that could not be read, with the reason.
    pub problems: Vec<String>,
}

/// Gather up to `limit` page URLs from the given sitemaps, following indexes.
///
/// Reading stops as soon as the limit is reached, so a huge site does not
/// cost hundreds of downloads when only a sample is wanted.
pub async fn collect_urls(http: &reqwest::Client, sitemaps: &[String], limit: usize) -> Collected {
    let mut collected = Collected::default();
    let mut queue: VecDeque<String> = sitemaps.iter().cloned().collect();
    let mut visited: HashSet<String> = HashSet::new();
    let mut seen_urls: HashSet<String> = HashSet::new();

    while let Some(sitemap) = queue.pop_front() {
        if !visited.insert(sitemap.clone()) {
            continue;
        }
        if collected.urls.len() >= limit || visited.len() > MAX_SITEMAPS {
            collected.truncated = true;
            break;
        }
        let document = match fetch(http, &sitemap).await.and_then(|bytes| parse(&bytes)) {
            Ok(document) => document,
            Err(reason) => {
                collected.problems.push(format!("{sitemap}: {reason}"));
                continue;
            }
        };
        match document {
            Document::Index(children) => queue.extend(children),
            Document::UrlSet(urls) => {
                for url in urls {
                    if !seen_urls.insert(url.clone()) {
                        continue;
                    }
                    if collected.urls.len() >= limit {
                        collected.truncated = true;
                        break;
                    }
                    collected.urls.push(url);
                }
            }
        }
    }
    collected
}

async fn fetch(http: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let response = http
        .get(url)
        .send()
        .await
        .map_err(|e| format!("could not be fetched ({e})"))?;
    if !response.status().is_success() {
        return Err(format!("the server answered HTTP {}", response.status()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("could not be downloaded ({e})"))?;
    if bytes.len() as u64 > MAX_SITEMAP_BYTES {
        return Err("it is larger than 64 MB".to_owned());
    }
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::GzEncoder;

    use super::*;

    fn url_set(urls: &[&str]) -> Document {
        Document::UrlSet(urls.iter().map(|u| u.to_string()).collect())
    }

    #[test]
    fn reads_page_urls_from_a_standard_sitemap() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url>
    <loc>https://example.com/</loc>
    <lastmod>2026-11-01</lastmod>
    <changefreq>daily</changefreq>
  </url>
  <url><loc>
    https://example.com/pricing
  </loc></url>
</urlset>"#;
        assert_eq!(
            parse(xml.as_bytes()).unwrap(),
            url_set(&["https://example.com/", "https://example.com/pricing"])
        );
    }

    #[test]
    fn decodes_entities_character_references_and_cdata() {
        let xml = r#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url><loc>https://example.com/search?q=a&amp;page=2</loc></url>
  <url><loc>https://example.com/caf&#233;?x=&#x31;</loc></url>
  <url><loc><![CDATA[https://example.com/raw?a=1&b=2]]></loc></url>
</urlset>"#;
        assert_eq!(
            parse(xml.as_bytes()).unwrap(),
            url_set(&[
                "https://example.com/search?q=a&page=2",
                "https://example.com/café?x=1",
                "https://example.com/raw?a=1&b=2",
            ])
        );
    }

    #[test]
    fn ignores_locations_belonging_to_extensions() {
        let xml = r#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"
        xmlns:image="http://www.google.com/schemas/sitemap-image/1.1"
        xmlns:video="http://www.google.com/schemas/sitemap-video/1.1">
  <url>
    <loc>https://example.com/gallery</loc>
    <image:image><image:loc>https://example.com/photo.jpg</image:loc></image:image>
    <video:video><video:content_loc>https://example.com/clip.mp4</video:content_loc></video:video>
  </url>
</urlset>"#;
        assert_eq!(
            parse(xml.as_bytes()).unwrap(),
            url_set(&["https://example.com/gallery"]),
            "image and video URLs are not pages"
        );
    }

    #[test]
    fn recognises_a_sitemap_index() {
        let xml = r#"<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <sitemap><loc>https://example.com/sitemap-posts.xml</loc><lastmod>2026-11-01</lastmod></sitemap>
  <sitemap><loc>https://example.com/sitemap-pages.xml.gz</loc></sitemap>
</sitemapindex>"#;
        assert_eq!(
            parse(xml.as_bytes()).unwrap(),
            Document::Index(vec![
                "https://example.com/sitemap-posts.xml".into(),
                "https://example.com/sitemap-pages.xml.gz".into(),
            ])
        );
    }

    #[test]
    fn handles_a_namespace_prefix_on_the_sitemap_elements_themselves() {
        let xml = r#"<sm:urlset xmlns:sm="http://www.sitemaps.org/schemas/sitemap/0.9">
  <sm:url><sm:loc>https://example.com/a</sm:loc></sm:url>
</sm:urlset>"#;
        assert_eq!(
            parse(xml.as_bytes()).unwrap(),
            url_set(&["https://example.com/a"])
        );
    }

    #[test]
    fn decompresses_gzip_and_strips_a_byte_order_mark() {
        let xml = "\u{feff}<urlset><url><loc>https://example.com/z</loc></url></urlset>";
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(xml.as_bytes()).unwrap();
        let compressed = encoder.finish().unwrap();
        assert!(compressed.starts_with(&GZIP_MAGIC));
        assert_eq!(
            parse(&compressed).unwrap(),
            url_set(&["https://example.com/z"])
        );
        assert_eq!(
            parse(xml.as_bytes()).unwrap(),
            url_set(&["https://example.com/z"])
        );
    }

    #[test]
    fn reads_a_plain_text_sitemap() {
        let text = "https://example.com/a\n\n  https://example.com/b  \nnot a url\n";
        assert_eq!(
            parse(text.as_bytes()).unwrap(),
            url_set(&["https://example.com/a", "https://example.com/b"])
        );
    }

    #[test]
    fn an_empty_sitemap_is_valid_and_has_no_urls() {
        assert_eq!(parse(b"<urlset></urlset>").unwrap(), url_set(&[]));
    }

    #[test]
    fn rejects_things_that_are_not_sitemaps() {
        assert_eq!(parse(b"").unwrap_err(), "the file is empty");
        assert_eq!(parse(b"   \n ").unwrap_err(), "the file is empty");
        assert_eq!(
            parse(b"<html><body>Not found</body></html>").unwrap_err(),
            "it is not a sitemap: the root element is <html>"
        );
        assert_eq!(
            parse(b"just some words").unwrap_err(),
            "it is neither XML nor a list of URLs"
        );
        assert!(
            parse(b"<urlset><url><loc>x</url></urlset>")
                .unwrap_err()
                .starts_with("invalid XML")
        );
        assert!(
            parse(&[0x1f, 0x8b, 0x00, 0x01])
                .unwrap_err()
                .starts_with("invalid gzip data")
        );
    }
}
