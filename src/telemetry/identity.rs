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

/// Produce a stable SHA-256 hash of a normalized repo URL alone.
///
/// Unlike [`hash_repo_identity`], this hash does not include the commit hash,
/// so it remains stable across commits and can be used to count unique repos.
///
/// Normalization: strip trailing `.git`, trailing slashes, lowercase.
/// Returns a 64-character lowercase hex string.
pub fn hash_repo_url(repo_url: &str) -> String {
    let normalized = normalize_repo_url(repo_url);
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Normalize a repo URL for stable hashing across minor formatting differences.
///
/// - Trims whitespace
/// - Lowercases the URL
/// - Strips a trailing `.git` suffix
/// - Strips trailing slashes
fn normalize_repo_url(repo_url: &str) -> String {
    let url = repo_url.trim().to_lowercase();
    let url = url.trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    let url = url.trim_end_matches('/');
    url.to_string()
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

    // ── hash_repo_url tests ──

    /// hash_repo_url returns a 64-character lowercase hex string.
    #[test]
    fn test_hash_repo_url_returns_64_char_hex() {
        let hash = hash_repo_url("https://github.com/org/repo");
        assert_eq!(hash.len(), 64);
        assert!(hash
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    /// Same URL always produces the same hash.
    #[test]
    fn test_hash_repo_url_deterministic() {
        let a = hash_repo_url("https://github.com/org/repo");
        let b = hash_repo_url("https://github.com/org/repo");
        assert_eq!(a, b);
    }

    /// Different URLs produce different hashes.
    #[test]
    fn test_hash_repo_url_different_urls_differ() {
        let a = hash_repo_url("https://github.com/org/repo");
        let b = hash_repo_url("https://github.com/org/other");
        assert_ne!(a, b);
    }

    /// hash_repo_url differs from hash_repo_identity for the same URL.
    #[test]
    fn test_hash_repo_url_differs_from_identity_hash() {
        let url_hash = hash_repo_url("https://github.com/org/repo");
        let identity_hash = hash_repo_identity("https://github.com/org/repo", "abc123");
        assert_ne!(
            url_hash, identity_hash,
            "url-only hash must differ from url+commit hash"
        );
    }

    /// Trailing .git is stripped before hashing, so both forms collide.
    #[test]
    fn test_hash_repo_url_strips_dot_git() {
        let without = hash_repo_url("https://github.com/org/repo");
        let with_git = hash_repo_url("https://github.com/org/repo.git");
        assert_eq!(without, with_git, ".git suffix must be normalized away");
    }

    /// Trailing slashes are stripped before hashing.
    #[test]
    fn test_hash_repo_url_strips_trailing_slashes() {
        let without = hash_repo_url("https://github.com/org/repo");
        let with_slash = hash_repo_url("https://github.com/org/repo/");
        assert_eq!(
            without, with_slash,
            "trailing slash must be normalized away"
        );
    }

    /// URL is lowercased before hashing.
    #[test]
    fn test_hash_repo_url_lowercased() {
        let lower = hash_repo_url("https://github.com/org/repo");
        let upper = hash_repo_url("https://GITHUB.COM/ORG/REPO");
        assert_eq!(lower, upper, "URL must be lowercased before hashing");
    }

    /// Combination of .git suffix and trailing slash normalizes correctly.
    #[test]
    fn test_hash_repo_url_git_and_slash() {
        let base = hash_repo_url("https://github.com/org/repo");
        let messy = hash_repo_url("https://github.com/org/repo.git/");
        assert_eq!(base, messy, ".git and trailing slash must both be stripped");
    }

    /// Empty URL produces a valid 64-char hash.
    #[test]
    fn test_hash_repo_url_empty() {
        let hash = hash_repo_url("");
        assert_eq!(hash.len(), 64);
        assert!(hash
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    // ── normalize_repo_url tests ──

    #[test]
    fn test_normalize_repo_url_trims_whitespace() {
        assert_eq!(
            normalize_repo_url("  https://github.com/org/repo  "),
            "https://github.com/org/repo"
        );
    }

    #[test]
    fn test_normalize_repo_url_lowercases() {
        assert_eq!(
            normalize_repo_url("HTTPS://GITHUB.COM/ORG/REPO"),
            "https://github.com/org/repo"
        );
    }

    #[test]
    fn test_normalize_repo_url_strips_git_suffix() {
        assert_eq!(
            normalize_repo_url("https://github.com/org/repo.git"),
            "https://github.com/org/repo"
        );
    }

    #[test]
    fn test_normalize_repo_url_strips_trailing_slash() {
        assert_eq!(
            normalize_repo_url("https://github.com/org/repo/"),
            "https://github.com/org/repo"
        );
    }

    #[test]
    fn test_normalize_repo_url_strips_git_then_slash() {
        assert_eq!(
            normalize_repo_url("https://github.com/org/repo.git/"),
            "https://github.com/org/repo"
        );
    }
}
