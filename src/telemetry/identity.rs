use sha2::{Digest, Sha256};

/// Produce a deterministic SHA-256 hash from a repo URL and commit hash.
///
/// The two inputs are separated by a null byte (`\0`) before hashing so that
/// concatenation ambiguities are avoided (e.g. "ab"+"cd" differs from "abc"+"d").
/// Returns a 64-character lowercase hex string.
pub fn hash_repo_identity(repo_url: &str, commit_hash: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(repo_url.as_bytes());
    hasher.update(b"\0");
    hasher.update(commit_hash.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known input produces a deterministic, pre-computed hash.
    #[test]
    fn test_known_input_produces_deterministic_hash() {
        let hash = hash_repo_identity("https://github.com/org/repo", "abc123");
        // Pre-computed: printf 'https://github.com/org/repo\x00abc123' | shasum -a 256
        assert_eq!(
            hash, "6d5af53bb0c5de39a3186d5724eb27a48cd1350370173b44beb19e0c98af3f83",
            "hash of known inputs must match pre-computed value"
        );
    }

    /// Calling the function twice with the same inputs yields the same hash.
    #[test]
    fn test_same_inputs_same_hash() {
        let a = hash_repo_identity("https://github.com/org/repo", "abc123");
        let b = hash_repo_identity("https://github.com/org/repo", "abc123");
        assert_eq!(a, b, "identical inputs must produce identical hashes");
    }

    /// Different URLs or different commits produce different hashes.
    #[test]
    fn test_different_inputs_different_hashes() {
        let base = hash_repo_identity("https://github.com/org/repo", "abc123");
        let different_url = hash_repo_identity("https://github.com/org/other", "abc123");
        let different_commit = hash_repo_identity("https://github.com/org/repo", "def456");

        assert_ne!(
            base, different_url,
            "different URLs must produce different hashes"
        );
        assert_ne!(
            base, different_commit,
            "different commits must produce different hashes"
        );

        // Verify separator prevents concatenation ambiguity
        let ab_cd = hash_repo_identity("ab", "cd");
        let abc_d = hash_repo_identity("abc", "d");
        assert_ne!(
            ab_cd, abc_d,
            "separator must prevent concatenation ambiguity"
        );
    }

    /// Output is a 64-character lowercase hex string (SHA-256 digest).
    #[test]
    fn test_hash_is_64_char_lowercase_hex() {
        let hash = hash_repo_identity("https://github.com/org/repo", "abc123");
        assert_eq!(hash.len(), 64, "SHA-256 hex digest must be 64 characters");
        assert!(
            hash.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "hash must contain only lowercase hex characters, got: {hash}"
        );
    }

    /// Empty strings still produce a valid 64-character hex hash.
    #[test]
    fn test_empty_inputs() {
        let hash = hash_repo_identity("", "");
        assert_eq!(
            hash.len(),
            64,
            "empty inputs must still produce a 64-char hash"
        );
        assert!(
            hash.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "hash of empty inputs must be valid lowercase hex"
        );
    }
}
