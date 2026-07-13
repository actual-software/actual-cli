//! Per-repo remembered scope for the `actual advisor` command.
//!
//! Mirrors [`crate::config::rejections`]: three free functions over a
//! `HashMap<repo_key, StickyScope>` stored on [`Config`]. The repo key is the
//! same SHA-256-of-origin-URL used elsewhere (see `sync::cache::compute_repo_key`),
//! so a repo has one stable key across every per-repo config feature.

use std::collections::HashMap;

use crate::config::types::StickyScope;
use crate::config::Config;

/// Remember an advisor scope for a repo, replacing any prior pin.
pub fn set_scope(config: &mut Config, repo_key: &str, scope: StickyScope) {
    config
        .sticky_repo_scope
        .get_or_insert_with(HashMap::new)
        .insert(repo_key.to_string(), scope);
}

/// Get the remembered advisor scope for a repo, if one is pinned.
pub fn get_scope(config: &Config, repo_key: &str) -> Option<StickyScope> {
    config
        .sticky_repo_scope
        .as_ref()
        .and_then(|map| map.get(repo_key))
        .cloned()
}

/// Forget the remembered advisor scope for a repo (revert to auto-detect).
pub fn clear_scope(config: &mut Config, repo_key: &str) {
    if let Some(map) = config.sticky_repo_scope.as_mut() {
        map.remove(repo_key);
        if map.is_empty() {
            config.sticky_repo_scope = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_and_get_repo_scope() {
        let mut config = Config::default();
        set_scope(
            &mut config,
            "key-a",
            StickyScope::repo("id-1", Some("owner/repo".to_string())),
        );

        let scope = get_scope(&config, "key-a").unwrap();
        assert_eq!(scope.repo_unique_id.as_deref(), Some("id-1"));
        assert_eq!(scope.qualified_name.as_deref(), Some("owner/repo"));
    }

    #[test]
    fn test_set_org_level_scope() {
        let mut config = Config::default();
        set_scope(&mut config, "key-a", StickyScope::org_level());

        let scope = get_scope(&config, "key-a").unwrap();
        assert!(scope.repo_unique_id.is_none());
        assert!(scope.qualified_name.is_none());
    }

    #[test]
    fn test_set_scope_replaces_prior_pin() {
        let mut config = Config::default();
        set_scope(&mut config, "key-a", StickyScope::repo("id-1", None));
        set_scope(
            &mut config,
            "key-a",
            StickyScope::repo("id-2", Some("o/r".to_string())),
        );

        let scope = get_scope(&config, "key-a").unwrap();
        assert_eq!(scope.repo_unique_id.as_deref(), Some("id-2"));
        assert_eq!(scope.qualified_name.as_deref(), Some("o/r"));
    }

    #[test]
    fn test_get_scope_unknown_repo_returns_none() {
        let config = Config::default();
        assert!(get_scope(&config, "missing").is_none());
    }

    #[test]
    fn test_clear_scope() {
        let mut config = Config::default();
        set_scope(&mut config, "key-a", StickyScope::repo("id-1", None));

        clear_scope(&mut config, "key-a");

        assert!(get_scope(&config, "key-a").is_none());
        assert!(config.sticky_repo_scope.is_none());
    }

    #[test]
    fn test_clear_scope_leaves_other_repos() {
        let mut config = Config::default();
        set_scope(&mut config, "key-a", StickyScope::repo("id-1", None));
        set_scope(&mut config, "key-b", StickyScope::org_level());

        clear_scope(&mut config, "key-a");

        assert!(get_scope(&config, "key-a").is_none());
        assert_eq!(get_scope(&config, "key-b"), Some(StickyScope::org_level()));
        // Map should still exist because key-b remains.
        assert!(config.sticky_repo_scope.is_some());
    }

    #[test]
    fn test_clear_scope_noop_when_none() {
        let mut config = Config::default();
        assert!(config.sticky_repo_scope.is_none());

        clear_scope(&mut config, "nonexistent");

        assert!(config.sticky_repo_scope.is_none());
    }

    #[test]
    fn test_scope_persists_through_save_load_cycle() {
        let mut config = Config::default();
        set_scope(
            &mut config,
            "key-a",
            StickyScope::repo("id-1", Some("owner/repo".to_string())),
        );
        set_scope(&mut config, "key-b", StickyScope::org_level());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        crate::config::paths::save_to(&config, &path).unwrap();
        let loaded = crate::config::paths::load_from(&path).unwrap();

        let a = get_scope(&loaded, "key-a").unwrap();
        assert_eq!(a.repo_unique_id.as_deref(), Some("id-1"));
        assert_eq!(a.qualified_name.as_deref(), Some("owner/repo"));
        assert_eq!(get_scope(&loaded, "key-b"), Some(StickyScope::org_level()));
        // Debug/Clone round-trip so both derives are exercised on this file.
        assert!(format!("{a:?}").contains("id-1"));
        let _cloned = a.clone();
    }

    #[test]
    fn test_scope_keys_are_independent() {
        let key_a = "a".repeat(64);
        let key_b = "b".repeat(64);
        assert_ne!(key_a, key_b);

        let mut config = Config::default();
        set_scope(&mut config, &key_a, StickyScope::repo("id-a", None));
        set_scope(&mut config, &key_b, StickyScope::repo("id-b", None));

        assert_eq!(
            get_scope(&config, &key_a)
                .unwrap()
                .repo_unique_id
                .as_deref(),
            Some("id-a")
        );
        assert_eq!(
            get_scope(&config, &key_b)
                .unwrap()
                .repo_unique_id
                .as_deref(),
            Some("id-b")
        );
    }
}
