use std::collections::HashMap;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use anyhow::{Context, Result};
use streaming_iterator::StreamingIterator;

use super::language_resolver::TreeSitterLanguage;
use super::{EvidenceSpan, SignalSource, ToolMatch};

/// Metadata extracted from query pack comment annotations in `.scm` files.
#[derive(Debug, Clone)]
pub struct QueryDefinition {
    pub rule_id: String,
    pub facet_slot: String,
    pub leaf_id: String,
    pub confidence: f64,
    pub query_text: String,
    pub capture_name: Option<String>,
}

/// A query definition paired with its pre-compiled tree-sitter query.
///
/// Queries are compiled once at load time and reused across files, avoiding
/// redundant compilation on every `query_file` call.
struct CompiledQuery {
    definition: QueryDefinition,
    query: tree_sitter::Query,
}

/// Tree-sitter parser and query executor.
///
/// Parses source files using tree-sitter grammars and executes `.scm` query
/// packs to extract architecture signals.
pub struct TreeSitterAnalyzer {
    parser_cache: HashMap<TreeSitterLanguage, tree_sitter::Parser>,
    compiled_packs: HashMap<TreeSitterLanguage, Vec<CompiledQuery>>,
}

impl TreeSitterAnalyzer {
    /// Create a new analyzer that loads query packs from `query_packs_dir`.
    ///
    /// Each file in the directory should be named `<language_id>.scm`
    /// (e.g. `rust.scm`, `typescript.scm`).
    pub fn new(query_packs_dir: &Path) -> Result<Self> {
        let compiled_packs = Self::load_all_query_packs(query_packs_dir)?;
        Ok(Self {
            parser_cache: HashMap::new(),
            compiled_packs,
        })
    }

    /// Create an analyzer without any query packs (useful for parse-only usage).
    pub fn without_query_packs() -> Self {
        Self {
            parser_cache: HashMap::new(),
            compiled_packs: HashMap::new(),
        }
    }

    /// Parse source code into a tree-sitter [`tree_sitter::Tree`].
    pub fn parse(
        &mut self,
        content: &str,
        language: TreeSitterLanguage,
    ) -> Result<tree_sitter::Tree> {
        let parser = self.get_or_create_parser(language)?;
        parser
            .parse(content.as_bytes(), None)
            .context("tree-sitter parsing failed")
    }

    /// Execute all queries for `language` against `content` and return matches.
    pub fn query_file(
        &mut self,
        content: &str,
        file_path: &str,
        language: TreeSitterLanguage,
    ) -> Result<Vec<ToolMatch>> {
        let tree = self.parse(content, language)?;
        let content_bytes = content.as_bytes();
        let mut matches = Vec::new();

        if let Some(compiled) = self.compiled_packs.get(&language) {
            for cq in compiled {
                let def = &cq.definition;
                let query = &cq.query;

                let mut cursor = tree_sitter::QueryCursor::new();
                let mut query_matches = cursor.matches(query, tree.root_node(), content_bytes);

                while let Some(m) = query_matches.next() {
                    for capture in m.captures {
                        // If the definition specifies a specific capture name, filter.
                        if let Some(ref expected) = def.capture_name {
                            let capture_name = query.capture_names()[capture.index as usize];
                            if capture_name != expected {
                                continue;
                            }
                        }

                        let node = capture.node;
                        let value = &content[node.byte_range()];
                        let span = EvidenceSpan {
                            file_path: file_path.to_string(),
                            start_byte: node.start_byte(),
                            end_byte: node.end_byte(),
                            start_line: Some(node.start_position().row + 1), // 1-indexed
                            end_line: Some(node.end_position().row + 1),
                            source: SignalSource::TreeSitter,
                        };

                        matches.push(ToolMatch {
                            rule_id: def.rule_id.clone(),
                            facet_slot: def.facet_slot.clone(),
                            leaf_id: def.leaf_id.clone(),
                            value: serde_json::Value::String(value.to_string()),
                            confidence: def.confidence,
                            spans: vec![span],
                            raw: None,
                        });
                    }
                }
            }
        }

        Ok(matches)
    }

    // ------- private helpers -------

