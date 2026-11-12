//! Small filesystem helpers for the config directory.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use crate::error::{Error, Result};

/// Read a file as UTF-8, returning `None` if it does not exist.
pub fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(format!("reading {}", path.display()), e)),
    }
}

/// Write a file that only the current user can read.
///
/// The config directory holds OAuth refresh tokens, so everything in it is
/// created with owner-only permissions. The write goes through a temporary
/// file and a rename so a crash or a concurrent `gsc` process can never leave
/// a half-written file behind.
pub fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::internal(format!("{} has no parent directory", path.display())))?;
    create_private_dir(dir)?;

    let file_name = path
        .file_name()
        .ok_or_else(|| Error::internal(format!("{} has no file name", path.display())))?
        .to_string_lossy();
    let tmp = dir.join(format!(".{file_name}.{}.tmp", std::process::id()));

    let write = || -> io::Result<()> {
        let mut file = private_file_options().open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = fs::remove_file(&tmp);
        Error::io(format!("writing {}", path.display()), e)
    })
}

/// Delete a file, treating "already gone" as success. Returns whether it existed.
pub fn remove_if_exists(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::io(format!("removing {}", path.display()), e)),
    }
}

fn create_private_dir(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| Error::io(format!("creating {}", dir.display()), e))
}

fn private_file_options() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_creates_missing_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("file.txt");

        assert_eq!(read_optional(&path).unwrap(), None);
        write_private(&path, b"hello").unwrap();
        assert_eq!(read_optional(&path).unwrap().as_deref(), Some("hello"));

        write_private(&path, b"replaced").unwrap();
        assert_eq!(read_optional(&path).unwrap().as_deref(), Some("replaced"));

        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            leftovers,
            ["file.txt"],
            "temporary file must be renamed away"
        );
    }

    #[cfg(unix)]
    #[test]
    fn files_and_directories_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("gsc");
        let path = dir.join("credentials.json");
        write_private(&path, b"{}").unwrap();

        let file_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let dir_mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
    }

    #[test]
    fn remove_reports_whether_the_file_existed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("x");
        assert!(!remove_if_exists(&path).unwrap());
        write_private(&path, b"x").unwrap();
        assert!(remove_if_exists(&path).unwrap());
        assert!(!path.exists());
    }
}
