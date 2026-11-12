//! Small filesystem helpers for the config directory.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use crate::error::{Error, ErrorKind, Result};

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
///
/// The temporary file has an unpredictable name and is created exclusively.
/// Otherwise another user with write access to the directory could create
/// that file first, with permissions of their choosing, and read the secret
/// as it is written.
pub fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::internal(format!("{} has no parent directory", path.display())))?;
    ensure_private_dir(dir)?;

    let file_name = path
        .file_name()
        .ok_or_else(|| Error::internal(format!("{} has no file name", path.display())))?
        .to_string_lossy();

    let mut attempts = 0;
    let (tmp, mut file) = loop {
        let tmp = dir.join(format!(".{file_name}.{}.tmp", random_suffix()?));
        match exclusive_private_file().open(&tmp) {
            Ok(file) => break (tmp, file),
            // A 64-bit collision is not an accident; try another name a few
            // times, then give up rather than loop against an adversary.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && attempts < 4 => attempts += 1,
            Err(e) => return Err(Error::io(format!("writing {}", path.display()), e)),
        }
    };

    let result = file
        .write_all(contents)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&tmp, path));
    result.map_err(|e| {
        let _ = fs::remove_file(&tmp);
        Error::io(format!("writing {}", path.display()), e)
    })
}

fn random_suffix() -> Result<String> {
    let mut bytes = [0u8; 8];
    aws_lc_rs::rand::fill(&mut bytes)
        .map_err(|_| Error::internal("the system random number generator failed"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Delete a file, treating "already gone" as success. Returns whether it existed.
pub fn remove_if_exists(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::io(format!("removing {}", path.display()), e)),
    }
}

/// Make sure `dir` exists and is a safe place for secrets.
fn ensure_private_dir(dir: &Path) -> Result<()> {
    match fs::metadata(dir) {
        Ok(metadata) if metadata.is_dir() => refuse_if_world_writable(dir, &metadata),
        Ok(_) => Err(Error::new(
            ErrorKind::Io,
            format!("{} exists but is not a directory", dir.display()),
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
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
        Err(e) => Err(Error::io(format!("reading {}", dir.display()), e)),
    }
}

/// In a directory anyone can write to, any user could swap the credentials
/// file for one of their own. Owner-only file permissions do not prevent that.
#[cfg(unix)]
fn refuse_if_world_writable(dir: &Path, metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if metadata.permissions().mode() & 0o002 == 0 {
        return Ok(());
    }
    Err(Error::new(
        ErrorKind::Io,
        format!(
            "refusing to store files in {}: every user on this machine can write to it",
            dir.display()
        ),
    )
    .with_hint(format!(
        "set {} to a directory only you can write to, or run `chmod o-w` on this one",
        crate::config::ENV_CONFIG_DIR
    )))
}

#[cfg(not(unix))]
fn refuse_if_world_writable(_dir: &Path, _metadata: &fs::Metadata) -> Result<()> {
    Ok(())
}

/// Options for a new file readable only by its owner. `create_new` fails if
/// anything already exists at the path, including a symbolic link.
fn exclusive_private_file() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
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

    #[cfg(unix)]
    #[test]
    fn refuses_a_directory_that_every_user_can_write_to() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let shared = tmp.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();

        let path = shared.join("credentials.json");
        let err = write_private(&path, b"secret").unwrap_err();
        assert_eq!(err.kind, ErrorKind::Io);
        assert!(
            err.message
                .contains("every user on this machine can write to it")
        );
        assert!(err.hint.unwrap().contains("GSC_CONFIG_DIR"));
        assert_eq!(
            fs::read_dir(&shared).unwrap().count(),
            0,
            "nothing, not even a temporary file, may be written there"
        );

        // Group-writable is common with per-user groups and is allowed.
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o770)).unwrap();
        write_private(&path, b"secret").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_at_the_destination_is_replaced_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("elsewhere.txt");
        fs::write(&elsewhere, "untouched").unwrap();
        let path = tmp.path().join("credentials.json");
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();

        write_private(&path, b"secret").unwrap();

        assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "untouched");
        assert!(
            !fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "secret");
    }

    #[test]
    fn temporary_names_are_unpredictable() {
        let a = random_suffix().unwrap();
        let b = random_suffix().unwrap();
        assert_eq!(a.len(), 16);
        assert_ne!(a, b);
    }

    #[test]
    fn a_file_in_place_of_the_directory_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let not_a_dir = tmp.path().join("gsc");
        fs::write(&not_a_dir, "x").unwrap();
        let err = write_private(&not_a_dir.join("config.toml"), b"x").unwrap_err();
        assert!(err.message.contains("is not a directory"));
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
