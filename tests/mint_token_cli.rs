//! End-to-end tests for `actual mint-token`, the headless jwt-bearer client.
//!
//! These drive the REAL binary (arg parsing, env reading, key loading, signing,
//! the HTTP round-trip, and the stdout/stderr split) against a mock OAuth token
//! endpoint. The keypair is generated at runtime — no key material is committed.
//!
//! Note on live coverage: an end-to-end mint against a live authorization
//! server is not exercised here — the server-side jwt-bearer grant is not yet
//! reachable from this test suite (it needs a full app + database stack plus a
//! pre-registered key). Instead these tests prove the CLIENT end-to-end: the
//! unit tests verify the signed assertion under the server's exact RFC 7523
//! validation semantics, and these tests confirm the binary captures and prints
//! the token correctly.

use assert_cmd::cargo::cargo_bin_cmd;
use mockito::Matcher;
use p256::pkcs8::{EncodePrivateKey, LineEnding};
use predicates::prelude::*;

const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
const SERVICE_ACCOUNT_UUID: &str = "3f8a1c2e-4b5d-4e6f-8a9b-0c1d2e3f4a5b";

/// Generate an ephemeral EC P-256 private key as a PKCS#8 PEM string.
fn ec_private_key_pem() -> String {
    p256::SecretKey::random(&mut rand::thread_rng())
        .to_pkcs8_pem(LineEnding::LF)
        .expect("ec pkcs8 pem")
        .to_string()
}

/// Generate an ephemeral EC P-256 private key as a **SEC1** PEM string — the
/// RFC 5915 `EC PRIVATE KEY` encoding `openssl ecparam -genkey` writes, which
/// the assertion signer cannot load.
fn ec_private_key_sec1_pem() -> String {
    p256::SecretKey::random(&mut rand::thread_rng())
        .to_sec1_pem(LineEnding::LF)
        .expect("ec sec1 pem")
        .to_string()
}

/// A successful ES256 mint: stdout is EXACTLY the token, stderr carries status,
/// and the request reaches the token endpoint with the right grant + scope.
#[test]
fn mint_token_prints_only_the_token_on_success() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/api/oauth/token")
        .match_body(Matcher::AllOf(vec![
            Matcher::UrlEncoded("grant_type".into(), GRANT_TYPE.into()),
            Matcher::UrlEncoded("scope".into(), "adr:query adr:review".into()),
        ]))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"token_type":"Bearer","access_token":"minted-token-e2e","expires_in":300,"scope":"adr:query adr:review"}"#,
        )
        .create();

    let key_pem = ec_private_key_pem();

    let mut cmd = cargo_bin_cmd!("actual");
    cmd.env("ACTUAL_SERVICE_ACCOUNT_KEY", &key_pem).args([
        "mint-token",
        "--service-account-id",
        SERVICE_ACCOUNT_UUID,
        "--kid",
        "test-key-1",
        "--alg",
        "es256",
        "--issuer",
        &server.url(),
        "--scope",
        "adr:query",
        "--scope",
        "adr:review",
    ]);

    cmd.assert()
        .success()
        // stdout carries ONLY the token followed by a newline — the load-bearing
        // capture contract, so TOKEN=$(actual mint-token …) is exactly the token.
        .stdout(predicate::eq("minted-token-e2e\n"))
        // status goes to stderr, never the token.
        .stderr(predicate::str::contains("minted token").and(predicate::str::contains("ES256")))
        .stderr(predicate::str::contains("minted-token-e2e").not());

    mock.assert();
}

/// `--json` mode still keeps stdout machine-parseable and free of status noise.
#[test]
fn mint_token_json_mode_emits_the_full_response_on_stdout() {
    let mut server = mockito::Server::new();
    let _mock = server
        .mock("POST", "/api/oauth/token")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"token_type":"Bearer","access_token":"minted-json","expires_in":120,"scope":"adr:query"}"#)
        .create();

    let key_pem = ec_private_key_pem();
    let mut cmd = cargo_bin_cmd!("actual");
    cmd.env("ACTUAL_SERVICE_ACCOUNT_KEY", &key_pem).args([
        "mint-token",
        "--service-account-id",
        SERVICE_ACCOUNT_UUID,
        "--kid",
        "test-key-1",
        "--alg",
        "es256",
        "--issuer",
        &server.url(),
        "--json",
    ]);

    let output = cmd.assert().success().get_output().stdout.clone();
    let value: serde_json::Value =
        serde_json::from_slice(&output).expect("stdout must be a single JSON object");
    assert_eq!(value["access_token"], "minted-json");
    assert_eq!(value["expires_in"], 120);
}

