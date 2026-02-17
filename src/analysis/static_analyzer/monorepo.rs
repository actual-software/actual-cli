use std::fs;
use std::path::Path;

use glob::glob;
use serde::Deserialize;

/// Result of monorepo detection.
#[derive(Debug)]
pub struct MonorepoInfo {
    pub is_monorepo: bool,
    pub projects: Vec<ProjectInfo>,
}

/// Basic project info extracted from workspace analysis.
#[derive(Debug)]
pub struct ProjectInfo {
    /// Relative path from repo root (e.g., "packages/web", ".").
    pub path: String,
    /// Extracted project name.
    pub name: String,
}

/// Detect whether a repository root is a monorepo and enumerate its projects.
///
/// Checks workspace configuration files in priority order and returns the first
/// match. If no workspace config is found, returns a single-project result.
pub fn detect_monorepo(root: &Path) -> Result<MonorepoInfo, std::io::Error> {
    // PNPM workspaces
    if let Some(info) = detect_pnpm(root)? {
        return Ok(info);
    }

    // npm/yarn workspaces (also covers Nx and Turborepo which use package.json workspaces)
    if let Some(info) = detect_npm_workspaces(root)? {
        return Ok(info);
    }

    // Lerna
    if let Some(info) = detect_lerna(root)? {
        return Ok(info);
    }

    // Nx with workspace.json (only reached if no package.json workspaces)
    if root.join("nx.json").exists() {
        if let Some(info) = detect_workspace_json(root)? {
            return Ok(info);
        }
    }

    // Cargo workspace
    if let Some(info) = detect_cargo_workspace(root)? {
        return Ok(info);
    }

    // Go workspace
    if let Some(info) = detect_go_workspace(root)? {
        return Ok(info);
    }

    // Single project fallback
    Ok(MonorepoInfo {
        is_monorepo: false,
        projects: vec![ProjectInfo {
            path: ".".to_string(),
            name: extract_project_name(root),
        }],
    })
}

/// Detect PNPM workspaces from `pnpm-workspace.yaml`.
fn detect_pnpm(root: &Path) -> Result<Option<MonorepoInfo>, std::io::Error> {
    let path = root.join("pnpm-workspace.yaml");
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)?;

    #[derive(Deserialize)]
    struct PnpmWorkspace {
        packages: Option<Vec<String>>,
    }

    let workspace: PnpmWorkspace = serde_yaml::from_str(&content).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid pnpm-workspace.yaml: {e}"),
        )
    })?;

    let patterns = workspace.packages.unwrap_or_default();
    if patterns.is_empty() {
        return Ok(None);
    }

    let projects = expand_glob_patterns(root, &patterns);
    Ok(Some(MonorepoInfo {
        is_monorepo: true,
        projects,
    }))
}

/// Detect npm/yarn workspaces from `package.json`.
fn detect_npm_workspaces(root: &Path) -> Result<Option<MonorepoInfo>, std::io::Error> {
    let path = root.join("package.json");
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)?;
    let json: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid package.json: {e}"),
        )
    })?;

    let patterns = extract_workspace_patterns(&json);
    if patterns.is_empty() {
        return Ok(None);
    }

    let projects = expand_glob_patterns(root, &patterns);
    Ok(Some(MonorepoInfo {
        is_monorepo: true,
        projects,
    }))
}

/// Extract workspace glob patterns from a package.json value.
///
/// Handles both array format (`"workspaces": ["packages/*"]`) and
/// object format (`"workspaces": {"packages": ["apps/*"]}`).
fn extract_workspace_patterns(json: &serde_json::Value) -> Vec<String> {
    match json.get("workspaces") {
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        Some(serde_json::Value::Object(obj)) => {
            if let Some(serde_json::Value::Array(arr)) = obj.get("packages") {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            } else {
                vec![]
            }
        }
        _ => vec![],
    }
}

/// Detect Lerna workspaces from `lerna.json`.
fn detect_lerna(root: &Path) -> Result<Option<MonorepoInfo>, std::io::Error> {
    let path = root.join("lerna.json");
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)?;
    let json: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid lerna.json: {e}"),
        )
    })?;

    let patterns: Vec<String> = match json.get("packages") {
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => return Ok(None),
    };

    if patterns.is_empty() {
        return Ok(None);
    }

    let projects = expand_glob_patterns(root, &patterns);
    Ok(Some(MonorepoInfo {
        is_monorepo: true,
        projects,
    }))
}

