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
        assert!(count > 0);
    }

    #[test]
    fn embedded_semgrep_rules_are_present() {
        let count = EmbeddedSemgrepRules::iter().count();
        assert!(count > 0);
    }

    #[test]
    fn embedded_tree_sitter_queries_get_known_file() {
        // Verify that every file returned by iter() can also be retrieved via get().
        let first = EmbeddedTreeSitterQueries::iter().next();
        assert!(first.is_some());
        let filename = first.unwrap();
        let file = EmbeddedTreeSitterQueries::get(&filename);
        assert!(file.is_some());
    }

    #[test]
    fn embedded_tree_sitter_queries_get_missing_returns_none() {
        let file = EmbeddedTreeSitterQueries::get("nonexistent_file_that_does_not_exist.scm");
        assert!(file.is_none());
    }

    #[test]
    fn embedded_semgrep_rules_get_known_file() {
        let first = EmbeddedSemgrepRules::iter().next();
        assert!(first.is_some());
        let filename = first.unwrap();
        let file = EmbeddedSemgrepRules::get(&filename);
        assert!(file.is_some());
    }

    #[test]
    fn embedded_semgrep_rules_get_missing_returns_none() {
        let file = EmbeddedSemgrepRules::get("nonexistent_rule_that_does_not_exist.yml");
        assert!(file.is_none());
    }

    #[test]
    fn all_tree_sitter_query_files_are_gettable() {
        for name in EmbeddedTreeSitterQueries::iter() {
            let file = EmbeddedTreeSitterQueries::get(&name);
            assert!(file.is_some());
            // Verify content is non-empty valid UTF-8
            let embedded = file.unwrap();
            let content = std::str::from_utf8(embedded.data.as_ref());
            assert!(content.is_ok());
            assert!(!content.unwrap().is_empty());
        }
    }

    #[test]
    fn all_semgrep_rule_files_are_gettable() {
        for name in EmbeddedSemgrepRules::iter() {
            let file = EmbeddedSemgrepRules::get(&name);
            assert!(file.is_some());
            let embedded = file.unwrap();
            let content = std::str::from_utf8(embedded.data.as_ref());
            assert!(content.is_ok());
            assert!(!content.unwrap().is_empty());
        }
    }
}
