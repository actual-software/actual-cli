//! Signing with an Ed25519 key held in Cloud KMS, through the REST API and the
//! signer's own `gcloud` login.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::Deserialize;

use crate::minisign::Signer;
use crate::spawn::spawn_with_etxtbsy_retry;

const ENDPOINT: &str = "https://cloudkms.googleapis.com";

pub struct KmsSigner {
    client: reqwest::blocking::Client,
    endpoint: String,
    key_version: String,
    access_token: String,
}

#[derive(Deserialize)]
struct SignResponse {
    signature: String,
    name: String,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: String,
}

impl KmsSigner {
    pub fn new(key_version: &str, access_token: String) -> Result<Self> {
        Self::with_endpoint(ENDPOINT, key_version, access_token)
    }

    fn with_endpoint(endpoint: &str, key_version: &str, access_token: String) -> Result<Self> {
        check_key_version_name(key_version)?;
        Ok(Self {
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()?,
            endpoint: endpoint.to_owned(),
            key_version: key_version.to_owned(),
            access_token,
        })
    }
}

impl Signer for KmsSigner {
    fn sign(&self, message: &[u8]) -> Result<[u8; 64]> {
        // Ed25519 keys take the message itself in `data`, never a `digest`.
        let response = self
            .client
            .post(format!(
                "{}/v1/{}:asymmetricSign",
                self.endpoint, self.key_version
            ))
            .bearer_auth(&self.access_token)
            .json(&serde_json::json!({ "data": BASE64.encode(message) }))
            .send()
            .context("calling Cloud KMS")?;
        let status = response.status();
        let body = response.bytes().context("reading the Cloud KMS response")?;
        if !status.is_success() {
            let detail = serde_json::from_slice::<ErrorResponse>(&body)
                .map(|e| e.error.message)
                .unwrap_or_else(|_| String::from_utf8_lossy(&body).into_owned());
            bail!("Cloud KMS refused to sign ({status}): {detail}");
        }
        let signed: SignResponse =
            serde_json::from_slice(&body).context("reading the Cloud KMS response")?;
        ensure!(
            signed.name == self.key_version,
            "Cloud KMS answered for {} instead of {}",
            signed.name,
            self.key_version
        );
        let signature = BASE64
            .decode(&signed.signature)
            .context("the Cloud KMS signature isn't base64")?;
        signature.try_into().map_err(|s: Vec<u8>| {
            anyhow::anyhow!(
                "Cloud KMS returned a {}-byte signature; an Ed25519 key returns 64",
                s.len()
            )
        })
    }
}

/// Requires the full resource name of one key version, so a signature is never made with whichever version is primary.
fn check_key_version_name(name: &str) -> Result<()> {
    let parts: Vec<&str> = name.split('/').collect();
    let shape = [
        "projects",
        "",
        "locations",
        "",
        "keyRings",
        "",
        "cryptoKeys",
        "",
        "cryptoKeyVersions",
    ];
    let well_formed = parts.len() == 10
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        })
        && shape
            .iter()
            .zip(&parts)
            .all(|(want, got)| want.is_empty() || want == got)
        && parts[9].bytes().all(|b| b.is_ascii_digit());
    ensure!(
        well_formed,
        "--kms-key takes a key version's full name, projects/<project>/locations/<location>/keyRings/<key ring>/cryptoKeys/<key>/cryptoKeyVersions/<n>, not `{name}`"
    );
    Ok(())
}

