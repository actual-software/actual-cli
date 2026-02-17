use std::collections::HashSet;
use std::path::Path;

/// Dependency information extracted from manifest files.
#[derive(Debug, Default)]
pub struct DependencyInfo {
    /// Production dependency names.
    pub dependencies: Vec<String>,
    /// Development-only dependency names.
    pub dev_dependencies: Vec<String>,
}

/// Parse dependencies from all recognized manifest files in `project_dir`.
///
/// Scans for package.json, Cargo.toml, pyproject.toml, requirements.txt,
/// Pipfile, go.mod, Gemfile, pom.xml, build.gradle(.kts), and Package.swift.
/// Missing files are silently skipped — an empty project returns empty deps.
pub fn parse_dependencies(project_dir: &Path) -> DependencyInfo {
    let mut deps = HashSet::new();
    let mut dev_deps = HashSet::new();

    parse_package_json(project_dir, &mut deps, &mut dev_deps);
    parse_cargo_toml(project_dir, &mut deps, &mut dev_deps);
    parse_pyproject_toml(project_dir, &mut deps, &mut dev_deps);
    parse_requirements_txt(project_dir, &mut deps);
    parse_pipfile(project_dir, &mut deps, &mut dev_deps);
    parse_go_mod(project_dir, &mut deps);
    parse_gemfile(project_dir, &mut deps);
    parse_pom_xml(project_dir, &mut deps);
    parse_build_gradle(project_dir, &mut deps);
    parse_package_swift(project_dir, &mut deps);

    let mut dependencies: Vec<String> = deps.into_iter().collect();
    dependencies.sort();
    let mut dev_dependencies: Vec<String> = dev_deps.into_iter().collect();
    dev_dependencies.sort();

    DependencyInfo {
        dependencies,
        dev_dependencies,
    }
}

// ── package.json ─────────────────────────────────────────────────────

fn parse_package_json(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    dev_deps: &mut HashSet<String>,
) {
    let path = project_dir.join("package.json");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Warning: failed to parse {}: {e}", path.display());
            return;
        }
    };

    if let Some(obj) = parsed.get("dependencies").and_then(|v| v.as_object()) {
        for key in obj.keys() {
            deps.insert(key.clone());
        }
    }
    if let Some(obj) = parsed.get("devDependencies").and_then(|v| v.as_object()) {
        for key in obj.keys() {
            dev_deps.insert(key.clone());
        }
    }
}

// ── Cargo.toml ───────────────────────────────────────────────────────

fn parse_cargo_toml(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    dev_deps: &mut HashSet<String>,
) {
    let path = project_dir.join("Cargo.toml");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let parsed: toml::Value = match content.parse() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Warning: failed to parse {}: {e}", path.display());
            return;
        }
    };

    if let Some(table) = parsed.get("dependencies").and_then(|v| v.as_table()) {
        for key in table.keys() {
            deps.insert(key.clone());
        }
    }
    if let Some(table) = parsed.get("dev-dependencies").and_then(|v| v.as_table()) {
        for key in table.keys() {
            dev_deps.insert(key.clone());
        }
    }
}

// ── pyproject.toml ───────────────────────────────────────────────────

fn parse_pyproject_toml(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    dev_deps: &mut HashSet<String>,
) {
    let path = project_dir.join("pyproject.toml");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let parsed: toml::Value = match content.parse() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Warning: failed to parse {}: {e}", path.display());
            return;
        }
    };

    // PEP 631 format: [project].dependencies is a list of strings like "requests>=2.0"
    if let Some(arr) = parsed
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_array())
    {
        for item in arr {
            if let Some(s) = item.as_str() {
                if let Some(name) = strip_python_version_specifier(s) {
                    deps.insert(name);
                }
            }
        }
    }

    // PEP 631 optional/dev dependencies
    if let Some(table) = parsed
        .get("project")
        .and_then(|p| p.get("optional-dependencies"))
        .and_then(|d| d.as_table())
    {
        for (_group, arr) in table {
            if let Some(arr) = arr.as_array() {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        if let Some(name) = strip_python_version_specifier(s) {
                            dev_deps.insert(name);
                        }
                    }
                }
            }
        }
    }

    // Poetry format: [tool.poetry].dependencies is a table
    if let Some(table) = parsed
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_table())
    {
        for key in table.keys() {
            if key != "python" {
                deps.insert(key.clone());
            }
        }
    }

    // Poetry dev dependencies
    if let Some(table) = parsed
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("dev-dependencies"))
        .and_then(|d| d.as_table())
    {
        for key in table.keys() {
            dev_deps.insert(key.clone());
        }
    }
}

