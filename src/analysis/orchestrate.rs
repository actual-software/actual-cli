use std::path::Path;

use crate::analysis::static_analyzer::frameworks::{
    detect_config_frameworks, detect_frameworks, detect_package_manager,
};
use crate::analysis::static_analyzer::languages::detect_languages;
use crate::analysis::static_analyzer::manifests::parse_dependencies;
use crate::analysis::static_analyzer::monorepo::detect_monorepo;
use crate::analysis::types::{Framework, Language, Project, RepoAnalysis};
use crate::error::ActualError;

/// Normalize a project path relative to a working directory.
///
/// Converts absolute paths to relative paths, strips leading `./` and trailing
/// `/`, and normalizes empty strings to `"."`.  Also handles macOS's
/// `/private/var` ↔ `/var` symlink by trying both the raw and canonicalized
/// `working_dir` when stripping prefixes.
///
/// `canonical_dir` should be `working_dir.canonicalize().ok()` — passed in
/// so the caller can compute it once for all projects.
pub(crate) fn normalize_project_path(
    path: &str,
    working_dir: &Path,
    canonical_dir: Option<&Path>,
) -> String {
    let p = Path::new(path);

    // If absolute, try to make relative to working_dir
    if p.is_absolute() {
        if let Ok(relative) = p.strip_prefix(working_dir) {
            return relative_or_dot(relative);
        }
        // Also try with canonicalized working_dir (macOS /var -> /private/var)
        if let Some(canonical) = canonical_dir {
            if let Ok(relative) = p.strip_prefix(canonical) {
                return relative_or_dot(relative);
            }
        }
        // Can't make relative — leave as-is (edge case)
        return path.to_string();
    }

    // Strip leading "./"
    let path = path.strip_prefix("./").unwrap_or(path);

    // Strip trailing "/"
    let path = path.strip_suffix('/').unwrap_or(path);

    // Empty string -> "."
    if path.is_empty() {
        return ".".to_string();
    }

    path.to_string()
}

/// Convert a relative path to a string, using `"."` for the empty path.
fn relative_or_dot(relative: &Path) -> String {
    if relative == Path::new("") {
        ".".to_string()
    } else {
        relative.to_string_lossy().to_string()
    }
}

/// Normalize all project paths in an analysis result.
///
/// Canonicalization of `working_dir` may fail if the directory doesn't exist
/// or isn't accessible. In that case the canonical fallback is simply skipped
/// and only the raw `working_dir` prefix is tried — absolute paths that only
/// match via the canonical form will pass through unchanged.
fn normalize_analysis(analysis: &mut RepoAnalysis, working_dir: &Path) {
    let canonical = working_dir.canonicalize().ok();
    for project in &mut analysis.projects {
        project.path = normalize_project_path(&project.path, working_dir, canonical.as_deref());
    }
}

/// Run repository analysis using deterministic static analysis.
///
/// Layer 1: Fast manifest-based analysis (< 500ms, no external tools).
/// 1. Detect monorepo structure → project paths
/// 2. For each project: detect languages, parse manifests, detect frameworks
/// 3. Assemble RepoAnalysis with normalized paths
pub fn run_static_analysis(working_dir: &Path) -> Result<RepoAnalysis, ActualError> {
    let mono_info = detect_monorepo(working_dir)
        .map_err(|e| ActualError::ConfigError(format!("Monorepo detection failed: {e}")))?;

    let mut projects = Vec::new();
    for project_info in &mono_info.projects {
        let project_dir = working_dir.join(&project_info.path);

        // Detect languages via tokei
        let languages: Vec<Language> = detect_languages(&project_dir)
            .into_iter()
            .map(|(lang, _loc)| lang)
            .collect();

        // Parse manifests for dependencies
        let deps = parse_dependencies(&project_dir);

        // Detect frameworks from deps + config files
        let mut frameworks = detect_frameworks(&deps, &project_dir);
        let config_frameworks = detect_config_frameworks(&project_dir);
        for cf in config_frameworks {
            if !frameworks.iter().any(|f| f.name == cf.name) {
                frameworks.push(cf);
            }
        }

        // Detect package manager from lockfiles
        let package_manager = detect_package_manager(&project_dir);

        // Build description from detected info
        let description = build_project_description(&languages, &frameworks);

        projects.push(Project {
            path: project_info.path.clone(),
            name: project_info.name.clone(),
            languages,
            frameworks,
            package_manager,
            description,
        });
    }

    if projects.is_empty() {
        return Err(ActualError::AnalysisEmpty);
    }

    let mut analysis = RepoAnalysis {
        is_monorepo: mono_info.is_monorepo,
        projects,
    };

    normalize_analysis(&mut analysis, working_dir);
    Ok(analysis)
}