/// Detect Nx workspace.json projects.
fn detect_workspace_json(root: &Path) -> Result<Option<MonorepoInfo>, std::io::Error> {
    let path = root.join("workspace.json");
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)?;
    let json: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid workspace.json: {e}"),
        )
    })?;

    let projects: Vec<ProjectInfo> = match json.get("projects") {
        Some(serde_json::Value::Object(obj)) => obj
            .iter()
            .filter_map(|(name, val)| {
                val.as_str().map(|p| ProjectInfo {
                    path: p.to_string(),
                    name: name.clone(),
                })
            })
            .collect(),
        _ => return Ok(None),
    };

    if projects.is_empty() {
        return Ok(None);
    }

    Ok(Some(MonorepoInfo {
        is_monorepo: true,
        projects,
    }))
}

/// Detect Cargo workspace from `Cargo.toml`.
fn detect_cargo_workspace(root: &Path) -> Result<Option<MonorepoInfo>, std::io::Error> {
    let path = root.join("Cargo.toml");
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)?;
    let toml_val: toml::Value = content.parse().map_err(|e: toml::de::Error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid Cargo.toml: {e}"),
        )
    })?;

    let members = match toml_val.get("workspace").and_then(|w| w.get("members")) {
        Some(toml::Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect::<Vec<_>>(),
        _ => return Ok(None),
    };

    if members.is_empty() {
        return Ok(None);
    }

    let projects = expand_glob_patterns(root, &members);
    Ok(Some(MonorepoInfo {
        is_monorepo: true,
        projects,
    }))
}

/// Detect Go workspace from `go.work`.
fn detect_go_workspace(root: &Path) -> Result<Option<MonorepoInfo>, std::io::Error> {
    let path = root.join("go.work");
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)?;
    let mut dirs = Vec::new();

    let mut in_use_block = false;
    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed == ")" {
            in_use_block = false;
            continue;
        }

        if in_use_block {
            if !trimmed.is_empty() && !trimmed.starts_with("//") {
                dirs.push(trimmed.to_string());
            }
            continue;
        }

        if trimmed.starts_with("use (") || trimmed == "use (" {
            in_use_block = true;
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("use ") {
            let dir = rest.trim();
            if !dir.is_empty() && !dir.starts_with("//") && dir != "(" {
                dirs.push(dir.to_string());
            }
        }
    }

    if dirs.is_empty() {
        return Ok(None);
    }

    let projects = dirs
        .into_iter()
        .filter(|d| root.join(d).is_dir())
        .map(|d| {
            let name = extract_project_name(&root.join(&d));
            ProjectInfo { path: d, name }
        })
        .collect();

    Ok(Some(MonorepoInfo {
        is_monorepo: true,
        projects,
    }))
}

/// Expand glob patterns relative to root and build project info for each match.
fn expand_glob_patterns(root: &Path, patterns: &[String]) -> Vec<ProjectInfo> {
    let mut projects = Vec::new();

    for pattern in patterns {
        let full_pattern = root.join(pattern);
        let pattern_str = full_pattern.to_string_lossy().to_string();

        let entries = match glob(&pattern_str) {
            Ok(paths) => paths,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            if !entry.is_dir() {
                continue;
            }

            let rel_path = entry
                .strip_prefix(root)
                .map(|rel| rel.to_string_lossy().to_string())
                .unwrap_or_else(|_| entry.to_string_lossy().to_string());

            let name = extract_project_name(&entry);
            projects.push(ProjectInfo {
                path: rel_path,
                name,
            });
        }
    }

    projects
}

