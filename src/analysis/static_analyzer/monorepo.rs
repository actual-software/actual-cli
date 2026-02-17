use std::fs;
use std::path::Path;

use ignore::WalkBuilder;
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

    // Nx / legacy workspace.json (can exist with or without nx.json)
    if let Some(info) = detect_workspace_json(root)? {
        return Ok(info);
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
    if projects.is_empty() {
        return Ok(None);
    }
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
    if projects.is_empty() {
        return Ok(None);
    }
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
    if projects.is_empty() {
        return Ok(None);
    }
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

    let canonical_root = fs::canonicalize(root)?;
    let projects: Vec<ProjectInfo> = match json.get("projects") {
        Some(serde_json::Value::Object(obj)) => obj
            .iter()
            .filter_map(|(_, val)| {
                // Handle both string format ("apps/one") and object format ({"root": "apps/one"})
                let path_str = val.as_str().map(|s| s.to_string()).or_else(|| {
                    val.as_object()
                        .and_then(|o| o.get("root"))
                        .and_then(|r| r.as_str())
                        .map(|s| s.to_string())
                })?;
                // Use canonicalize to reject paths that escape the repo root
                let resolved = root.join(&path_str);
                let canonical = match fs::canonicalize(&resolved) {
                    Ok(c) if c.starts_with(&canonical_root) && c.is_dir() => c,
                    _ => return None,
                };
                let rel = canonical
                    .strip_prefix(&canonical_root)
                    .expect("starts_with already verified")
                    .to_string_lossy()
                    .to_string();
                let name = extract_project_name(&canonical);
                Some(ProjectInfo { path: rel, name })
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
    if projects.is_empty() {
        return Ok(None);
    }
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
                let dir_str = trimmed.split("//").next().unwrap_or("").trim();
                let cleaned = dir_str.strip_prefix("./").unwrap_or(dir_str);
                if !cleaned.is_empty() && !cleaned.split('/').any(|c| c == "..") {
                    dirs.push(cleaned.to_string());
                }
            }
            continue;
        }

        if let Some(after_use) = trimmed.strip_prefix("use (") {
            in_use_block = true;
            // Process any entry on the remainder of this line after "use ("
            let remainder = after_use.trim();
            if remainder == ")" {
                in_use_block = false;
            } else if !remainder.is_empty() && !remainder.starts_with("//") {
                let dir_str = remainder.split("//").next().unwrap_or("").trim();
                let dir_str = if let Some(stripped) = dir_str.strip_suffix(')') {
                    in_use_block = false;
                    stripped.trim()
                } else {
                    dir_str
                };
                let cleaned = dir_str.strip_prefix("./").unwrap_or(dir_str);
                if !cleaned.is_empty() && !cleaned.split('/').any(|c| c == "..") {
                    dirs.push(cleaned.to_string());
                }
            }
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("use ") {
            let dir = rest.split("//").next().unwrap_or("").trim();
            let cleaned = dir.strip_prefix("./").unwrap_or(dir);
            if !cleaned.is_empty() && cleaned != "(" && !cleaned.split('/').any(|c| c == "..") {
                dirs.push(cleaned.to_string());
            }
        }
    }

    if dirs.is_empty() {
        return Ok(None);
    }

    let canonical_root = fs::canonicalize(root)?;
    let projects: Vec<ProjectInfo> = dirs
        .into_iter()
        .filter_map(|d| {
            let resolved = root.join(&d);
            let canonical = match fs::canonicalize(&resolved) {
                Ok(cp) if cp.starts_with(&canonical_root) && cp.is_dir() => cp,
                _ => return None,
            };
            let rel = canonical
                .strip_prefix(&canonical_root)
                .expect("starts_with already verified")
                .to_string_lossy()
                .to_string();
            let name = extract_project_name(&canonical);
            Some(ProjectInfo { path: rel, name })
        })
        .collect();

    if projects.is_empty() {
        return Ok(None);
    }

    Ok(Some(MonorepoInfo {
        is_monorepo: true,
        projects,
    }))
}

/// Expand glob patterns relative to root and build project info for each match.
///
/// Supports negation patterns (prefixed with `!`) which exclude matching directories
/// from the result set. Uses the `ignore` crate's [`WalkBuilder`] to walk the
/// filesystem while respecting `.gitignore` rules, which automatically skips
/// dependency directories (e.g. `node_modules`), build artifacts, and hidden
/// directories that are gitignored. This prevents catastrophic slowdowns when
/// glob patterns like `apps/**` would otherwise recurse into tens of thousands
/// of nested directories.
///
/// Uses `fs::canonicalize` to resolve symlinks and reject entries that escape
/// the repo root.
fn expand_glob_patterns(root: &Path, patterns: &[String]) -> Vec<ProjectInfo> {
    // Canonicalize root once; if this fails, root doesn't exist and there's
    // nothing to expand — callers will see an empty vec and fall through.
    let Ok(canonical_root) = fs::canonicalize(root) else {
        return Vec::new();
    };

    // Separate inclusion and exclusion patterns, compile them into glob matchers.
    let mut inclusion_globs = Vec::new();
    let mut exclusion_globs = Vec::new();
    for pattern in patterns {
        if let Some(negated) = pattern.strip_prefix('!') {
            if let Ok(pat) = glob::Pattern::new(negated) {
                exclusion_globs.push(pat);
            }
        } else if let Ok(pat) = glob::Pattern::new(pattern) {
            inclusion_globs.push(pat);
        }
    }

    if inclusion_globs.is_empty() {
        return Vec::new();
    }

    // Walk the filesystem respecting .gitignore rules. This is the key
    // performance fix: the walker skips gitignored directories (node_modules,
    // .next, dist, etc.) without ever descending into them.
    let walker = WalkBuilder::new(root)
        .hidden(false) // Don't skip hidden dirs — let .gitignore decide
        .follow_links(false) // Don't follow symlinks (security: prevents escaping root)
        .build();

    let mut projects = Vec::new();
    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        // Canonicalize each entry to resolve symlinks and `..` segments,
        // then verify it is within the canonical repo root.
        let canonical_entry = match fs::canonicalize(path) {
            Ok(c) if c.starts_with(&canonical_root) => c,
            _ => continue,
        };
        // strip_prefix is guaranteed to succeed after starts_with
        let rel = canonical_entry
            .strip_prefix(&canonical_root)
            .expect("starts_with already verified")
            .to_string_lossy()
            .to_string();
        if rel.is_empty() {
            continue;
        }

        // Check if the relative path matches any inclusion glob pattern
        if !inclusion_globs.iter().any(|g| g.matches(&rel)) {
            continue;
        }

        // Check exclusion patterns
        if exclusion_globs.iter().any(|g| g.matches(&rel)) {
            continue;
        }

        let name = extract_project_name(&canonical_entry);
        projects.push(ProjectInfo { path: rel, name });
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
            // Take the last segment after '/', skipping Go major-version suffixes
            // (e.g., "github.com/user/myapp/v2" → "myapp", not "v2")
            let name = match module.rsplit_once('/') {
                Some((prefix, last))
                    if last.starts_with('v') && last[1..].parse::<u64>().is_ok() =>
                {
                    // Major version suffix — use the segment before it
                    prefix.rsplit_once('/').map(|(_, n)| n).unwrap_or(prefix)
                }
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
        assert_eq!(info.projects[0].path, "mymod");
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
        assert_eq!(info.projects[0].path, "exists");
    }

    #[test]
    fn test_go_work_path_traversal_rejected() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Create a sibling directory outside root that would match ../sibling
        let sibling = dir.path().join("inside");
        fs::create_dir_all(&sibling).unwrap();

        // go.work references a path with `..` to escape the repo root
        fs::write(
            root.join("go.work"),
            "go 1.21\n\nuse (\n\t../sibling\n\t./inside\n)\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("inside")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        // Only "inside" should be included; "../sibling" should be rejected
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, "inside");
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
    fn test_workspace_json_without_nx_json() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // workspace.json exists but no nx.json — should still be detected
        fs::write(
            root.join("package.json"),
            r#"{"name": "root", "version": "1.0.0"}"#,
        )
        .unwrap();
        fs::write(
            root.join("workspace.json"),
            r#"{"projects": {"app": "apps/one"}}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("apps/one")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, "apps/one");
    }

    #[test]
    fn test_workspace_json_object_format_projects() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Modern Nx workspace.json with object-valued project entries
        fs::write(root.join("nx.json"), "{}").unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"name": "root", "version": "1.0.0"}"#,
        )
        .unwrap();
        fs::write(
            root.join("workspace.json"),
            r#"{"projects": {"my-app": {"root": "apps/my-app", "projectType": "application"}, "my-lib": {"root": "libs/my-lib"}}}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("apps/my-app")).unwrap();
        fs::create_dir_all(root.join("libs/my-lib")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 2);

        let mut paths: Vec<&str> = info.projects.iter().map(|p| p.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, vec!["apps/my-app", "libs/my-lib"]);
    }

    #[test]
    fn test_workspace_json_mixed_string_and_object_format() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Mix of string paths and object-valued project entries
        fs::write(root.join("nx.json"), "{}").unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"name": "root", "version": "1.0.0"}"#,
        )
        .unwrap();
        fs::write(
            root.join("workspace.json"),
            r#"{"projects": {"legacy-app": "apps/legacy", "modern-app": {"root": "apps/modern"}}}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("apps/legacy")).unwrap();
        fs::create_dir_all(root.join("apps/modern")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 2);

        let mut paths: Vec<&str> = info.projects.iter().map(|p| p.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, vec!["apps/legacy", "apps/modern"]);
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
        fs::create_dir_all(root.join("apps/one")).unwrap();
        fs::create_dir_all(root.join("libs/two")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 2);

        let mut names: Vec<&str> = info.projects.iter().map(|p| p.name.as_str()).collect();
        names.sort();
        // Names come from extract_project_name (directory name fallback)
        assert_eq!(names, vec!["one", "two"]);
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
        assert_eq!(info.projects[0].path, "mymod");
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
        // All dirs nonexistent, falls through to single-project mode
        assert!(!info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, ".");
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
    fn test_name_from_go_mod_v2_suffix_skipped() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("go.mod"),
            "module github.com/user/myapp/v2\n\ngo 1.21\n",
        )
        .unwrap();

        assert_eq!(name_from_go_mod(root), Some("myapp".to_string()));
    }

    #[test]
    fn test_name_from_go_mod_v3_suffix_skipped() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("go.mod"),
            "module github.com/org/lib/v3\n\ngo 1.21\n",
        )
        .unwrap();

        assert_eq!(name_from_go_mod(root), Some("lib".to_string()));
    }

    #[test]
    fn test_name_from_go_mod_v_not_version_kept() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // "validator" starts with 'v' but isn't a version suffix
        fs::write(
            root.join("go.mod"),
            "module github.com/go-playground/validator\n\ngo 1.21\n",
        )
        .unwrap();

        assert_eq!(name_from_go_mod(root), Some("validator".to_string()));
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
    fn test_workspace_json_projects_value_not_string_or_object() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("nx.json"), "{}").unwrap();
        fs::write(root.join("package.json"), r#"{"name": "root"}"#).unwrap();
        // Value is a number — neither string path nor object with "root" key
        fs::write(root.join("workspace.json"), r#"{"projects": {"app": 42}}"#).unwrap();

        let info = detect_monorepo(root).unwrap();
        // Project value is not a string or valid object, gets filtered out
        assert!(!info.is_monorepo);
    }

    #[test]
    fn test_workspace_json_object_without_root_key() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("nx.json"), "{}").unwrap();
        fs::write(root.join("package.json"), r#"{"name": "root"}"#).unwrap();
        // Object without "root" key — should be filtered out
        fs::write(
            root.join("workspace.json"),
            r#"{"projects": {"app": {"sourceRoot": "apps/app/src"}}}"#,
        )
        .unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
    }

    #[test]
    fn test_workspace_json_path_traversal_rejected() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("nx.json"), "{}").unwrap();
        fs::write(root.join("package.json"), r#"{"name": "root"}"#).unwrap();
        // Path with `..` that escapes the repo root — should be rejected
        fs::write(
            root.join("workspace.json"),
            r#"{"projects": {"evil": "../../escape"}}"#,
        )
        .unwrap();

        let info = detect_monorepo(root).unwrap();
        // Path traversal rejected, falls through to single-project mode
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
    fn test_glob_expansion_nonexistent_root_returns_empty() {
        let patterns = vec!["packages/*".to_string()];
        let projects = expand_glob_patterns(Path::new("/nonexistent/root/path"), &patterns);
        assert!(projects.is_empty());
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

    #[test]
    fn test_glob_expansion_skips_out_of_root_entries() {
        let dir = tempdir().unwrap();
        // Use a subdirectory as root so `../*` reliably produces entries
        // (the sibling directory) that fail strip_prefix.
        let root = dir.path().join("workspace");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(dir.path().join("sibling")).unwrap();

        // `../*` from workspace/ resolves to workspace/ and sibling/ inside the
        // tempdir. sibling/ fails strip_prefix(workspace/) and must be skipped.
        let patterns = vec!["../*".to_string()];
        let projects = expand_glob_patterns(&root, &patterns);

        // No entries should escape the root; the only in-root glob result is
        // workspace/ itself, but that resolves to an empty relative path and is
        // also skipped.
        assert!(
            projects.is_empty(),
            "expected no projects but got: {projects:?}"
        );
    }

    #[test]
    fn test_glob_expansion_negation_pattern_excludes() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::create_dir_all(root.join("packages/included")).unwrap();
        fs::create_dir_all(root.join("packages/excluded")).unwrap();

        let patterns = vec!["packages/*".to_string(), "!packages/excluded".to_string()];
        let projects = expand_glob_patterns(root, &patterns);

        // Negation pattern excludes the matching directory
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, "packages/included");
    }

    #[cfg(unix)]
    #[test]
    fn test_glob_expansion_rejects_symlink_escaping_root() {
        use std::os::unix::fs as unix_fs;

        let dir = tempdir().unwrap();
        let root = dir.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(root.join("packages")).unwrap();

        // Create a real directory inside root
        fs::create_dir_all(root.join("packages/legit")).unwrap();

        // Create a directory outside root
        let outside = dir.path().join("outside-secret");
        fs::create_dir_all(&outside).unwrap();

        // Create a symlink inside root that points outside
        unix_fs::symlink(&outside, root.join("packages/evil")).unwrap();

        let patterns = vec!["packages/*".to_string()];
        let projects = expand_glob_patterns(&root, &patterns);

        // Only "legit" should appear; the symlink to outside should be rejected
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, "packages/legit");
    }

    #[test]
    fn test_go_work_double_dot_in_dirname_accepted() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // A directory name containing ".." as a substring (not a path component)
        // should be accepted, e.g., "foo..bar"
        fs::write(root.join("go.work"), "go 1.21\n\nuse ./foo..bar\n").unwrap();
        fs::create_dir_all(root.join("foo..bar")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, "foo..bar");
    }

    #[test]
    fn test_go_work_inline_comments_in_use_block() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("go.work"),
            "go 1.21\n\nuse (\n\t./cmd/server // legacy\n\t./pkg/lib\n)\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("cmd/server")).unwrap();
        fs::create_dir_all(root.join("pkg/lib")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 2);

        let paths: Vec<&str> = info.projects.iter().map(|p| p.path.as_str()).collect();
        assert!(paths.contains(&"cmd/server"));
        assert!(paths.contains(&"pkg/lib"));
    }

    #[test]
    fn test_go_work_inline_comment_single_use() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("go.work"),
            "go 1.21\n\nuse ./mymod // inline comment\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("mymod")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, "mymod");
    }

    #[test]
    fn test_pnpm_workspace_no_matching_dirs_falls_through() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Patterns configured but no matching directories exist
        fs::write(
            root.join("pnpm-workspace.yaml"),
            "packages:\n  - \"packages/*\"\n",
        )
        .unwrap();
        // Don't create packages/ directory

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, ".");
    }

    #[test]
    fn test_npm_workspace_no_matching_dirs_falls_through() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("package.json"),
            r#"{"name": "root", "workspaces": ["packages/*"]}"#,
        )
        .unwrap();
        // Don't create packages/ directory

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, ".");
    }

    #[test]
    fn test_lerna_no_matching_dirs_falls_through() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("lerna.json"), r#"{"packages": ["packages/*"]}"#).unwrap();
        // Don't create packages/ directory

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, ".");
    }

    #[test]
    fn test_cargo_workspace_no_matching_dirs_falls_through() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        // Don't create crates/ directory

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, ".");
    }

    #[test]
    fn test_pnpm_negation_pattern_excludes_directory() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("pnpm-workspace.yaml"),
            "packages:\n  - \"packages/*\"\n  - \"!packages/internal\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("packages/public")).unwrap();
        fs::create_dir_all(root.join("packages/internal")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, "packages/public");
    }

    #[test]
    fn test_npm_negation_pattern_excludes_directory() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("package.json"),
            r#"{"name": "root", "workspaces": ["packages/*", "!packages/template"]}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("packages/app")).unwrap();
        fs::create_dir_all(root.join("packages/template")).unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, "packages/app");
    }

    #[test]
    fn test_workspace_json_nonexistent_paths_falls_through() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("nx.json"), "{}").unwrap();
        fs::write(root.join("package.json"), r#"{"name": "root"}"#).unwrap();
        fs::write(
            root.join("workspace.json"),
            r#"{"projects": {"app": "apps/web", "lib": "libs/shared"}}"#,
        )
        .unwrap();
        // Don't create the directories

        let info = detect_monorepo(root).unwrap();
        // All paths nonexistent, falls through to single-project mode
        assert!(!info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, ".");
    }

    #[test]
    fn test_go_work_entry_on_same_line_as_paren() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::create_dir_all(root.join("mod1")).unwrap();
        fs::create_dir_all(root.join("mod2")).unwrap();

        // First entry on same line as "use ("
        fs::write(
            root.join("go.work"),
            "go 1.21\n\nuse (./mod1\n\t./mod2\n)\n",
        )
        .unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 2);

        let paths: Vec<&str> = info.projects.iter().map(|p| p.path.as_str()).collect();
        assert!(paths.contains(&"mod1"));
        assert!(paths.contains(&"mod2"));
    }

    #[test]
    fn test_go_work_single_entry_with_closing_paren() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::create_dir_all(root.join("mod1")).unwrap();

        // Single entry with closing paren on same line as "use ("
        fs::write(root.join("go.work"), "go 1.21\n\nuse (./mod1)\n").unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 1);
        assert_eq!(info.projects[0].path, "mod1");
    }

    #[test]
    fn test_go_work_entry_on_same_line_with_comment() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::create_dir_all(root.join("mod1")).unwrap();
        fs::create_dir_all(root.join("mod2")).unwrap();

        // Entry on same line with inline comment
        fs::write(
            root.join("go.work"),
            "go 1.21\n\nuse (./mod1 // main module\n\t./mod2\n)\n",
        )
        .unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(info.is_monorepo);
        assert_eq!(info.projects.len(), 2);

        let paths: Vec<&str> = info.projects.iter().map(|p| p.path.as_str()).collect();
        assert!(paths.contains(&"mod1"));
        assert!(paths.contains(&"mod2"));
    }

    #[test]
    fn test_go_work_empty_paren_on_same_line() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // "use ()" on same line — empty, no modules
        fs::write(root.join("go.work"), "go 1.21\n\nuse ()\n").unwrap();

        let info = detect_monorepo(root).unwrap();
        assert!(!info.is_monorepo);
    }
}
