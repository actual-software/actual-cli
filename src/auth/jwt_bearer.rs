//! Headless RFC 7523 JWT-bearer client: sign a service-account assertion with a
//! registered private key and mint a short-lived access token from the OAuth
//! token endpoint, with **no browser and no human**.
//!
//! This is the unattended agent path — contrast `login` (browser OAuth) and
//! `login --device` (a human approves a code), which are the human-delegated
//! *enrollment* leg. Here the agent already holds a private key whose matching
//! PUBLIC key is registered server-side; it self-issues a one-shot signed
//! assertion and exchanges it for a token.
//!
//! The contract is mirrored exactly from the authorization server's RFC 7523
//! jwt-bearer grant:
//!
//! - `grant_type` = [`JWT_BEARER_GRANT_TYPE`].
//! - Assertion header: `{ alg: RS256 | ES256, kid, typ: JWT }`. HS*/`none` are
//!   refused here (there is no way to construct them) and server-side, closing
//!   alg-confusion.
//! - Assertion claims: `iss == sub == <service_account_id>` (a valid UUID),
//!   `aud` one of the server's accepted audiences, a unique `jti` per call
//!   (server anti-replays), `iat = now`, `exp <= iat + 300`.
//! - Request: POST `${issuer}/api/oauth/token` with `grant_type` + `assertion`
//!   + optional `scope` (space-delimited subset of the principal's grant).
//!
//! Nothing in this module logs token or key material.

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ActualError;

/// RFC 7523 JWT-bearer grant type URN.
pub const JWT_BEARER_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

/// The authorization server's hard ceiling on an assertion's lifetime
/// (`exp - iat`), in seconds. An assertion whose window exceeds this is
/// rejected.
pub const MAX_ASSERTION_LIFETIME_SECONDS: u64 = 300;

/// Default assertion lifetime. Deliberately well under
/// [`MAX_ASSERTION_LIFETIME_SECONDS`] so that clock rounding can never push
/// `exp - iat` over the cap, while still leaving ample room for the mint
/// round-trip. The assertion is one-shot and used immediately.
pub const DEFAULT_ASSERTION_LIFETIME_SECONDS: u64 = 60;

/// The asymmetric JWS algorithms the authorization server accepts. Symmetric
/// (`HS*`) and unsigned (`none`) assertions are absent by design — they have no
/// public-key story and would open alg-confusion — so this enum has no way to
/// represent them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssertionAlgorithm {
    /// RSASSA-PKCS1-v1_5 using SHA-256; signs with an RSA private key.
    Rs256,
    /// ECDSA using P-256 and SHA-256; signs with an EC (P-256) private key.
    Es256,
}

impl AssertionAlgorithm {
    /// The `jsonwebtoken` algorithm this maps to.
    fn to_jwt(self) -> Algorithm {
        match self {
            AssertionAlgorithm::Rs256 => Algorithm::RS256,
            AssertionAlgorithm::Es256 => Algorithm::ES256,
        }
    }

    /// The canonical `alg` header string.
    pub fn as_str(self) -> &'static str {
        match self {
            AssertionAlgorithm::Rs256 => "RS256",
            AssertionAlgorithm::Es256 => "ES256",
        }
    }

    /// Parse an algorithm name, case-insensitively. Only the two asymmetric
    /// algorithms the server accepts are recognized; `HS256`, `none`, `RS512`,
    /// etc. are rejected so the client can never emit a forbidden `alg`.
    pub fn parse(value: &str) -> Result<Self, ActualError> {
        match value.trim().to_ascii_uppercase().as_str() {
            "RS256" => Ok(AssertionAlgorithm::Rs256),
            "ES256" => Ok(AssertionAlgorithm::Es256),
            other => Err(ActualError::ConfigError(format!(
                "Unsupported assertion algorithm '{other}'. \
                 Only RS256 and ES256 are accepted (HS*/none are refused)."
            ))),
        }
    }

    /// Infer the algorithm from a private-key PEM: an RSA key signs RS256, an
    /// EC key signs ES256. Used when the caller does not pass `--alg`.
    pub fn infer_from_pem(private_key_pem: &[u8]) -> Result<Self, ActualError> {
        let text = std::str::from_utf8(private_key_pem).map_err(|_| {
            ActualError::ConfigError("Private key is not valid UTF-8 PEM".to_string())
        })?;
        if text.contains("BEGIN RSA PRIVATE KEY") {
            return Ok(AssertionAlgorithm::Rs256);
        }
        if text.contains("BEGIN EC PRIVATE KEY") {
            return Ok(AssertionAlgorithm::Es256);
        }
        // PKCS#8 ("BEGIN PRIVATE KEY") does not name the key type in its header,
        // so try RSA then EC and let the key parser decide.
        if text.contains("BEGIN PRIVATE KEY") {
            if EncodingKey::from_rsa_pem(private_key_pem).is_ok() {
                return Ok(AssertionAlgorithm::Rs256);
            }
            if EncodingKey::from_ec_pem(private_key_pem).is_ok() {
                return Ok(AssertionAlgorithm::Es256);
            }
        }
        Err(ActualError::ConfigError(
            "Could not determine the key algorithm from the PEM. \
             Pass --alg rs256 or --alg es256 explicitly."
                .to_string(),
        ))
    }
}

