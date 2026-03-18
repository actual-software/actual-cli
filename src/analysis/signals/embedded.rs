use rust_embed::Embed;

#[derive(Embed)]
#[folder = "$CARGO_MANIFEST_DIR/src/analysis/detectors/tree_sitter_queries/"]
#[include = "*.scm"]
pub(crate) struct EmbeddedTreeSitterQueries;

#[derive(Embed)]
#[folder = "$CARGO_MANIFEST_DIR/src/analysis/detectors/semgrep_rules/"]
#[include = "*.yml"]
pub(crate) struct EmbeddedSemgrepRules;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_tree_sitter_queries_are_present() {
        let count = EmbeddedTreeSitterQueries::iter().count();
        assert!(
            count > 0,
            "expected embedded tree-sitter query files, got 0"
        );
    }

    #[test]
    fn embedded_semgrep_rules_are_present() {
        let count = EmbeddedSemgrepRules::iter().count();
        assert!(count > 0, "expected embedded semgrep rule files, got 0");
    }

    #[test]
    fn embedded_tree_sitter_queries_get_known_file() {
        // Verify that every file returned by iter() can also be retrieved via get().
        let first = EmbeddedTreeSitterQueries::iter().next();
        assert!(first.is_some(), "expected at least one embedded query file");
        let filename = first.unwrap();
        let file = EmbeddedTreeSitterQueries::get(&filename);
        assert!(
            file.is_some(),
            "get() should return Some for a file returned by iter()"
        );
    }

    #[test]
    fn embedded_tree_sitter_queries_get_missing_returns_none() {
        let file = EmbeddedTreeSitterQueries::get("nonexistent_file_that_does_not_exist.scm");
        assert!(file.is_none(), "get() should return None for unknown file");
    }

    #[test]
    fn embedded_semgrep_rules_get_known_file() {
        let first = EmbeddedSemgrepRules::iter().next();
        assert!(first.is_some(), "expected at least one embedded rule file");
        let filename = first.unwrap();
        let file = EmbeddedSemgrepRules::get(&filename);
        assert!(
            file.is_some(),
            "get() should return Some for a file returned by iter()"
        );
    }

    #[test]
    fn embedded_semgrep_rules_get_missing_returns_none() {
        let file = EmbeddedSemgrepRules::get("nonexistent_rule_that_does_not_exist.yml");
        assert!(file.is_none(), "get() should return None for unknown file");
    }
}
