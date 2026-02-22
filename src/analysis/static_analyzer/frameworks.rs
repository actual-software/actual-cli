use std::collections::HashSet;
use std::path::Path;

use crate::analysis::types::{Framework, FrameworkCategory};

use super::manifests::{DependencyInfo, ManifestSource};
use super::registry;

/// Detect frameworks from parsed dependency information.
///
/// Looks up each dependency (both production and dev) against the framework
/// registry, then merges with config-file-based detection. Results are
/// deduplicated by framework name and sorted alphabetically.
pub fn detect_frameworks(deps: &DependencyInfo, project_dir: &Path) -> Vec<Framework> {
    let mut seen = HashSet::new();
    let mut frameworks = Vec::new();

    // Check production + dev dependencies against the registry
    let all_deps = deps.dependencies.iter().chain(deps.dev_dependencies.iter());
    for dep in all_deps {
        if let Some(sig) = registry::lookup(dep) {
            if seen.insert(sig.framework_name.to_string()) {
                frameworks.push(Framework {
                    name: sig.framework_name.to_string(),
                    category: FrameworkCategory::from_str_insensitive(sig.category),
                    source: deps.sources.get(dep).cloned(),
                });
            }
        }
    }

    // Merge config-file-based detection
    for fw in detect_config_frameworks(project_dir) {
        if seen.insert(fw.name.clone()) {
            frameworks.push(fw);
        }
    }

    frameworks.sort_by(|a, b| a.name.cmp(&b.name));
    frameworks
}

/// Detect frameworks from the presence of configuration files.
///
/// Checks for well-known config files that indicate framework usage even
/// when the framework may not appear as a direct dependency.
pub fn detect_config_frameworks(project_dir: &Path) -> Vec<Framework> {
    let mut frameworks = Vec::new();

    let config_checks: &[(&[&str], &str, &str)] = &[
        (
            &[
                "next.config.js",
                "next.config.mjs",
                "next.config.ts",
                "next.config.cjs",
            ],
            "nextjs",
            "web-frontend",
        ),
        (&["angular.json"], "angular", "web-frontend"),
        (&["vue.config.js", "vue.config.ts"], "vue", "web-frontend"),
        (
            &[
                "vite.config.js",
                "vite.config.ts",
                "vite.config.mjs",
                "vite.config.cjs",
            ],
            "vite",
            "build-system",
        ),
        (
            &[
                "tailwind.config.js",
                "tailwind.config.ts",
                "tailwind.config.cjs",
                "tailwind.config.mjs",
            ],
            "tailwindcss",
            "web-frontend",
        ),
        (
            &["docker-compose.yml", "docker-compose.yaml"],
            "docker-compose",
            "devops",
        ),
        (&["Dockerfile"], "docker", "devops"),
    ];

    for (files, name, category) in config_checks {
        if files.iter().any(|f| project_dir.join(f).exists()) {
            frameworks.push(Framework {
                name: name.to_string(),
                category: FrameworkCategory::from_str_insensitive(category),
                source: Some(ManifestSource::ConfigFile),
            });
        }
    }

    // Check for Terraform files (*.tf)
    if let Ok(entries) = std::fs::read_dir(project_dir) {
        let has_tf = entries
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().is_some_and(|ext| ext == "tf"));
        if has_tf {
            frameworks.push(Framework {
                name: "terraform".to_string(),
                category: FrameworkCategory::from_str_insensitive("devops"),
                source: Some(ManifestSource::ConfigFile),
            });
        }
    }

    // Check for GitHub Actions
    if project_dir.join(".github/workflows").is_dir() {
        frameworks.push(Framework {
            name: "github-actions".to_string(),
            category: FrameworkCategory::from_str_insensitive("devops"),
            source: Some(ManifestSource::ConfigFile),
        });
    }

    frameworks
}

