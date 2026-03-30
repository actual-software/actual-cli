use std::path::Path;

use crate::api::types::Adr;
use crate::generation::writer::{WriteAction, WriteResult};
use crate::generation::OutputFormat;
use crate::tailoring::types::{AdrSection, FileOutput, TailoringOutput};

/// A raw file to write without managed section markers.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct V2RawFile {
    pub path: String,
    pub content: String,
}

/// Output of V2 ADR processing.
#[derive(Debug)]
pub(crate) struct V2Output {
    pub raw_files: Vec<V2RawFile>,
    pub governance_content: String,
}

/// Partition ADRs into (V1, V2).
pub(crate) fn partition_adrs(adrs: Vec<Adr>) -> (Vec<Adr>, Vec<Adr>) {
    adrs.into_iter().partition(|a| !a.is_v2())
}

/// Generate a filesystem-safe slug from an ADR title.
pub(crate) fn adr_slug(title: &str) -> String {
    let lower = title.to_lowercase();
    // Replace non-alphanumeric characters with hyphens
    let mut slug = String::with_capacity(lower.len());
    let mut last_was_hyphen = false;
    for ch in lower.chars() {
        if ch.is_alphanumeric() {
            slug.push(ch);
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            slug.push('-');
            last_was_hyphen = true;
        }
    }
    // Trim leading/trailing hyphens
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        "adr".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Derive a glob pattern from a list of languages.
fn derive_glob(languages: &[String]) -> &'static str {
    for lang in languages {
        match lang.to_lowercase().as_str() {
            "python" => return "**/*.py",
            "typescript" | "javascript" => return "**/*.{ts,js,tsx,jsx}",
            "rust" => return "**/*.rs",
            "go" => return "**/*.go",
            "java" => return "**/*.java",
            "ruby" => return "**/*.rb",
            _ => {}
        }
    }
    "**/*"
}

/// Generate the `.claude/rules/<slug>.md` content for an ADR.
fn generate_rules_file(adr: &Adr, _slug: &str) -> String {
    let glob = derive_glob(&adr.applies_to.languages);
    let mut content = format!("---\nglob: \"{glob}\"\n---\n\n");
    content.push_str(&format!(
        "<rule_activation adr-id=\"{}\">\n<!-- ADR: {} -->\n</rule_activation>\n\n",
        adr.id, adr.title
    ));

    // Try content_json.agent_documentation or content_json.enforcement first
    if let Some(ref cj) = adr.content_json {
        if let Some(agent_doc) = cj.get("agent_documentation").and_then(|v| v.as_str()) {
            content.push_str(agent_doc);
            content.push('\n');
            return content;
        }
        if let Some(enforcement) = cj.get("enforcement").and_then(|v| v.as_str()) {
            content.push_str(enforcement);
            content.push('\n');
            return content;
        }
    }

    // Fallback: policies as bullet points
    for policy in &adr.policies {
        content.push_str(&format!("- {policy}\n"));
    }
    content
}

/// Generate the `docs/adr/<slug>.md` content for an ADR.
fn generate_adr_doc(adr: &Adr) -> String {
    if let Some(ref md) = adr.content_md {
        md.clone()
    } else {
        // Fallback: generate from available fields
        let mut content = format!("# {}\n\n", adr.title);
        if let Some(ref ctx) = adr.context {
            content.push_str(&format!("## Context\n\n{ctx}\n\n"));
        }
        if !adr.policies.is_empty() {
            content.push_str("## Policies\n\n");
            for policy in &adr.policies {
                content.push_str(&format!("- {policy}\n"));
            }
        }
        content
    }
}

/// Generate V2 output from a slice of V2 ADRs.
pub(crate) fn generate_v2_output(v2_adrs: &[Adr]) -> V2Output {
    let mut raw_files = Vec::new();

    for adr in v2_adrs {
        let slug = adr_slug(&adr.title);

        // 1. docs/adr/<slug>.md
        raw_files.push(V2RawFile {
            path: format!("docs/adr/{slug}.md"),
            content: generate_adr_doc(adr),
        });

        // 2. .claude/rules/<slug>.md
        raw_files.push(V2RawFile {
            path: format!(".claude/rules/{slug}.md"),
            content: generate_rules_file(adr, &slug),
        });
    }

    let governance_content = "\
<adr_governance source=\"docs/adr/\">
ADRs govern validated architectural standards for this project.
Full ADR documents: @docs/adr/
</adr_governance>"
        .to_string();

    V2Output {
        raw_files,
        governance_content,
    }
}