    fn get_or_create_parser(
        &mut self,
        language: TreeSitterLanguage,
    ) -> Result<&mut tree_sitter::Parser> {
        use std::collections::hash_map::Entry;
        match self.parser_cache.entry(language) {
            Entry::Occupied(e) => Ok(e.into_mut()),
            Entry::Vacant(e) => {
                let ts_lang = language
                    .tree_sitter_language()
                    .with_context(|| format!("no grammar for {language:?}"))?;
                let mut parser = tree_sitter::Parser::new();
                let msg = format!(
                    "failed to set tree-sitter language for {:?}",
                    language.as_str()
                );
                parser.set_language(&ts_lang).context(msg)?;
                Ok(e.insert(parser))
            }
        }
    }

    /// Load all `<lang>.scm` files from a directory and pre-compile queries.
    fn load_all_query_packs(dir: &Path) -> Result<HashMap<TreeSitterLanguage, Vec<CompiledQuery>>> {
        let mut packs = HashMap::new();

        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "failed to read query packs directory {}: {e}",
                    dir.display()
                ));
            }
        };

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("scm") {
                continue;
            }

            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();

            let language = match Self::language_from_stem(stem) {
                Some(l) => l,
                None => continue, // skip unrecognised language files
            };

            let ts_lang = match language.tree_sitter_language() {
                Some(l) => l,
                None => continue, // skip languages without grammar crates
            };

            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("reading query pack {}", path.display()))?;
            let definitions = parse_query_pack(&content, stem);
            let compiled = Self::compile_definitions(definitions, &ts_lang, stem);
            if !compiled.is_empty() {
                packs.insert(language, compiled);
            }
        }

        Ok(packs)
    }

    /// Compile a list of query definitions against a tree-sitter language.
    ///
    /// Definitions that fail to compile are logged and skipped.
    fn compile_definitions(
        definitions: Vec<QueryDefinition>,
        ts_lang: &tree_sitter::Language,
        lang_id: &str,
    ) -> Vec<CompiledQuery> {
        definitions
            .into_iter()
            .filter_map(
                |def| match tree_sitter::Query::new(ts_lang, &def.query_text) {
                    Ok(query) => Some(CompiledQuery {
                        definition: def,
                        query,
                    }),
                    Err(e) => {
                        eprintln!(
                            "warning: skipping query {}/{} that failed to compile: {e}",
                            lang_id, def.rule_id
                        );
                        None
                    }
                },
            )
            .collect()
    }

    /// Map a `.scm` file stem (e.g. `"rust"`, `"tsx"`) to a language variant.
    fn language_from_stem(stem: &str) -> Option<TreeSitterLanguage> {
        match stem {
            "javascript" => Some(TreeSitterLanguage::JavaScript),
            "typescript" => Some(TreeSitterLanguage::TypeScript),
            "tsx" => Some(TreeSitterLanguage::Tsx),
            "python" => Some(TreeSitterLanguage::Python),
            "go" => Some(TreeSitterLanguage::Go),
            "java" => Some(TreeSitterLanguage::Java),
            "kotlin" => Some(TreeSitterLanguage::Kotlin),
            "rust" => Some(TreeSitterLanguage::Rust),
            "c" => Some(TreeSitterLanguage::C),
            "cpp" => Some(TreeSitterLanguage::Cpp),
            "csharp" => Some(TreeSitterLanguage::CSharp),
            "ruby" => Some(TreeSitterLanguage::Ruby),
            "php" => Some(TreeSitterLanguage::Php),
            "swift" => Some(TreeSitterLanguage::Swift),
            _ => None,
        }
    }
}

