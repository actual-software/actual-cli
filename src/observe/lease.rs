use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};

use crate::config::paths;
use crate::error::ActualError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseScope {
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    #[serde(default)]
    pub protected_paths: Vec<String>,
    #[serde(default)]
    pub forbidden_patterns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureLease {
    pub lease_id: String,
    pub session_id: String,
    pub issued_at: String,
    pub ttl_seconds: u64,
    pub scope: LeaseScope,
    #[serde(default)]
    pub escalation_triggers: Vec<String>,
}

impl ArchitectureLease {
    fn is_expired(&self) -> bool {
        let issued = match DateTime::parse_from_rfc3339(&self.issued_at) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => return true,
        };
        let expires = issued + ChronoDuration::seconds(self.ttl_seconds as i64);
        Utc::now() > expires
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseDecision {
    Allow,
    Deny(String),
    Escalate(String),
}

pub struct LeaseStore {
    dir: PathBuf,
}

impl LeaseStore {
    pub fn new() -> Result<Self, ActualError> {
        let dir = paths::config_dir()?.join("sessions");
        Ok(Self { dir })
    }

    #[cfg(test)]
    pub fn with_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn load(&self, session_id: &str) -> Option<ArchitectureLease> {
        let path = self.lease_path(session_id);
        let content = fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    pub fn store(&self, session_id: &str, lease: &ArchitectureLease) -> Result<(), ActualError> {
        fs::create_dir_all(&self.dir).map_err(|e| {
            ActualError::ConfigError(format!("failed to create sessions dir: {e}"))
        })?;

        let path = self.lease_path(session_id);
        let json = serde_json::to_string_pretty(lease)
            .map_err(|e| ActualError::ConfigError(format!("failed to serialize lease: {e}")))?;

        fs::write(&path, json).map_err(|e| {
            ActualError::ConfigError(format!("failed to write lease {}: {e}", path.display()))
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&path, perms).ok();
        }

        Ok(())
    }

    pub fn invalidate(&self, session_id: &str) {
        let path = self.lease_path(session_id);
        fs::remove_file(&path).ok();
    }

    fn lease_path(&self, session_id: &str) -> PathBuf {
        self.dir.join(format!("{}.lease.json", safe_id(session_id)))
    }
}

fn safe_id(session_id: &str) -> String {
    session_id
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

pub struct LeaseChecker;

impl LeaseChecker {
    pub fn check(
        lease: &ArchitectureLease,
        file_path: Option<&str>,
        tool_input_text: Option<&str>,
    ) -> LeaseDecision {
        if lease.is_expired() {
            return LeaseDecision::Escalate("Architecture lease expired".to_string());
        }

        if let Some(path) = file_path {
            for protected in &lease.scope.protected_paths {
                if glob_match(protected, path) {
                    return LeaseDecision::Escalate(format!(
                        "Path '{}' is in protected scope (matches '{}')",
                        path, protected
                    ));
                }
            }

            let in_scope = lease.scope.allowed_paths.is_empty()
                || lease.scope.allowed_paths.iter().any(|p| glob_match(p, path));

            if !in_scope {
                return LeaseDecision::Escalate(format!(
                    "Path '{}' is outside lease scope",
                    path
                ));
            }
        }

        if let Some(text) = tool_input_text {
            for pattern in &lease.scope.forbidden_patterns {
                if let Ok(re) = regex::Regex::new(pattern) {
                    if re.is_match(text) {
                        return LeaseDecision::Deny(format!(
                            "Forbidden pattern '{}' detected in tool input",
                            pattern
                        ));
                    }
                }
            }
        }

        LeaseDecision::Allow
    }
}

fn glob_match(pattern: &str, path: &str) -> bool {
    glob::Pattern::new(pattern)
        .map(|p| {
            let opts = glob::MatchOptions {
                require_literal_separator: true,
                ..Default::default()
            };
            p.matches_with(path, opts)
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_lease(ttl: u64) -> ArchitectureLease {
        ArchitectureLease {
            lease_id: "test-lease-id".to_string(),
            session_id: "test-session".to_string(),
            issued_at: Utc::now().to_rfc3339(),
            ttl_seconds: ttl,
            scope: LeaseScope {
                allowed_paths: vec!["apps/api-service/src/**".to_string()],
                protected_paths: vec![
                    "supabase/migrations/**".to_string(),
                    ".github/**".to_string(),
                ],
                forbidden_patterns: vec![
                    r"createClient\(".to_string(),
                    r"supabase\.from\(".to_string(),
                ],
            },
            escalation_triggers: vec!["new_dependency".to_string()],
        }
    }

    fn expired_lease() -> ArchitectureLease {
        ArchitectureLease {
            lease_id: "expired-lease".to_string(),
            session_id: "test-session".to_string(),
            issued_at: "2020-01-01T00:00:00Z".to_string(),
            ttl_seconds: 1,
            scope: LeaseScope {
                allowed_paths: vec!["**".to_string()],
                protected_paths: vec![],
                forbidden_patterns: vec![],
            },
            escalation_triggers: vec![],
        }
    }

    // ── LeaseStore ──

    #[test]
    fn test_store_and_load() {
        let dir = tempdir().unwrap();
        let store = LeaseStore::with_dir(dir.path().to_path_buf());
        let lease = test_lease(600);

        store.store("sess-1", &lease).unwrap();
        let loaded = store.load("sess-1");
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.lease_id, "test-lease-id");
        assert_eq!(loaded.scope.allowed_paths.len(), 1);
    }

    #[test]
    fn test_load_missing_returns_none() {
        let dir = tempdir().unwrap();
        let store = LeaseStore::with_dir(dir.path().to_path_buf());
        assert!(store.load("nonexistent").is_none());
    }

    #[test]
    fn test_invalidate_removes_file() {
        let dir = tempdir().unwrap();
        let store = LeaseStore::with_dir(dir.path().to_path_buf());
        let lease = test_lease(600);

        store.store("sess-1", &lease).unwrap();
        assert!(store.load("sess-1").is_some());

        store.invalidate("sess-1");
        assert!(store.load("sess-1").is_none());
    }

    #[test]
    fn test_invalidate_nonexistent_is_noop() {
        let dir = tempdir().unwrap();
        let store = LeaseStore::with_dir(dir.path().to_path_buf());
        store.invalidate("nonexistent");
    }

    #[cfg(unix)]
    #[test]
    fn test_lease_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let store = LeaseStore::with_dir(dir.path().to_path_buf());
        let lease = test_lease(600);

        store.store("sess-1", &lease).unwrap();

        let path = dir.path().join("sess-1.lease.json");
        let perms = fs::metadata(&path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);
    }

    // ── LeaseChecker ──

    #[test]
    fn test_allow_path_in_scope() {
        let lease = test_lease(600);
        let decision = LeaseChecker::check(
            &lease,
            Some("apps/api-service/src/handlers/foo.ts"),
            None,
        );
        assert_eq!(decision, LeaseDecision::Allow);
    }

    #[test]
    fn test_escalate_path_outside_scope() {
        let lease = test_lease(600);
        let decision = LeaseChecker::check(
            &lease,
            Some("packages/other-package/src/index.ts"),
            None,
        );
        assert!(matches!(decision, LeaseDecision::Escalate(_)));
    }

    #[test]
    fn test_escalate_protected_path() {
        let lease = test_lease(600);
        let decision = LeaseChecker::check(
            &lease,
            Some("supabase/migrations/20240101_add_table.sql"),
            None,
        );
        assert!(matches!(decision, LeaseDecision::Escalate(_)));
    }

    #[test]
    fn test_escalate_github_protected_path() {
        let lease = test_lease(600);
        let decision = LeaseChecker::check(
            &lease,
            Some(".github/workflows/ci.yml"),
            None,
        );
        assert!(matches!(decision, LeaseDecision::Escalate(_)));
    }

    #[test]
    fn test_deny_forbidden_pattern() {
        let lease = test_lease(600);
        let decision = LeaseChecker::check(
            &lease,
            Some("apps/api-service/src/handlers/foo.ts"),
            Some("const client = createClient()"),
        );
        assert!(matches!(decision, LeaseDecision::Deny(_)));
    }

    #[test]
    fn test_deny_supabase_from_pattern() {
        let lease = test_lease(600);
        let decision = LeaseChecker::check(
            &lease,
            Some("apps/api-service/src/handlers/foo.ts"),
            Some("supabase.from('users').select()"),
        );
        assert!(matches!(decision, LeaseDecision::Deny(_)));
    }

    #[test]
    fn test_allow_no_forbidden_match() {
        let lease = test_lease(600);
        let decision = LeaseChecker::check(
            &lease,
            Some("apps/api-service/src/handlers/foo.ts"),
            Some("const result = await handler()"),
        );
        assert_eq!(decision, LeaseDecision::Allow);
    }

    #[test]
    fn test_escalate_expired_lease() {
        let lease = expired_lease();
        let decision = LeaseChecker::check(
            &lease,
            Some("anything.ts"),
            None,
        );
        assert!(matches!(decision, LeaseDecision::Escalate(_)));
    }

    #[test]
    fn test_allow_no_path_no_input() {
        let lease = test_lease(600);
        let decision = LeaseChecker::check(&lease, None, None);
        assert_eq!(decision, LeaseDecision::Allow);
    }

    #[test]
    fn test_allow_empty_scope_allows_all_paths() {
        let mut lease = test_lease(600);
        lease.scope.allowed_paths = vec![];
        let decision = LeaseChecker::check(
            &lease,
            Some("any/random/path.ts"),
            None,
        );
        assert_eq!(decision, LeaseDecision::Allow);
    }

    #[test]
    fn test_protected_takes_priority_over_allowed() {
        let mut lease = test_lease(600);
        lease.scope.allowed_paths = vec!["**".to_string()];
        lease.scope.protected_paths = vec!["supabase/**".to_string()];
        let decision = LeaseChecker::check(
            &lease,
            Some("supabase/migrations/001.sql"),
            None,
        );
        assert!(matches!(decision, LeaseDecision::Escalate(_)));
    }

    // ── glob_match ──

    #[test]
    fn test_glob_match_wildcard() {
        assert!(glob_match("src/**", "src/foo/bar.ts"));
        assert!(glob_match("src/*.ts", "src/index.ts"));
        assert!(!glob_match("src/*.ts", "src/nested/index.ts"));
    }

    #[test]
    fn test_glob_match_double_star() {
        assert!(glob_match("apps/**/src/**", "apps/api/src/handler.ts"));
        assert!(!glob_match("apps/**/src/**", "packages/foo/bar.ts"));
    }
}