/// Write V2 raw files to disk (similar to write_files but without markers).
pub(crate) fn write_v2_raw_files(root_dir: &Path, files: &[V2RawFile]) -> Vec<WriteResult> {
    let canonical_root = match root_dir.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            return files
                .iter()
                .map(|file| WriteResult {
                    path: file.path.clone(),
                    action: WriteAction::Failed,
                    version: 0,
                    error: Some(format!("Failed to canonicalize root directory: {e}")),
                })
                .collect();
        }
    };

    files
        .iter()
        .map(|file| {
            // Layer 1: reject paths with .. components (path traversal guard) or
            // absolute paths (RootDir / Prefix)
            let path_components_invalid = Path::new(&file.path).components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            });
            if path_components_invalid {
                let is_absolute = Path::new(&file.path).is_absolute();
                let error_msg = if is_absolute {
                    "absolute paths are not allowed".to_string()
                } else {
                    "path traversal detected: path escapes root directory".to_string()
                };
                return WriteResult {
                    path: file.path.clone(),
                    action: WriteAction::Failed,
                    version: 0,
                    error: Some(error_msg),
                };
            }

            let full_path = root_dir.join(&file.path);

            // Determine action: Created or Updated
            let action = if full_path.exists() {
                WriteAction::Updated
            } else {
                WriteAction::Created
            };

            // Create parent directories
            let parent = full_path.parent().expect("joined path always has a parent");
            if let Err(e) = std::fs::create_dir_all(parent) {
                return WriteResult {
                    path: file.path.clone(),
                    action: WriteAction::Failed,
                    version: 0,
                    error: Some(format!("Failed to create directory: {e}")),
                };
            }

            // Layer 2: post-create canonicalization check (defense in depth).
            let canonical_parent = parent
                .canonicalize()
                .unwrap_or_else(|_| std::path::PathBuf::new());
            if !canonical_parent.starts_with(&canonical_root) {
                return WriteResult {
                    path: file.path.clone(),
                    action: WriteAction::Failed,
                    version: 0,
                    error: Some("path escapes root directory".to_string()),
                };
            }

            // Write file (raw, no markers)
            if let Err(e) = std::fs::write(&full_path, &file.content) {
                return WriteResult {
                    path: file.path.clone(),
                    action: WriteAction::Failed,
                    version: 0,
                    error: Some(format!("Failed to write file: {e}")),
                };
            }

            WriteResult {
                path: file.path.clone(),
                action,
                version: 0,
                error: None,
            }
        })
        .collect()
}