/// Build a human-readable description from detected languages and frameworks.
fn build_project_description(languages: &[Language], frameworks: &[Framework]) -> Option<String> {
    if languages.is_empty() && frameworks.is_empty() {
        return None;
    }
    let lang_str: Vec<String> = languages.iter().map(|l| l.to_string()).collect();
    let fw_str: Vec<String> = frameworks.iter().map(|f| f.name.clone()).collect();

    let mut parts = Vec::new();
    if !lang_str.is_empty() {
        parts.push(lang_str.join(", "));
    }
    if !fw_str.is_empty() {
        parts.push(format!("using {}", fw_str.join(", ")));
    }
    Some(parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::types::FrameworkCategory;

    // -- normalize_project_path tests --

    #[test]
    fn test_normalize_absolute_path_matching_working_dir() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(
            normalize_project_path("/home/user/project", working_dir, None),
            "."
        );
    }

    #[test]
    fn test_normalize_absolute_path_subdir_of_working_dir() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(
            normalize_project_path("/home/user/project/apps/web", working_dir, None),
            "apps/web"
        );
    }

    #[test]
    fn test_normalize_canonical_fallback_root_match() {
        let working_dir = Path::new("/var/folders/X");
        let canonical = Path::new("/private/var/folders/X");
        assert_eq!(
            normalize_project_path("/private/var/folders/X", working_dir, Some(canonical)),
            "."
        );
    }

    #[test]
    fn test_normalize_canonical_fallback_subdir() {
        let working_dir = Path::new("/var/folders/X");
        let canonical = Path::new("/private/var/folders/X");
        assert_eq!(
            normalize_project_path(
                "/private/var/folders/X/apps/web",
                working_dir,
                Some(canonical)
            ),
            "apps/web"
        );
    }

    #[test]
    fn test_normalize_canonical_none_falls_through() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(
            normalize_project_path("/other/path", working_dir, None),
            "/other/path"
        );
    }

    #[test]
    fn test_normalize_canonical_some_but_no_match_falls_through() {
        let working_dir = Path::new("/home/user/project");
        let canonical = Path::new("/real/home/user/project");
        assert_eq!(
            normalize_project_path("/unrelated/path", working_dir, Some(canonical)),
            "/unrelated/path"
        );
    }

    #[test]
    fn test_normalize_strip_leading_dot_slash() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(
            normalize_project_path("./apps/web", working_dir, None),
            "apps/web"
        );
    }

    #[test]
    fn test_normalize_strip_trailing_slash() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(
            normalize_project_path("apps/web/", working_dir, None),
            "apps/web"
        );
    }

    #[test]
    fn test_normalize_empty_to_dot() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(normalize_project_path("", working_dir, None), ".");
    }

    #[test]
    fn test_normalize_dot_unchanged() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(normalize_project_path(".", working_dir, None), ".");
    }

    #[test]
    fn test_normalize_relative_path_unchanged() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(
            normalize_project_path("apps/web", working_dir, None),
            "apps/web"
        );
    }

    #[test]
    fn test_normalize_absolute_path_unrelated_to_working_dir() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(
            normalize_project_path("/other/path/entirely", working_dir, None),
            "/other/path/entirely"
        );
    }

    #[test]
    fn test_normalize_dot_slash_only() {
        let working_dir = Path::new("/home/user/project");
        assert_eq!(normalize_project_path("./", working_dir, None), ".");
    }

    // -- run_static_analysis tests --

    #[test]
    fn test_static_analysis_rust_project() {
        let dir = tempfile::tempdir().unwrap();
        // Create a Cargo.toml so monorepo detection finds a single project
        std::fs::write(
            dir.path().join("Cargo.toml"),
            r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"

[dependencies]
actix-web = "4"
"#,
        )
        .unwrap();
        // Create a Cargo.lock so package manager is detected
        std::fs::write(dir.path().join("Cargo.lock"), "# lock file").unwrap();
        // Create a Rust source file so language detection picks it up
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        let result = run_static_analysis(dir.path()).unwrap();

        assert!(!result.is_monorepo);
        assert_eq!(result.projects.len(), 1);
        assert_eq!(result.projects[0].path, ".");
        assert!(result.projects[0].languages.contains(&Language::Rust));
        assert_eq!(result.projects[0].package_manager.as_deref(), Some("cargo"));
    }

    #[test]
    fn test_static_analysis_js_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name": "my-app", "dependencies": {"react": "^18.0.0"}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("package-lock.json"), "{}").unwrap();
        std::fs::write(
            dir.path().join("index.js"),
            "const x = 1;\nconsole.log(x);\n",
        )
        .unwrap();

        let result = run_static_analysis(dir.path()).unwrap();

        assert!(!result.is_monorepo);
        assert_eq!(result.projects.len(), 1);
        assert_eq!(result.projects[0].package_manager.as_deref(), Some("npm"));
    }

    #[test]
    fn test_static_analysis_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        // Empty dir — monorepo detection returns a single project with no manifest,
        // but there's still one project entry (the root). It should NOT be empty.
        // However, detect_monorepo always returns at least one project (the root).
        // So AnalysisEmpty won't trigger from an empty dir.
        let result = run_static_analysis(dir.path());
        // Should succeed with one project that has no languages
        assert!(result.is_ok());
        let analysis = result.unwrap();
        assert_eq!(analysis.projects.len(), 1);
        assert_eq!(analysis.projects[0].path, ".");
    }

    #[test]
    fn test_static_analysis_monorepo_detection() {
        let dir = tempfile::tempdir().unwrap();
        // Create a pnpm workspace monorepo
        std::fs::write(
            dir.path().join("pnpm-workspace.yaml"),
            "packages:\n  - apps/*\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"name": "monorepo"}"#).unwrap();
        // Create sub-projects
        std::fs::create_dir_all(dir.path().join("apps/web")).unwrap();
        std::fs::write(
            dir.path().join("apps/web/package.json"),
            r#"{"name": "web-app", "dependencies": {"react": "^18.0.0"}}"#,
        )
        .unwrap();

        let result = run_static_analysis(dir.path()).unwrap();

        assert!(result.is_monorepo);
        assert!(!result.projects.is_empty());
    }

    #[test]
    fn test_static_analysis_normalizes_paths() {
        // Use a real temp directory so canonicalize() succeeds inside
        // normalize_analysis, exercising the Ok path of canonicalize.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            r#"[package]
name = "test-proj"
version = "0.1.0"
"#,
        )
        .unwrap();

        let result = run_static_analysis(dir.path()).unwrap();

        // Path should be normalized to "."
        assert_eq!(result.projects[0].path, ".");
    }

    #[test]
    fn test_static_analysis_monorepo_io_error() {
        // detect_monorepo returns std::io::Error on malformed workspace files
        let dir = tempfile::tempdir().unwrap();
        // Write an invalid pnpm-workspace.yaml that will parse but is malformed
        std::fs::write(
            dir.path().join("pnpm-workspace.yaml"),
            "{{invalid yaml content",
        )
        .unwrap();

        let result = run_static_analysis(dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("Monorepo detection failed"),
            "expected 'Monorepo detection failed' in: {}",
            err
        );
    }

    // -- build_project_description tests --

    #[test]
    fn test_build_description_empty() {
        assert_eq!(build_project_description(&[], &[]), None);
    }

    #[test]
    fn test_build_description_languages_only() {
        let langs = vec![Language::Rust, Language::Python];
        let desc = build_project_description(&langs, &[]).unwrap();
        assert_eq!(desc, "rust, python");
    }

    #[test]
    fn test_build_description_frameworks_only() {
        let fws = vec![Framework {
            name: "actix-web".to_string(),
            category: FrameworkCategory::WebBackend,
        }];
        let desc = build_project_description(&[], &fws).unwrap();
        assert_eq!(desc, "using actix-web");
    }

    #[test]
    fn test_build_description_both() {
        let langs = vec![Language::Rust];
        let fws = vec![Framework {
            name: "actix-web".to_string(),
            category: FrameworkCategory::WebBackend,
        }];
        let desc = build_project_description(&langs, &fws).unwrap();
        assert_eq!(desc, "rust using actix-web");
    }

    #[test]
    fn test_build_description_multiple_frameworks() {
        let langs = vec![Language::TypeScript];
        let fws = vec![
            Framework {
                name: "react".to_string(),
                category: FrameworkCategory::WebFrontend,
            },
            Framework {
                name: "nextjs".to_string(),
                category: FrameworkCategory::WebFrontend,
            },
        ];
        let desc = build_project_description(&langs, &fws).unwrap();
        assert_eq!(desc, "typescript using react, nextjs");
    }

    #[test]
    fn test_static_analysis_config_frameworks_dedup() {
        // Test that config-detected frameworks are deduplicated with dep-detected ones
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name": "app", "dependencies": {"next": "^14.0.0"}}"#,
        )
        .unwrap();
        // Also create next.config.js (config detection)
        std::fs::write(dir.path().join("next.config.js"), "module.exports = {}").unwrap();
        std::fs::write(dir.path().join("index.js"), "const x = 1;\n").unwrap();

        let result = run_static_analysis(dir.path()).unwrap();

        // nextjs should appear only once, not duplicated
        let nextjs_count = result.projects[0]
            .frameworks
            .iter()
            .filter(|f| f.name == "nextjs")
            .count();
        assert_eq!(
            nextjs_count, 1,
            "nextjs should be deduplicated, found {nextjs_count}"
        );
    }
}
