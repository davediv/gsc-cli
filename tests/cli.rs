//! Smoke tests for the top-level command.

mod common;

use common::TestEnv;
use predicates::prelude::*;

#[test]
fn prints_version() {
    TestEnv::new()
        .gsc()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("gsc "));
}

#[test]
fn prints_help() {
    TestEnv::new()
        .gsc()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Google Search Console"));
}

#[test]
fn no_arguments_shows_help_and_fails() {
    TestEnv::new()
        .gsc()
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Usage: gsc"));
}