/// An access token for the signer's own `gcloud` login.
pub fn gcloud_access_token(gcloud: &Path) -> Result<String> {
    let output = spawn_with_etxtbsy_retry(|| {
        Command::new(gcloud)
            .args(["auth", "print-access-token"])
            .output()
    })
    .with_context(|| {
        format!(
            "running {} (is the Google Cloud CLI installed?)",
            gcloud.display()
        )
    })?;
    ensure!(
        output.status.success(),
        "`gcloud auth print-access-token` failed; run `gcloud auth login` with your work account first: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let token = String::from_utf8(output.stdout)?.trim().to_owned();
    ensure!(
        !token.is_empty(),
        "`gcloud auth print-access-token` printed nothing"
    );
    Ok(token)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    use ring::signature::{Ed25519KeyPair, KeyPair};

    use super::*;
    use crate::minisign::{encode_for_trusted_key, sign, PublicKey, TrustedKey};

    const KEY_VERSION: &str =
        "projects/p/locations/global/keyRings/release/cryptoKeys/release-signing-primary/cryptoKeyVersions/1";
    const SIGN_PATH: &str =
        "/v1/projects/p/locations/global/keyRings/release/cryptoKeys/release-signing-primary/cryptoKeyVersions/1:asymmetricSign";

    /// A stand-in for Cloud KMS that signs `data` with a local key, the way the real endpoint does.
    fn fake_kms(server: &mut mockito::Server) -> (mockito::Mock, PublicKey) {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = Arc::new(Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap());
        let public = PublicKey {
            key_id: [3; 8],
            key: pair.public_key().as_ref().try_into().unwrap(),
        };
        let mock = server
            .mock("POST", SIGN_PATH)
            .match_header("authorization", "Bearer test-token")
            .match_header("content-type", "application/json")
            .with_status(200)
            .with_body_from_request(move |request| {
                let body: serde_json::Value =
                    serde_json::from_slice(request.body().unwrap()).unwrap();
                assert!(
                    body.get("digest").is_none(),
                    "Ed25519 keys sign data, not a digest"
                );
                let data = BASE64.decode(body["data"].as_str().unwrap()).unwrap();
                serde_json::json!({
                    "signature": BASE64.encode(pair.sign(&data)),
                    "name": KEY_VERSION,
                    "protectionLevel": "SOFTWARE",
                })
                .to_string()
                .into_bytes()
            })
            .expect(2)
            .create();
        (mock, public)
    }

    #[test]
    fn signs_a_minisign_signature_through_the_kms_api() {
        let mut server = mockito::Server::new();
        let (mock, public) = fake_kms(&mut server);
        let signer =
            KmsSigner::with_endpoint(&server.url(), KEY_VERSION, "test-token".into()).unwrap();
        let data = b"some SHA256SUMS\n";
        let signature = sign(&signer, data, "timestamp:1\tfile:SHA256SUMS\ttag:v1.0.0").unwrap();
        let keys = [TrustedKey {
            name: "primary".into(),
            key: public,
        }];
        let (key, _) = encode_for_trusted_key(&signature, data, &keys).unwrap();
        assert_eq!(key.name, "primary");
        mock.assert();
    }

    #[test]
    fn reports_why_kms_refused() {
        let mut server = mockito::Server::new();
        server
            .mock("POST", SIGN_PATH)
            .with_status(403)
            .with_body(r#"{"error":{"code":403,"message":"Permission 'cloudkms.cryptoKeyVersions.useToSign' denied","status":"PERMISSION_DENIED"}}"#)
            .create();
        let signer =
            KmsSigner::with_endpoint(&server.url(), KEY_VERSION, "test-token".into()).unwrap();
        let err = signer.sign(b"x").unwrap_err().to_string();
        assert!(err.contains("403") && err.contains("useToSign"), "{err}");
    }

    #[test]
    fn reports_a_refusal_without_a_json_body() {
        let mut server = mockito::Server::new();
        server
            .mock("POST", SIGN_PATH)
            .with_status(502)
            .with_body("bad gateway")
            .create();
        let signer = KmsSigner::with_endpoint(&server.url(), KEY_VERSION, "t".into()).unwrap();
        let err = signer.sign(b"x").unwrap_err().to_string();
        assert!(err.contains("502") && err.contains("bad gateway"), "{err}");
    }

    #[test]
    fn refuses_a_response_for_another_key_or_of_the_wrong_size() {
        for (body, expected) in [
            (
                serde_json::json!({ "signature": BASE64.encode([0u8; 64]), "name": "projects/p/locations/global/keyRings/release/cryptoKeys/other/cryptoKeyVersions/1" }),
                "instead of",
            ),
            (
                serde_json::json!({ "signature": BASE64.encode([0u8; 63]), "name": KEY_VERSION }),
                "63-byte signature",
            ),
            (
                serde_json::json!({ "signature": "!!", "name": KEY_VERSION }),
                "isn't base64",
            ),
            (
                serde_json::json!({ "name": KEY_VERSION }),
                "reading the Cloud KMS response",
            ),
        ] {
            let mut server = mockito::Server::new();
            server
                .mock("POST", SIGN_PATH)
                .with_status(200)
                .with_body(body.to_string())
                .create();
            let signer = KmsSigner::with_endpoint(&server.url(), KEY_VERSION, "t".into()).unwrap();
            let err = signer.sign(b"x").unwrap_err();
            assert!(format!("{err:#}").contains(expected), "{err:#}");
        }
    }

    #[test]
    fn reports_an_unreachable_endpoint() {
        let signer =
            KmsSigner::with_endpoint("http://127.0.0.1:9", KEY_VERSION, "t".into()).unwrap();
        assert!(signer
            .sign(b"x")
            .unwrap_err()
            .to_string()
            .contains("calling Cloud KMS"));
    }

    #[test]
    fn needs_one_key_version_by_its_full_name() {
        assert!(KmsSigner::new(KEY_VERSION, "t".into()).is_ok());
        for name in [
            "projects/p/locations/global/keyRings/release/cryptoKeys/release-signing-primary",
            "projects/p/locations/global/keyRings/release/cryptoKeys/k/cryptoKeyVersions/latest",
            "projects/p/locations/global/keyRings/release/cryptoKeys/k/cryptoKeyVersions/1/extra",
            "projects//locations/global/keyRings/release/cryptoKeys/k/cryptoKeyVersions/1",
            "folders/p/locations/global/keyRings/release/cryptoKeys/k/cryptoKeyVersions/1",
            "projects/p/locations/global/keyRings/r?x=1/cryptoKeys/k/cryptoKeyVersions/1",
        ] {
            assert!(KmsSigner::new(name, "t".into()).is_err(), "accepted {name}");
        }
    }

    fn fake_gcloud(script: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gcloud");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    #[test]
    fn reads_the_access_token_from_gcloud() {
        let dir = fake_gcloud(r#"[ "$*" = "auth print-access-token" ] && echo ya29.token"#);
        assert_eq!(
            gcloud_access_token(&dir.path().join("gcloud")).unwrap(),
            "ya29.token"
        );
    }

    #[test]
    fn explains_a_missing_or_logged_out_gcloud() {
        let dir = fake_gcloud("echo 'ERROR: no credentialed accounts' >&2; exit 1");
        let err = gcloud_access_token(&dir.path().join("gcloud"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("gcloud auth login") && err.contains("no credentialed accounts"),
            "{err}"
        );
        let empty = fake_gcloud("exit 0");
        assert!(gcloud_access_token(&empty.path().join("gcloud")).is_err());
        assert!(gcloud_access_token(&dir.path().join("missing")).is_err());
    }
}