/// Strip PEP 508 version specifiers from a dependency string.
///
/// Handles specs like `requests>=2.0`, `flask[async]~=2.0`, `numpy ; python_version>="3.8"`.
fn strip_python_version_specifier(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // Find the first character that starts a version specifier or extra marker
    let end = s
        .find(['>', '<', '=', '!', '~', ';', '@', '['])
        .unwrap_or(s.len());

    let name = s[..end].trim();
    if name.is_empty() {
        return None;
    }

    // Normalize: PEP 503 says package names should be lowercased with
    // runs of [-_.] replaced by a single hyphen, but we keep the original
    // name as-is for registry matching since the registry uses the common
    // PyPI name form.
    Some(name.to_string())
}

// ── requirements.txt ─────────────────────────────────────────────────

fn parse_requirements_txt(project_dir: &Path, deps: &mut HashSet<String>) {
    let path = project_dir.join("requirements.txt");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };

    for line in content.lines() {
        let line = line.trim();
        // Skip empty lines, comments, and option flags
        if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
            continue;
        }
        if let Some(name) = strip_python_version_specifier(line) {
            deps.insert(name);
        }
    }
}

// ── Pipfile ──────────────────────────────────────────────────────────

fn parse_pipfile(project_dir: &Path, deps: &mut HashSet<String>, dev_deps: &mut HashSet<String>) {
    let path = project_dir.join("Pipfile");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let parsed: toml::Value = match content.parse() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Warning: failed to parse {}: {e}", path.display());
            return;
        }
    };

    if let Some(table) = parsed.get("packages").and_then(|v| v.as_table()) {
        for key in table.keys() {
            deps.insert(key.clone());
        }
    }
    if let Some(table) = parsed.get("dev-packages").and_then(|v| v.as_table()) {
        for key in table.keys() {
            dev_deps.insert(key.clone());
        }
    }
}

// ── go.mod ───────────────────────────────────────────────────────────

fn parse_go_mod(project_dir: &Path, deps: &mut HashSet<String>) {
    let path = project_dir.join("go.mod");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let mut in_require_block = false;

    for line in content.lines() {
        let line = line.trim();

        if let Some(after) = line
            .strip_prefix("require (")
            .or_else(|| line.strip_prefix("require("))
        {
            in_require_block = true;
            // Process any dependency on the remainder of this line
            let remainder = after.trim();
            if remainder == ")" {
                in_require_block = false;
            } else if !remainder.is_empty() && !remainder.starts_with("//") {
                // Strip inline comments before checking for closing paren,
                // matching the approach used in monorepo.rs parse_go_work.
                let remainder_no_comment = remainder.split("//").next().unwrap_or("").trim();
                let remainder = if let Some(stripped) = remainder_no_comment.strip_suffix(')') {
                    in_require_block = false;
                    stripped.trim()
                } else {
                    remainder_no_comment
                };
                for module in remainder.split_whitespace().take(1) {
                    deps.insert(module.to_string());
                }
            }
            continue;
        }

        if in_require_block {
            if line == ")" {
                in_require_block = false;
                continue;
            }
            // Lines like: github.com/gin-gonic/gin v1.9.1
            if let Some(module) = line.split_whitespace().next() {
                if !module.starts_with("//") {
                    deps.insert(module.to_string());
                }
            }
            continue;
        }

        // Single-line require: require github.com/foo/bar v1.0.0
        if let Some(rest) = line.strip_prefix("require ") {
            let rest = rest.trim();
            if let Some(module) = rest.split_whitespace().next() {
                deps.insert(module.to_string());
            }
        }
    }
}

