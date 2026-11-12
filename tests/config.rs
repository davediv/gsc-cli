//! `gsc config` end to end, against a temporary config directory.

mod common;

use common::{TestEnv, parse_json};
use predicates::prelude::*;
use serde_json::json;

#[test]
fn list_shows_every_key_even_when_unset() {
    let env = TestEnv::new();
    let doc = env.json(&["config", "list"]);
    assert_eq!(
        doc["settings"],
        json!([
            { "key": "site", "value": null },
            { "key": "format", "value": null },
        ])
    );
    assert!(doc["configFile"].as_str().unwrap().ends_with("config.toml"));
}

#[test]
fn set_get_unset_round_trip() {
    let env = TestEnv::new();

    let set = env.json(&["config", "set", "site", "sc-domain:example.com"]);
    assert_eq!(
        set,
        json!({ "key": "site", "value": "sc-domain:example.com" })
    );

    let on_disk = std::fs::read_to_string(env.config_dir.path().join("config.toml")).unwrap();
    assert_eq!(on_disk, "site = \"sc-domain:example.com\"\n");

    let get = env.json(&["config", "get", "site"]);
    assert_eq!(get["value"], "sc-domain:example.com");

    env.json(&["config", "unset", "site"]);
    let get = env.json(&["config", "get", "site"]);
    assert_eq!(get["value"], json!(null));
}

#[test]
fn saved_format_becomes_the_default_and_the_flag_overrides_it() {
    let env = TestEnv::new();
    env.json(&["config", "set", "format", "csv"]);

    env.gsc()
        .args(["config", "list"])
        .assert()
        .success()
        .stdout("key,value\nsite,\nformat,csv\n");

    env.gsc()
        .args(["config", "list", "--format", "table"])
        .assert()
        .success()
        .stdout("KEY     VALUE\nsite    -\nformat  csv\n");

    env.gsc()
        .args(["config", "get", "format"])
        .env("GSC_FORMAT", "json")
        .assert()
        .success()
        .stdout("{\"key\":\"format\",\"value\":\"csv\"}\n");
}

#[test]
fn path_prints_the_config_directory() {
    let env = TestEnv::new();
    let doc = env.json(&["config", "path"]);
    assert_eq!(
        doc["configDir"].as_str().unwrap(),
        env.config_dir.path().to_str().unwrap()
    );
}

#[test]
fn invalid_value_is_a_usage_error_reported_as_json() {
    let env = TestEnv::new();
    let output = env
        .gsc()
        .args(["config", "set", "format", "yaml"])
        .assert()
        .code(2)
        .stdout("")
        .get_output()
        .clone();
    let err = parse_json(&output.stderr);
    assert_eq!(err["error"]["kind"], "usage");
    assert_eq!(err["error"]["message"], "unknown format `yaml`");
    assert_eq!(err["error"]["hint"], "valid formats: table, json, csv");
}

#[test]
fn unknown_key_is_rejected_by_the_argument_parser() {
    TestEnv::new()
        .gsc()
        .args(["config", "get", "nope"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("possible values: site, format"));
}

#[test]
fn broken_config_file_is_reported_not_ignored() {
    let env = TestEnv::new();
    std::fs::write(env.config_dir.path().join("config.toml"), "site = [oops").unwrap();
    env.gsc()
        .args(["config", "list", "--format", "table"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("error: invalid config file"))
        .stderr(predicate::str::contains("hint:"));
}
