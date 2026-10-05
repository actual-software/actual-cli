//! Per-repo `rules select` score floor.
//!
//! Mirrors [`crate::config::sticky`]: free functions over a
//! `HashMap<repo_key, f64>` stored on [`Config`], keyed by the same
//! SHA-256-of-origin-URL as every other per-repo config feature. Scores are
//! sums of weighted coverages, so a useful floor depends on how deep the
//! rule set's verify paths run; a user-wide value cannot be right everywhere.

use std::collections::HashMap;

use crate::config::types::validate_rules_min_score;
use crate::config::Config;
use crate::error::ActualError;

/// Pin a floor for a repo, replacing any prior one. Rejects invalid values.
pub fn set_floor(config: &mut Config, repo_key: &str, floor: f64) -> Result<(), ActualError> {
    let floor = validate_rules_min_score(floor).map_err(ActualError::ConfigError)?;
    config
        .rules_min_score_by_repo
        .get_or_insert_with(HashMap::new)
        .insert(repo_key.to_string(), floor);
    Ok(())
}

/// The floor pinned for a repo, if any.
pub fn get_floor(config: &Config, repo_key: &str) -> Option<f64> {
    config
        .rules_min_score_by_repo
        .as_ref()
        .and_then(|map| map.get(repo_key))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_and_get_floor_per_repo() {
        let mut config = Config::default();
        set_floor(&mut config, "key-a", 1.5).unwrap();
        set_floor(&mut config, "key-b", 0.75).unwrap();
        assert_eq!(get_floor(&config, "key-a"), Some(1.5));
        assert_eq!(get_floor(&config, "key-b"), Some(0.75));
        assert_eq!(get_floor(&config, "key-c"), None);
    }

    #[test]
    fn test_set_floor_replaces_prior_value() {
        let mut config = Config::default();
        set_floor(&mut config, "key-a", 1.5).unwrap();
        set_floor(&mut config, "key-a", 2.0).unwrap();
        assert_eq!(get_floor(&config, "key-a"), Some(2.0));
    }

    #[test]
    fn test_set_floor_rejects_invalid_values() {
        let mut config = Config::default();
        for bad in [-1.0, f64::NAN, f64::INFINITY] {
            assert!(set_floor(&mut config, "key-a", bad).is_err());
        }
        assert!(config.rules_min_score_by_repo.is_none());
    }
}
