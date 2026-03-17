use rust_embed::Embed;

#[derive(Embed)]
#[folder = "$CARGO_MANIFEST_DIR/src/analysis/detectors/tree_sitter_queries/"]
#[include = "*.scm"]
pub(crate) struct EmbeddedTreeSitterQueries;

#[derive(Embed)]
#[folder = "$CARGO_MANIFEST_DIR/src/analysis/detectors/semgrep_rules/"]
#[include = "*.yml"]
pub(crate) struct EmbeddedSemgrepRules;