/// Extract a project name from a directory by checking manifest files.
///
/// Checks in order: `package.json`, `Cargo.toml`, `pyproject.toml`, `go.mod`.
/// Falls back to the directory name.
fn extract_project_name(dir: &Path) -> String {
    if let Some(name) = name_from_package_json(dir) {
        return name;
    }
    if let Some(name) = name_from_cargo_toml(dir) {
        return name;
    }
    if let Some(name) = name_from_pyproject_toml(dir) {
        return name;
    }
    if let Some(name) = name_from_go_mod(dir) {
        return name;
    }

    // Fallback: directory name
    dir.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn name_from_package_json(dir: &Path) -> Option<String> {
    let path = dir.join("package.json");
    let content = fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json.get("name")?.as_str().map(String::from)
}

fn name_from_cargo_toml(dir: &Path) -> Option<String> {
    let path = dir.join("Cargo.toml");
    let content = fs::read_to_string(path).ok()?;
    let toml_val: toml::Value = content.parse().ok()?;
    toml_val
        .get("package")?
        .get("name")?
        .as_str()
        .map(String::from)
}

fn name_from_pyproject_toml(dir: &Path) -> Option<String> {
    let path = dir.join("pyproject.toml");
    let content = fs::read_to_string(path).ok()?;
    let toml_val: toml::Value = content.parse().ok()?;

    // Try [project].name first
    if let Some(name) = toml_val
        .get("project")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
    {
        return Some(name.to_string());
    }

    // Then [tool.poetry].name
    toml_val
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(String::from)
}

fn name_from_go_mod(dir: &Path) -> Option<String> {
    let path = dir.join("go.mod");
    let content = fs::read_to_string(path).ok()?;

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("module ") {
            let module = rest.trim();
            // Take the last segment after '/'
            let name = match module.rsplit_once('/') {
                Some((_, last)) => last,
                None => module,
            };
            return Some(name.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_pnpm_workspace_detection() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Create workspace structure
        fs::write(
            root.join("pnpm-workspace.yaml"),
            "packages:\n  - \"packages/*\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("packages/app-a")).unwrap();
        fs::create_dir_all(root.join("packages/app-b")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 2);

        let mut names: Vec<&str> = info.projects.iter().map(|p| p.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["app-a", "app-b"]);
    }

    #[test]
    fn test_npm_workspace_array_format() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("package.json"),
            r#"{"name": "root", "workspaces": ["packages/*"]}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("packages/web")).unwrap();
        fs::write(
            root.join("packages/web/package.json"),
            r#"{"name": "@myorg/web"}"#,
        )
        .unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].name, "@myorg/web");
        assert_eq!(info.projects[0].path, "packages/web");
    }

    #[test]
    fn test_npm_workspace_object_format() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("package.json"),
            r#"{"name": "root", "workspaces": {"packages": ["apps/*"]}}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("apps/frontend")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].name, "frontend");
        assert_eq!(info.projects[0].path, "apps/frontend");
    }

    #[test]
    fn test_cargo_workspace_detection() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("crates/core")).unwrap();
        fs::write(
            root.join("crates/core/Cargo.toml"),
            "[package]\nname = \"my-core\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].name, "my-core");
        assert_eq!(info.projects[0].path, "crates/core");
    }

    #[test]
    fn test_go_workspace_detection() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("go.work"),
            "go 1.21\n\nuse (\n\t./cmd/server\n\t./pkg/lib\n)\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("cmd/server")).unwrap();
        fs::write(
            root.join("cmd/server/go.mod"),
            "module github.com/example/myapp/cmd/server\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("pkg/lib")).unwrap();
        fs::write(
            root.join("pkg/lib/go.mod"),
            "module github.com/example/myapp/pkg/lib\n",
        )
        .unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 2);
        assert_eq!(info.projects[0].name, "server");
        assert_eq!(info.projects[1].name, "lib");
    }

    #[test]
    fn test_lerna_detection() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("lerna.json"),
            r#"{"packages": ["packages/*"], "version": "independent"}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("packages/ui")).unwrap();
        fs::create_dir_all(root.join("packages/utils")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 2);

        let mut names: Vec<&str> = info.projects.iter().map(|p| p.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["ui", "utils"]);
    }

    #[test]
    fn test_nx_detection_with_package_json_workspaces() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("nx.json"), r#"{"npmScope": "myorg"}"#).unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"name": "root", "workspaces": ["libs/*"]}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("libs/shared")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].name, "shared");
    }

    #[test]
    fn test_turbo_detection_with_package_json_workspaces() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("turbo.json"), r#"{"pipeline": {}}"#).unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"name": "root", "workspaces": ["apps/*"]}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("apps/web")).unwrap();
        fs::write(root.join("apps/web/package.json"), r#"{"name": "web-app"}"#).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].name, "web-app");
    }

    #[test]
    fn test_single_project_non_monorepo() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("package.json"),
            r#"{"name": "my-app", "version": "1.0.0"}"#,
        )
        .unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, ".");
        assert_eq!(info.projects[0].name, "my-app");
    }

    #[test]
    fn test_project_name_from_package_json() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("package.json"), r#"{"name": "cool-pkg"}"#).unwrap();

        let name = extract_project_name(root);
        assert_eq!(name, "cool-pkg");
    }

    #[test]
    fn test_project_name_from_cargo_toml() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"my-crate\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let name = extract_project_name(root);
        assert_eq!(name, "my-crate");
    }

    #[test]
    fn test_project_name_from_pyproject_toml() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("pyproject.toml"),
            "[project]\nname = \"my-python-pkg\"\n",
        )
        .unwrap();

        let name = extract_project_name(root);
        assert_eq!(name, "my-python-pkg");
    }

    #[test]
    fn test_project_name_from_pyproject_poetry() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("pyproject.toml"),
            "[tool.poetry]\nname = \"poetry-app\"\n",
        )
        .unwrap();

        let name = extract_project_name(root);
        assert_eq!(name, "poetry-app");
    }

    #[test]
    fn test_project_name_from_go_mod() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("go.mod"),
            "module github.com/user/my-service\n\ngo 1.21\n",
        )
        .unwrap();

        let name = extract_project_name(root);
        assert_eq!(name, "my-service");
    }

    #[test]
    fn test_project_name_fallback_to_directory() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let sub = root.join("my-project");
        fs::create_dir_all(&sub).unwrap();

        let name = extract_project_name(&sub);
        assert_eq!(name, "my-project");
    }

    #[test]
    fn test_glob_expansion_creates_correct_project_list() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::create_dir_all(root.join("packages/alpha")).unwrap();
        fs::create_dir_all(root.join("packages/beta")).unwrap();
        // Create a file that should not be matched
        fs::write(root.join("packages/not-a-dir.txt"), "").unwrap();

        let patterns = vec!["packages/*".to_string()];
        let projects = expand_glob_patterns(root, &patterns);

        assert_eq!(projects.len(), 2);
        let mut names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn test_empty_pnpm_workspace() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("pnpm-workspace.yaml"), "packages: []\n").unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, ".");
    }

    #[test]
    fn test_empty_workspace_no_packages_key() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("pnpm-workspace.yaml"), "# empty\n").unwrap();

        let info = detect_monorepo(root).unwrap();
        // No packages key means not a monorepo
        assert!(!info.is_monorepo);
    }

    #[test]
    fn test_go_work_single_use_directive() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("go.work"), "go 1.21\n\nuse ./mymod\n").unwrap();
        fs::create_dir_all(root.join("mymod")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, "./mymod");
    }

    #[test]
    fn test_go_work_nonexistent_dir_filtered() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("go.work"),
            "go 1.21\n\nuse (\n\t./exists\n\t./missing\n)\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("exists")).unwrap();
        // ./missing does not exist

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, "./exists");
    }

    #[test]
    fn test_multiple_glob_patterns() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("package.json"),
            r#"{"name": "root", "workspaces": ["apps/*", "libs/*"]}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("apps/web")).unwrap();
        fs::create_dir_all(root.join("libs/shared")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 2);

        let paths: Vec<&str> = info.projects.iter().map(|p| p.path.as_str()).collect();
        assert!(paths.contains(&"apps/web"));
        assert!(paths.contains(&"libs/shared"));
    }

    #[test]
    fn test_empty_directory_is_not_monorepo() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, ".");
    }

    #[test]
    fn test_cargo_workspace_no_workspace_section() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
        assert_eq!(info.projects[0].name, "solo");
    }

    #[test]
    fn test_pnpm_takes_priority_over_npm() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Both PNPM and npm workspaces present — PNPM wins
        fs::write(
            root.join("pnpm-workspace.yaml"),
            "packages:\n  - \"pnpm-pkgs/*\"\n",
        )
        .unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"name": "root", "workspaces": ["npm-pkgs/*"]}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("pnpm-pkgs/a")).unwrap();
        fs::create_dir_all(root.join("npm-pkgs/b")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, "pnpm-pkgs/a");
    }

    #[test]
    fn test_nx_without_workspaces_not_detected() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // nx.json exists but no package.json workspaces or workspace.json
        fs::write(root.join("nx.json"), "{}").unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"name": "root", "version": "1.0.0"}"#,
        )
        .unwrap();

        let info = detect_monorepo(root).unwrap();
        // Falls through to single-project since there are no workspace patterns
        assert!(!info.is_monorepo);
    }

    #[test]
    fn test_nx_with_workspace_json() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("nx.json"), "{}").unwrap();
        // No package.json workspaces, but workspace.json exists
        fs::write(
            root.join("package.json"),
            r#"{"name": "root", "version": "1.0.0"}"#,
        )
        .unwrap();
        fs::write(
            root.join("workspace.json"),
            r#"{"projects": {"app-one": "apps/one", "lib-two": "libs/two"}}"#,
        )
        .unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 2);

        let mut names: Vec<&str> = info.projects.iter().map(|p| p.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["app-one", "lib-two"]);
    }

    #[test]
    fn test_workspace_json_empty_projects() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("nx.json"), "{}").unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"name": "root", "version": "1.0.0"}"#,
        )
        .unwrap();
        fs::write(root.join("workspace.json"), r#"{"projects": {}}"#).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
    }

    #[test]
    fn test_workspace_json_non_object_projects() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("nx.json"), "{}").unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"name": "root", "version": "1.0.0"}"#,
        )
        .unwrap();
        fs::write(
            root.join("workspace.json"),
            r#"{"projects": ["not-an-object"]}"#,
        )
        .unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
    }

    #[test]
    fn test_turbo_without_workspaces_falls_through() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("turbo.json"), r#"{"pipeline": {}}"#).unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"name": "solo-turbo", "version": "1.0.0"}"#,
        )
        .unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
        assert_eq!(info.projects[0].name, "solo-turbo");
    }

    #[test]
    fn test_lerna_without_packages_key() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("lerna.json"), r#"{"version": "independent"}"#).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
    }

    #[test]
    fn test_lerna_with_empty_packages() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("lerna.json"), r#"{"packages": []}"#).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
    }

    #[test]
    fn test_cargo_workspace_with_empty_members() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
    }

    #[test]
    fn test_go_work_with_comments_in_use_block() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("go.work"),
            "go 1.21\n\nuse (\n\t// this is a comment\n\t./mymod\n\n)\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("mymod")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, "./mymod");
    }

    #[test]
    fn test_go_work_all_dirs_nonexistent_falls_through() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("go.work"),
            "go 1.21\n\nuse (\n\t./missing1\n\t./missing2\n)\n",
        )
        .unwrap();

        let info = detect_monorepo(root).unwrap();
        // All dirs nonexistent, but still detected as monorepo (has use directives)
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 0);
    }

    #[test]
    fn test_go_work_empty_file() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("go.work"), "go 1.21\n").unwrap();

        let info = detect_monorepo(root).unwrap();
        // No use directives, not a monorepo
        assert!(!info.is_monorepo);
    }

    #[test]
    fn test_go_work_use_with_parenthesis_on_same_line() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("go.work"), "go 1.21\n\nuse (\n\t./mod1\n)\n").unwrap();
        fs::create_dir_all(root.join("mod1")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
    }

    #[test]
    fn test_extract_workspace_patterns_workspaces_is_number() {
        let json: serde_json::Value = serde_json::from_str(r#"{"workspaces": 42}"#).unwrap();
        let patterns = extract_workspace_patterns(&json);
        assert!(patterns.is_empty());
    }

    #[test]
    fn test_extract_workspace_patterns_no_workspaces_key() {
        let json: serde_json::Value = serde_json::from_str(r#"{"name": "root"}"#).unwrap();
        let patterns = extract_workspace_patterns(&json);
        assert!(patterns.is_empty());
    }

    #[test]
    fn test_extract_workspace_patterns_object_without_packages() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"workspaces": {"nohoist": ["**/react"]}}"#).unwrap();
        let patterns = extract_workspace_patterns(&json);
        assert!(patterns.is_empty());
    }

    #[test]
    fn test_extract_workspace_patterns_array_with_non_strings() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"workspaces": ["packages/*", 42, null]}"#).unwrap();
        let patterns = extract_workspace_patterns(&json);
        assert_eq!(patterns, vec!["packages/*"]);
    }

    #[test]
    fn test_name_from_package_json_no_name_field() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("package.json"), r#"{"version": "1.0.0"}"#).unwrap();

        assert!(name_from_package_json(root).is_none());
    }

    #[test]
    fn test_name_from_package_json_invalid_json() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("package.json"), "not json").unwrap();

        assert!(name_from_package_json(root).is_none());
    }

    #[test]
    fn test_name_from_package_json_name_is_not_string() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("package.json"), r#"{"name": 123}"#).unwrap();

        assert!(name_from_package_json(root).is_none());
    }

    #[test]
    fn test_name_from_package_json_nonexistent() {
        let dir = tempdir().unwrap();
        assert!(name_from_package_json(dir.path()).is_none());
    }

    #[test]
    fn test_name_from_cargo_toml_no_package_section() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();

        assert!(name_from_cargo_toml(root).is_none());
    }

    #[test]
    fn test_name_from_cargo_toml_invalid_toml() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("Cargo.toml"), "not valid toml [[[").unwrap();

        assert!(name_from_cargo_toml(root).is_none());
    }

    #[test]
    fn test_name_from_cargo_toml_nonexistent() {
        let dir = tempdir().unwrap();
        assert!(name_from_cargo_toml(dir.path()).is_none());
    }

    #[test]
    fn test_name_from_pyproject_toml_nonexistent() {
        let dir = tempdir().unwrap();
        assert!(name_from_pyproject_toml(dir.path()).is_none());
    }

    #[test]
    fn test_name_from_pyproject_toml_no_project_or_poetry() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("pyproject.toml"),
            "[build-system]\nrequires = [\"setuptools\"]\n",
        )
        .unwrap();

        assert!(name_from_pyproject_toml(root).is_none());
    }

    #[test]
    fn test_name_from_pyproject_toml_invalid_toml() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("pyproject.toml"), "not valid [[[").unwrap();

        assert!(name_from_pyproject_toml(root).is_none());
    }

    #[test]
    fn test_name_from_go_mod_nonexistent() {
        let dir = tempdir().unwrap();
        assert!(name_from_go_mod(dir.path()).is_none());
    }

    #[test]
    fn test_name_from_go_mod_no_module_line() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("go.mod"), "go 1.21\n\nrequire (\n)\n").unwrap();

        assert!(name_from_go_mod(root).is_none());
    }

    #[test]
    fn test_name_from_go_mod_simple_module_no_slash() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("go.mod"), "module myapp\n\ngo 1.21\n").unwrap();

        assert_eq!(name_from_go_mod(root), Some("myapp".to_string()));
    }

    #[test]
    fn test_project_name_unknown_fallback() {
        // Path::new("/") has no file_name() — triggers the "unknown" fallback
        let name = extract_project_name(Path::new("/"));
        assert_eq!(name, "unknown");
    }

    #[test]
    fn test_invalid_pnpm_workspace_yaml() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("pnpm-workspace.yaml"),
            ":\n  - not: valid: yaml: {{{\n",
        )
        .unwrap();

        let result = detect_monorepo(root);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_package_json() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("package.json"), "not valid json!!!").unwrap();

        let result = detect_monorepo(root);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_lerna_json() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("lerna.json"), "not json").unwrap();

        let result = detect_monorepo(root);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_cargo_toml() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("Cargo.toml"), "not valid toml [[[").unwrap();

        let result = detect_monorepo(root);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_workspace_json() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("nx.json"), "{}").unwrap();
        fs::write(root.join("package.json"), r#"{"name": "root"}"#).unwrap();
        fs::write(root.join("workspace.json"), "not json!!!").unwrap();

        let result = detect_monorepo(root);
        assert!(result.is_err());
    }

    #[test]
    fn test_lerna_packages_not_array() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("lerna.json"), r#"{"packages": "not-an-array"}"#).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
    }

    #[test]
    fn test_cargo_workspace_members_not_array() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = \"not-an-array\"\n",
        )
        .unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
    }

    #[test]
    fn test_workspace_json_projects_value_not_string() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("nx.json"), "{}").unwrap();
        fs::write(root.join("package.json"), r#"{"name": "root"}"#).unwrap();
        fs::write(root.join("workspace.json"), r#"{"projects": {"app": 42}}"#).unwrap();

        let info = detect_monorepo(root).unwrap();
        // Project value is not a string, gets filtered out, so empty projects → falls through
        assert!(!info.is_monorepo);
    }

    #[test]
    fn test_name_from_cargo_toml_package_name_not_string() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("Cargo.toml"), "[package]\nname = 42\n").unwrap();

        assert!(name_from_cargo_toml(root).is_none());
    }

    #[test]
    fn test_glob_expansion_invalid_pattern_skipped() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // An invalid glob pattern (unclosed bracket) should be skipped
        let patterns = vec!["packages/[invalid".to_string()];
        let projects = expand_glob_patterns(root, &patterns);
        assert!(projects.is_empty());
    }
}
