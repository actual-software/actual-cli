use sha2::{Digest, Sha256};

use crate::auth::store::StoredCredentials;

// NOTE (legacy fallback org namespace): earlier fallback org ids in our database
// were generated as UUIDv5 over the namespace
// `9b2e8f1a-3c4d-5e6f-8a9b-0c1d2e3f4a5b`. That value is kept here only as a
// historical record of what produced that existing data — we do NOT reuse it.
// Going forward, `fallback_org_id` derives the id from our own SHA-256 hash (see
// below), so new anonymous runs never fall back to the old namespace scheme.

/// Domain-separated, **unsalted** SHA-256 of a raw identifier, as 64-char
/// lowercase hex. `domain` is a fixed, non-secret label (`"user"`, `"org"`,
/// `"repo"`) that namespaces the hash so the three id spaces cannot collide or
/// share one rainbow table.
///
/// This is deliberately *not* salted: the value must be reproducible byte-for-byte
/// by our backend from Supabase's own raw rows so a `hash -> raw` lookup can be
/// built server-side. Secrecy against enumeration is added by a server-side
/// pepper (HMAC) in api-service, never on this (open-source) side. The raw id
/// itself is never sent — only this hash leaves the machine.
pub fn hash_identifier(domain: &str, id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update(b":");
    hasher.update(id.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Extract an `owner/repo` slug from a git remote URL: lowercased, `.git` and
/// trailing slashes stripped, scheme and host dropped. Handles both
/// `https://host/owner/repo` and scp-style `git@host:owner/repo`. Falls back to
/// the normalized full URL when an `owner/repo` shape can't be found.
pub fn repo_slug(repo_url: &str) -> String {
    let normalized = normalize_repo_url(repo_url);
    // Drop any `scheme://` prefix, then turn the first `:` (scp-style separator)
    // into a `/` so both URL forms split the same way.
    let without_scheme = normalized
        .split_once("://")
        .map(|(_, rest)| rest.to_string())
        .unwrap_or_else(|| normalized.clone());
    let unified = without_scheme.replacen(':', "/", 1);
    let segments: Vec<&str> = unified.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() >= 2 {
        format!(
            "{}/{}",
            segments[segments.len() - 2],
            segments[segments.len() - 1]
        )
    } else {
        normalized
    }
}

/// A deterministic fallback org id (a UUID) derived from a repo slug, for the
/// unauthenticated case where no real `organization_id` is known.
///
/// Derived from our own `SHA-256("slug:"+slug)` (via [`hash_identifier`]) with the
/// version/variant bits stamped to UUID v5 form, so the same repo always maps to
/// the same fallback org across anonymous installs. Depends only on the slug
/// (never on `distinct_id`), so two developers on the same repo share one fallback
/// org id. This is the current scheme; the older UUIDv5-namespace scheme is not
/// reused (see the legacy note above).
pub fn fallback_org_id(slug: &str) -> String {
    let digest = hash_identifier("slug", slug);
    let mut bytes = [0u8; 16];
    for (i, b) in bytes.iter_mut().enumerate() {
        // `hash_identifier` always returns 64 hex chars, so this range is valid.
        *b = u8::from_str_radix(&digest[i * 2..i * 2 + 2], 16).unwrap_or(0);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x50; // version 5 nibble
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant
    uuid::Uuid::from_bytes(bytes).to_string()
}

/// The identity + timestamp envelope stamped onto every published governance /
/// scope event so metrics can be sliced by user, org, and repo.
///
/// Authenticated runs carry domain-separated hashes of the real ids (never the
/// raw ids). Unauthenticated runs carry only a synthetic `org_id` derived from
/// the repo slug; the existing anonymous keys (`distinct_id`, `repo_hash`,
/// `repo_url_hash`) continue to ride alongside on the event.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IdentityEnvelope {
    /// `SHA-256("user:"+subject)` when authenticated.
    pub user_id_hash: Option<String>,
    /// `SHA-256("org:"+organization_id)` when authenticated.
    pub org_id_hash: Option<String>,
    /// `SHA-256("repo:"+repo_unique_id)` when the connected repo id is known.
    pub repo_id_hash: Option<String>,
    /// Synthetic fallback org id (UUID from the repo slug), set only when not
    /// authenticated.
    pub org_id: Option<String>,
    /// RFC3339 UTC timestamp for this event.
    pub datetime_utc: String,
}

impl IdentityEnvelope {
    /// The current time as an RFC3339 UTC string.
    pub fn now_utc() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    /// Build the envelope from whatever identity is available locally.
    ///
    /// * `creds` — stored OAuth credentials, if the user is logged in. When
    ///   present, real ids are hashed (subject, falling back to member id, for
    ///   the user; `organization_id` for the org).
    /// * `repo_unique_id` — the connected-repo id if already known locally (e.g.
    ///   from a config sticky-scope pin). Never fetched over the network here.
    /// * `repo_url` — the git remote URL, used only to derive the fallback org id
    ///   when unauthenticated.
    ///
    /// Reads nothing from disk or the network itself; callers pass in what they
    /// have, so this stays pure and cheap on the hook path.
    pub fn build(
        creds: Option<&StoredCredentials>,
        repo_unique_id: Option<&str>,
        repo_url: Option<&str>,
    ) -> Self {
        let mut envelope = Self {
            datetime_utc: Self::now_utc(),
            ..Default::default()
        };

        if let Some(creds) = creds {
            let user = creds
                .subject
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(creds.member_id.as_str());
            if !user.is_empty() {
                envelope.user_id_hash = Some(hash_identifier("user", user));
            }
            if !creds.organization_id.is_empty() {
                envelope.org_id_hash = Some(hash_identifier("org", &creds.organization_id));
            }
        } else if let Some(url) = repo_url.filter(|u| !u.is_empty()) {
            envelope.org_id = Some(fallback_org_id(&repo_slug(url)));
        }

        if let Some(repo_unique_id) = repo_unique_id.filter(|r| !r.is_empty()) {
            envelope.repo_id_hash = Some(hash_identifier("repo", repo_unique_id));
        }

        envelope
    }

    /// Resolve the envelope from local state alone: stored OAuth credentials and
    /// a config sticky-scope pin (for the connected-repo id). `repo_url` is the
    /// git remote URL, used both to look up the sticky pin and to derive the
    /// anonymous fallback org id. Never touches the network.
    ///
    /// The single local-only identity lookup shared by every plan-governance /
    /// scope emitter, so no event type can bypass the envelope.
    pub fn resolve(cfg: &crate::config::types::Config, repo_url: &str) -> Self {
        let creds = crate::auth::store::load().ok().flatten();
        // Match `sync::cache::compute_repo_key`'s sticky key (raw SHA-256 of the
        // origin URL) without re-spawning git: we already have the URL in hand.
        let repo_unique_id = if repo_url.is_empty() {
            None
        } else {
            let mut hasher = Sha256::new();
            hasher.update(repo_url.as_bytes());
            let repo_key = format!("{:x}", hasher.finalize());
            crate::config::sticky::get_scope(cfg, &repo_key).and_then(|s| s.repo_unique_id)
        };
        Self::build(creds.as_ref(), repo_unique_id.as_deref(), Some(repo_url))
    }

    /// Stamp this envelope's identity + timestamp fields onto an event's
    /// properties. Every plan-governance / scope event goes through here so the
    /// envelope is applied identically and cannot be omitted. Raw identifiers are
    /// never touched — only the hashes / synthetic id / timestamp this envelope
    /// already holds.
    pub fn apply_to(&self, props: &mut crate::api::types::PlanGovernanceEventProperties) {
        props.user_id_hash = self.user_id_hash.clone();
        props.org_id_hash = self.org_id_hash.clone();
        props.repo_id_hash = self.repo_id_hash.clone();
        props.org_id = self.org_id.clone();
        props.datetime_utc = Some(self.datetime_utc.clone());
    }
}

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

    // ── hash_identifier tests ──

    #[test]
    fn test_hash_identifier_is_deterministic_and_64_hex() {
        let a = hash_identifier("user", "11111111-1111-1111-1111-111111111111");
        let b = hash_identifier("user", "11111111-1111-1111-1111-111111111111");
        assert_eq!(a, b, "same domain+id must hash identically");
        assert_eq!(a.len(), 64);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn test_hash_identifier_domain_separates() {
        // The same raw id under different domains must not collide.
        let id = "same-id";
        assert_ne!(
            hash_identifier("user", id),
            hash_identifier("org", id),
            "domain label must namespace the hash"
        );
        assert_ne!(hash_identifier("org", id), hash_identifier("repo", id));
    }

    #[test]
    fn test_hash_identifier_matches_prefixed_sha256() {
        // Documents the exact wire contract Supabase must reproduce:
        // hash_identifier("user", X) == SHA256("user:" + X).
        let expected = {
            let mut h = Sha256::new();
            h.update(b"user:");
            h.update(b"abc");
            format!("{:x}", h.finalize())
        };
        assert_eq!(hash_identifier("user", "abc"), expected);
    }

    // ── repo_slug tests ──

    #[test]
    fn test_repo_slug_https() {
        assert_eq!(repo_slug("https://github.com/Org/Repo.git"), "org/repo");
    }

    #[test]
    fn test_repo_slug_scp_style() {
        assert_eq!(repo_slug("git@github.com:Org/Repo.git"), "org/repo");
    }

    #[test]
    fn test_repo_slug_trailing_slash_and_case() {
        assert_eq!(repo_slug("https://GITHUB.com/org/repo/"), "org/repo");
    }

    #[test]
    fn test_repo_slug_falls_back_to_normalized_when_no_owner_repo() {
        // Fewer than two path segments (no owner/repo shape) -> the normalized
        // URL is returned as-is.
        assert_eq!(repo_slug("singleword"), "singleword");
        assert_eq!(repo_slug(""), "");
    }

    // ── fallback_org_id tests ──

    #[test]
    fn test_fallback_org_id_is_stable_uuid_for_same_slug() {
        let a = fallback_org_id("org/repo");
        let b = fallback_org_id("org/repo");
        assert_eq!(a, b, "same slug must yield the same fallback org id");
        assert!(
            uuid::Uuid::parse_str(&a).is_ok(),
            "fallback org id must be a valid UUID, got {a}"
        );
    }

    #[test]
    fn test_fallback_org_id_differs_by_slug() {
        assert_ne!(fallback_org_id("org/repo"), fallback_org_id("org/other"));
    }

    #[test]
    fn test_fallback_org_id_is_version_5() {
        let id = fallback_org_id("org/repo");
        let uuid = uuid::Uuid::parse_str(&id).unwrap();
        assert_eq!(uuid.get_version_num(), 5, "must stamp UUID version 5 bits");
    }

    // ── IdentityEnvelope tests ──

    fn creds(subject: Option<&str>, member: &str, org: &str) -> StoredCredentials {
        StoredCredentials {
            access_token: "tok".to_string(),
            refresh_token: "ref".to_string(),
            token_type: "Bearer".to_string(),
            expires_at: None,
            scope: None,
            organization_id: org.to_string(),
            member_id: member.to_string(),
            email: None,
            subject: subject.map(|s| s.to_string()),
            auth_url: None,
        }
    }

    #[test]
    fn test_envelope_authenticated_hashes_user_and_org_no_raw() {
        let c = creds(Some("user-sub-1"), "member-1", "org-1");
        let env = IdentityEnvelope::build(Some(&c), Some("repo-uid-1"), Some("https://x/y/z"));

        assert_eq!(
            env.user_id_hash,
            Some(hash_identifier("user", "user-sub-1"))
        );
        assert_eq!(env.org_id_hash, Some(hash_identifier("org", "org-1")));
        assert_eq!(
            env.repo_id_hash,
            Some(hash_identifier("repo", "repo-uid-1"))
        );
        // Authenticated runs carry no synthetic fallback org id.
        assert_eq!(env.org_id, None);
        assert!(!env.datetime_utc.is_empty());

        // No raw identifier appears anywhere in the envelope.
        for field in [&env.user_id_hash, &env.org_id_hash, &env.repo_id_hash] {
            let v = field.as_deref().unwrap();
            assert!(!v.contains("user-sub-1"));
            assert!(!v.contains("org-1"));
            assert!(!v.contains("repo-uid-1"));
        }
    }

    #[test]
    fn test_envelope_authenticated_falls_back_to_member_id_when_no_subject() {
        let c = creds(None, "member-9", "org-9");
        let env = IdentityEnvelope::build(Some(&c), None, None);
        assert_eq!(env.user_id_hash, Some(hash_identifier("user", "member-9")));
        assert_eq!(env.repo_id_hash, None);
    }

    #[test]
    fn test_envelope_fallback_sets_synthetic_org_from_slug_only() {
        let env = IdentityEnvelope::build(None, None, Some("https://github.com/org/repo.git"));
        assert_eq!(env.user_id_hash, None);
        assert_eq!(env.org_id_hash, None);
        assert_eq!(env.org_id, Some(fallback_org_id("org/repo")));
        assert!(!env.datetime_utc.is_empty());
    }

    #[test]
    fn test_envelope_fallback_sets_repo_hash_when_repo_id_known() {
        // repo_unique_id may be known from a sticky pin even without login.
        let env = IdentityEnvelope::build(None, Some("repo-uid-2"), Some("https://h/o/r"));
        assert_eq!(
            env.repo_id_hash,
            Some(hash_identifier("repo", "repo-uid-2"))
        );
        assert_eq!(env.org_id, Some(fallback_org_id("o/r")));
    }
}
