//! Reading lists of URLs supplied by the user.

use std::io::Read;
use std::path::Path;

use crate::error::{Error, Result};

/// Read URLs from a file, or from standard input when `path` is `-`.
///
/// One URL per line; blank lines and lines starting with `#` are ignored.
pub fn read(path: &Path) -> Result<Vec<String>> {
    let text = if path == Path::new("-") {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|e| Error::io("reading URLs from stdin", e))?;
        buffer
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| Error::io(format!("reading {}", path.display()), e))?
    };
    Ok(parse(&text))
}

pub fn parse(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_blanks_and_comments() {
        let text =
            "# pages to check\nhttps://example.com/a\n\n  /pricing  \n#https://example.com/skip\n";
        assert_eq!(parse(text), ["https://example.com/a", "/pricing"]);
        assert!(parse("\n\n").is_empty());
    }

    #[test]
    fn reads_from_a_file_and_reports_a_missing_one() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("urls.txt");
        std::fs::write(&path, "https://example.com/a\nhttps://example.com/b\n").unwrap();
        assert_eq!(
            read(&path).unwrap(),
            ["https://example.com/a", "https://example.com/b"]
        );

        let err = read(&tmp.path().join("nope.txt")).unwrap_err();
        assert_eq!(err.kind, crate::error::ErrorKind::Io);
        assert!(err.message.contains("nope.txt"));
    }
}
