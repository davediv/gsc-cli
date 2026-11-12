//! Service-account authentication.
//!
//! A service account proves its identity by signing a short-lived JWT with
//! its private key and exchanging it for an access token (RFC 7523). No
//! browser is involved, which makes it the right choice for servers, CI,
//! and unattended agents.

use std::fmt;
use std::path::Path;

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::json;

use super::store::CachedToken;
use super::{default_token_uri, token_request};
use crate::error::{Error, Result};

const JWT_BEARER_GRANT: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
/// Google rejects assertions valid for more than an hour.
const ASSERTION_LIFETIME_SECS: i64 = 3600;

/// The JSON key file downloaded for a service account.
#[derive(Clone, Deserialize)]
pub struct ServiceAccountKey {
    pub client_email: String,
    /// PEM-encoded RSA private key.
    pub private_key: String,
    #[serde(default)]
    pub private_key_id: Option<String>,
    #[serde(default = "default_token_uri")]
    pub token_uri: String,
}

/// The private key is never printed, even in debug output.
impl fmt::Debug for ServiceAccountKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceAccountKey")
            .field("client_email", &self.client_email)
            .field("private_key_id", &self.private_key_id)
            .finish_non_exhaustive()
    }
}

impl ServiceAccountKey {
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            Error::auth(format!(
                "could not read the service-account key at {}: {e}",
                path.display()
            ))
            .with_hint("check the path, or run `gsc auth login --service-account <key.json>` again")
        })?;
        let key: Self = serde_json::from_str(&text).map_err(|e| {
            Error::auth(format!(
                "{} is not a service-account key file: {e}",
                path.display()
            ))
        })?;
        // Catch a damaged key now, with the file name, rather than on first use.
        key.key_pair().map_err(|e| {
            Error::auth(format!("{}: {}", path.display(), e.message))
                .with_hint("download a fresh JSON key for the service account")
        })?;
        Ok(key)
    }

    fn key_pair(&self) -> Result<RsaKeyPair> {
        let (label, der) = decode_pem(&self.private_key)
            .ok_or_else(|| Error::auth("the private key is not valid PEM"))?;
        let parsed = match label.as_str() {
            "PRIVATE KEY" => RsaKeyPair::from_pkcs8(&der),
            "RSA PRIVATE KEY" => RsaKeyPair::from_der(&der),
            other => {
                return Err(Error::auth(format!(
                    "unsupported private key type `{other}`; expected an RSA key"
                )));
            }
        };
        parsed.map_err(|e| Error::auth(format!("the private key was rejected ({e})")))
    }

    /// Build the signed JWT that is exchanged for an access token.
    pub fn assertion(&self, scopes: &[&str], now: i64) -> Result<String> {
        let mut header = json!({ "alg": "RS256", "typ": "JWT" });
        if let Some(key_id) = &self.private_key_id {
            header["kid"] = json!(key_id);
        }
        let claims = json!({
            "iss": self.client_email,
            "scope": scopes.join(" "),
            "aud": self.token_uri,
            "iat": now,
            "exp": now + ASSERTION_LIFETIME_SECS,
        });
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );

        let key_pair = self.key_pair()?;
        let mut signature = vec![0u8; key_pair.public_modulus_len()];
        key_pair
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signing_input.as_bytes(),
                &mut signature,
            )
            .map_err(|_| Error::internal("could not sign the service-account assertion"))?;

        Ok(format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature)
        ))
    }
}

/// Exchange a signed assertion for an access token with the given scopes.
pub async fn fetch_token(
    http: &reqwest::Client,
    key: &ServiceAccountKey,
    scopes: &[&str],
) -> Result<CachedToken> {
    let assertion = key.assertion(scopes, super::unix_now())?;
    let response = token_request(
        http,
        &key.token_uri,
        &[("grant_type", JWT_BEARER_GRANT), ("assertion", &assertion)],
    )
    .await
    .map_err(|e| match e.code.as_deref() {
        Some("invalid_grant") => e.with_hint(
            "the key may have been deleted or the service account disabled; \
             also check that this machine's clock is correct",
        ),
        _ => e,
    })?;
    Ok(response.to_cached())
}

/// Decode a single PEM block into its label and DER bytes.
fn decode_pem(pem: &str) -> Option<(String, Vec<u8>)> {
    let mut lines = pem.lines().map(str::trim).filter(|line| !line.is_empty());
    let label = lines
        .next()?
        .strip_prefix("-----BEGIN ")?
        .strip_suffix("-----")?
        .to_owned();
    let footer = format!("-----END {label}-----");

    let mut body = String::new();
    let mut closed = false;
    for line in lines {
        if line == footer {
            closed = true;
            break;
        }
        body.push_str(line);
    }
    if !closed {
        return None;
    }
    STANDARD.decode(body).ok().map(|der| (label, der))
}

#[cfg(test)]
pub(crate) mod test_support {
    use aws_lc_rs::encoding::{AsDer, Pkcs8V1Der};
    use aws_lc_rs::rsa::KeySize;

    use super::*;

    /// A throwaway RSA key, so no private key ever needs to live in the repo.
    pub fn generate_key_pair() -> RsaKeyPair {
        RsaKeyPair::generate(KeySize::Rsa2048).expect("generate RSA key")
    }

