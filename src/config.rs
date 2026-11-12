//! Where `gsc` keeps its state, and the user's saved defaults.

use std::path::{Path, PathBuf};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::fsutil;
use crate::output::Format;

/// Overrides the config directory. Used by tests and for isolated profiles.
pub const ENV_CONFIG_DIR: &str = "GSC_CONFIG_DIR";

const CONFIG_FILE: &str = "config.toml";

/// Directory holding `config.toml` and credentials.
///
/// `$GSC_CONFIG_DIR`, else `$XDG_CONFIG_HOME/gsc`, else `~/.config/gsc`
/// (`%APPDATA%\gsc` on Windows).
pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = env_path(ENV_CONFIG_DIR) {
        return Ok(dir);
    }
    if let Some(dir) = env_path("XDG_CONFIG_HOME") {
        return Ok(dir.join("gsc"));
    }
    if cfg!(windows)
        && let Some(dir) = env_path("APPDATA")
    {
        return Ok(dir.join("gsc"));
    }
    std::env::home_dir()
        .map(|home| home.join(".config").join("gsc"))
        .ok_or_else(|| {
            Error::internal("could not determine the home directory")
                .with_hint(format!("set {ENV_CONFIG_DIR} to a writable directory"))
        })
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Defaults saved with `gsc config set`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Property used when `--site` is not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    /// Output format used when `--format` is not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Format>,
}

/// A setting that can be read and written by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Key {
    /// Default Search Console property, e.g. `sc-domain:example.com`
    Site,
    /// Default output format: table, json, or csv
    Format,
}

impl Key {
    pub const ALL: [Key; 2] = [Key::Site, Key::Format];

    pub fn name(self) -> &'static str {
        match self {
            Key::Site => "site",
            Key::Format => "format",
        }
    }
}

impl Config {
    pub fn file_path(dir: &Path) -> PathBuf {
        dir.join(CONFIG_FILE)
    }

    /// Load the config file, or defaults if it does not exist yet.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = Self::file_path(dir);
        let Some(text) = fsutil::read_optional(&path)? else {
            return Ok(Self::default());
        };
        toml::from_str(&text).map_err(|e| {
            Error::usage(format!(
                "invalid config file {}: {}",
                path.display(),
                e.message()
            ))
            .with_hint("fix the file by hand, or delete it to start over")
        })
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| Error::internal(format!("could not serialize config: {e}")))?;
        fsutil::write_private(&Self::file_path(dir), text.as_bytes())
    }

    pub fn get(&self, key: Key) -> Option<String> {
        match key {
            Key::Site => self.site.clone(),
            Key::Format => self.format.map(|f| f.as_str().to_owned()),
        }
    }

    pub fn set(&mut self, key: Key, value: &str) -> Result<()> {
        let value = value.trim();
        if value.is_empty() {
            return Err(
                Error::usage(format!("{} cannot be empty", key.name())).with_hint(format!(
                    "to clear it, run `gsc config unset {}`",
                    key.name()
                )),
            );
        }
        match key {
            Key::Site => self.site = Some(value.to_owned()),
            Key::Format => {
                let format = Format::from_str(value, true).map_err(|_| {
                    Error::usage(format!("unknown format `{value}`"))
                        .with_hint("valid formats: table, json, csv")
                })?;
                self.format = Some(format);
            }
        }
        Ok(())
    }

    pub fn unset(&mut self, key: Key) {
        match key {
            Key::Site => self.site = None,
            Key::Format => self.format = None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(tmp.path()).unwrap(), Config::default());
    }

    #[test]
    fn save_and_load_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.set(Key::Site, "  sc-domain:example.com ").unwrap();
        config.set(Key::Format, "JSON").unwrap();
        config.save(tmp.path()).unwrap();

        let loaded = Config::load(tmp.path()).unwrap();
        assert_eq!(loaded.site.as_deref(), Some("sc-domain:example.com"));
        assert_eq!(loaded.format, Some(Format::Json));
        assert_eq!(loaded.get(Key::Format).as_deref(), Some("json"));
    }

    #[test]
    fn rejects_bad_values() {
        let mut config = Config::default();
        assert!(config.set(Key::Format, "yaml").is_err());
        assert!(config.set(Key::Site, "   ").is_err());
        assert_eq!(config, Config::default());
    }

    #[test]
    fn unset_clears_a_value() {
        let mut config = Config::default();
        config.set(Key::Site, "https://example.com/").unwrap();
        config.unset(Key::Site);
        assert_eq!(config.get(Key::Site), None);
    }

    #[test]
    fn unknown_keys_in_the_file_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            Config::file_path(tmp.path()),
            "site = \"sc-domain:example.com\"\nfuture_option = true\n",
        )
        .unwrap();
        let loaded = Config::load(tmp.path()).unwrap();
        assert_eq!(loaded.site.as_deref(), Some("sc-domain:example.com"));
    }

    #[test]
    fn malformed_file_is_a_usage_error_with_a_hint() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(Config::file_path(tmp.path()), "site = [unterminated").unwrap();
        let err = Config::load(tmp.path()).unwrap_err();
        assert_eq!(err.kind, crate::error::ErrorKind::Usage);
        assert!(err.hint.is_some());
    }
}