// ── Gemfile ──────────────────────────────────────────────────────────

fn parse_gemfile(project_dir: &Path, deps: &mut HashSet<String>) {
    let path = project_dir.join("Gemfile");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };

    // Match: gem "name" or gem 'name'
    let re = regex_gem_name();
    for cap in re.captures_iter(&content) {
        // The name is in capture group 1 (double quotes) or 2 (single quotes)
        let name = cap
            .get(1)
            .or_else(|| cap.get(2))
            .map(|m| m.as_str().to_string());
        if let Some(name) = name {
            deps.insert(name);
        }
    }
}

/// Regex for matching `gem "name"` or `gem 'name'` in Gemfiles.
fn regex_gem_name() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"gem\s+"([^"]+)"|gem\s+'([^']+)'"#).unwrap()
    });
    &RE
}

// ── pom.xml ──────────────────────────────────────────────────────────

fn parse_pom_xml(project_dir: &Path, deps: &mut HashSet<String>) {
    let path = project_dir.join("pom.xml");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };

    // Match <dependency> blocks and extract groupId + artifactId
    let dep_block_re = regex_pom_dependency();
    let group_re = regex_pom_group_id();
    let artifact_re = regex_pom_artifact_id();

    for dep_match in dep_block_re.find_iter(&content) {
        let block = dep_match.as_str();
        let group_id = group_re
            .captures(block)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str());
        let artifact_id = artifact_re
            .captures(block)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str());

        if let Some(group) = group_id {
            deps.insert(group.to_string());
        }
        if let Some(artifact) = artifact_id {
            deps.insert(artifact.to_string());
        }
    }
}

fn regex_pom_dependency() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?s)<dependency>.*?</dependency>").unwrap()
    });
    &RE
}

fn regex_pom_group_id() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"<groupId>\s*([^<\s]+)\s*</groupId>").unwrap()
    });
    &RE
}

fn regex_pom_artifact_id() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"<artifactId>\s*([^<\s]+)\s*</artifactId>").unwrap()
    });
    &RE
}

// ── build.gradle / build.gradle.kts ──────────────────────────────────

fn parse_build_gradle(project_dir: &Path, deps: &mut HashSet<String>) {
    for filename in &["build.gradle", "build.gradle.kts"] {
        let path = project_dir.join(filename);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let re = regex_gradle_dependency();
        for cap in re.captures_iter(&content) {
            // At least one of the 3 capture groups always matches
            let coord = cap
                .get(1)
                .or_else(|| cap.get(2))
                .or_else(|| cap.get(3))
                .expect("regex guarantees at least one group matches")
                .as_str();
            // Format: group:artifact:version — extract artifact
            let parts: Vec<&str> = coord.split(':').collect();
            if parts.len() >= 2 {
                deps.insert(parts[1].to_string());
                // Also insert the groupId for Java/Kotlin framework matching
                deps.insert(parts[0].to_string());
            }
        }
    }
}

fn regex_gradle_dependency() -> &'static regex::Regex {
    // Matches: implementation("group:artifact:version") or implementation 'group:artifact:version'
    // Also matches other configurations like api, compileOnly, runtimeOnly, testImplementation, etc.
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r#"(?:implementation|api|compileOnly|runtimeOnly|testImplementation|testRuntimeOnly|kapt|annotationProcessor)\s*(?:\(\s*"([^"]+)"\s*\)|'([^']+)'|\(\s*'([^']+)'\s*\))"#,
        )
        .unwrap()
    });
    &RE
}

// ── Package.swift ────────────────────────────────────────────────────

fn parse_package_swift(project_dir: &Path, deps: &mut HashSet<String>) {
    let path = project_dir.join("Package.swift");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };

    // Match: .package(name: "Name" or .package(url: "...Name.git"
    let name_re = regex_swift_package_name();
    for cap in name_re.captures_iter(&content) {
        if let Some(name) = cap.get(1) {
            deps.insert(name.as_str().to_string());
        }
    }

    let url_re = regex_swift_package_url();
    for cap in url_re.captures_iter(&content) {
        // Group 1 always matches since the regex requires it
        let url_str = cap
            .get(1)
            .expect("regex guarantees group 1 matches")
            .as_str();
        let name = url_str
            .rsplit('/')
            .next()
            .map(|s| s.strip_suffix(".git").unwrap_or(s));
        if let Some(name) = name {
            deps.insert(name.to_string());
        }
    }
}

