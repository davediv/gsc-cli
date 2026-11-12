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

/// A freshly generated service account, so tests never need a private key
/// checked into the repository.
pub struct TestServiceAccount {
    pub key_file: std::path::PathBuf,
    pub client_email: &'static str,
    key_pair: aws_lc_rs::signature::RsaKeyPair,
}

impl TestServiceAccount {
    /// Write a key file into `dir` whose tokens come from `token_uri`.
    pub fn create(dir: &std::path::Path, token_uri: &str) -> Self {
        use aws_lc_rs::encoding::{AsDer, Pkcs8V1Der};
        use base64::Engine as _;

        let client_email = "robot@test-project.iam.gserviceaccount.com";
        let key_pair = aws_lc_rs::signature::RsaKeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048)
            .expect("generate RSA key");
        let der: Pkcs8V1Der = key_pair.as_der().expect("encode key");
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            base64::engine::general_purpose::STANDARD.encode(der.as_ref())
        );

        let key_file = dir.join("service-account-key.json");
        let body = serde_json::json!({
            "type": "service_account",
            "project_id": "test-project",
            "private_key_id": "test-key-id",
            "private_key": pem,
            "client_email": client_email,
            "token_uri": token_uri,
        });
        std::fs::write(&key_file, body.to_string()).expect("write key file");
        Self {
            key_file,
            client_email,
            key_pair,
        }
    }

    /// Check a JWT's signature against this account's public key and return its claims.
    pub fn verify_assertion(&self, jwt: &str) -> serde_json::Value {
        use aws_lc_rs::signature::{KeyPair, RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
        use base64::Engine as _;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;

        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "a JWT has three segments");
        let signature = URL_SAFE_NO_PAD
            .decode(parts[2])
            .expect("signature is base64url");
        UnparsedPublicKey::new(
            &RSA_PKCS1_2048_8192_SHA256,
            self.key_pair.public_key().as_ref(),
        )
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .expect("assertion is signed by the service account's key");
        parse_json(
            &URL_SAFE_NO_PAD
                .decode(parts[1])
                .expect("claims are base64url"),
        )
    }
}

/// The bearer token every [`MockApi`] command sends.
pub const TEST_TOKEN: &str = "test-access-token";

/// A local server standing in for Google's APIs, plus a `gsc` wired to it.
pub struct MockApi {
    pub env: TestEnv,
    pub server: mockito::ServerGuard,
}

impl MockApi {
    pub fn new() -> Self {
        Self {
            env: TestEnv::new(),
            server: mockito::Server::new(),
        }
    }

    /// A `gsc` command that talks to the mock server. Retries are off unless
    /// a test turns them back on, so failure cases stay fast.
    pub fn gsc(&self) -> Command {
        let mut cmd = self.env.gsc();
        cmd.env("GSC_API_BASE_URL", self.server.url())
            .env("GSC_INDEXING_BASE_URL", self.server.url())
            .env("GSC_ACCESS_TOKEN", TEST_TOKEN)
            .env("GSC_MAX_RETRIES", "0");
        cmd
    }

    /// Run `gsc`, require success, and parse stdout as JSON.
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

    /// Run `gsc`, require the given exit code, and parse the JSON error on stderr.
    pub fn error(&self, args: &[&str], exit_code: i32) -> serde_json::Value {
        let output = self
            .gsc()
            .args(args)
            .assert()
            .code(exit_code)
            .get_output()
            .clone();
        parse_json(&output.stderr)["error"].clone()
    }

    /// Start defining a response. The request must carry the test token.
    pub fn on(&mut self, method: &str, path: &str) -> mockito::Mock {
        self.server
            .mock(method, path)
            .match_header("authorization", format!("Bearer {TEST_TOKEN}").as_str())
            .with_header("content-type", "application/json; charset=UTF-8")
    }
}