/// Parse a `.scm` query pack file into [`QueryDefinition`]s.
///
/// The format uses metadata comment annotations before each S-expression query:
///
/// ```text
/// ; @rule_id: ts.rust.pub_function
/// ; @facet_slot: api.public.contracts
/// ; @leaf_id: 25
/// ; @confidence: 0.9
/// (function_item
///   (visibility_modifier) @vis (#eq? @vis "pub")
///   name: (identifier) @fn_name)
/// ```
///
/// The parser tracks parenthesis depth so it can flush a query definition
/// when a new metadata block begins at depth 0.
pub fn parse_query_pack(content: &str, language_id: &str) -> Vec<QueryDefinition> {
    let mut queries = Vec::new();
    let mut current_metadata: HashMap<String, String> = HashMap::new();
    let mut current_query_lines: Vec<String> = Vec::new();
    let mut paren_depth: i32 = 0;

    let flush = |metadata: &mut HashMap<String, String>,
                 query_lines: &mut Vec<String>,
                 queries: &mut Vec<QueryDefinition>,
                 lang_id: &str| {
        let query_text = query_lines.join("\n").trim().to_string();
        if !query_text.is_empty() {
            queries.push(QueryDefinition {
                rule_id: metadata
                    .remove("rule_id")
                    .unwrap_or_else(|| format!("ts.{lang_id}.unnamed")),
                facet_slot: metadata
                    .remove("facet_slot")
                    .unwrap_or_else(|| "unknown".to_string()),
                leaf_id: metadata
                    .remove("leaf_id")
                    .unwrap_or_else(|| "0".to_string()),
                confidence: metadata
                    .remove("confidence")
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(0.7),
                capture_name: metadata.remove("capture"),
                query_text,
            });
        }
        metadata.clear();
        query_lines.clear();
    };

    for line in content.lines() {
        let stripped = line.trim();

        if let Some(key_value) = stripped.strip_prefix("; @") {
            // Flush previous query if we have one and parentheses are balanced.
            if !current_query_lines.is_empty() && paren_depth == 0 {
                flush(
                    &mut current_metadata,
                    &mut current_query_lines,
                    &mut queries,
                    language_id,
                );
            }

            // Parse metadata key: value
            if let Some((key, value)) = key_value.split_once(':') {
                current_metadata.insert(key.trim().to_string(), value.trim().to_string());
            }
        } else if stripped.starts_with(';') {
            // Regular comment — skip.
        } else if !stripped.is_empty() {
            // Query content — track parentheses to know when query ends.
            paren_depth += count_parens(stripped);
            current_query_lines.push(line.to_string());
        }
    }

    // Flush the last query.
    flush(
        &mut current_metadata,
        &mut current_query_lines,
        &mut queries,
        language_id,
    );

    queries
}

