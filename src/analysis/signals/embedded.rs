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
    use rust_embed::Embed;

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
}
