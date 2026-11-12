//! `gsc sites` and property resolution, against a mock API.

mod common;

use common::MockApi;
use serde_json::json;

const SITES: &str = r#"{"siteEntry":[
  {"siteUrl":"https://www.example.com/","permissionLevel":"siteFullUser"},
  {"siteUrl":"sc-domain:pending.dev","permissionLevel":"siteUnverifiedUser"},
  {"siteUrl":"sc-domain:example.com","permissionLevel":"siteOwner"},
  {"siteUrl":"https://blog.example.org/","permissionLevel":"SITE_RESTRICTED_USER"}
]}"#;

const DOMAIN_PATH: &str = "/webmasters/v3/sites/sc-domain%3Aexample.com";

fn with_sites(api: &mut MockApi) -> mockito::Mock {
    api.on("GET", "/webmasters/v3/sites")
        .with_body(SITES)
        .create()
}

#[test]
fn list_sorts_usable_properties_first_and_normalises_permissions() {
    let mut api = MockApi::new();
    with_sites(&mut api);

    let doc = api.json(&["sites", "list"]);
    assert_eq!(
        doc,
        json!({ "sites": [
            { "siteUrl": "https://blog.example.org/", "type": "urlPrefix",
              "permissionLevel": "siteRestrictedUser", "verified": true },
            { "siteUrl": "https://www.example.com/", "type": "urlPrefix",
              "permissionLevel": "siteFullUser", "verified": true },
            { "siteUrl": "sc-domain:example.com", "type": "domain",
              "permissionLevel": "siteOwner", "verified": true },
            { "siteUrl": "sc-domain:pending.dev", "type": "domain",
              "permissionLevel": "siteUnverifiedUser", "verified": false },
        ]})
    );

    api.gsc()
        .args(["sites", "list", "--format", "table"])
        .assert()
        .success()
        .stdout(
            "\
PROPERTY                   TYPE       PERMISSION
https://blog.example.org/  urlPrefix  siteRestrictedUser
https://www.example.com/   urlPrefix  siteFullUser
sc-domain:example.com      domain     siteOwner
sc-domain:pending.dev      domain     siteUnverifiedUser
",
        );

    api.gsc()
        .args(["sites", "list", "--format", "csv"])
        .assert()
        .success()
        .stdout(predicates::str::starts_with(
            "siteUrl,type,permissionLevel\nhttps://blog.example.org/,urlPrefix,siteRestrictedUser\n",
        ));
}

#[test]
fn empty_account_explains_the_service_account_gotcha() {
    let mut api = MockApi::new();
    api.on("GET", "/webmasters/v3/sites")
        .with_body("{}")
        .create();
    let doc = api.json(&["sites", "list"]);
    assert_eq!(doc["sites"], json!([]));
    assert!(
        doc["notes"][0]
            .as_str()
            .unwrap()
            .contains("service account")
    );
}