fn regex_swift_package_name() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"\.package\s*\(\s*name\s*:\s*"([^"]+)""#).unwrap()
    });
    &RE
}

fn regex_swift_package_url() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"\.package\s*\(\s*url\s*:\s*"([^"]+)""#).unwrap()
    });
    &RE
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_package_json() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{
                "dependencies": {
                    "react": "^18.0.0",
                    "next": "^14.0.0"
                },
                "devDependencies": {
                    "jest": "^29.0.0",
                    "typescript": "^5.0.0"
                }
            }"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.contains(&"react".to_string()));
        assert!(info.dependencies.contains(&"next".to_string()));
        assert!(info.dev_dependencies.contains(&"jest".to_string()));
        assert!(info.dev_dependencies.contains(&"typescript".to_string()));
    }

    #[test]
    fn test_cargo_toml() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            r#"
[package]
name = "my-app"
version = "0.1.0"

[dependencies]
actix-web = "4"
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["full"] }

[dev-dependencies]
tempfile = "3"
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.contains(&"actix-web".to_string()));
        assert!(info.dependencies.contains(&"serde".to_string()));
        assert!(info.dependencies.contains(&"tokio".to_string()));
        assert!(info.dev_dependencies.contains(&"tempfile".to_string()));
    }

    #[test]
    fn test_pyproject_toml_pep631() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            r#"
[project]
name = "my-app"
dependencies = [
    "django>=4.0",
    "celery~=5.3",
    "requests",
]

[project.optional-dependencies]
dev = [
    "pytest>=7.0",
    "black",
]
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.contains(&"django".to_string()));
        assert!(info.dependencies.contains(&"celery".to_string()));
        assert!(info.dependencies.contains(&"requests".to_string()));
        assert!(info.dev_dependencies.contains(&"pytest".to_string()));
        assert!(info.dev_dependencies.contains(&"black".to_string()));
    }

    #[test]
    fn test_pyproject_toml_poetry() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            r#"
[tool.poetry]
name = "my-app"

[tool.poetry.dependencies]
python = "^3.11"
fastapi = "^0.100"
uvicorn = "^0.23"

[tool.poetry.dev-dependencies]
pytest = "^7.0"
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.contains(&"fastapi".to_string()));
        assert!(info.dependencies.contains(&"uvicorn".to_string()));
        // "python" should be excluded
        assert!(!info.dependencies.contains(&"python".to_string()));
        assert!(info.dev_dependencies.contains(&"pytest".to_string()));
    }

    #[test]
    fn test_requirements_txt() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("requirements.txt"),
            r#"
# This is a comment
flask>=2.0
numpy==1.24.0
pandas~=2.0

requests
# Another comment

scikit-learn>=1.0
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.contains(&"flask".to_string()));
        assert!(info.dependencies.contains(&"numpy".to_string()));
        assert!(info.dependencies.contains(&"pandas".to_string()));
        assert!(info.dependencies.contains(&"requests".to_string()));
        assert!(info.dependencies.contains(&"scikit-learn".to_string()));
    }

    #[test]
    fn test_go_mod() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            r#"
module github.com/myorg/myapp

go 1.21

require (
	github.com/gin-gonic/gin v1.9.1
	github.com/spf13/cobra v1.7.0
)

require github.com/spf13/viper v1.16.0
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info
            .dependencies
            .contains(&"github.com/gin-gonic/gin".to_string()));
        assert!(info
            .dependencies
            .contains(&"github.com/spf13/cobra".to_string()));
        assert!(info
            .dependencies
            .contains(&"github.com/spf13/viper".to_string()));
    }

    #[test]
    fn test_gemfile() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Gemfile"),
            r#"
source "https://rubygems.org"