/// A server rejection surfaces cleanly (the OAuth error + description), with a
/// non-zero exit and no token on stdout.
#[test]
fn mint_token_surfaces_server_rejection_cleanly() {
    let mut server = mockito::Server::new();
    let _mock = server
        .mock("POST", "/api/oauth/token")
        .with_status(400)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"error":"invalid_grant","error_description":"Assertion verification failed"}"#,
        )
        .create();

    let key_pem = ec_private_key_pem();
    let mut cmd = cargo_bin_cmd!("actual");
    cmd.env("ACTUAL_SERVICE_ACCOUNT_KEY", &key_pem).args([
        "mint-token",
        "--service-account-id",
        SERVICE_ACCOUNT_UUID,
        "--kid",
        "test-key-1",
        "--alg",
        "es256",
        "--issuer",
        &server.url(),
    ]);

    // The shared error panel truncates long messages to terminal width, so we
    // assert on the stable prefix of the surfaced OAuth failure. The full
    // `invalid_grant: Assertion verification failed` is covered by the
    // function-level test `mint_token_surfaces_invalid_grant_cleanly`. What
    // matters end-to-end: a clean non-zero exit, no token on stdout, and the
    // failure surfaced (not a stack trace).
    cmd.assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("Token mint failed"))
        .stderr(predicate::str::contains("400"));
}

/// A non-HTTPS, non-loopback issuer is refused before any assertion is sent —
/// the transport guard protects the token in flight.
#[test]
fn mint_token_rejects_non_https_issuer() {
    let key_pem = ec_private_key_pem();
    let mut cmd = cargo_bin_cmd!("actual");
    cmd.env("ACTUAL_SERVICE_ACCOUNT_KEY", &key_pem).args([
        "mint-token",
        "--service-account-id",
        SERVICE_ACCOUNT_UUID,
        "--kid",
        "test-key-1",
        "--alg",
        "es256",
        "--issuer",
        "http://not-loopback.example.com",
    ]);

    cmd.assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("HTTPS"));
}

/// A SEC1 EC key is refused, and the conversion command survives all the way to
/// the user's terminal.
///
/// What this pins is delivery, not detection. The unit tests pin the error's
/// message and hint as strings; the error panel then truncates every row to the
/// terminal width, so guidance that fits in a string can still be invisible in
/// practice. Its injection axis is the shared emitter: move the `openssl` line
/// off `sec1_ec_key_error`'s hint and into its message, and the row truncates
/// mid-command and this test goes red.
///
/// It is not the regression control for the inference fix, and it cannot be.
/// Two guards enforce one property here. `infer_from_pem` and the `EncodingKey`
/// loader both answer a SEC1 key with the same `sec1_ec_key_error()`, so
/// restoring the original defect in `infer_from_pem` (returning `Es256` on the
/// header match) leaves the loader emitting identical stderr, and both
/// iterations below stay green. The control for that defect is the unit test
/// `infer_from_pem_rejects_a_genuine_sec1_ec_key`, which drives inference
/// directly and does go red. Both entry points run here for coverage of the
/// surface a user reads, which is a weaker claim than telling them apart.
#[test]
fn mint_token_refuses_a_sec1_ec_key_and_shows_the_conversion_command() {
    let key_pem = ec_private_key_sec1_pem();

    for alg in [None, Some("es256")] {
        let mut cmd = cargo_bin_cmd!("actual");
        cmd.env("ACTUAL_SERVICE_ACCOUNT_KEY", &key_pem).args([
            "mint-token",
            "--service-account-id",
            SERVICE_ACCOUNT_UUID,
            "--kid",
            "test-key-1",
            "--issuer",
            "https://app.example.test",
        ]);
        if let Some(alg) = alg {
            cmd.args(["--alg", alg]);
        }

        cmd.assert()
            .failure()
            .stdout(predicate::str::is_empty())
            // The condition, naming the encoding the user will see in their file.
            .stderr(predicate::str::contains("SEC1"))
            // The remedy, whole and untruncated.
            .stderr(predicate::str::contains(
                "openssl pkcs8 -topk8 -nocrypt -in <key.pem> -out <key.pk8.pem>",
            ));
    }
}

/// A non-UUID service-account id is rejected with a clean message and no token.
#[test]
fn mint_token_rejects_non_uuid_principal() {
    let key_pem = ec_private_key_pem();
    let mut cmd = cargo_bin_cmd!("actual");
    cmd.env("ACTUAL_SERVICE_ACCOUNT_KEY", &key_pem).args([
        "mint-token",
        "--service-account-id",
        "not-a-uuid",
        "--kid",
        "test-key-1",
        "--alg",
        "es256",
        "--issuer",
        "https://app.example.test",
    ]);

    cmd.assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("UUID"));
}
