//! PKCE (RFC 7636) support for the platform-auth browser OAuth flow.
//!
//! We always use the `S256` challenge method: the verifier is high-entropy
//! random bytes, base64url-encoded; the challenge is the base64url-encoded
//! SHA-256 of the verifier's ASCII bytes.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};

/// A PKCE verifier/challenge pair for one authorization request.
#[derive(Clone)]
pub struct PkcePair {
    /// The secret verifier, sent only on the back-channel token exchange.
    pub verifier: String,
    /// The challenge derived from the verifier, sent on the front-channel
    /// `/authorize` request.
    pub challenge: String,
}

impl PkcePair {
    /// The challenge method this pair uses (always `S256`).
    pub const METHOD: &'static str = "S256";

    /// Generate a fresh pair with a 32-byte (256-bit) random verifier.
    ///
    /// Randomness comes from two v4 UUIDs (CSPRNG-backed via `getrandom`),
    /// concatenated to 32 bytes and base64url-encoded → a 43-char verifier,
    /// within RFC 7636's 43–128 char range.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        let verifier = URL_SAFE_NO_PAD.encode(bytes);
        Self::from_verifier(verifier)
    }

    /// Construct a pair from an existing verifier, computing its `S256`
    /// challenge. Exposed for testing against the RFC 7636 vector.
    pub fn from_verifier(verifier: impl Into<String>) -> Self {
        let verifier = verifier.into();
        let digest = Sha256::digest(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(digest);
        Self {
            verifier,
            challenge,
        }
    }
}

/// Generate a high-entropy, URL-safe `state` value for CSRF protection on the
/// authorization request.
pub fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 Appendix B worked example: a fixed verifier must produce the
    /// published S256 challenge.
    #[test]
    fn test_rfc7636_vector() {
        let pair = PkcePair::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(
            pair.challenge,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn test_generate_verifier_length_and_charset() {
        let pair = PkcePair::generate();
        // 32 bytes base64url-no-pad => 43 chars, within RFC 7636's 43..=128.
        assert_eq!(pair.verifier.len(), 43);
        assert!(pair.verifier.len() >= 43 && pair.verifier.len() <= 128);
        // Unreserved / URL-safe-no-pad alphabet only.
        assert!(
            pair.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "verifier must be URL-safe: {}",
            pair.verifier
        );
    }

    #[test]
    fn test_generate_is_random() {
        let a = PkcePair::generate();
        let b = PkcePair::generate();
        assert_ne!(a.verifier, b.verifier, "verifiers must be unique");
        assert_ne!(a.challenge, b.challenge, "challenges must differ");
    }

    #[test]
    fn test_challenge_matches_recomputation() {
        let pair = PkcePair::generate();
        let recomputed = URL_SAFE_NO_PAD.encode(Sha256::digest(pair.verifier.as_bytes()));
        assert_eq!(pair.challenge, recomputed);
    }

    #[test]
    fn test_challenge_is_url_safe() {
        let pair = PkcePair::generate();
        assert!(
            pair.challenge
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "challenge must be URL-safe: {}",
            pair.challenge
        );
    }

    #[test]
    fn test_method_is_s256() {
        assert_eq!(PkcePair::METHOD, "S256");
    }

    #[test]
    fn test_generate_state_unique_and_url_safe() {
        let s1 = generate_state();
        let s2 = generate_state();
        assert_ne!(s1, s2);
        assert_eq!(s1.len(), 43);
        assert!(s1
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }
}
