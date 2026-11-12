//! Shared helpers for integration tests.
//!
//! Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use assert_cmd::Command;
use tempfile::TempDir;

/// An isolated `gsc` installation: its own config directory and a clean
/// environment, so tests never read or write the developer's real credentials.
pub struct TestEnv {
    pub config_dir: TempDir,
}

impl TestEnv {
    pub fn new() -> Self {
        Self {
            config_dir: tempfile::tempdir().expect("create temp config dir"),
        }
    }

    /// A `gsc` command that sees only this test's config directory.
    pub fn gsc(&self) -> Command {
        let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("gsc");
        cmd.env_clear()
            .env("GSC_CONFIG_DIR", self.config_dir.path());
        cmd
    }

    /// Run `gsc` with the given arguments, require success, and parse stdout as JSON.
    pub fn json(&self, args: &[&str]) -> serde_json::Value {
        let output = self
            .gsc()
            .args(args)
            .assert()
            .success()
            .get_output()
            .clone();
        parse_json(&output.stdout)
    }
}

pub fn parse_json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap_or_else(|e| {
        panic!(
            "expected JSON but got: {}\n(parse error: {e})",
            String::from_utf8_lossy(bytes)
        )
    })
}