    pub fn to_pkcs8_pem(key_pair: &RsaKeyPair) -> String {
        let der: Pkcs8V1Der = key_pair.as_der().expect("encode PKCS#8");
        let encoded = STANDARD.encode(der.as_ref());
        let mut pem = String::from("-----BEGIN PRIVATE KEY-----\n");
        for chunk in encoded.as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(chunk).unwrap());
            pem.push('\n');
        }
        pem.push_str("-----END PRIVATE KEY-----\n");
        pem
    }

    pub fn key(key_pair: &RsaKeyPair) -> ServiceAccountKey {
        ServiceAccountKey {
            client_email: "robot@project.iam.gserviceaccount.com".into(),
            private_key: to_pkcs8_pem(key_pair),
            private_key_id: Some("key-id-1".into()),
            token_uri: "https://oauth2.googleapis.com/token".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use aws_lc_rs::signature::{KeyPair, RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
    use serde_json::Value;

    use super::test_support::*;
    use super::*;
    use crate::auth::{SCOPE_INDEXING, SCOPE_WEBMASTERS};

    fn decode_segment(segment: &str) -> Value {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(segment).unwrap()).unwrap()
    }

    #[test]
    fn assertion_has_the_claims_google_requires_and_a_valid_signature() {
        let key_pair = generate_key_pair();
        let key = key(&key_pair);
        let jwt = key
            .assertion(&[SCOPE_WEBMASTERS, SCOPE_INDEXING], 1_700_000_000)
            .unwrap();

        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(
            decode_segment(parts[0]),
            json!({ "alg": "RS256", "typ": "JWT", "kid": "key-id-1" })
        );
        assert_eq!(
            decode_segment(parts[1]),
            json!({
                "iss": "robot@project.iam.gserviceaccount.com",
                "scope": format!("{SCOPE_WEBMASTERS} {SCOPE_INDEXING}"),
                "aud": "https://oauth2.googleapis.com/token",
                "iat": 1_700_000_000,
                "exp": 1_700_003_600,
            })
        );

        let signed = format!("{}.{}", parts[0], parts[1]);
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key_pair.public_key().as_ref())
            .verify(signed.as_bytes(), &signature)
            .expect("signature verifies against the key's public half");
    }

    #[test]
    fn key_id_is_optional() {
        let key_pair = generate_key_pair();
        let mut key = key(&key_pair);
        key.private_key_id = None;
        let jwt = key.assertion(&[SCOPE_WEBMASTERS], 0).unwrap();
        let header = decode_segment(jwt.split('.').next().unwrap());
        assert_eq!(header, json!({ "alg": "RS256", "typ": "JWT" }));
    }

    #[test]
    fn damaged_keys_are_rejected_with_auth_errors() {
        let key_pair = generate_key_pair();
        let mut broken = key(&key_pair);

        broken.private_key = "not a pem".into();
        assert!(
            broken
                .assertion(&[], 0)
                .unwrap_err()
                .message
                .contains("not valid PEM")
        );

        broken.private_key =
            "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n".into();
        assert!(
            broken
                .assertion(&[], 0)
                .unwrap_err()
                .message
                .contains("rejected")
        );

        broken.private_key =
            "-----BEGIN EC PRIVATE KEY-----\nAAAA\n-----END EC PRIVATE KEY-----\n".into();
        assert!(
            broken
                .assertion(&[], 0)
                .unwrap_err()
                .message
                .contains("unsupported private key type")
        );
    }

    #[test]
    fn pem_decoding_requires_matching_markers() {
        let (label, der) =
            decode_pem("-----BEGIN PRIVATE KEY-----\n  AQID\n\nBAU=\n-----END PRIVATE KEY-----\n")
                .unwrap();
        assert_eq!(label, "PRIVATE KEY");
        assert_eq!(der, [1, 2, 3, 4, 5]);

        assert!(decode_pem("-----BEGIN PRIVATE KEY-----\nAQID\n").is_none());
        assert!(
            decode_pem("-----BEGIN PRIVATE KEY-----\nAQID\n-----END PUBLIC KEY-----").is_none()
        );
        assert!(decode_pem("").is_none());
    }

    #[test]
    fn debug_output_redacts_the_private_key() {
        let key_pair = generate_key_pair();
        let text = format!("{:?}", key(&key_pair));
        assert!(text.contains("robot@project.iam.gserviceaccount.com"));
        assert!(!text.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn key_file_parsing_ignores_extra_fields_and_defaults_the_token_uri() {
        let key_pair = generate_key_pair();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("key.json");
        std::fs::write(
            &path,
            json!({
                "type": "service_account",
                "project_id": "my-project",
                "client_email": "robot@project.iam.gserviceaccount.com",
                "private_key": to_pkcs8_pem(&key_pair),
                "universe_domain": "googleapis.com",
            })
            .to_string(),
        )
        .unwrap();

        let key = ServiceAccountKey::from_file(&path).unwrap();
        assert_eq!(key.token_uri, crate::auth::DEFAULT_TOKEN_URI);
        assert_eq!(key.private_key_id, None);
    }

    #[test]
    fn unreadable_or_invalid_key_files_are_auth_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = ServiceAccountKey::from_file(&tmp.path().join("nope.json")).unwrap_err();
        assert_eq!(missing.kind, crate::error::ErrorKind::Auth);

        let path = tmp.path().join("bad.json");
        std::fs::write(&path, r#"{"client_email":"a@b","private_key":"garbage"}"#).unwrap();
        let invalid = ServiceAccountKey::from_file(&path).unwrap_err();
        assert!(invalid.message.contains("bad.json"));
        assert!(invalid.message.contains("not valid PEM"));
    }
}