gem "rails", "~> 7.0"
gem 'sidekiq'
gem "rspec", group: :test
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.contains(&"rails".to_string()));
        assert!(info.dependencies.contains(&"sidekiq".to_string()));
        assert!(info.dependencies.contains(&"rspec".to_string()));
    }

    #[test]
    fn test_build_gradle() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("build.gradle"),
            r#"
plugins {
    id 'java'
}

dependencies {
    implementation 'org.springframework.boot:spring-boot-starter-web:3.1.0'
    testImplementation 'junit:junit:4.13.2'
}
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info
            .dependencies
            .contains(&"org.springframework.boot".to_string()));
        assert!(info
            .dependencies
            .contains(&"spring-boot-starter-web".to_string()));
    }

    #[test]
    fn test_build_gradle_kts() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("build.gradle.kts"),
            r#"
dependencies {
    implementation("io.ktor:ktor-server-core:2.3.0")
    implementation("io.ktor:ktor-server-netty:2.3.0")
}
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.contains(&"io.ktor".to_string()));
        assert!(info.dependencies.contains(&"ktor-server-core".to_string()));
    }

    #[test]
    fn test_missing_manifests_return_empty_deps() {
        let dir = tempdir().unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.is_empty());
        assert!(info.dev_dependencies.is_empty());
    }

    #[test]
    fn test_dependencies_are_sorted() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{
                "dependencies": {
                    "zod": "^3.0.0",
                    "axios": "^1.0.0",
                    "react": "^18.0.0"
                }
            }"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert_eq!(info.dependencies, vec!["axios", "react", "zod"]);
    }

    #[test]
    fn test_pom_xml() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pom.xml"),
            r#"
<project>
    <dependencies>
        <dependency>
            <groupId>org.springframework.boot</groupId>
            <artifactId>spring-boot-starter-web</artifactId>
            <version>3.1.0</version>
        </dependency>
    </dependencies>
</project>
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info
            .dependencies
            .contains(&"org.springframework.boot".to_string()));
        assert!(info
            .dependencies
            .contains(&"spring-boot-starter-web".to_string()));
    }

    #[test]
    fn test_package_swift() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Package.swift"),
            r#"
// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "MyApp",
    dependencies: [
        .package(name: "Alamofire", url: "https://github.com/Alamofire/Alamofire.git", from: "5.0.0"),
        .package(url: "https://github.com/vapor/vapor.git", from: "4.0.0"),
    ]
)
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.contains(&"Alamofire".to_string()));
        assert!(info.dependencies.contains(&"vapor".to_string()));
    }

    #[test]
    fn test_pipfile() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Pipfile"),
            r#"
[packages]
django = "*"
celery = ">=5.0"

[dev-packages]
pytest = "*"
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.contains(&"django".to_string()));
        assert!(info.dependencies.contains(&"celery".to_string()));
        assert!(info.dev_dependencies.contains(&"pytest".to_string()));
    }

    #[test]
    fn test_package_json_invalid_json() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "not json at all").unwrap();
        let info = parse_dependencies(dir.path());
        // Should not crash, just return empty
        assert!(info.dependencies.is_empty());
    }

    #[test]
    fn test_package_json_no_dependencies_key() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{ "name": "my-app", "version": "1.0.0" }"#,
        )
        .unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.is_empty());
        assert!(info.dev_dependencies.is_empty());
    }

    #[test]
    fn test_cargo_toml_invalid_toml() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "not valid toml [[[").unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.is_empty());
    }

    #[test]
    fn test_pyproject_toml_invalid_toml() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "[[[invalid").unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.is_empty());
    }

    #[test]
    fn test_pipfile_invalid_toml() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Pipfile"), "[[[invalid").unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.is_empty());
    }

    #[test]
    fn test_go_mod_with_comments_in_require_block() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            r#"
module github.com/myorg/myapp

go 1.21

require (
	// This is an indirect dependency
	github.com/gin-gonic/gin v1.9.1
	github.com/spf13/cobra v1.7.0
)
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        // The comment line should be skipped
        assert!(!info.dependencies.iter().any(|d| d.starts_with("//")));
        assert!(info
            .dependencies
            .contains(&"github.com/gin-gonic/gin".to_string()));
        assert!(info
            .dependencies
            .contains(&"github.com/spf13/cobra".to_string()));
    }

    #[test]
    fn test_go_mod_require_no_space_before_paren() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module example.com/foo\n\ngo 1.21\n\nrequire(\n\tgithub.com/bar/baz v1.0.0\n)\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info
            .dependencies
            .contains(&"github.com/bar/baz".to_string()));
    }

    #[test]
    fn test_requirements_txt_with_flags() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("requirements.txt"),
            "-r other-requirements.txt\n-e git+https://example.com/foo.git\nflask>=2.0\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        // -r and -e lines should be skipped
        assert!(info.dependencies.contains(&"flask".to_string()));
        assert_eq!(info.dependencies.len(), 1);
    }

    #[test]
    fn test_package_swift_url_without_git_suffix() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Package.swift"),
            r#"