/// RFC 7523 assertion claims. `iss` and `sub` are both the service-account id;
/// `aud` targets the authorization server; `jti` is unique per assertion.
#[derive(Debug, Serialize, Deserialize)]
struct AssertionClaims {
    iss: String,
    sub: String,
    aud: String,
    jti: String,
    iat: u64,
    exp: u64,
}

/// Inputs to build one assertion. Borrowed so the caller owns the strings.
pub struct AssertionParams<'a> {
    /// The service-account principal id — becomes `iss` and `sub`. Must be a
    /// valid UUID (the server feeds it a uuid-typed column and rejects a
    /// non-uuid `sub` with a generic `invalid_grant`).
    pub service_account_id: &'a str,
    /// The audience — one of the server's accepted values (its OAuth issuer, or
    /// `${issuer}/api/oauth/token`).
    pub audience: &'a str,
    /// The registered key id, placed in the JWS header so the server knows
    /// which public key to verify against.
    pub kid: &'a str,
    /// The signing algorithm; must match the registered key's algorithm.
    pub algorithm: AssertionAlgorithm,
    /// Requested lifetime in seconds. Clamped to `[1, 300]`.
    pub lifetime_seconds: u64,
}

/// Return `true` if `value` is a well-formed RFC 4122 UUID, matching the
/// server's `isUuid` check exactly (version 1–5, variant 8/9/a/b) so the client
/// accepts a principal id iff the server would.
pub fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    // 8-4-4-4-12 with dashes at fixed positions.
    if bytes.len() != 36 {
        return false;
    }
    for (i, b) in bytes.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if *b != b'-' {
                    return false;
                }
            }
            14 => {
                // version nibble: 1..=5
                if !matches!(b, b'1'..=b'5') {
                    return false;
                }
            }
            19 => {
                // variant nibble: 8, 9, a, b (case-insensitive)
                if !matches!(b, b'8' | b'9' | b'a' | b'b' | b'A' | b'B') {
                    return false;
                }
            }
            _ => {
                if !b.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

/// Build and sign an RFC 7523 assertion. `now_unix` is the current time in
/// seconds since the epoch (injected for testability). Each call mints a fresh
/// unique `jti`.
///
/// Fails with a clean [`ActualError::ConfigError`] on a non-UUID principal id,
/// an empty `kid`, an unreadable/mismatched key, or a signing error — never a
/// panic, and never with key material in the message.
pub fn build_and_sign_assertion(
    params: &AssertionParams<'_>,
    private_key_pem: &[u8],
    now_unix: u64,
) -> Result<String, ActualError> {
    if !is_uuid(params.service_account_id) {
        return Err(ActualError::ConfigError(
            "Service account id must be a UUID (it is the assertion iss/sub)".to_string(),
        ));
    }
    if params.kid.trim().is_empty() {
        return Err(ActualError::ConfigError(
            "A key id (--kid) is required".to_string(),
        ));
    }
    if params.audience.trim().is_empty() {
        return Err(ActualError::ConfigError(
            "An assertion audience is required".to_string(),
        ));
    }

    let lifetime = params
        .lifetime_seconds
        .clamp(1, MAX_ASSERTION_LIFETIME_SECONDS);
    let claims = AssertionClaims {
        iss: params.service_account_id.to_string(),
        sub: params.service_account_id.to_string(),
        aud: params.audience.to_string(),
        jti: Uuid::new_v4().to_string(),
        iat: now_unix,
        exp: now_unix + lifetime,
    };

    let mut header = Header::new(params.algorithm.to_jwt());
    header.kid = Some(params.kid.to_string());
    // `typ` defaults to "JWT" in jsonwebtoken's Header.

    let key = match params.algorithm {
        AssertionAlgorithm::Rs256 => EncodingKey::from_rsa_pem(private_key_pem),
        AssertionAlgorithm::Es256 => EncodingKey::from_ec_pem(private_key_pem),
    }
    .map_err(|_| {
        ActualError::ConfigError(format!(
            "Failed to load the {} private key from PEM (is it the right key type?)",
            params.algorithm.as_str()
        ))
    })?;

    encode(&header, &claims, &key)
        .map_err(|_| ActualError::ConfigError("Failed to sign the assertion".to_string()))
}

/// The `/api/oauth/token` success response for the jwt-bearer grant. The server
/// mints `{ token_type, access_token, expires_in, scope }`; no refresh token is
/// issued for a service-account grant (the agent re-mints from a fresh
/// assertion).
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default = "default_bearer")]
    token_type: String,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
}