/// Inject V2 governance section into the root FileOutput of a TailoringOutput.
pub(crate) fn inject_v2_governance(
    mut output: TailoringOutput,
    governance_content: String,
    format: &OutputFormat,
) -> TailoringOutput {
    let root_filename = format.filename();
    let governance_section = AdrSection {
        adr_id: "v2-governance".to_string(),
        content: governance_content,
    };

    // Find the root file
    if let Some(root_file) = output.files.iter_mut().find(|f| f.path == root_filename) {
        // Insert governance as the first section
        root_file.sections.insert(0, governance_section);
    } else {
        // Create a new root file with just the governance section
        let new_file = FileOutput {
            path: root_filename.to_string(),
            sections: vec![governance_section],
            reasoning: "V2 governance pointer".to_string(),
        };
        output.files.insert(0, new_file);
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{AdrCategory, AppliesTo};
    use crate::tailoring::types::{AdrSection, SkippedAdr, TailoringSummary};

    fn make_v1_adr(id: &str, title: &str) -> Adr {
        Adr {
            id: id.to_string(),
            title: title.to_string(),
            context: None,
            policies: vec!["Do something".to_string()],
            instructions: None,
            category: AdrCategory {
                id: "cat-1".to_string(),
                name: "General".to_string(),
                path: "General".to_string(),
            },
            applies_to: AppliesTo {
                languages: vec!["rust".to_string()],
                frameworks: vec![],
                packages: vec![],
                version_range: None,
                facets: vec![],
            },
            matched_projects: vec![],
            schema_version: None,
            content_md: None,
            content_json: None,
            source: None,
        }
    }

    fn make_v2_adr(id: &str, title: &str) -> Adr {
        Adr {
            id: id.to_string(),
            title: title.to_string(),
            context: None,
            policies: vec!["Policy one".to_string()],
            instructions: None,
            category: AdrCategory {
                id: "cat-1".to_string(),
                name: "General".to_string(),
                path: "General".to_string(),
            },
            applies_to: AppliesTo {
                languages: vec!["typescript".to_string()],
                frameworks: vec![],
                packages: vec![],
                version_range: None,
                facets: vec![],
            },
            matched_projects: vec![],
            schema_version: Some(2),
            content_md: Some("# Title\n\nContent here.".to_string()),
            content_json: None,
            source: None,
        }
    }

    // ── partition_adrs tests ──

    #[test]
    fn test_partition_adrs_empty() {
        let (v1, v2) = partition_adrs(vec![]);
        assert!(v1.is_empty());
        assert!(v2.is_empty());
    }

    #[test]
    fn test_partition_adrs_all_v1() {
        let adrs = vec![make_v1_adr("adr-1", "ADR 1"), make_v1_adr("adr-2", "ADR 2")];
        let (v1, v2) = partition_adrs(adrs);
        assert_eq!(v1.len(), 2);
        assert!(v2.is_empty());
    }

    #[test]
    fn test_partition_adrs_all_v2() {
        let adrs = vec![make_v2_adr("adr-1", "ADR 1"), make_v2_adr("adr-2", "ADR 2")];
        let (v1, v2) = partition_adrs(adrs);
        assert!(v1.is_empty());
        assert_eq!(v2.len(), 2);
    }

    #[test]
    fn test_partition_adrs_mixed() {
        let adrs = vec![
            make_v1_adr("v1-adr", "V1 ADR"),
            make_v2_adr("v2-adr", "V2 ADR"),
            make_v1_adr("another-v1", "Another V1"),
        ];
        let (v1, v2) = partition_adrs(adrs);
        assert_eq!(v1.len(), 2);
        assert_eq!(v2.len(), 1);
        assert_eq!(v1[0].id, "v1-adr");
        assert_eq!(v1[1].id, "another-v1");
        assert_eq!(v2[0].id, "v2-adr");
    }

    #[test]
    fn test_partition_adrs_v2_via_content_md_only() {
        let mut adr = make_v1_adr("adr-x", "Content MD only");
        adr.content_md = Some("# Some content".to_string());
        // schema_version is None, but content_md is present -> is_v2() == true
        let (v1, v2) = partition_adrs(vec![adr]);
        assert!(v1.is_empty());
        assert_eq!(v2.len(), 1);
    }

    // ── adr_slug tests ──

    #[test]
    fn test_adr_slug_simple() {
        assert_eq!(adr_slug("Use App Router"), "use-app-router");
    }

    #[test]
    fn test_adr_slug_lowercase() {
        assert_eq!(adr_slug("Use HTTPS Everywhere"), "use-https-everywhere");
    }

    #[test]
    fn test_adr_slug_special_chars() {
        assert_eq!(
            adr_slug("Use async/await (always)"),
            "use-async-await-always"
        );
    }

    #[test]
    fn test_adr_slug_multiple_spaces() {
        assert_eq!(adr_slug("Multiple   Spaces   Here"), "multiple-spaces-here");
    }

    #[test]
    fn test_adr_slug_leading_trailing_special() {
        assert_eq!(adr_slug("  Leading and trailing  "), "leading-and-trailing");
    }

    #[test]
    fn test_adr_slug_all_special_chars() {
        assert_eq!(adr_slug("---"), "adr");
    }

    #[test]
    fn test_adr_slug_empty_string() {
        assert_eq!(adr_slug(""), "adr");
    }

    #[test]
    fn test_adr_slug_numbers() {
        assert_eq!(adr_slug("ADR 001: My Decision"), "adr-001-my-decision");
    }

    #[test]
    fn test_adr_slug_unicode() {
        // Unicode alphanumeric characters are preserved in the slug
        assert_eq!(adr_slug("Use café patterns"), "use-café-patterns");
    }

    // ── derive_glob tests ──

    #[test]
    fn test_derive_glob_python() {
        assert_eq!(derive_glob(&["python".to_string()]), "**/*.py");
    }

    #[test]
    fn test_derive_glob_typescript() {
        assert_eq!(
            derive_glob(&["typescript".to_string()]),
            "**/*.{ts,js,tsx,jsx}"
        );
    }

    #[test]
    fn test_derive_glob_javascript() {
        assert_eq!(
            derive_glob(&["javascript".to_string()]),
            "**/*.{ts,js,tsx,jsx}"
        );
    }

    #[test]
    fn test_derive_glob_rust() {
        assert_eq!(derive_glob(&["rust".to_string()]), "**/*.rs");
    }

    #[test]
    fn test_derive_glob_go() {
        assert_eq!(derive_glob(&["go".to_string()]), "**/*.go");
    }

    #[test]
    fn test_derive_glob_java() {
        assert_eq!(derive_glob(&["java".to_string()]), "**/*.java");
    }

    #[test]
    fn test_derive_glob_ruby() {
        assert_eq!(derive_glob(&["ruby".to_string()]), "**/*.rb");
    }

    #[test]
    fn test_derive_glob_unknown() {
        assert_eq!(derive_glob(&["cobol".to_string()]), "**/*");
    }

    #[test]
    fn test_derive_glob_empty() {
        assert_eq!(derive_glob(&[]), "**/*");
    }

    #[test]
    fn test_derive_glob_first_match_wins() {
        // Python is first, so python glob wins
        assert_eq!(
            derive_glob(&["python".to_string(), "rust".to_string()]),
            "**/*.py"
        );
    }

    // ── generate_v2_output tests ──

    #[test]
    fn test_generate_v2_output_empty() {
        let output = generate_v2_output(&[]);
        assert!(output.raw_files.is_empty());
        assert!(output.governance_content.contains("adr_governance"));
        assert!(output.governance_content.contains("docs/adr/"));
    }

    #[test]
    fn test_generate_v2_output_with_content_md() {
        let adr = make_v2_adr("adr-001", "Use App Router");
        let output = generate_v2_output(&[adr]);

        // Should generate 2 files: docs/adr/ and .claude/rules/
        assert_eq!(output.raw_files.len(), 2);

        let docs_file = output
            .raw_files
            .iter()
            .find(|f| f.path.starts_with("docs/adr/"))
            .expect("expected docs/adr/ file");
        assert_eq!(docs_file.path, "docs/adr/use-app-router.md");
        assert_eq!(docs_file.content, "# Title\n\nContent here.");

        let rules_file = output
            .raw_files
            .iter()
            .find(|f| f.path.starts_with(".claude/rules/"))
            .expect("expected .claude/rules/ file");
        assert_eq!(rules_file.path, ".claude/rules/use-app-router.md");
        assert!(rules_file.content.contains("adr-id=\"adr-001\""));
        assert!(rules_file.content.contains("<!-- ADR: Use App Router -->"));
    }

    #[test]
    fn test_generate_v2_output_without_content_md_falls_back() {
        let mut adr = make_v1_adr("adr-002", "Fallback ADR");
        adr.schema_version = Some(2);
        adr.content_md = None;
        adr.context = Some("Some context here.".to_string());
        let output = generate_v2_output(&[adr]);

        assert_eq!(output.raw_files.len(), 2);
        let docs_file = output
            .raw_files
            .iter()
            .find(|f| f.path.starts_with("docs/adr/"))
            .unwrap();
        // Should contain title and context
        assert!(docs_file.content.contains("# Fallback ADR"));
        assert!(docs_file.content.contains("Some context here."));
        assert!(docs_file.content.contains("Do something"));
    }

    #[test]
    fn test_generate_v2_output_with_content_json_agent_documentation() {
        let mut adr = make_v2_adr("adr-003", "JSON Rules ADR");
        adr.content_json = Some(serde_json::json!({
            "agent_documentation": "Always use structured logging."
        }));
        let output = generate_v2_output(&[adr]);

        let rules_file = output
            .raw_files
            .iter()
            .find(|f| f.path.starts_with(".claude/rules/"))
            .unwrap();
        assert!(rules_file
            .content
            .contains("Always use structured logging."));
        // Should NOT contain bullet-point policies
        assert!(!rules_file.content.contains("- Policy one"));
    }

    #[test]
    fn test_generate_v2_output_with_content_json_enforcement() {
        let mut adr = make_v2_adr("adr-004", "Enforcement ADR");
        adr.content_json = Some(serde_json::json!({
            "enforcement": "Enforce strict null checks."
        }));
        let output = generate_v2_output(&[adr]);

        let rules_file = output
            .raw_files
            .iter()
            .find(|f| f.path.starts_with(".claude/rules/"))
            .unwrap();
        assert!(rules_file.content.contains("Enforce strict null checks."));
    }

    #[test]
    fn test_generate_v2_output_rules_fallback_to_policies() {
        let mut adr = make_v2_adr("adr-005", "Policy Fallback");
        adr.content_json = None;
        let output = generate_v2_output(&[adr]);

        let rules_file = output
            .raw_files
            .iter()
            .find(|f| f.path.starts_with(".claude/rules/"))
            .unwrap();
        // Should contain policies as bullet points
        assert!(rules_file.content.contains("- Policy one"));
    }

    #[test]
    fn test_generate_v2_output_rules_file_has_frontmatter() {
        let adr = make_v2_adr("adr-006", "Frontmatter Test");
        let output = generate_v2_output(&[adr]);

        let rules_file = output
            .raw_files
            .iter()
            .find(|f| f.path.starts_with(".claude/rules/"))
            .unwrap();
        assert!(
            rules_file.content.starts_with("---\nglob:"),
            "rules file should start with frontmatter"
        );
        assert!(rules_file.content.contains("**/*.{ts,js,tsx,jsx}"));
    }

    #[test]
    fn test_generate_v2_output_governance_block() {
        let adr = make_v2_adr("adr-007", "Gov Test");
        let output = generate_v2_output(&[adr]);
        assert!(output.governance_content.contains("<adr_governance"));
        assert!(output.governance_content.contains("docs/adr/"));
        assert!(output.governance_content.contains("@docs/adr/"));
        assert!(output.governance_content.contains("</adr_governance>"));
    }

    #[test]
    fn test_generate_v2_output_multiple_adrs() {
        let adrs = vec![
            make_v2_adr("adr-a", "First ADR"),
            make_v2_adr("adr-b", "Second ADR"),
        ];
        let output = generate_v2_output(&adrs);
        // 2 ADRs × 2 files each = 4 files
        assert_eq!(output.raw_files.len(), 4);
    }

    // ── write_v2_raw_files tests ──

    #[test]
    fn test_write_v2_raw_files_success_created() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let files = vec![V2RawFile {
            path: "docs/adr/my-adr.md".to_string(),
            content: "# My ADR\n\nContent.".to_string(),
        }];

        let results = write_v2_raw_files(dir.path(), &files);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].action, WriteAction::Created);
        assert_eq!(results[0].version, 0);
        assert!(results[0].error.is_none());

        let written = std::fs::read_to_string(dir.path().join("docs/adr/my-adr.md")).unwrap();
        assert_eq!(written, "# My ADR\n\nContent.");
    }

    #[test]
    fn test_write_v2_raw_files_success_updated() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        // Pre-create the file so it gets Updated
        std::fs::create_dir_all(dir.path().join("docs/adr")).unwrap();
        std::fs::write(dir.path().join("docs/adr/existing.md"), "old content").unwrap();

        let files = vec![V2RawFile {
            path: "docs/adr/existing.md".to_string(),
            content: "new content".to_string(),
        }];

        let results = write_v2_raw_files(dir.path(), &files);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].action, WriteAction::Updated);
        assert_eq!(results[0].version, 0);
        assert!(results[0].error.is_none());

        let written = std::fs::read_to_string(dir.path().join("docs/adr/existing.md")).unwrap();
        assert_eq!(written, "new content");
    }

    #[test]
    fn test_write_v2_raw_files_path_traversal_fails() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let files = vec![V2RawFile {
            path: "../escape.md".to_string(),
            content: "malicious".to_string(),
        }];

        let results = write_v2_raw_files(dir.path(), &files);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].action, WriteAction::Failed);
        assert_eq!(results[0].version, 0);
        let err = results[0].error.as_ref().unwrap();
        assert!(err.contains("path traversal"), "got: {err}");
    }

    #[test]
    #[cfg(unix)]
    fn test_write_v2_raw_files_absolute_path_fails() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let files = vec![V2RawFile {
            path: "/etc/passwd".to_string(),
            content: "malicious".to_string(),
        }];

        let results = write_v2_raw_files(dir.path(), &files);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].action, WriteAction::Failed);
        let err = results[0].error.as_ref().unwrap();
        assert!(err.contains("absolute paths are not allowed"), "got: {err}");
    }

    #[test]
    fn test_write_v2_raw_files_root_dir_nonexistent_fails() {
        let nonexistent = Path::new("/tmp/__nonexistent_v2_test_dir__");
        let files = vec![V2RawFile {
            path: "docs/adr/test.md".to_string(),
            content: "content".to_string(),
        }];

        let results = write_v2_raw_files(nonexistent, &files);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].action, WriteAction::Failed);
        let err = results[0].error.as_ref().unwrap();
        assert!(
            err.contains("Failed to canonicalize root directory"),
            "got: {err}"
        );
    }

    #[test]
    fn test_write_v2_raw_files_directory_create_failure() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        // Create a regular file where the parent directory needs to be
        std::fs::write(dir.path().join("blocker"), "I am a file").unwrap();

        let files = vec![V2RawFile {
            path: "blocker/subdir/test.md".to_string(),
            content: "content".to_string(),
        }];

        let results = write_v2_raw_files(dir.path(), &files);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].action, WriteAction::Failed);
        let err = results[0].error.as_ref().unwrap();
        assert!(err.contains("Failed to create directory"), "got: {err}");
    }

    #[test]
    #[cfg(unix)]
    fn test_write_v2_raw_files_write_failure() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("failed to create temp dir");
        // Create a read-only directory so writing fails
        let subdir = dir.path().join("readonly");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::set_permissions(&subdir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let files = vec![V2RawFile {
            path: "readonly/test.md".to_string(),
            content: "content".to_string(),
        }];

        let results = write_v2_raw_files(dir.path(), &files);

        // Restore permissions for cleanup
        std::fs::set_permissions(&subdir, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].action, WriteAction::Failed);
        let err = results[0].error.as_ref().unwrap();
        assert!(err.contains("Failed to write file"), "got: {err}");
    }

    #[test]
    #[cfg(unix)]
    fn test_write_v2_raw_files_canonicalization_symlink_escape() {
        use std::os::unix::fs::symlink;

        let outer_dir = tempfile::tempdir().expect("failed to create outer temp dir");
        let inner_dir = tempfile::tempdir().expect("failed to create inner temp dir");

        // Create a symlink inside inner_dir that points outside (to outer_dir)
        let symlink_path = inner_dir.path().join("escape");
        symlink(outer_dir.path(), &symlink_path).expect("failed to create symlink");

        let files = vec![V2RawFile {
            path: "escape/malicious.md".to_string(),
            content: "malicious content".to_string(),
        }];

        let results = write_v2_raw_files(inner_dir.path(), &files);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].action, WriteAction::Failed);
        let err = results[0].error.as_ref().unwrap();
        assert!(err.contains("path escapes root directory"), "got: {err}");
    }

    #[test]
    fn test_write_v2_raw_files_multiple_files() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let files = vec![
            V2RawFile {
                path: "docs/adr/adr-one.md".to_string(),
                content: "# ADR One".to_string(),
            },
            V2RawFile {
                path: ".claude/rules/adr-one.md".to_string(),
                content: "---\nglob: \"**/*\"\n---\n\nRules.".to_string(),
            },
        ];

        let results = write_v2_raw_files(dir.path(), &files);
        assert_eq!(results.len(), 2);
        for r in &results {
            assert_eq!(r.action, WriteAction::Created);
            assert!(r.error.is_none());
        }
    }

    // ── inject_v2_governance tests ──

    #[test]
    fn test_inject_v2_governance_root_file_exists() {
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "CLAUDE.md".to_string(),
                sections: vec![AdrSection {
                    adr_id: "adr-001".to_string(),
                    content: "Existing content".to_string(),
                }],
                reasoning: "some reason".to_string(),
            }],
            skipped_adrs: vec![],
            summary: TailoringSummary::default(),
        };

        let result = inject_v2_governance(
            output,
            "governance content".to_string(),
            &OutputFormat::ClaudeMd,
        );

        assert_eq!(result.files.len(), 1);
        let root = &result.files[0];
        assert_eq!(root.path, "CLAUDE.md");
        // Governance should be the FIRST section
        assert_eq!(root.sections[0].adr_id, "v2-governance");
        assert_eq!(root.sections[0].content, "governance content");
        // Original section should still be there
        assert_eq!(root.sections[1].adr_id, "adr-001");
    }

    #[test]
    fn test_inject_v2_governance_root_file_does_not_exist() {
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "apps/web/CLAUDE.md".to_string(),
                sections: vec![AdrSection {
                    adr_id: "adr-001".to_string(),
                    content: "Web content".to_string(),
                }],
                reasoning: "web reason".to_string(),
            }],
            skipped_adrs: vec![],
            summary: TailoringSummary::default(),
        };

        let result = inject_v2_governance(
            output,
            "governance content".to_string(),
            &OutputFormat::ClaudeMd,
        );

        // Should have created a new root file at index 0
        assert_eq!(result.files.len(), 2);
        let root = &result.files[0];
        assert_eq!(root.path, "CLAUDE.md");
        assert_eq!(root.sections.len(), 1);
        assert_eq!(root.sections[0].adr_id, "v2-governance");
        assert_eq!(root.sections[0].content, "governance content");

        // Original sub-file still present
        assert_eq!(result.files[1].path, "apps/web/CLAUDE.md");
    }

    #[test]
    fn test_inject_v2_governance_empty_output() {
        let output = TailoringOutput {
            files: vec![],
            skipped_adrs: vec![],
            summary: TailoringSummary::default(),
        };

        let result = inject_v2_governance(
            output,
            "governance content".to_string(),
            &OutputFormat::ClaudeMd,
        );

        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].path, "CLAUDE.md");
        assert_eq!(result.files[0].sections[0].adr_id, "v2-governance");
    }

    #[test]
    fn test_inject_v2_governance_agents_md_format() {
        let output = TailoringOutput {
            files: vec![FileOutput {
                path: "AGENTS.md".to_string(),
                sections: vec![AdrSection {
                    adr_id: "adr-a".to_string(),
                    content: "Agent content".to_string(),
                }],
                reasoning: "agent reason".to_string(),
            }],
            skipped_adrs: vec![],
            summary: TailoringSummary::default(),
        };

        let result = inject_v2_governance(
            output,
            "agents governance".to_string(),
            &OutputFormat::AgentsMd,
        );

        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].path, "AGENTS.md");
        assert_eq!(result.files[0].sections[0].adr_id, "v2-governance");
        assert_eq!(result.files[0].sections[0].content, "agents governance");
        assert_eq!(result.files[0].sections[1].adr_id, "adr-a");
    }

    #[test]
    fn test_inject_v2_governance_skipped_adrs_preserved() {
        let output = TailoringOutput {
            files: vec![],
            skipped_adrs: vec![SkippedAdr {
                id: "adr-skip".to_string(),
                reason: "not applicable".to_string(),
            }],
            summary: TailoringSummary {
                total_input: 1,
                applicable: 0,
                not_applicable: 1,
                files_generated: 0,
            },
        };

        let result = inject_v2_governance(output, "gov".to_string(), &OutputFormat::ClaudeMd);

        assert_eq!(result.skipped_adrs.len(), 1);
        assert_eq!(result.skipped_adrs[0].id, "adr-skip");
    }
}