/// Detect the package manager from lock files.
///
/// Returns the first matching package manager name, or `None` if no
/// recognized lock file is found.
pub fn detect_package_manager(project_dir: &Path) -> Option<String> {
    let lock_files: &[(&str, &str)] = &[
        ("package-lock.json", "npm"),
        ("yarn.lock", "yarn"),
        ("pnpm-lock.yaml", "pnpm"),
        ("Cargo.lock", "cargo"),
        ("poetry.lock", "poetry"),
        ("Pipfile.lock", "pipenv"),
        ("go.sum", "go"),
        ("Gemfile.lock", "bundler"),
        ("composer.lock", "composer"),
    ];

    for (file, manager) in lock_files {
        if project_dir.join(file).exists() {
            return Some(manager.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_detect_frameworks_from_dependencies() {
        let dir = tempdir().unwrap();
        let deps = DependencyInfo {
            dependencies: vec!["react".to_string(), "express".to_string()],
            dev_dependencies: vec!["jest".to_string()],
            ..Default::default()
        };

        let frameworks = detect_frameworks(&deps, dir.path());
        let names: Vec<&str> = frameworks.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"react"));
        assert!(names.contains(&"express"));
        assert!(names.contains(&"jest"));
    }

    #[test]
    fn test_detect_frameworks_categories() {
        let dir = tempdir().unwrap();
        let deps = DependencyInfo {
            dependencies: vec!["react".to_string(), "django".to_string()],
            dev_dependencies: vec![],
            ..Default::default()
        };

        let frameworks = detect_frameworks(&deps, dir.path());
        let react = frameworks.iter().find(|f| f.name == "react").unwrap();
        assert_eq!(react.category, FrameworkCategory::WebFrontend);
        let django = frameworks.iter().find(|f| f.name == "django").unwrap();
        assert_eq!(django.category, FrameworkCategory::WebBackend);
    }

    #[test]
    fn test_detect_config_frameworks() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("next.config.js"), "module.exports = {}").unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM node:18").unwrap();

        let frameworks = detect_config_frameworks(dir.path());
        let names: Vec<&str> = frameworks.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"nextjs"));
        assert!(names.contains(&"docker"));
    }

    #[test]
    fn test_detect_config_frameworks_terraform() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("main.tf"), "resource {}").unwrap();

        let frameworks = detect_config_frameworks(dir.path());
        let names: Vec<&str> = frameworks.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"terraform"));
    }

    #[test]
    fn test_detect_config_frameworks_github_actions() {
        let dir = tempdir().unwrap();
        let workflows = dir.path().join(".github/workflows");
        fs::create_dir_all(&workflows).unwrap();
        fs::write(workflows.join("ci.yml"), "name: CI").unwrap();

        let frameworks = detect_config_frameworks(dir.path());
        let names: Vec<&str> = frameworks.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"github-actions"));
    }

    #[test]
    fn test_detect_package_manager() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("yarn.lock"), "").unwrap();
        assert_eq!(detect_package_manager(dir.path()), Some("yarn".to_string()));
    }

    #[test]
    fn test_detect_package_manager_cargo() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.lock"), "").unwrap();
        assert_eq!(
            detect_package_manager(dir.path()),
            Some("cargo".to_string())
        );
    }

    #[test]
    fn test_detect_package_manager_none() {
        let dir = tempdir().unwrap();
        assert_eq!(detect_package_manager(dir.path()), None);
    }

    #[test]
    fn test_deduplication() {
        let dir = tempdir().unwrap();
        // Create next.config.js so nextjs comes from config detection too
        fs::write(dir.path().join("next.config.js"), "module.exports = {}").unwrap();

        let deps = DependencyInfo {
            dependencies: vec!["next".to_string()],
            dev_dependencies: vec![],
            ..Default::default()
        };

        let frameworks = detect_frameworks(&deps, dir.path());
        let nextjs_count = frameworks.iter().filter(|f| f.name == "nextjs").count();
        assert_eq!(nextjs_count, 1, "nextjs should appear exactly once");
    }

    #[test]
    fn test_frameworks_sorted() {
        let dir = tempdir().unwrap();
        let deps = DependencyInfo {
            dependencies: vec![
                "webpack".to_string(),
                "react".to_string(),
                "express".to_string(),
            ],
            dev_dependencies: vec![],
            ..Default::default()
        };

        let frameworks = detect_frameworks(&deps, dir.path());
        let names: Vec<&str> = frameworks.iter().map(|f| f.name.as_str()).collect();
        let mut sorted_names = names.clone();
        sorted_names.sort();
        assert_eq!(names, sorted_names, "frameworks should be sorted");
    }

    #[test]
    fn test_empty_deps_with_no_config() {
        let dir = tempdir().unwrap();
        let deps = DependencyInfo {
            dependencies: vec![],
            dev_dependencies: vec![],
            ..Default::default()
        };
        let frameworks = detect_frameworks(&deps, dir.path());
        assert!(frameworks.is_empty());
    }

    #[test]
    fn test_detect_config_no_terraform_files() {
        // Dir with files but no .tf — terraform should NOT be detected
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        fs::write(dir.path().join("README.md"), "# Hello").unwrap();

        let frameworks = detect_config_frameworks(dir.path());
        let names: Vec<&str> = frameworks.iter().map(|f| f.name.as_str()).collect();
        assert!(!names.contains(&"terraform"));
    }

    #[test]
    fn test_detect_config_docker_compose() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("docker-compose.yml"), "version: '3'").unwrap();

        let frameworks = detect_config_frameworks(dir.path());
        let names: Vec<&str> = frameworks.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"docker-compose"));
    }

    #[test]
    fn test_detect_config_angular() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("angular.json"), "{}").unwrap();

        let frameworks = detect_config_frameworks(dir.path());
        let names: Vec<&str> = frameworks.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"angular"));
    }

    #[test]
    fn test_detect_config_vue() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("vue.config.js"), "module.exports = {}").unwrap();

        let frameworks = detect_config_frameworks(dir.path());
        let names: Vec<&str> = frameworks.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"vue"));
    }

    #[test]
    fn test_detect_config_tailwind() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("tailwind.config.js"), "module.exports = {}").unwrap();

        let frameworks = detect_config_frameworks(dir.path());
        let names: Vec<&str> = frameworks.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"tailwindcss"));
    }

    #[test]
    fn test_detect_config_vite() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("vite.config.ts"), "export default {}").unwrap();

        let frameworks = detect_config_frameworks(dir.path());
        let names: Vec<&str> = frameworks.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"vite"));
    }

    #[test]
    fn test_detect_frameworks_config_only_no_deps() {
        // Config frameworks should be added via detect_frameworks
        // when there are no dependencies (tests the merge branch)
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM node:18").unwrap();

        let deps = DependencyInfo {
            dependencies: vec![],
            dev_dependencies: vec![],
            ..Default::default()
        };

        let frameworks = detect_frameworks(&deps, dir.path());
        let names: Vec<&str> = frameworks.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"docker"));
    }

    #[test]
    fn test_detect_config_nonexistent_dir() {
        // read_dir fails for nonexistent path — tests the Err branch of read_dir
        let frameworks = detect_config_frameworks(std::path::Path::new("/nonexistent/path/12345"));
        // Should not panic, just skip terraform detection
        assert!(!frameworks.iter().any(|f| f.name == "terraform"));
    }

    #[test]
    fn test_detect_unknown_dependency_not_in_registry() {
        let dir = tempdir().unwrap();
        let deps = DependencyInfo {
            dependencies: vec!["some-unknown-package".to_string()],
            dev_dependencies: vec![],
            ..Default::default()
        };

        let frameworks = detect_frameworks(&deps, dir.path());
        assert!(frameworks.is_empty());
    }

    #[test]
    fn test_source_propagated_from_dependency_info() {
        let dir = tempdir().unwrap();
        let mut sources = HashMap::new();
        sources.insert("tokio".to_string(), ManifestSource::CargoToml);
        let deps = DependencyInfo {
            dependencies: vec!["tokio".to_string()],
            dev_dependencies: vec![],
            sources,
        };

        let frameworks = detect_frameworks(&deps, dir.path());
        let tokio_fw = frameworks.iter().find(|f| f.name == "tokio").unwrap();
        assert_eq!(tokio_fw.source, Some(ManifestSource::CargoToml));
    }

    #[test]
    fn test_source_none_when_not_in_sources_map() {
        let dir = tempdir().unwrap();
        // "react" is in the registry but not in the sources map
        let deps = DependencyInfo {
            dependencies: vec!["react".to_string()],
            dev_dependencies: vec![],
            sources: HashMap::new(),
        };

        let frameworks = detect_frameworks(&deps, dir.path());
        let react_fw = frameworks.iter().find(|f| f.name == "react").unwrap();
        assert_eq!(react_fw.source, None);
    }

    #[test]
    fn test_config_framework_source_is_config_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM node:18").unwrap();

        let frameworks = detect_config_frameworks(dir.path());
        let docker = frameworks.iter().find(|f| f.name == "docker").unwrap();
        assert_eq!(docker.source, Some(ManifestSource::ConfigFile));
    }

    #[test]
    fn test_terraform_source_is_config_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("main.tf"), "resource {}").unwrap();

        let frameworks = detect_config_frameworks(dir.path());
        let tf = frameworks.iter().find(|f| f.name == "terraform").unwrap();
        assert_eq!(tf.source, Some(ManifestSource::ConfigFile));
    }

    #[test]
    fn test_github_actions_source_is_config_file() {
        let dir = tempdir().unwrap();
        let workflows = dir.path().join(".github/workflows");
        fs::create_dir_all(&workflows).unwrap();
        fs::write(workflows.join("ci.yml"), "name: CI").unwrap();

        let frameworks = detect_config_frameworks(dir.path());
        let gha = frameworks
            .iter()
            .find(|f| f.name == "github-actions")
            .unwrap();
        assert_eq!(gha.source, Some(ManifestSource::ConfigFile));
    }

    #[test]
    fn test_mixed_sources_registry_and_config() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM node:18").unwrap();

        let mut sources = HashMap::new();
        sources.insert("express".to_string(), ManifestSource::PackageJson);
        let deps = DependencyInfo {
            dependencies: vec!["express".to_string()],
            dev_dependencies: vec![],
            sources,
        };

        let frameworks = detect_frameworks(&deps, dir.path());
        let express = frameworks.iter().find(|f| f.name == "express").unwrap();
        assert_eq!(express.source, Some(ManifestSource::PackageJson));
        let docker = frameworks.iter().find(|f| f.name == "docker").unwrap();
        assert_eq!(docker.source, Some(ManifestSource::ConfigFile));
    }
}