fn default_bearer() -> String {
    "Bearer".to_string()
}

/// A minted access token. `access_token` is **sensitive**; the [`std::fmt::Debug`]
/// impl redacts it so it never reaches logs or `{:?}`.
#[derive(Clone, PartialEq, Eq)]
pub struct MintedToken {
    /// The bearer access token. **Sensitive — never logged.**
    pub access_token: String,
    /// Token type, normally `Bearer`.
    pub token_type: String,
    /// Lifetime in seconds, if the server reported one.
    pub expires_in: Option<i64>,
    /// Space-delimited granted scopes, if the server reported them.
    pub scope: Option<String>,
}

impl std::fmt::Debug for MintedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MintedToken")
            .field("access_token", &"<redacted>")
            .field("token_type", &self.token_type)
            .field("expires_in", &self.expires_in)
            .field("scope", &self.scope)
            .finish()
    }
}

/// Map an unsuccessful token-endpoint response to a clean [`ActualError`]. The
/// body is OAuth error JSON (`{ error, error_description }`), not a secret; we
/// surface `error: description` when present, otherwise a truncated body. No
/// stack traces, no assertion or key material.
async fn mint_error(response: reqwest::Response) -> ActualError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if let Ok(parsed) = serde_json::from_str::<OAuthError>(&body) {
        if let Some(error) = parsed.error {
            let detail = parsed
                .error_description
                .filter(|d| !d.is_empty())
                .map(|d| format!(": {d}"))
                .unwrap_or_default();
            return ActualError::ApiError(format!(
                "Token mint failed (HTTP {status}): {error}{detail}"
            ));
        }
    }
    let truncated: String = body.chars().take(2048).collect();
    ActualError::ApiError(format!("Token mint failed (HTTP {status}): {truncated}"))
}

#[derive(Debug, Deserialize)]
struct OAuthError {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// Exchange a signed assertion for an access token at
/// `${base_url}/api/oauth/token`. `scope`, when non-empty, is sent as the
/// space-delimited `scope` param (a subset the server clamps to the principal's
/// grant); when omitted the server mints the principal's full whitelist.
///
/// `http` must already enforce HTTPS for non-loopback hosts — build it with
/// [`crate::auth::oauth::build_http_client`].
pub async fn mint_token(
    http: &reqwest::Client,
    base_url: &str,
    assertion: &str,
    scope: Option<&str>,
) -> Result<MintedToken, ActualError> {
    let base = base_url.trim_end_matches('/');
    let url = format!("{base}/api/oauth/token");
    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", JWT_BEARER_GRANT_TYPE),
        ("assertion", assertion),
    ];
    if let Some(scope) = scope {
        if !scope.trim().is_empty() {
            form.push(("scope", scope));
        }
    }

    let response = http
        .post(&url)
        .form(&form)
        .send()
        .await
        .map_err(|e| ActualError::ApiError(format!("Token mint request failed: {e}")))?;

    if !response.status().is_success() {
        return Err(mint_error(response).await);
    }