let package = Package(
    dependencies: [
        .package(url: "https://github.com/apple/swift-argument-parser", from: "1.0.0"),
    ]
)
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info
            .dependencies
            .contains(&"swift-argument-parser".to_string()));
    }

    #[test]
    fn test_build_gradle_api_configuration() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("build.gradle"),
            r#"
dependencies {
    api 'com.google.guava:guava:31.1-jre'
    compileOnly 'org.projectlombok:lombok:1.18.24'
    runtimeOnly 'org.postgresql:postgresql:42.5.1'
}
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.contains(&"guava".to_string()));
        assert!(info.dependencies.contains(&"lombok".to_string()));
        assert!(info.dependencies.contains(&"postgresql".to_string()));
    }

    #[test]
    fn test_build_gradle_single_quoted_parens() {
        // Tests regex group 3: implementation('group:artifact:version')
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("build.gradle"),
            "dependencies {\n    implementation('com.example:mylib:1.0.0')\n}\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.contains(&"mylib".to_string()));
        assert!(info.dependencies.contains(&"com.example".to_string()));
    }

    #[test]
    fn test_pom_xml_dependency_without_group_id() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pom.xml"),
            r#"
<project>
    <dependencies>
        <dependency>
            <artifactId>junit</artifactId>
            <version>4.13</version>
        </dependency>
    </dependencies>
</project>
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.contains(&"junit".to_string()));
    }

    #[test]
    fn test_pom_xml_dependency_without_artifact_id() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pom.xml"),
            r#"
<project>
    <dependencies>
        <dependency>
            <groupId>com.example</groupId>
            <version>1.0</version>
        </dependency>
    </dependencies>
