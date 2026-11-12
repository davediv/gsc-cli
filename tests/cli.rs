//! Smoke tests for the top-level command.

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

#[test]
fn prints_version() {
    cargo_bin_cmd!("gsc")
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("gsc "));
}

#[test]
fn prints_help() {
    cargo_bin_cmd!("gsc")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Google Search Console"));
}