#[test]
fn get_with_an_exact_identifier_needs_no_lookup() {
    let mut api = MockApi::new();
    let list = with_sites(&mut api).expect(0);
    let get = api
        .on("GET", DOMAIN_PATH)
        .with_body(r#"{"siteUrl":"sc-domain:example.com","permissionLevel":"siteOwner"}"#)
        .create();

    let doc = api.json(&["sites", "get", "sc-domain:example.com"]);
    assert_eq!(doc["siteUrl"], "sc-domain:example.com");
    assert_eq!(doc["type"], "domain");
    assert_eq!(doc["verified"], true);
    get.assert();
    list.assert();
}

#[test]
fn bare_domain_is_matched_against_the_account() {
    let mut api = MockApi::new();
    with_sites(&mut api);
    let get = api
        .on("GET", DOMAIN_PATH)
        .with_body(r#"{"siteUrl":"sc-domain:example.com","permissionLevel":"siteOwner"}"#)
        .create();

    // example.com → the Domain property, preferred over https://www.example.com/.
    assert_eq!(
        api.json(&["sites", "get", "example.com"])["siteUrl"],
        "sc-domain:example.com"
    );
    get.assert();
}

#[test]
fn property_comes_from_flag_then_environment_then_config() {
    let mut api = MockApi::new();
    for (path, url) in [
        (
            "/webmasters/v3/sites/sc-domain%3Afrom-flag.com",
            "sc-domain:from-flag.com",
        ),
        (
            "/webmasters/v3/sites/sc-domain%3Afrom-env.com",
            "sc-domain:from-env.com",
        ),
        (
            "/webmasters/v3/sites/sc-domain%3Afrom-config.com",
            "sc-domain:from-config.com",
        ),
    ] {
        api.on("GET", path)
            .with_body(json!({ "siteUrl": url, "permissionLevel": "siteOwner" }).to_string())
            .create();
    }
    api.gsc()
        .args(["config", "set", "site", "sc-domain:from-config.com"])
        .assert()
        .success();

    let site_used = |env: Option<&str>, flag: Option<&str>| {
        let mut cmd = api.gsc();
        cmd.args(["sites", "get"]);
        if let Some(value) = env {
            cmd.env("GSC_SITE", value);
        }
        if let Some(value) = flag {
            cmd.args(["--site", value]);
        }
        let output = cmd.assert().success().get_output().clone();
        common::parse_json(&output.stdout)["siteUrl"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    assert_eq!(site_used(None, None), "sc-domain:from-config.com");
    assert_eq!(
        site_used(Some("sc-domain:from-env.com"), None),
        "sc-domain:from-env.com"
    );
    assert_eq!(
        site_used(
            Some("sc-domain:from-env.com"),
            Some("sc-domain:from-flag.com")
        ),
        "sc-domain:from-flag.com"
    );
}

#[test]
fn missing_property_is_a_usage_error_that_says_how_to_choose_one() {
    let api = MockApi::new();
    let err = api.error(&["sites", "get"], 2);
    assert_eq!(err["message"], "no property selected");
    let hint = err["hint"].as_str().unwrap();
    assert!(hint.contains("--site"));
    assert!(hint.contains("gsc config set site"));
    assert!(hint.contains("gsc sites list"));
}

#[test]
fn unknown_bare_domain_lists_the_available_properties() {
    let mut api = MockApi::new();
    with_sites(&mut api);
    let err = api.error(&["sites", "get", "nope.io"], 2);
    assert_eq!(err["message"], "no property matches `nope.io`");
    assert_eq!(
        err["hint"],
        "available properties: https://blog.example.org/, https://www.example.com/, sc-domain:example.com"
    );
}

#[test]
fn add_puts_the_encoded_property() {
    let mut api = MockApi::new();
    let put = api
        .on(
            "PUT",
            "/webmasters/v3/sites/https%3A%2F%2Fnew.example.com%2F",
        )
        .with_status(204)
        .create();

    // The trailing slash a URL-prefix property needs is added automatically.
    let doc = api.json(&["sites", "add", "https://new.example.com"]);
    assert_eq!(doc["siteUrl"], "https://new.example.com/");
    assert_eq!(doc["added"], true);
    assert!(doc["notes"][0].as_str().unwrap().contains("unverified"));
    put.assert();
}

#[test]
fn add_refuses_to_guess_the_property_type() {
    let api = MockApi::new();
    let err = api.error(&["sites", "add", "new.example.com"], 2);
    assert_eq!(
        err["hint"],
        "use `sc-domain:new.example.com` for a Domain property, or `https://new.example.com/` for a URL-prefix property"
    );
}

#[test]
fn remove_requires_confirmation() {
    let mut api = MockApi::new();
    let delete = api
        .on("DELETE", DOMAIN_PATH)
        .with_status(204)
        .expect(1)
        .create();

    let err = api.error(&["sites", "remove", "sc-domain:example.com"], 2);
    assert_eq!(
        err["message"],
        "refusing to remove sc-domain:example.com without confirmation"
    );
    assert!(!delete.matched(), "nothing may be deleted without --yes");

    let doc = api.json(&["sites", "remove", "sc-domain:example.com", "--yes"]);
    assert_eq!(
        doc,
        json!({ "siteUrl": "sc-domain:example.com", "removed": true })
    );
    delete.assert();
}

#[test]
fn api_passthrough_substitutes_the_selected_property() {
    let mut api = MockApi::new();
    with_sites(&mut api);
    let sitemaps = api
        .on(
            "GET",
            "/webmasters/v3/sites/sc-domain%3Aexample.com/sitemaps",
        )
        .with_body(r#"{"sitemap":[]}"#)
        .create();

    let doc = api.json(&[
        "api",
        "GET",
        "/webmasters/v3/sites/{site}/sitemaps",
        "--site",
        "example.com",
    ]);
    assert_eq!(doc, json!({ "sitemap": [] }));
    sitemaps.assert();
}