</project>
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.contains(&"com.example".to_string()));
    }

    #[test]
    fn test_cargo_toml_no_dependencies_section() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"my-app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.is_empty());
        assert!(info.dev_dependencies.is_empty());
    }

    #[test]
    fn test_pipfile_no_packages_section() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Pipfile"),
            "[source]\nurl = \"https://pypi.org/simple\"\n",
        )
        .unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.is_empty());
        assert!(info.dev_dependencies.is_empty());
    }

    #[test]
    fn test_pyproject_toml_non_string_dependency() {
        // Test where item.as_str() returns None in PEP 631 dependencies
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"my-app\"\ndependencies = [42, \"flask\"]\n",
        )
        .unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.contains(&"flask".to_string()));
        // 42 should be silently skipped
    }

    #[test]
    fn test_pyproject_toml_optional_deps_non_array_group() {
        // Test where optional-dependencies group value is not an array
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"my-app\"\n\n[project.optional-dependencies]\ndev = \"not-an-array\"\n",
        )
        .unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dev_dependencies.is_empty());
    }

    #[test]
    fn test_pyproject_toml_optional_deps_non_string_items() {
        // Test where optional-dependencies items are not strings
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"my-app\"\n\n[project.optional-dependencies]\ndev = [42, \"pytest\"]\n",
        )
        .unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dev_dependencies.contains(&"pytest".to_string()));
    }

    #[test]
    fn test_strip_python_specifier_only_specifier() {
        // A string like ">=3.0" with no package name before the specifier
        assert_eq!(strip_python_version_specifier(">=3.0"), None);
    }

    #[test]
    fn test_go_mod_empty_lines_in_require_block() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module example.com/foo\n\ngo 1.21\n\nrequire (\n\n\tgithub.com/bar/baz v1.0.0\n\n)\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info
            .dependencies
            .contains(&"github.com/bar/baz".to_string()));
    }

    #[test]
    fn test_build_gradle_no_matching_pattern() {
        // A gradle file with no matching dependency patterns
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("build.gradle"),
            "plugins {\n    id 'java'\n}\n",
        )
        .unwrap();
        let info = parse_dependencies(dir.path());
        // No deps from this file
        assert!(info.dependencies.is_empty());
    }

    #[test]
    fn test_package_swift_url_only_name_portion() {
        // URL that is just a bare name without .git suffix
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Package.swift"),
            ".package(url: \"https://github.com/apple/swift-nio\", from: \"2.0.0\")\n",
        )
        .unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.contains(&"swift-nio".to_string()));
    }

    #[test]
    fn test_go_mod_require_entry_on_same_line() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module example.com/foo\n\ngo 1.21\n\nrequire (github.com/bar/baz v1.0.0\n\tgithub.com/qux/quux v2.0.0\n)\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info
            .dependencies
            .contains(&"github.com/bar/baz".to_string()));
        assert!(info
            .dependencies
            .contains(&"github.com/qux/quux".to_string()));
    }

    #[test]
    fn test_go_mod_require_single_entry_with_closing_paren() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module example.com/foo\n\ngo 1.21\n\nrequire (github.com/bar/baz v1.0.0)\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info
            .dependencies
            .contains(&"github.com/bar/baz".to_string()));
    }

    #[test]
    fn test_go_mod_require_empty_parens() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module example.com/foo\n\ngo 1.21\n\nrequire ()\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.is_empty());
    }

    #[test]
    fn test_go_mod_require_inline_comment_after_paren() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module example.com/foo\n\ngo 1.21\n\nrequire (// indirect deps\n\tgithub.com/bar/baz v1.0.0\n)\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        // The comment on the same line as "require (" should be skipped,
        // but the next line should still be parsed
        assert!(info
            .dependencies
            .contains(&"github.com/bar/baz".to_string()));
    }

    #[test]
    fn test_go_mod_require_no_space_inline_entry() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module example.com/foo\n\ngo 1.21\n\nrequire(github.com/bar/baz v1.0.0\n\tgithub.com/qux/quux v2.0.0\n)\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info
            .dependencies
            .contains(&"github.com/bar/baz".to_string()));
        assert!(info
            .dependencies
            .contains(&"github.com/qux/quux".to_string()));
    }

    #[test]
    fn test_go_mod_require_whitespace_before_closing_paren() {
        // Covers the branch where the remainder after `require (` trims to ")",
        // triggering the early `remainder == ")"` check and not entering the
        // `else if` branch at all.
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module example.com/foo\n\ngo 1.21\n\nrequire ( \t )\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.is_empty());
    }

    #[test]
    fn test_strip_python_version_specifier() {
        assert_eq!(
            strip_python_version_specifier("flask>=2.0"),
            Some("flask".to_string())
        );
        assert_eq!(
            strip_python_version_specifier("numpy==1.24.0"),
            Some("numpy".to_string())
        );
        assert_eq!(
            strip_python_version_specifier("pandas~=2.0"),
            Some("pandas".to_string())
        );
        assert_eq!(
            strip_python_version_specifier("requests"),
            Some("requests".to_string())
        );
        assert_eq!(
            strip_python_version_specifier("scikit-learn>=1.0,<2.0"),
            Some("scikit-learn".to_string())
        );
        assert_eq!(
            strip_python_version_specifier("  torch ; python_version >= \"3.8\""),
            Some("torch".to_string())
        );
        assert_eq!(strip_python_version_specifier(""), None);
        assert_eq!(
            strip_python_version_specifier("flask[async]~=2.0"),
            Some("flask".to_string())
        );
    }
}
