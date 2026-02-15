use std::collections::HashMap;

use crate::config::Config;

/// Add an ADR ID to the rejection list for a repo. Deduplicates (no-op if already present).
pub fn add_rejection(config: &mut Config, repo_key: &str, adr_id: &str) {
    let map = config.rejected_adrs.get_or_insert_with(HashMap::new);
    let ids = map.entry(repo_key.to_string()).or_default();
    if !ids.contains(&adr_id.to_string()) {
        ids.push(adr_id.to_string());
    }
}

/// Get the rejection list for a repo. Returns empty vec for unknown repos.
pub fn get_rejections(config: &Config, repo_key: &str) -> Vec<String> {
    config
        .rejected_adrs
        .as_ref()
        .and_then(|map| map.get(repo_key))
        .cloned()
        .unwrap_or_default()
}

/// Clear all rejections for a repo.
pub fn clear_rejections(config: &mut Config, repo_key: &str) {
    if let Some(map) = config.rejected_adrs.as_mut() {
        map.remove(repo_key);
        if map.is_empty() {
            config.rejected_adrs = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_and_get_three_rejections() {
        let mut config = Config::default();
        add_rejection(&mut config, "abc123", "adr-001");
        add_rejection(&mut config, "abc123", "adr-002");
        add_rejection(&mut config, "abc123", "adr-003");

        let rejections = get_rejections(&config, "abc123");
        assert_eq!(rejections.len(), 3);
        assert!(rejections.contains(&"adr-001".to_string()));
        assert!(rejections.contains(&"adr-002".to_string()));
        assert!(rejections.contains(&"adr-003".to_string()));
    }

    #[test]
    fn test_add_duplicate_rejection_no_duplicates() {
        let mut config = Config::default();
        add_rejection(&mut config, "abc123", "adr-001");
        add_rejection(&mut config, "abc123", "adr-001");

        let rejections = get_rejections(&config, "abc123");
        assert_eq!(rejections.len(), 1);
    }

    #[test]
    fn test_clear_rejections() {
        let mut config = Config::default();
        add_rejection(&mut config, "abc123", "adr-001");
        add_rejection(&mut config, "abc123", "adr-002");

        clear_rejections(&mut config, "abc123");

        let rejections = get_rejections(&config, "abc123");
        assert!(rejections.is_empty());
        assert!(config.rejected_adrs.is_none());
    }

    #[test]
    fn test_rejections_persist_through_save_load_cycle() {
        let mut config = Config::default();
        add_rejection(&mut config, "abc123", "adr-001");
        add_rejection(&mut config, "abc123", "adr-002");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");

        crate::config::paths::save_to(&config, &path).unwrap();
        let loaded = crate::config::paths::load_from(&path).unwrap();

        let rejections = get_rejections(&loaded, "abc123");
        assert_eq!(rejections.len(), 2);
        assert!(rejections.contains(&"adr-001".to_string()));
        assert!(rejections.contains(&"adr-002".to_string()));
    }

    #[test]
    fn test_get_rejections_unknown_repo_returns_empty() {
        let config = Config::default();
        let rejections = get_rejections(&config, "unknown_repo");
        assert!(rejections.is_empty());
    }
}