    let token = response
        .json::<TokenResponse>()
        .await
        .map_err(|e| ActualError::ApiError(format!("Failed to parse token response: {e}")))?;

    Ok(MintedToken {
        access_token: token.access_token,
        token_type: token.token_type,
        expires_in: token.expires_in,
        scope: token.scope,
    })
}

/// Write the minted token to `out` for machine capture. In the default mode the
/// output is **only** the raw access token followed by a newline — nothing
/// else, so `TOKEN=$(actual mint-token …)` captures exactly the token. With
/// `json`, the full response is written as a single-line JSON object.
///
/// Status/diagnostics never go through `out`; the command writes those to
/// stderr. This function is the load-bearing "stdout carries only the token"
/// contract, and is unit-tested against an in-memory writer.
pub fn write_token_output(
    out: &mut impl std::io::Write,
    token: &MintedToken,
    json: bool,
) -> std::io::Result<()> {
    if json {
        let value = serde_json::json!({
            "access_token": token.access_token,
            "token_type": token.token_type,
            "expires_in": token.expires_in,
            "scope": token.scope,
        });
        writeln!(out, "{value}")
    } else {
        writeln!(out, "{}", token.access_token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{decode, DecodingKey, Validation};
    use p256::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
    use rsa::pkcs8::{
        EncodePrivateKey as RsaEncodePrivateKey, EncodePublicKey as RsaEncodePublicKey,
    };
    use rsa::{RsaPrivateKey, RsaPublicKey};

    const TEST_UUID: &str = "3f8a1c2e-4b5d-4e6f-8a9b-0c1d2e3f4a5b";
    const TEST_KID: &str = "test-key-1";
    const TEST_AUD: &str = "https://app.example.test";

    /// Real wall-clock seconds — used where the assertion must still be
    /// unexpired when `jsonwebtoken::decode` validates `exp` against `now`.
    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// Decode an assertion's claims WITHOUT checking the signature or the time
    /// claims — for tests that only inspect `jti`/`exp`/`iat` values.
    fn decode_claims_only(assertion: &str, key: &DecodingKey, alg: Algorithm) -> AssertionClaims {
        let mut validation = Validation::new(alg);
        validation.insecure_disable_signature_validation();
        validation.validate_exp = false;
        validation.validate_aud = false;
        decode::<AssertionClaims>(assertion, key, &validation)
            .expect("decode claims")
            .claims
    }

    /// Generate an ephemeral RSA-2048 keypair as (private PKCS#8 PEM, public
    /// SPKI PEM). Generated at runtime so no private key material is committed.
    fn rsa_keypair() -> (Vec<u8>, Vec<u8>) {
        let mut rng = rand::thread_rng();
        let private = RsaPrivateKey::new(&mut rng, 2048).expect("rsa keygen");
        let public = RsaPublicKey::from(&private);
        let priv_pem = RsaEncodePrivateKey::to_pkcs8_pem(&private, LineEnding::LF)
            .expect("rsa priv pem")
            .as_bytes()
            .to_vec();
        let pub_pem = RsaEncodePublicKey::to_public_key_pem(&public, LineEnding::LF)
            .expect("rsa pub pem")
            .into_bytes();
        (priv_pem, pub_pem)
    }

    /// Generate an ephemeral EC P-256 keypair as (private PKCS#8 PEM, public
    /// SPKI PEM). Generated at runtime; nothing committed.
    fn ec_keypair() -> (Vec<u8>, Vec<u8>) {
        let secret = p256::SecretKey::random(&mut rand::thread_rng());
        let priv_pem = secret
            .to_pkcs8_pem(LineEnding::LF)
            .expect("ec priv pem")
            .as_bytes()
            .to_vec();
        let pub_pem = secret
            .public_key()
            .to_public_key_pem(LineEnding::LF)
            .expect("ec pub pem")
            .into_bytes();
        (priv_pem, pub_pem)
    }

    fn params(alg: AssertionAlgorithm) -> AssertionParams<'static> {
        AssertionParams {
            service_account_id: TEST_UUID,
            audience: TEST_AUD,
            kid: TEST_KID,
            algorithm: alg,
            lifetime_seconds: DEFAULT_ASSERTION_LIFETIME_SECONDS,
        }
    }

    /// Verify an assertion the way the server does: pin the algorithm, require
    /// the audience, require `exp`, and allow the same 60s clock-skew leeway.
    /// Returns the decoded claims for the caller to assert on.
    fn server_style_verify(
        assertion: &str,
        alg: AssertionAlgorithm,
        decoding_key: &DecodingKey,
    ) -> AssertionClaims {
        let mut validation = Validation::new(alg.to_jwt());
        validation.set_audience(&[TEST_AUD]);
        validation.set_required_spec_claims(&["exp"]);
        validation.leeway = 60;
        validation.validate_exp = true;
        decode::<AssertionClaims>(assertion, decoding_key, &validation)
            .expect("assertion should verify under server-style validation")
            .claims
    }

    #[test]
    fn rs256_assertion_verifies_and_has_the_right_claims() {
        let (priv_pem, pub_pem) = rsa_keypair();
        let now = now_secs();
        let assertion =
            build_and_sign_assertion(&params(AssertionAlgorithm::Rs256), &priv_pem, now)
                .expect("sign RS256");

        let decoding_key = DecodingKey::from_rsa_pem(&pub_pem).expect("rsa decoding key");
        let claims = server_style_verify(&assertion, AssertionAlgorithm::Rs256, &decoding_key);

        assert_eq!(claims.iss, TEST_UUID);
        assert_eq!(claims.sub, TEST_UUID);
        assert_eq!(claims.iss, claims.sub, "iss must equal sub (RFC 7523)");
        assert_eq!(claims.aud, TEST_AUD);
        assert_eq!(claims.iat, now);
        assert_eq!(claims.exp, now + DEFAULT_ASSERTION_LIFETIME_SECONDS);
        assert!(
            claims.exp - claims.iat <= MAX_ASSERTION_LIFETIME_SECONDS,
            "lifetime must be within the server cap"
        );
        assert!(!claims.jti.is_empty(), "jti must be present");
    }

    #[test]
    fn es256_assertion_verifies_and_has_the_right_claims() {
        let (priv_pem, pub_pem) = ec_keypair();
        let now = now_secs();
        let assertion =
            build_and_sign_assertion(&params(AssertionAlgorithm::Es256), &priv_pem, now)
                .expect("sign ES256");

        let decoding_key = DecodingKey::from_ec_pem(&pub_pem).expect("ec decoding key");
        let claims = server_style_verify(&assertion, AssertionAlgorithm::Es256, &decoding_key);

        assert_eq!(claims.iss, TEST_UUID);
        assert_eq!(claims.sub, TEST_UUID);
        assert_eq!(claims.aud, TEST_AUD);
        assert_eq!(claims.exp - claims.iat, DEFAULT_ASSERTION_LIFETIME_SECONDS);
    }

    #[test]
    fn assertion_header_carries_alg_and_kid() {
        let (priv_pem, _pub) = ec_keypair();
        let assertion =
            build_and_sign_assertion(&params(AssertionAlgorithm::Es256), &priv_pem, 1_700_000_000)
                .expect("sign");
        let header = jsonwebtoken::decode_header(&assertion).expect("decode header");
        assert_eq!(header.alg, Algorithm::ES256);
        assert_eq!(header.kid.as_deref(), Some(TEST_KID));
        assert_eq!(header.typ.as_deref(), Some("JWT"));
    }

    #[test]
    fn each_assertion_gets_a_fresh_unique_jti() {
        let (priv_pem, pub_pem) = ec_keypair();
        let a =
            build_and_sign_assertion(&params(AssertionAlgorithm::Es256), &priv_pem, 1_700_000_000)
                .unwrap();
        let b =
            build_and_sign_assertion(&params(AssertionAlgorithm::Es256), &priv_pem, 1_700_000_000)
                .unwrap();

        let key = DecodingKey::from_ec_pem(&pub_pem).unwrap();
        let ja = decode_claims_only(&a, &key, Algorithm::ES256).jti;
        let jb = decode_claims_only(&b, &key, Algorithm::ES256).jti;
        assert_ne!(
            ja, jb,
            "each assertion must carry a unique jti (anti-replay)"
        );
    }

    #[test]
    fn lifetime_is_clamped_to_the_server_cap() {
        let (priv_pem, pub_pem) = ec_keypair();
        let mut p = params(AssertionAlgorithm::Es256);
        p.lifetime_seconds = 10_000; // absurd; must clamp to 300
        let now = 1_700_000_000;
        let assertion = build_and_sign_assertion(&p, &priv_pem, now).unwrap();

        let key = DecodingKey::from_ec_pem(&pub_pem).unwrap();
        let claims = decode_claims_only(&assertion, &key, Algorithm::ES256);
        assert_eq!(claims.exp - claims.iat, MAX_ASSERTION_LIFETIME_SECONDS);
    }

    #[test]
    fn non_uuid_service_account_id_is_rejected_before_signing() {
        let (priv_pem, _pub) = ec_keypair();
        let mut p = params(AssertionAlgorithm::Es256);
        p.service_account_id = "not-a-uuid";
        let err = build_and_sign_assertion(&p, &priv_pem, 1_700_000_000).unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("UUID")),
            "expected a clean UUID config error, got: {err:?}"
        );
    }

    #[test]
    fn empty_kid_is_rejected() {
        let (priv_pem, _pub) = ec_keypair();
        let mut p = params(AssertionAlgorithm::Es256);
        p.kid = "  ";
        let err = build_and_sign_assertion(&p, &priv_pem, 1_700_000_000).unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(ref m) if m.contains("key id")));
    }

    #[test]
    fn wrong_key_type_for_alg_is_a_clean_error_not_a_panic() {
        // An EC key handed to the RS256 path must fail cleanly.
        let (ec_priv, _pub) = ec_keypair();
        let mut p = params(AssertionAlgorithm::Rs256);
        p.kid = TEST_KID;
        let err = build_and_sign_assertion(&p, &ec_priv, 1_700_000_000).unwrap_err();
        assert!(matches!(err, ActualError::ConfigError(_)));
    }

    #[test]
    fn is_uuid_matches_the_server_contract() {
        assert!(is_uuid("3f8a1c2e-4b5d-4e6f-8a9b-0c1d2e3f4a5b"));
        assert!(is_uuid("3F8A1C2E-4B5D-4E6F-8A9B-0C1D2E3F4A5B")); // case-insensitive
        assert!(!is_uuid("not-a-uuid"));
        assert!(!is_uuid("3f8a1c2e4b5d4e6f8a9b0c1d2e3f4a5b")); // no dashes
        assert!(!is_uuid("3f8a1c2e-4b5d-6e6f-8a9b-0c1d2e3f4a5b")); // version 6
        assert!(!is_uuid("3f8a1c2e-4b5d-4e6f-0a9b-0c1d2e3f4a5b")); // variant 0
        assert!(!is_uuid("")); // empty
    }

    #[test]
    fn algorithm_parse_rejects_symmetric_and_unsigned() {
        assert_eq!(
            AssertionAlgorithm::parse("rs256").unwrap(),
            AssertionAlgorithm::Rs256
        );
        assert_eq!(
            AssertionAlgorithm::parse("ES256").unwrap(),
            AssertionAlgorithm::Es256
        );
        for bad in ["HS256", "hs512", "none", "None", "RS512", "EdDSA", ""] {
            assert!(
                AssertionAlgorithm::parse(bad).is_err(),
                "algorithm '{bad}' must be refused"
            );
        }
    }

    #[test]
    fn algorithm_infers_from_key_pem() {
        let (rsa_priv, _r) = rsa_keypair();
        let (ec_priv, _e) = ec_keypair();
        assert_eq!(
            AssertionAlgorithm::infer_from_pem(&rsa_priv).unwrap(),
            AssertionAlgorithm::Rs256
        );
        assert_eq!(
            AssertionAlgorithm::infer_from_pem(&ec_priv).unwrap(),
            AssertionAlgorithm::Es256
        );
    }

    #[test]
    fn write_token_output_default_is_only_the_token() {
        let token = MintedToken {
            access_token: "the-access-token".to_string(),
            token_type: "Bearer".to_string(),
            expires_in: Some(300),
            scope: Some("adr:query".to_string()),
        };
        let mut buf = Vec::new();
        write_token_output(&mut buf, &token, false).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert_eq!(out, "the-access-token\n", "stdout must be ONLY the token");
    }

    #[test]
    fn write_token_output_json_carries_the_full_response() {
        let token = MintedToken {
            access_token: "the-access-token".to_string(),
            token_type: "Bearer".to_string(),
            expires_in: Some(300),
            scope: Some("adr:query adr:review".to_string()),
        };
        let mut buf = Vec::new();
        write_token_output(&mut buf, &token, true).unwrap();
        let out = String::from_utf8(buf).unwrap();
        let value: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(value["access_token"], "the-access-token");
        assert_eq!(value["token_type"], "Bearer");
        assert_eq!(value["expires_in"], 300);
        assert_eq!(value["scope"], "adr:query adr:review");
    }

    #[test]
    fn minted_token_debug_redacts_the_access_token() {
        let token = MintedToken {
            access_token: "super-secret-token-value".to_string(),
            token_type: "Bearer".to_string(),
            expires_in: None,
            scope: None,
        };
        let debug = format!("{token:?}");
        assert!(
            !debug.contains("super-secret-token-value"),
            "debug leaked the token: {debug}"
        );
        assert!(debug.contains("<redacted>"));
    }

    #[tokio::test]
    async fn mint_token_sends_the_right_wire_shape_and_parses_the_token() {
        let (priv_pem, _pub) = ec_keypair();
        let assertion =
            build_and_sign_assertion(&params(AssertionAlgorithm::Es256), &priv_pem, 1_700_000_000)
                .unwrap();

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/oauth/token")
            .match_header("content-type", "application/x-www-form-urlencoded")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("grant_type".into(), JWT_BEARER_GRANT_TYPE.into()),
                mockito::Matcher::UrlEncoded("assertion".into(), assertion.clone()),
                mockito::Matcher::UrlEncoded("scope".into(), "adr:query adr:review".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"token_type":"Bearer","access_token":"minted-abc","expires_in":300,"scope":"adr:query adr:review"}"#,
            )
            .create_async()
            .await;

        let http = crate::auth::oauth::build_http_client(&server.url()).unwrap();
        let token = mint_token(
            &http,
            &server.url(),
            &assertion,
            Some("adr:query adr:review"),
        )
        .await
        .expect("mint should succeed");

        mock.assert_async().await;
        assert_eq!(token.access_token, "minted-abc");
        assert_eq!(token.token_type, "Bearer");
        assert_eq!(token.expires_in, Some(300));
        assert_eq!(token.scope.as_deref(), Some("adr:query adr:review"));
    }

    #[tokio::test]
    async fn mint_token_omits_scope_when_none_and_defaults_token_type() {
        let (priv_pem, _pub) = ec_keypair();
        let assertion =
            build_and_sign_assertion(&params(AssertionAlgorithm::Es256), &priv_pem, 1_700_000_000)
                .unwrap();

        // No `scope` form field when the caller passes None; the response omits
        // token_type, which must default to Bearer.
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/oauth/token")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("grant_type".into(), JWT_BEARER_GRANT_TYPE.into()),
                mockito::Matcher::UrlEncoded("assertion".into(), assertion.clone()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"minted-noscope"}"#)
            .create_async()
            .await;

        let http = crate::auth::oauth::build_http_client(&server.url()).unwrap();
        let token = mint_token(&http, &server.url(), &assertion, None)
            .await
            .expect("mint should succeed without scope");
        mock.assert_async().await;
        assert_eq!(token.access_token, "minted-noscope");
        assert_eq!(token.token_type, "Bearer", "token_type defaults to Bearer");
        assert_eq!(token.expires_in, None);
        assert_eq!(token.scope, None);
    }

    #[tokio::test]
    async fn mint_token_surfaces_invalid_grant_cleanly() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/api/oauth/token")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"error":"invalid_grant","error_description":"Assertion verification failed"}"#,
            )
            .create_async()
            .await;

        let http = crate::auth::oauth::build_http_client(&server.url()).unwrap();
        let err = mint_token(&http, &server.url(), "bad.assertion.here", None)
            .await
            .unwrap_err();
        match err {
            ActualError::ApiError(msg) => {
                assert!(
                    msg.contains("invalid_grant"),
                    "should surface the server error: {msg}"
                );
                assert!(
                    msg.contains("Assertion verification failed"),
                    "should surface the description: {msg}"
                );
                assert!(!msg.contains("panic"), "no stack traces");
            }
            other => panic!("expected ApiError, got {other:?}"),
        }
    }
}