/// Count open minus close parentheses, ignoring those inside strings.
fn count_parens(s: &str) -> i32 {
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut in_escape = false;

    for ch in s.chars() {
        if in_escape {
            in_escape = false;
        } else if ch == '\\' && in_string {
            in_escape = true;
        } else if ch == '"' {
            in_string = !in_string;
        } else if !in_string {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
        }
    }
    depth
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Default path to the bundled tree-sitter query packs (test-only).
    fn default_query_packs_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("analysis")
            .join("detectors")
            .join("tree_sitter_queries")
    }

    /// Helper to compile a [`QueryDefinition`] for Rust and insert it into
    /// an analyzer's `compiled_packs`. Panics if the query text is invalid
    /// Rust tree-sitter syntax (use for intentionally valid test queries).
    fn insert_compiled_rust(analyzer: &mut TreeSitterAnalyzer, defs: Vec<QueryDefinition>) {
        let ts_lang = TreeSitterLanguage::Rust.tree_sitter_language().unwrap();
        let compiled: Vec<CompiledQuery> = defs
            .into_iter()
            .filter_map(|def| {
                tree_sitter::Query::new(&ts_lang, &def.query_text)
                    .ok()
                    .map(|query| CompiledQuery {
                        definition: def,
                        query,
                    })
            })
            .collect();
        analyzer
            .compiled_packs
            .insert(TreeSitterLanguage::Rust, compiled);
    }

    // ---- parse_query_pack ----

    #[test]
    fn parse_query_pack_basic() {
        let content = r#"
; @rule_id: ts.rust.pub_function
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(function_item
  (visibility_modifier) @vis (#eq? @vis "pub")
  name: (identifier) @fn_name)

; @rule_id: ts.rust.pub_struct
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(struct_item
  (visibility_modifier) @vis (#eq? @vis "pub")
  name: (type_identifier) @struct_name)
"#;

        let defs = parse_query_pack(content, "rust");
        assert_eq!(defs.len(), 2);

        assert_eq!(defs[0].rule_id, "ts.rust.pub_function");
        assert_eq!(defs[0].facet_slot, "api.public.contracts");
        assert_eq!(defs[0].leaf_id, "25");
        assert!((defs[0].confidence - 0.9).abs() < f64::EPSILON);
        assert!(defs[0].query_text.contains("function_item"));

        assert_eq!(defs[1].rule_id, "ts.rust.pub_struct");
        assert!(defs[1].query_text.contains("struct_item"));
    }

    #[test]
    fn parse_query_pack_defaults_on_missing_metadata() {
        let content = "(identifier) @name\n";
        let defs = parse_query_pack(content, "test");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].rule_id, "ts.test.unnamed");
        assert_eq!(defs[0].facet_slot, "unknown");
        assert_eq!(defs[0].leaf_id, "0");
        assert!((defs[0].confidence - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_query_pack_with_capture_annotation() {
        let content = r#"
; @rule_id: ts.test.cap
; @facet_slot: test.slot
; @leaf_id: 1
; @confidence: 0.5
; @capture: fn_name
(function_item name: (identifier) @fn_name)
"#;
        let defs = parse_query_pack(content, "test");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].capture_name.as_deref(), Some("fn_name"));
    }

    #[test]
    fn parse_query_pack_skips_plain_comments() {
        let content = r#"
; This is a plain comment
; Another comment

; @rule_id: ts.test.item
; @facet_slot: test
; @leaf_id: 1
; @confidence: 0.8
(identifier) @name
"#;
        let defs = parse_query_pack(content, "test");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].rule_id, "ts.test.item");
    }

    #[test]
    fn parse_query_pack_multi_line_query() {
        let content = r#"
; @rule_id: ts.test.multiline
; @facet_slot: test.slot
; @leaf_id: 42
; @confidence: 0.95
(function_item
  (visibility_modifier) @vis
  name: (identifier) @fn_name
  parameters: (parameters) @params)
"#;
        let defs = parse_query_pack(content, "test");
        assert_eq!(defs.len(), 1);
        assert!(defs[0].query_text.contains("function_item"));
        assert!(defs[0].query_text.contains("parameters"));
    }

    #[test]
    fn parse_query_pack_empty_content() {
        let defs = parse_query_pack("", "test");
        assert!(defs.is_empty());
    }

    #[test]
    fn parse_query_pack_only_comments() {
        let content = "; just a comment\n; another comment\n";
        let defs = parse_query_pack(content, "test");
        assert!(defs.is_empty());
    }

    // ---- count_parens ----

    #[test]
    fn count_parens_balanced() {
        assert_eq!(count_parens("(foo (bar))"), 0);
    }

    #[test]
    fn count_parens_open() {
        assert_eq!(count_parens("(foo"), 1);
    }

    #[test]
    fn count_parens_ignores_strings() {
        assert_eq!(count_parens(r#"(#eq? @vis "pub")"#), 0);
    }

    #[test]
    fn count_parens_empty() {
        assert_eq!(count_parens(""), 0);
    }

    #[test]
    fn count_parens_escaped_backslash_before_quote() {
        // Double-escaped backslash followed by closing quote: \\"  ->  the quote closes the string.
        assert_eq!(count_parens(r#"("\\") (foo)"#), 0);
    }

    #[test]
    fn count_parens_escaped_quote_in_string() {
        // Escaped quote inside string: \" -> the quote doesn't close the string.
        assert_eq!(count_parens(r#"("he said \"hi\"") (foo)"#), 0);
    }

    // ---- TreeSitterAnalyzer::parse ----

    #[test]
    fn parse_rust_code() {
        let mut analyzer = TreeSitterAnalyzer::without_query_packs();
        let code = "pub fn hello() {}";
        let tree = analyzer
            .parse(code, TreeSitterLanguage::Rust)
            .expect("should parse");
        let root = tree.root_node();
        assert_eq!(root.kind(), "source_file");
        assert!(root.child_count() > 0);
    }

    #[test]
    fn parse_typescript_code() {
        let mut analyzer = TreeSitterAnalyzer::without_query_packs();
        let code = "export function greet(): string { return 'hi'; }";
        let tree = analyzer
            .parse(code, TreeSitterLanguage::TypeScript)
            .expect("should parse");
        let root = tree.root_node();
        assert_eq!(root.kind(), "program");
    }

    #[test]
    fn parse_python_code() {
        let mut analyzer = TreeSitterAnalyzer::without_query_packs();
        let code = "def hello():\n    pass\n";
        let tree = analyzer
            .parse(code, TreeSitterLanguage::Python)
            .expect("should parse");
        let root = tree.root_node();
        assert_eq!(root.kind(), "module");
    }

    #[test]
    fn parse_javascript_code() {
        let mut analyzer = TreeSitterAnalyzer::without_query_packs();
        let code = "function hello() { return 42; }";
        let tree = analyzer
            .parse(code, TreeSitterLanguage::JavaScript)
            .expect("should parse");
        let root = tree.root_node();
        assert_eq!(root.kind(), "program");
    }

    #[test]
    fn parse_go_code() {
        let mut analyzer = TreeSitterAnalyzer::without_query_packs();
        let code = "package main\nfunc main() {}\n";
        let tree = analyzer
            .parse(code, TreeSitterLanguage::Go)
            .expect("should parse");
        let root = tree.root_node();
        assert_eq!(root.kind(), "source_file");
    }

    #[test]
    fn parse_java_code() {
        let mut analyzer = TreeSitterAnalyzer::without_query_packs();
        let code = "public class Main { public static void main(String[] args) {} }";
        let tree = analyzer
            .parse(code, TreeSitterLanguage::Java)
            .expect("should parse");
        let root = tree.root_node();
        assert_eq!(root.kind(), "program");
    }

    #[test]
    fn parse_c_code() {
        let mut analyzer = TreeSitterAnalyzer::without_query_packs();
        let code = "int main() { return 0; }";
        let tree = analyzer
            .parse(code, TreeSitterLanguage::C)
            .expect("should parse");
        let root = tree.root_node();
        assert_eq!(root.kind(), "translation_unit");
    }

    #[test]
    fn parse_cpp_code() {
        let mut analyzer = TreeSitterAnalyzer::without_query_packs();
        let code = "#include <iostream>\nint main() { std::cout << \"hi\"; }";
        let tree = analyzer
            .parse(code, TreeSitterLanguage::Cpp)
            .expect("should parse");
        let root = tree.root_node();
        assert_eq!(root.kind(), "translation_unit");
    }

    #[test]
    fn parse_reuses_cached_parser() {
        let mut analyzer = TreeSitterAnalyzer::without_query_packs();
        // First parse creates the parser
        let _ = analyzer
            .parse("fn a() {}", TreeSitterLanguage::Rust)
            .unwrap();
        // Second parse should reuse the cached parser (covers the non-vacant path)
        let tree = analyzer
            .parse("fn b() {}", TreeSitterLanguage::Rust)
            .unwrap();
        assert_eq!(tree.root_node().kind(), "source_file");
    }

    #[test]
    fn parse_invalid_code_does_not_panic() {
        let mut analyzer = TreeSitterAnalyzer::without_query_packs();
        let code = "}{}{}{}{this is not valid rust}{}{}{";
        let result = analyzer.parse(code, TreeSitterLanguage::Rust);
        // tree-sitter is lenient: it produces a tree with ERROR nodes rather
        // than returning Err.
        assert!(result.is_ok());
    }

    #[test]
    fn parse_unsupported_language_returns_error() {
        let mut analyzer = TreeSitterAnalyzer::without_query_packs();
        let result = analyzer.parse("val x = 1", TreeSitterLanguage::Kotlin);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("no grammar"));
    }

    // ---- TreeSitterAnalyzer with query packs ----

    #[test]
    fn load_and_query_rust_file() {
        let dir = default_query_packs_dir();
        let mut analyzer = TreeSitterAnalyzer::new(&dir).expect("should load query packs");

        let code = "pub fn serve() {}\nfn private() {}";
        let matches = analyzer
            .query_file(code, "src/lib.rs", TreeSitterLanguage::Rust)
            .expect("should query");

        // We should get at least one match for `pub fn serve`.
        assert!(!matches.is_empty());
        assert!(matches
            .iter()
            .any(|m| m.value.as_str().map_or(false, |s| s.contains("serve"))));
    }

    #[test]
    fn query_file_populates_spans() {
        let dir = default_query_packs_dir();
        let mut analyzer = TreeSitterAnalyzer::new(&dir).expect("should load query packs");

        let code = "pub fn example() {}";
        let matches = analyzer
            .query_file(code, "src/main.rs", TreeSitterLanguage::Rust)
            .expect("should query");

        for m in &matches {
            for span in &m.spans {
                assert_eq!(span.file_path, "src/main.rs");
                assert_eq!(span.source, SignalSource::TreeSitter);
                assert!(span.start_byte < span.end_byte);
                assert!(span.start_line.unwrap_or(0) >= 1);
            }
        }
    }

    #[test]
    fn query_file_with_inline_query() {
        // Test querying with an inline query pack (not from .scm files).
        let mut analyzer = TreeSitterAnalyzer::without_query_packs();
        insert_compiled_rust(
            &mut analyzer,
            vec![QueryDefinition {
                rule_id: "test.fn_name".to_string(),
                facet_slot: "test.slot".to_string(),
                leaf_id: "1".to_string(),
                confidence: 0.9,
                query_text: "(function_item name: (identifier) @name)".to_string(),
                capture_name: Some("name".to_string()),
            }],
        );

        let code = "fn hello() {}\nfn world() {}";
        let matches = analyzer
            .query_file(code, "test.rs", TreeSitterLanguage::Rust)
            .expect("should query");

        assert_eq!(matches.len(), 2);
        assert_eq!(
            matches[0].value,
            serde_json::Value::String("hello".to_string())
        );
        assert_eq!(
            matches[1].value,
            serde_json::Value::String("world".to_string())
        );
    }

    #[test]
    fn query_file_capture_name_filters_mismatches() {
        // When capture_name is set, only matching captures should produce ToolMatches.
        // The query "(function_item name: (identifier) @name)" produces captures
        // named "name". If we set capture_name to "other", we should get 0 matches.
        let mut analyzer = TreeSitterAnalyzer::without_query_packs();
        insert_compiled_rust(
            &mut analyzer,
            vec![QueryDefinition {
                rule_id: "test.filter".to_string(),
                facet_slot: "test.slot".to_string(),
                leaf_id: "1".to_string(),
                confidence: 0.9,
                query_text: "(function_item name: (identifier) @name)".to_string(),
                capture_name: Some("nonexistent_capture".to_string()),
            }],
        );

        let code = "fn hello() {}";
        let matches = analyzer
            .query_file(code, "test.rs", TreeSitterLanguage::Rust)
            .expect("should query");
        assert!(matches.is_empty());
    }

    #[test]
    fn query_file_no_capture_name_returns_all_captures() {
        // When capture_name is None, all captures produce ToolMatches.
        let mut analyzer = TreeSitterAnalyzer::without_query_packs();
        insert_compiled_rust(
            &mut analyzer,
            vec![QueryDefinition {
                rule_id: "test.all".to_string(),
                facet_slot: "test.slot".to_string(),
                leaf_id: "1".to_string(),
                confidence: 0.9,
                query_text: "(function_item name: (identifier) @name)".to_string(),
                capture_name: None,
            }],
        );

        let code = "fn hello() {}";
        let matches = analyzer
            .query_file(code, "test.rs", TreeSitterLanguage::Rust)
            .expect("should query");
        assert!(!matches.is_empty());
    }

    #[test]
    fn bad_query_skipped_at_compile_time() {
        // A query definition with invalid query_text should be skipped during
        // compilation, so it never appears in compiled_packs.
        let ts_lang = TreeSitterLanguage::Rust.tree_sitter_language().unwrap();
        let defs = vec![QueryDefinition {
            rule_id: "test.bad".to_string(),
            facet_slot: "test.slot".to_string(),
            leaf_id: "1".to_string(),
            confidence: 0.5,
            query_text: "(this_is_not_a_valid_node_type) @x".to_string(),
            capture_name: None,
        }];
        let compiled = TreeSitterAnalyzer::compile_definitions(defs, &ts_lang, "rust");
        assert!(compiled.is_empty());
    }

    #[test]
    fn query_file_no_packs_returns_empty() {
        let mut analyzer = TreeSitterAnalyzer::without_query_packs();
        let matches = analyzer
            .query_file("fn hello() {}", "test.rs", TreeSitterLanguage::Rust)
            .expect("should succeed with no query packs");
        assert!(matches.is_empty());
    }

    #[test]
    fn query_file_unsupported_language_returns_error() {
        let mut analyzer = TreeSitterAnalyzer::without_query_packs();
        let result = analyzer.query_file("val x = 1", "test.kt", TreeSitterLanguage::Kotlin);
        assert!(result.is_err());
    }

    #[test]
    fn load_query_packs_nonexistent_dir() {
        let result = TreeSitterAnalyzer::new(Path::new("/nonexistent/path"));
        assert!(result.is_err());
    }

    #[test]
    fn load_query_packs_with_non_scm_files() {
        // Create a temp dir with a non-scm file and an scm file.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("readme.txt"), "not a query pack").unwrap();
        std::fs::write(
            tmp.path().join("rust.scm"),
            "; @rule_id: ts.rust.test\n; @facet_slot: test\n; @leaf_id: 1\n; @confidence: 0.8\n(function_item name: (identifier) @name)\n",
        ).unwrap();
        // Also add a file with unknown language stem
        std::fs::write(
            tmp.path().join("haskell.scm"),
            "; @rule_id: ts.haskell.test\n(identifier) @name\n",
        )
        .unwrap();

        let analyzer = TreeSitterAnalyzer::new(tmp.path()).expect("should load");
        // Should only load rust.scm, skip readme.txt and haskell.scm
        assert!(analyzer
            .compiled_packs
            .contains_key(&TreeSitterLanguage::Rust));
        assert!(!analyzer
            .compiled_packs
            .contains_key(&TreeSitterLanguage::Kotlin));
    }

    #[test]
    fn load_query_packs_skips_unsupported_languages() {
        // Kotlin has a .scm file but no compatible grammar crate, so
        // its query pack should be skipped at load time.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("kotlin.scm"),
            "; @rule_id: ts.kotlin.class\n; @facet_slot: test\n; @leaf_id: 1\n; @confidence: 0.8\n(class_declaration name: (type_identifier) @name)\n",
        )
        .unwrap();

        let analyzer = TreeSitterAnalyzer::new(tmp.path()).expect("should load");
        assert!(!analyzer
            .compiled_packs
            .contains_key(&TreeSitterLanguage::Kotlin));
    }

    #[test]
    fn load_query_packs_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let analyzer = TreeSitterAnalyzer::new(tmp.path()).expect("should load empty");
        assert!(analyzer.compiled_packs.is_empty());
    }

    #[test]
    fn default_query_packs_dir_exists() {
        let dir = default_query_packs_dir();
        assert!(dir.exists());
    }

    #[test]
    fn language_from_stem_all_known() {
        let cases = [
            ("javascript", TreeSitterLanguage::JavaScript),
            ("typescript", TreeSitterLanguage::TypeScript),
            ("tsx", TreeSitterLanguage::Tsx),
            ("python", TreeSitterLanguage::Python),
            ("go", TreeSitterLanguage::Go),
            ("java", TreeSitterLanguage::Java),
            ("kotlin", TreeSitterLanguage::Kotlin),
            ("rust", TreeSitterLanguage::Rust),
            ("c", TreeSitterLanguage::C),
            ("cpp", TreeSitterLanguage::Cpp),
            ("csharp", TreeSitterLanguage::CSharp),
            ("ruby", TreeSitterLanguage::Ruby),
            ("php", TreeSitterLanguage::Php),
            ("swift", TreeSitterLanguage::Swift),
        ];
        for (stem, expected) in &cases {
            assert_eq!(
                TreeSitterAnalyzer::language_from_stem(stem),
                Some(*expected)
            );
        }
    }

    #[test]
    fn language_from_stem_unknown() {
        assert_eq!(TreeSitterAnalyzer::language_from_stem("haskell"), None);
        assert_eq!(TreeSitterAnalyzer::language_from_stem(""), None);
    }
}
