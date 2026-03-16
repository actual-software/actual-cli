use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::analysis::types::Language;

/// Maximum manifest file size to read into memory (10 MB).
const MAX_MANIFEST_SIZE: u64 = 10 * 1024 * 1024;

/// Read a manifest file, returning `None` if the file is missing, unreadable,
/// or exceeds [`MAX_MANIFEST_SIZE`].  This prevents OOM on adversarial repos
/// that contain abnormally large manifest files.
fn read_manifest_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() as u64 > MAX_MANIFEST_SIZE {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Which manifest file produced a dependency.
///
/// Used to filter frameworks by the language(s) selected for a project:
/// e.g. if the user picks TypeScript, only deps from `PackageJson` are relevant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestSource {
    PackageJson,
    CargoToml,
    PyprojectToml,
    RequirementsTxt,
    Pipfile,
    GoMod,
    Gemfile,
    PomXml,
    BuildGradle,
    GradleVersionCatalog,
    PackageSwift,
    CsprojFile,
    CMakeListsTxt,
    VcpkgJson,
    ConanFileTxt,
    ConfigFile,
}

impl ManifestSource {
    /// Languages that are compatible with dependencies from this manifest type.
    ///
    /// An empty vec means "compatible with all languages" (used for `ConfigFile`
    /// where the language is indeterminate and filtering is deferred).
    pub fn compatible_languages(&self) -> Vec<Language> {
        match self {
            ManifestSource::PackageJson => {
                vec![Language::TypeScript, Language::JavaScript]
            }
            ManifestSource::CargoToml => vec![Language::Rust],
            ManifestSource::PyprojectToml
            | ManifestSource::RequirementsTxt
            | ManifestSource::Pipfile => vec![Language::Python],
            ManifestSource::GoMod => vec![Language::Go],
            ManifestSource::Gemfile => vec![Language::Ruby],
            ManifestSource::PomXml => vec![Language::Java],
            ManifestSource::BuildGradle | ManifestSource::GradleVersionCatalog => {
                vec![Language::Java, Language::Kotlin]
            }
            ManifestSource::PackageSwift => vec![Language::Swift],
            ManifestSource::CsprojFile => vec![Language::CSharp],
            ManifestSource::CMakeListsTxt
            | ManifestSource::VcpkgJson
            | ManifestSource::ConanFileTxt => vec![Language::C, Language::Cpp],
            ManifestSource::ConfigFile => vec![],
        }
    }
}

/// Dependency information extracted from manifest files.
#[derive(Debug, Default)]
pub struct DependencyInfo {
    /// Production dependency names.
    pub dependencies: Vec<String>,
    /// Development-only dependency names.
    pub dev_dependencies: Vec<String>,
    /// Which manifest file each dependency came from (first source wins).
    pub sources: HashMap<String, ManifestSource>,
}

/// Parse dependencies from all recognized manifest files in `project_dir`.
///
/// Scans for package.json, Cargo.toml, pyproject.toml, requirements.txt,
/// Pipfile, go.mod, Gemfile, pom.xml, build.gradle(.kts),
/// gradle/libs.versions.toml, and Package.swift.
/// Missing files are silently skipped — an empty project returns empty deps.
pub fn parse_dependencies(project_dir: &Path) -> DependencyInfo {
    let mut deps = HashSet::new();
    let mut dev_deps = HashSet::new();
    let mut sources = HashMap::new();

    parse_package_json(project_dir, &mut deps, &mut dev_deps, &mut sources);
    parse_cargo_toml(project_dir, &mut deps, &mut dev_deps, &mut sources);
    parse_pyproject_toml(project_dir, &mut deps, &mut dev_deps, &mut sources);
    parse_requirements_txt(project_dir, &mut deps, &mut sources);
    parse_pipfile(project_dir, &mut deps, &mut dev_deps, &mut sources);
    parse_go_mod(project_dir, &mut deps, &mut sources);
    parse_gemfile(project_dir, &mut deps, &mut sources);
    parse_pom_xml(project_dir, &mut deps, &mut sources);
    parse_build_gradle(project_dir, &mut deps, &mut sources);
    parse_gradle_version_catalog(project_dir, &mut deps, &mut sources);
    parse_package_swift(project_dir, &mut deps, &mut sources);
    parse_csproj(project_dir, &mut deps, &mut sources);
    parse_cmake_lists(project_dir, &mut deps, &mut sources);
    parse_vcpkg_json(project_dir, &mut deps, &mut sources);
    parse_conan_file_txt(project_dir, &mut deps, &mut sources);

    let mut dependencies: Vec<String> = deps.into_iter().collect();
    dependencies.sort();
    let mut dev_dependencies: Vec<String> = dev_deps.into_iter().collect();
    dev_dependencies.sort();

    DependencyInfo {
        dependencies,
        dev_dependencies,
        sources,
    }
}

// ── package.json ─────────────────────────────────────────────────────

fn parse_package_json(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    dev_deps: &mut HashSet<String>,
    sources: &mut HashMap<String, ManifestSource>,
) {
    let path = project_dir.join("package.json");
    let content = match read_manifest_file(&path) {
        Some(c) => c,
        None => return,
    };
    let parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("failed to parse {}: {e}", path.display());
            return;
        }
    };

    if let Some(obj) = parsed.get("dependencies").and_then(|v| v.as_object()) {
        for key in obj.keys() {
            deps.insert(key.clone());
            sources
                .entry(key.clone())
                .or_insert(ManifestSource::PackageJson);
        }
    }
    if let Some(obj) = parsed.get("devDependencies").and_then(|v| v.as_object()) {
        for key in obj.keys() {
            dev_deps.insert(key.clone());
            sources
                .entry(key.clone())
                .or_insert(ManifestSource::PackageJson);
        }
    }
}

// ── Cargo.toml ───────────────────────────────────────────────────────

fn parse_cargo_toml(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    dev_deps: &mut HashSet<String>,
    sources: &mut HashMap<String, ManifestSource>,
) {
    let path = project_dir.join("Cargo.toml");
    let content = match read_manifest_file(&path) {
        Some(c) => c,
        None => return,
    };
    let parsed: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("failed to parse {}: {e}", path.display());
            return;
        }
    };

    if let Some(table) = parsed.get("dependencies").and_then(|v| v.as_table()) {
        for key in table.keys() {
            deps.insert(key.clone());
            sources
                .entry(key.clone())
                .or_insert(ManifestSource::CargoToml);
        }
    }
    if let Some(table) = parsed.get("dev-dependencies").and_then(|v| v.as_table()) {
        for key in table.keys() {
            dev_deps.insert(key.clone());
            sources
                .entry(key.clone())
                .or_insert(ManifestSource::CargoToml);
        }
    }
}

// ── pyproject.toml ───────────────────────────────────────────────────

/// Group names in `[project.optional-dependencies]` that are considered dev-only.
/// Any group whose name is NOT in this list is treated as a production extra.
const DEV_GROUP_NAMES: &[&str] = &[
    "dev",
    "develop",
    "development",
    "test",
    "testing",
    "tests",
    "lint",
    "linting",
    "docs",
    "doc",
    "documentation",
    "typing",
    "type-checking",
    "types",
    "ci",
    "style",
    "format",
    "formatting",
];

fn parse_pyproject_toml(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    dev_deps: &mut HashSet<String>,
    sources: &mut HashMap<String, ManifestSource>,
) {
    let path = project_dir.join("pyproject.toml");
    let content = match read_manifest_file(&path) {
        Some(c) => c,
        None => return,
    };
    let parsed: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("failed to parse {}: {e}", path.display());
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
                    deps.insert(name.clone());
                    sources.entry(name).or_insert(ManifestSource::PyprojectToml);
                }
            }
        }
    }

    // PEP 631 optional-dependencies (heuristic: dev-like group names → dev_deps, others → deps)
    if let Some(table) = parsed
        .get("project")
        .and_then(|p| p.get("optional-dependencies"))
        .and_then(|d| d.as_table())
    {
        for (group, arr) in table {
            if let Some(arr) = arr.as_array() {
                let target = if DEV_GROUP_NAMES.contains(&group.as_str()) {
                    &mut *dev_deps
                } else {
                    &mut *deps
                };
                for item in arr {
                    if let Some(s) = item.as_str() {
                        if let Some(name) = strip_python_version_specifier(s) {
                            target.insert(name.clone());
                            sources.entry(name).or_insert(ManifestSource::PyprojectToml);
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
                sources
                    .entry(key.clone())
                    .or_insert(ManifestSource::PyprojectToml);
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
            sources
                .entry(key.clone())
                .or_insert(ManifestSource::PyprojectToml);
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

fn parse_requirements_txt(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    sources: &mut HashMap<String, ManifestSource>,
) {
    let path = project_dir.join("requirements.txt");
    let content = match read_manifest_file(&path) {
        Some(c) => c,
        None => return,
    };

    for line in content.lines() {
        let line = line.trim();
        // Skip empty lines, comments, and option flags
        if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
            continue;
        }
        if let Some(name) = strip_python_version_specifier(line) {
            deps.insert(name.clone());
            sources
                .entry(name)
                .or_insert(ManifestSource::RequirementsTxt);
        }
    }
}

// ── Pipfile ──────────────────────────────────────────────────────────

fn parse_pipfile(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    dev_deps: &mut HashSet<String>,
    sources: &mut HashMap<String, ManifestSource>,
) {
    let path = project_dir.join("Pipfile");
    let content = match read_manifest_file(&path) {
        Some(c) => c,
        None => return,
    };
    let parsed: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("failed to parse {}: {e}", path.display());
            return;
        }
    };

    if let Some(table) = parsed.get("packages").and_then(|v| v.as_table()) {
        for key in table.keys() {
            deps.insert(key.clone());
            sources
                .entry(key.clone())
                .or_insert(ManifestSource::Pipfile);
        }
    }
    if let Some(table) = parsed.get("dev-packages").and_then(|v| v.as_table()) {
        for key in table.keys() {
            dev_deps.insert(key.clone());
            sources
                .entry(key.clone())
                .or_insert(ManifestSource::Pipfile);
        }
    }
}

// ── go.mod ───────────────────────────────────────────────────────────

fn parse_go_mod(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    sources: &mut HashMap<String, ManifestSource>,
) {
    let path = project_dir.join("go.mod");
    let content = match read_manifest_file(&path) {
        Some(c) => c,
        None => return,
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
                    let name = module.to_string();
                    deps.insert(name.clone());
                    sources.entry(name).or_insert(ManifestSource::GoMod);
                }
            }
            continue;
        }

        if in_require_block {
            if line == ")" {
                in_require_block = false;
                continue;
            }
            // Skip indirect (transitive) dependencies
            if line.contains("// indirect") {
                continue;
            }
            // Lines like: github.com/gin-gonic/gin v1.9.1
            if let Some(module) = line.split_whitespace().next() {
                if !module.starts_with("//") {
                    let name = module.to_string();
                    deps.insert(name.clone());
                    sources.entry(name).or_insert(ManifestSource::GoMod);
                }
            }
            continue;
        }

        // Single-line require: require github.com/foo/bar v1.0.0
        if let Some(rest) = line.strip_prefix("require ") {
            // Skip indirect (transitive) dependencies
            if line.contains("// indirect") {
                continue;
            }
            let rest = rest.trim();
            if let Some(module) = rest.split_whitespace().next() {
                let name = module.to_string();
                deps.insert(name.clone());
                sources.entry(name).or_insert(ManifestSource::GoMod);
            }
        }
    }
}

// ── Gemfile ──────────────────────────────────────────────────────────

fn parse_gemfile(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    sources: &mut HashMap<String, ManifestSource>,
) {
    let path = project_dir.join("Gemfile");
    let content = match read_manifest_file(&path) {
        Some(c) => c,
        None => return,
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
            deps.insert(name.clone());
            sources.entry(name).or_insert(ManifestSource::Gemfile);
        }
    }
}

/// Regex for matching `gem "name"` or `gem 'name'` in Gemfiles.
fn regex_gem_name() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"gem\s+"([^"]+)"|gem\s+'([^']+)'"#)
            .expect("valid regex — this is a programmer error")
    });
    &RE
}

// ── pom.xml ──────────────────────────────────────────────────────────

fn parse_pom_xml(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    sources: &mut HashMap<String, ManifestSource>,
) {
    let path = project_dir.join("pom.xml");
    let content = match read_manifest_file(&path) {
        Some(c) => c,
        None => return,
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
            let group_name = group.to_string();
            deps.insert(group_name.clone());
            sources.entry(group_name).or_insert(ManifestSource::PomXml);
            if let Some(artifact) = artifact_id {
                let coord = format!("{group}:{artifact}");
                deps.insert(coord.clone());
                sources.entry(coord).or_insert(ManifestSource::PomXml);
            }
        } else if let Some(artifact) = artifact_id {
            let name = artifact.to_string();
            deps.insert(name.clone());
            sources.entry(name).or_insert(ManifestSource::PomXml);
        }
    }
}

fn regex_pom_dependency() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?s)<dependency>.*?</dependency>")
            .expect("valid regex — this is a programmer error")
    });
    &RE
}

fn regex_pom_group_id() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"<groupId>\s*([^<\s]+)\s*</groupId>")
            .expect("valid regex — this is a programmer error")
    });
    &RE
}

fn regex_pom_artifact_id() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"<artifactId>\s*([^<\s]+)\s*</artifactId>")
            .expect("valid regex — this is a programmer error")
    });
    &RE
}

// ── build.gradle / build.gradle.kts ──────────────────────────────────

fn parse_build_gradle(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    sources: &mut HashMap<String, ManifestSource>,
) {
    for filename in &["build.gradle", "build.gradle.kts"] {
        let path = project_dir.join(filename);
        let content = match read_manifest_file(&path) {
            Some(c) => c,
            None => continue,
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
            insert_gradle_coord(coord, deps, sources, ManifestSource::BuildGradle);
        }
    }
}

fn regex_gradle_dependency() -> &'static regex::Regex {
    // Matches: implementation("group:artifact:version") or implementation 'group:artifact:version'
    // Also matches other configurations like api, compileOnly, runtimeOnly, testImplementation, etc.
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r#"(?:implementation|api|compileOnly|runtimeOnly|testImplementation|testRuntimeOnly|kapt|annotationProcessor|classpath)\s*(?:\(\s*"([^"]+)"\s*\)|'([^']+)'|\(\s*'([^']+)'\s*\))"#,
        )
        .expect("valid regex — this is a programmer error")
    });
    &RE
}

// ── gradle/libs.versions.toml (Gradle Version Catalog) ───────────────

/// Parse `gradle/libs.versions.toml` (Gradle Version Catalog) for library
/// coordinates. Handles three declaration forms in the `[libraries]` section:
///
/// 1. Inline string:  `alias = "group:artifact:version"`
/// 2. `module` key:   `alias = { module = "group:artifact", version.ref = "x" }`
/// 3. `group`+`name`: `alias = { group = "com.example", name = "lib", ... }`
///
/// Missing file or parse errors are silently skipped.
pub(crate) fn parse_gradle_version_catalog(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    sources: &mut HashMap<String, ManifestSource>,
) {
    let path = project_dir.join("gradle/libs.versions.toml");
    let content = match read_manifest_file(&path) {
        Some(c) => c,
        None => return,
    };

    let table: toml::Table = match toml::from_str(&content) {
        Ok(t) => t,
        Err(_) => return,
    };

    let libraries = match table.get("libraries").and_then(|v| v.as_table()) {
        Some(t) => t,
        None => return,
    };

    for (_alias, value) in libraries {
        match value {
            toml::Value::String(s) => {
                insert_gradle_coord(s, deps, sources, ManifestSource::GradleVersionCatalog);
            }
            toml::Value::Table(t) => {
                if let Some(toml::Value::String(module)) = t.get("module") {
                    insert_gradle_coord(
                        module,
                        deps,
                        sources,
                        ManifestSource::GradleVersionCatalog,
                    );
                } else if let (Some(toml::Value::String(group)), Some(toml::Value::String(name))) =
                    (t.get("group"), t.get("name"))
                {
                    let coord = format!("{group}:{name}");
                    insert_gradle_coord(
                        &coord,
                        deps,
                        sources,
                        ManifestSource::GradleVersionCatalog,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Insert a `group:artifact[:version]` coordinate into `deps`, emitting both
/// the bare groupId and the combined `group:artifact` coordinate.
fn insert_gradle_coord(
    coord: &str,
    deps: &mut HashSet<String>,
    sources: &mut HashMap<String, ManifestSource>,
    manifest_source: ManifestSource,
) {
    let parts: Vec<&str> = coord.split(':').collect();
    if parts.len() >= 2 {
        let group = parts[0].to_string();
        let combined = format!("{}:{}", parts[0], parts[1]);
        deps.insert(group.clone());
        sources.entry(group).or_insert(manifest_source.clone());
        deps.insert(combined.clone());
        sources.entry(combined).or_insert(manifest_source);
    }
}

// ── Package.swift ────────────────────────────────────────────────────

fn parse_package_swift(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    sources: &mut HashMap<String, ManifestSource>,
) {
    let path = project_dir.join("Package.swift");
    let content = match read_manifest_file(&path) {
        Some(c) => c,
        None => return,
    };

    // Match: .package(name: "Name" or .package(url: "...Name.git"
    let name_re = regex_swift_package_name();
    for cap in name_re.captures_iter(&content) {
        if let Some(name) = cap.get(1) {
            let dep_name = name.as_str().to_string();
            deps.insert(dep_name.clone());
            sources
                .entry(dep_name)
                .or_insert(ManifestSource::PackageSwift);
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
            let dep_name = name.to_string();
            deps.insert(dep_name.clone());
            sources
                .entry(dep_name)
                .or_insert(ManifestSource::PackageSwift);
        }
    }
}

fn regex_swift_package_name() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"\.package\s*\(\s*name\s*:\s*"([^"]+)""#)
            .expect("valid regex — this is a programmer error")
    });
    &RE
}

fn regex_swift_package_url() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"\.package\s*\(\s*url\s*:\s*"([^"]+)""#)
            .expect("valid regex — this is a programmer error")
    });
    &RE
}

// ── *.csproj ─────────────────────────────────────────────────────────

fn parse_csproj(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    sources: &mut HashMap<String, ManifestSource>,
) {
    let entries = match std::fs::read_dir(project_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("csproj"))
        {
            parse_single_csproj(&path, deps, sources);
        }
    }
}

fn parse_single_csproj(
    path: &Path,
    deps: &mut HashSet<String>,
    sources: &mut HashMap<String, ManifestSource>,
) {
    let content = match read_manifest_file(path) {
        Some(c) => c,
        None => return,
    };

    // Project Sdk attribute → treat as a synthetic dep (e.g. "Microsoft.NET.Sdk.Web")
    let sdk_re = regex_csproj_sdk();
    if let Some(cap) = sdk_re.captures(&content) {
        if let Some(m) = cap.get(1) {
            let name = m.as_str().to_string();
            deps.insert(name.clone());
            sources.entry(name).or_insert(ManifestSource::CsprojFile);
        }
    }

    // PackageReference Include values
    let pkg_re = regex_csproj_package_reference();
    for cap in pkg_re.captures_iter(&content) {
        if let Some(m) = cap.get(1) {
            let name = m.as_str().to_string();
            deps.insert(name.clone());
            sources.entry(name).or_insert(ManifestSource::CsprojFile);
        }
    }
}

fn regex_csproj_sdk() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)<Project\s[^>]*Sdk\s*=\s*"([^"]+)""#)
            .expect("valid regex — this is a programmer error")
    });
    &RE
}

fn regex_csproj_package_reference() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)<PackageReference\s+Include\s*=\s*"([^"]+)""#)
            .expect("valid regex — this is a programmer error")
    });
    &RE
}

// ── CMakeLists.txt ────────────────────────────────────────────────────

fn parse_cmake_lists(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    sources: &mut HashMap<String, ManifestSource>,
) {
    let path = project_dir.join("CMakeLists.txt");
    let content = match read_manifest_file(&path) {
        Some(c) => c,
        None => return,
    };
    for cap in regex_cmake_find_package().captures_iter(&content) {
        let name = cap[1].to_string();
        deps.insert(name.clone());
        sources.entry(name).or_insert(ManifestSource::CMakeListsTxt);
    }
    for cap in regex_cmake_fetch_content().captures_iter(&content) {
        let name = cap[1].to_string();
        deps.insert(name.clone());
        sources.entry(name).or_insert(ManifestSource::CMakeListsTxt);
    }
}

fn regex_cmake_find_package() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\bfind_package\s*\(\s*([A-Za-z][A-Za-z0-9_.-]*)")
            .expect("valid regex — this is a programmer error")
    });
    &RE
}

fn regex_cmake_fetch_content() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\bFetchContent_Declare\s*\(\s*([A-Za-z][A-Za-z0-9_.-]*)")
            .expect("valid regex — this is a programmer error")
    });
    &RE
}

// ── vcpkg.json ────────────────────────────────────────────────────────

fn parse_vcpkg_json(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    sources: &mut HashMap<String, ManifestSource>,
) {
    let path = project_dir.join("vcpkg.json");
    let content = match read_manifest_file(&path) {
        Some(c) => c,
        None => return,
    };
    let parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("failed to parse {}: {e}", path.display());
            return;
        }
    };
    if let Some(arr) = parsed.get("dependencies").and_then(|v| v.as_array()) {
        for item in arr {
            let name = if let Some(s) = item.as_str() {
                s.to_string()
            } else if let Some(n) = item.get("name").and_then(|v| v.as_str()) {
                n.to_string()
            } else {
                continue;
            };
            deps.insert(name.clone());
            sources.entry(name).or_insert(ManifestSource::VcpkgJson);
        }
    }
}

// ── conanfile.txt ─────────────────────────────────────────────────────

fn parse_conan_file_txt(
    project_dir: &Path,
    deps: &mut HashSet<String>,
    sources: &mut HashMap<String, ManifestSource>,
) {
    let path = project_dir.join("conanfile.txt");
    let content = match read_manifest_file(&path) {
        Some(c) => c,
        None => return,
    };
    let mut in_requires = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_requires = matches!(line, "[requires]" | "[build_requires]");
            continue;
        }
        if !in_requires || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let name = line.split('/').next().unwrap_or(line).trim().to_string();
        if !name.is_empty() {
            deps.insert(name.clone());
            sources.entry(name).or_insert(ManifestSource::ConanFileTxt);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    // ── ManifestSource tests ─────────────────────────────────────────

    #[test]
    fn test_manifest_source_compatible_languages_package_json() {
        let langs = ManifestSource::PackageJson.compatible_languages();
        assert_eq!(langs, vec![Language::TypeScript, Language::JavaScript]);
    }

    #[test]
    fn test_manifest_source_compatible_languages_cargo_toml() {
        let langs = ManifestSource::CargoToml.compatible_languages();
        assert_eq!(langs, vec![Language::Rust]);
    }

    #[test]
    fn test_manifest_source_compatible_languages_python_manifests() {
        for src in &[
            ManifestSource::PyprojectToml,
            ManifestSource::RequirementsTxt,
            ManifestSource::Pipfile,
        ] {
            let langs = src.compatible_languages();
            assert_eq!(langs, vec![Language::Python], "failed for {src:?}");
        }
    }

    #[test]
    fn test_manifest_source_compatible_languages_go_mod() {
        let langs = ManifestSource::GoMod.compatible_languages();
        assert_eq!(langs, vec![Language::Go]);
    }

    #[test]
    fn test_manifest_source_compatible_languages_gemfile() {
        let langs = ManifestSource::Gemfile.compatible_languages();
        assert_eq!(langs, vec![Language::Ruby]);
    }

    #[test]
    fn test_manifest_source_compatible_languages_pom_xml() {
        let langs = ManifestSource::PomXml.compatible_languages();
        assert_eq!(langs, vec![Language::Java]);
    }

    #[test]
    fn test_manifest_source_compatible_languages_gradle() {
        for src in &[
            ManifestSource::BuildGradle,
            ManifestSource::GradleVersionCatalog,
        ] {
            let langs = src.compatible_languages();
            assert_eq!(
                langs,
                vec![Language::Java, Language::Kotlin],
                "failed for {src:?}"
            );
        }
    }

    #[test]
    fn test_manifest_source_compatible_languages_package_swift() {
        let langs = ManifestSource::PackageSwift.compatible_languages();
        assert_eq!(langs, vec![Language::Swift]);
    }

    #[test]
    fn test_manifest_source_compatible_languages_config_file() {
        let langs = ManifestSource::ConfigFile.compatible_languages();
        assert!(langs.is_empty(), "ConfigFile should return empty vec");
    }

    #[test]
    fn test_manifest_source_serde_round_trip() {
        let all_sources = vec![
            ManifestSource::PackageJson,
            ManifestSource::CargoToml,
            ManifestSource::PyprojectToml,
            ManifestSource::RequirementsTxt,
            ManifestSource::Pipfile,
            ManifestSource::GoMod,
            ManifestSource::Gemfile,
            ManifestSource::PomXml,
            ManifestSource::BuildGradle,
            ManifestSource::GradleVersionCatalog,
            ManifestSource::PackageSwift,
            ManifestSource::CsprojFile,
            ManifestSource::CMakeListsTxt,
            ManifestSource::VcpkgJson,
            ManifestSource::ConanFileTxt,
            ManifestSource::ConfigFile,
        ];
        for src in &all_sources {
            let json = serde_json::to_string(src).unwrap();
            let deserialized: ManifestSource = serde_json::from_str(&json).unwrap();
            assert_eq!(&deserialized, src, "Round-trip failed for {json}");
        }
    }

    #[test]
    fn test_manifest_source_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&ManifestSource::PackageJson).unwrap(),
            "\"package_json\""
        );
        assert_eq!(
            serde_json::to_string(&ManifestSource::CargoToml).unwrap(),
            "\"cargo_toml\""
        );
        assert_eq!(
            serde_json::to_string(&ManifestSource::GradleVersionCatalog).unwrap(),
            "\"gradle_version_catalog\""
        );
    }

    // ── Sources HashMap population tests ─────────────────────────────

    #[test]
    fn test_sources_populated_for_package_json() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies": {"react": "^18.0.0"}, "devDependencies": {"jest": "^29.0.0"}}"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert_eq!(
            info.sources.get("react"),
            Some(&ManifestSource::PackageJson)
        );
        assert_eq!(info.sources.get("jest"), Some(&ManifestSource::PackageJson));
    }

    #[test]
    fn test_sources_populated_for_cargo_toml() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1\"\n\n[dev-dependencies]\ntempfile = \"3\"\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert_eq!(info.sources.get("serde"), Some(&ManifestSource::CargoToml));
        assert_eq!(
            info.sources.get("tempfile"),
            Some(&ManifestSource::CargoToml)
        );
    }

    #[test]
    fn test_sources_first_source_wins() {
        // If a dep appears in multiple manifests, the first parser's source wins.
        // parse_package_json runs before parse_requirements_txt, so PackageJson wins.
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies": {"flask": "^2.0.0"}}"#,
        )
        .unwrap();
        fs::write(dir.path().join("requirements.txt"), "flask>=2.0\n").unwrap();

        let info = parse_dependencies(dir.path());
        assert_eq!(
            info.sources.get("flask"),
            Some(&ManifestSource::PackageJson),
            "First source should win"
        );
    }

    #[test]
    fn test_sources_populated_for_go_mod() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module example.com/foo\n\ngo 1.21\n\nrequire github.com/gin-gonic/gin v1.9.1\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert_eq!(
            info.sources.get("github.com/gin-gonic/gin"),
            Some(&ManifestSource::GoMod)
        );
    }

    #[test]
    fn test_sources_populated_for_gemfile() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Gemfile"), "gem \"rails\", \"~> 7.0\"\n").unwrap();

        let info = parse_dependencies(dir.path());
        assert_eq!(info.sources.get("rails"), Some(&ManifestSource::Gemfile));
    }

    #[test]
    fn test_sources_populated_for_pom_xml() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pom.xml"),
            "<project><dependencies><dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-web</artifactId></dependency></dependencies></project>",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert_eq!(
            info.sources.get("org.springframework.boot"),
            Some(&ManifestSource::PomXml)
        );
        assert_eq!(
            info.sources
                .get("org.springframework.boot:spring-boot-starter-web"),
            Some(&ManifestSource::PomXml)
        );
    }

    #[test]
    fn test_sources_populated_for_build_gradle() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("build.gradle"),
            "dependencies {\n    implementation 'io.ktor:ktor-server-core:2.3.0'\n}\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert_eq!(
            info.sources.get("io.ktor"),
            Some(&ManifestSource::BuildGradle)
        );
        assert_eq!(
            info.sources.get("io.ktor:ktor-server-core"),
            Some(&ManifestSource::BuildGradle)
        );
    }

    #[test]
    fn test_sources_populated_for_gradle_version_catalog() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("gradle")).unwrap();
        fs::write(
            dir.path().join("gradle/libs.versions.toml"),
            "[libraries]\nguava = \"com.google.guava:guava:31.1-jre\"\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert_eq!(
            info.sources.get("com.google.guava"),
            Some(&ManifestSource::GradleVersionCatalog)
        );
        assert_eq!(
            info.sources.get("com.google.guava:guava"),
            Some(&ManifestSource::GradleVersionCatalog)
        );
    }

    #[test]
    fn test_sources_populated_for_package_swift() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Package.swift"),
            ".package(url: \"https://github.com/vapor/vapor.git\", from: \"4.0.0\")\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert_eq!(
            info.sources.get("vapor"),
            Some(&ManifestSource::PackageSwift)
        );
    }

    #[test]
    fn test_sources_populated_for_pyproject_toml() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"x\"\ndependencies = [\"django>=4.0\"]\n\n[project.optional-dependencies]\ndev = [\"pytest\"]\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert_eq!(
            info.sources.get("django"),
            Some(&ManifestSource::PyprojectToml)
        );
        assert_eq!(
            info.sources.get("pytest"),
            Some(&ManifestSource::PyprojectToml)
        );
    }

    #[test]
    fn test_sources_populated_for_requirements_txt() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("requirements.txt"), "flask>=2.0\nnumpy\n").unwrap();

        let info = parse_dependencies(dir.path());
        assert_eq!(
            info.sources.get("flask"),
            Some(&ManifestSource::RequirementsTxt)
        );
        assert_eq!(
            info.sources.get("numpy"),
            Some(&ManifestSource::RequirementsTxt)
        );
    }

    #[test]
    fn test_sources_populated_for_pipfile() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Pipfile"),
            "[packages]\ndjango = \"*\"\n\n[dev-packages]\npytest = \"*\"\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert_eq!(info.sources.get("django"), Some(&ManifestSource::Pipfile));
        assert_eq!(info.sources.get("pytest"), Some(&ManifestSource::Pipfile));
    }

    #[test]
    fn test_sources_empty_for_no_manifests() {
        let dir = tempdir().unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.sources.is_empty());
    }

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
            .contains(&"org.springframework.boot:spring-boot-starter-web".to_string()));
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
        assert!(info
            .dependencies
            .contains(&"io.ktor:ktor-server-core".to_string()));
    }

    // ── 4eo.5: tracing::warn! is emitted for malformed manifests ──

    #[tracing_test::traced_test]
    #[test]
    fn test_malformed_package_json_emits_tracing_warn() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "not valid json at all").unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.is_empty());
        assert!(logs_contain("package.json"));
    }

    #[tracing_test::traced_test]
    #[test]
    fn test_malformed_cargo_toml_emits_tracing_warn() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "not valid toml [[[").unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.is_empty());
        assert!(logs_contain("Cargo.toml"));
    }

    #[tracing_test::traced_test]
    #[test]
    fn test_malformed_pyproject_toml_emits_tracing_warn() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "[[[invalid").unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.is_empty());
        assert!(logs_contain("pyproject.toml"));
    }

    #[tracing_test::traced_test]
    #[test]
    fn test_malformed_pipfile_emits_tracing_warn() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Pipfile"), "[[[invalid").unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.is_empty());
        assert!(logs_contain("Pipfile"));
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
            .contains(&"org.springframework.boot:spring-boot-starter-web".to_string()));
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
        assert!(info
            .dependencies
            .contains(&"com.google.guava:guava".to_string()));
        assert!(info
            .dependencies
            .contains(&"org.projectlombok:lombok".to_string()));
        assert!(info
            .dependencies
            .contains(&"org.postgresql:postgresql".to_string()));
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
        assert!(info.dependencies.contains(&"com.example:mylib".to_string()));
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
    fn test_pom_xml_combined_coordinate() {
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
        <dependency>
            <groupId>com.fasterxml.jackson.core</groupId>
            <artifactId>jackson-databind</artifactId>
            <version>2.15.0</version>
        </dependency>
    </dependencies>
</project>
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        // Combined groupId:artifactId coordinates
        assert!(info
            .dependencies
            .contains(&"org.springframework.boot:spring-boot-starter-web".to_string()));
        assert!(info
            .dependencies
            .contains(&"com.fasterxml.jackson.core:jackson-databind".to_string()));
        // groupId alone still present for backward-compatible registry lookup
        assert!(info
            .dependencies
            .contains(&"org.springframework.boot".to_string()));
        assert!(info
            .dependencies
            .contains(&"com.fasterxml.jackson.core".to_string()));
        // Standalone artifactId should NOT be present
        assert!(!info
            .dependencies
            .contains(&"spring-boot-starter-web".to_string()));
        assert!(!info.dependencies.contains(&"jackson-databind".to_string()));
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
    fn test_pyproject_optional_deps_classification() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            r#"
[project]
name = "my-app"
dependencies = ["django>=4.0"]

[project.optional-dependencies]
dev = ["pytest>=7.0"]
test = ["coverage>=7.0"]
lint = ["ruff>=0.1"]
postgres = ["psycopg2>=2.9"]
redis = ["redis>=4.0"]
all = ["uvicorn>=0.24"]
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        // Dev-like groups
        assert!(info.dev_dependencies.contains(&"pytest".to_string()));
        assert!(info.dev_dependencies.contains(&"coverage".to_string()));
        assert!(info.dev_dependencies.contains(&"ruff".to_string()));
        // Production extras
        assert!(info.dependencies.contains(&"psycopg2".to_string()));
        assert!(info.dependencies.contains(&"redis".to_string()));
        assert!(info.dependencies.contains(&"uvicorn".to_string()));
        // Base deps
        assert!(info.dependencies.contains(&"django".to_string()));
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

    #[test]
    fn test_go_mod_indirect_deps() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            r#"
module github.com/myorg/myapp

go 1.21

require (
	github.com/gin-gonic/gin v1.9.1
	github.com/spf13/cobra v1.7.0
	golang.org/x/text v0.14.0 // indirect
	golang.org/x/sys v0.15.0 // indirect
)
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        // Direct dependencies should be present
        assert!(info
            .dependencies
            .contains(&"github.com/gin-gonic/gin".to_string()));
        assert!(info
            .dependencies
            .contains(&"github.com/spf13/cobra".to_string()));
        // Indirect dependencies should be filtered out
        assert!(!info.dependencies.contains(&"golang.org/x/text".to_string()));
        assert!(!info.dependencies.contains(&"golang.org/x/sys".to_string()));
    }

    #[test]
    fn test_go_mod_single_line_indirect() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module example.com/foo\n\ngo 1.21\n\nrequire github.com/bar/baz v1.0.0 // indirect\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(!info
            .dependencies
            .contains(&"github.com/bar/baz".to_string()));
    }

    #[test]
    fn test_go_mod_mixed_direct_indirect() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            r#"
module example.com/foo

go 1.21

require (
	github.com/direct/dep v1.0.0
	github.com/indirect/dep v2.0.0 // indirect
)

require github.com/another/direct v3.0.0
require github.com/another/indirect v4.0.0 // indirect
"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());
        assert!(info
            .dependencies
            .contains(&"github.com/direct/dep".to_string()));
        assert!(info
            .dependencies
            .contains(&"github.com/another/direct".to_string()));
        assert!(!info
            .dependencies
            .contains(&"github.com/indirect/dep".to_string()));
        assert!(!info
            .dependencies
            .contains(&"github.com/another/indirect".to_string()));
    }

    #[test]
    fn test_gradle_version_catalog_module_form() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("gradle")).unwrap();
        fs::write(
            dir.path().join("gradle/libs.versions.toml"),
            "[versions]\nktor = \"2.3.4\"\n\n[libraries]\nktor-server-core = { module = \"io.ktor:ktor-server-core\", version.ref = \"ktor\" }\nktor-server-netty = { module = \"io.ktor:ktor-server-netty\", version.ref = \"ktor\" }\n",
        ).unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.contains(&"io.ktor".to_string()));
        assert!(info
            .dependencies
            .contains(&"io.ktor:ktor-server-core".to_string()));
        assert!(info
            .dependencies
            .contains(&"io.ktor:ktor-server-netty".to_string()));
    }

    #[test]
    fn test_gradle_version_catalog_inline_string_form() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("gradle")).unwrap();
        fs::write(
            dir.path().join("gradle/libs.versions.toml"),
            "[libraries]\nguava = \"com.google.guava:guava:31.1-jre\"\n",
        )
        .unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.contains(&"com.google.guava".to_string()));
        assert!(info
            .dependencies
            .contains(&"com.google.guava:guava".to_string()));
    }

    #[test]
    fn test_gradle_version_catalog_group_name_form() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("gradle")).unwrap();
        fs::write(
            dir.path().join("gradle/libs.versions.toml"),
            "[versions]\nkotlin = \"1.9.0\"\n\n[libraries]\nkotlin-stdlib = { group = \"org.jetbrains.kotlin\", name = \"kotlin-stdlib\", version.ref = \"kotlin\" }\n",
        ).unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info
            .dependencies
            .contains(&"org.jetbrains.kotlin".to_string()));
        assert!(info
            .dependencies
            .contains(&"org.jetbrains.kotlin:kotlin-stdlib".to_string()));
    }

    #[test]
    fn test_gradle_version_catalog_invalid_toml_returns_empty() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("gradle")).unwrap();
        fs::write(
            dir.path().join("gradle/libs.versions.toml"),
            "[[[invalid toml",
        )
        .unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.is_empty());
    }

    #[test]
    fn test_gradle_version_catalog_no_libraries_section() {
        // Directly test the catalog parser: a file with only [versions] and no
        // [libraries] section should add nothing to the dependency set.
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("gradle")).unwrap();
        fs::write(
            dir.path().join("gradle/libs.versions.toml"),
            "[versions]\nktor = \"2.3.4\"\n",
        )
        .unwrap();
        let mut deps = std::collections::HashSet::new();
        let mut sources = std::collections::HashMap::new();
        parse_gradle_version_catalog(dir.path(), &mut deps, &mut sources);
        assert!(deps.is_empty());
    }

    #[test]
    fn test_build_gradle_classpath_configuration() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("build.gradle"),
            "buildscript {\n    dependencies {\n        classpath 'com.android.tools.build:gradle:8.1.0'\n        classpath(\"org.jetbrains.kotlin:kotlin-gradle-plugin:1.9.0\")\n    }\n}\n",
        ).unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info
            .dependencies
            .contains(&"com.android.tools.build".to_string()));
        assert!(info
            .dependencies
            .contains(&"com.android.tools.build:gradle".to_string()));
        assert!(info
            .dependencies
            .contains(&"org.jetbrains.kotlin".to_string()));
    }

    #[test]
    fn test_gradle_version_catalog_non_string_non_table_value_skipped() {
        // Library entry that is neither a String nor a Table (e.g., an integer)
        // exercises the `_ => {}` branch and should be silently skipped.
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("gradle")).unwrap();
        fs::write(
            dir.path().join("gradle/libs.versions.toml"),
            "[libraries]\nbad-entry = 42\ngood-entry = \"com.example:lib:1.0\"\n",
        )
        .unwrap();
        let info = parse_dependencies(dir.path());
        // The integer entry is skipped; the string entry is parsed normally.
        assert!(info.dependencies.contains(&"com.example".to_string()));
        assert!(info.dependencies.contains(&"com.example:lib".to_string()));
        // Ensure no garbage from the integer entry
        assert_eq!(info.dependencies.len(), 2);
    }

    #[test]
    fn test_gradle_version_catalog_combined_with_build_gradle() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("gradle")).unwrap();
        fs::write(
            dir.path().join("build.gradle.kts"),
            "dependencies {\n    implementation(\"com.squareup.okhttp3:okhttp:4.11.0\")\n}\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("gradle/libs.versions.toml"),
            "[libraries]\nktor-server-core = { module = \"io.ktor:ktor-server-core\", version.ref = \"ktor\" }\n",
        ).unwrap();
        let info = parse_dependencies(dir.path());
        assert!(info
            .dependencies
            .contains(&"io.ktor:ktor-server-core".to_string()));
        assert!(info
            .dependencies
            .contains(&"com.squareup.okhttp3:okhttp".to_string()));
    }

    /// Verify that all static `LazyLock<Regex>` values in this module compile
    /// and initialize without panicking. Touching each accessor triggers the
    /// `LazyLock` initializer, so any invalid regex pattern would panic here
    /// rather than silently during production analysis.
    #[test]
    fn test_static_regexes_all_initialize_without_panic() {
        // Each call forces the LazyLock to run Regex::new(...).expect(...).
        // If any pattern is invalid, this test will panic with a clear message.
        let _ = regex_gem_name();
        let _ = regex_pom_dependency();
        let _ = regex_pom_group_id();
        let _ = regex_pom_artifact_id();
        let _ = regex_gradle_dependency();
        let _ = regex_swift_package_name();
        let _ = regex_swift_package_url();
    }

    // ── File-size limit tests (g5j.17) ───────────────────────────────

    /// Helper: write exactly `size` bytes to `path`.
    fn write_file_of_size(path: &std::path::Path, size: usize) {
        // Write a file filled with 'x' bytes.
        let content = vec![b'x'; size];
        fs::write(path, content).unwrap();
    }

    #[test]
    fn test_manifest_oversized_package_json_is_skipped() {
        let dir = tempdir().unwrap();
        // Write a file just over 10 MB — the content isn't valid JSON, but the
        // size check happens before parsing, so parse_dependencies should return
        // empty deps rather than panicking or reading the huge file.
        let path = dir.path().join("package.json");
        let over_limit = (super::MAX_MANIFEST_SIZE + 1) as usize;
        write_file_of_size(&path, over_limit);

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.is_empty());
    }

    #[test]
    fn test_manifest_oversized_cargo_toml_is_skipped() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("Cargo.toml");
        let over_limit = (super::MAX_MANIFEST_SIZE + 1) as usize;
        write_file_of_size(&path, over_limit);

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.is_empty());
    }

    #[test]
    fn test_manifest_oversized_requirements_txt_is_skipped() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("requirements.txt");
        let over_limit = (super::MAX_MANIFEST_SIZE + 1) as usize;
        write_file_of_size(&path, over_limit);

        let info = parse_dependencies(dir.path());
        assert!(info.dependencies.is_empty());
    }

    #[test]
    fn test_read_manifest_file_returns_none_for_oversized_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("big.txt");
        let over_limit = (super::MAX_MANIFEST_SIZE + 1) as usize;
        write_file_of_size(&path, over_limit);

        let result = super::read_manifest_file(&path);
        assert!(result.is_none());
    }

    #[test]
    fn test_read_manifest_file_returns_content_for_normal_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("small.txt");
        fs::write(&path, "hello world").unwrap();

        let result = super::read_manifest_file(&path);
        assert_eq!(result, Some("hello world".to_string()));
    }

    #[test]
    fn test_read_manifest_file_returns_none_for_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.txt");

        let result = super::read_manifest_file(&path);
        assert!(result.is_none());
    }

    #[test]
    fn test_read_manifest_file_returns_none_for_invalid_utf8() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("invalid_utf8.txt");
        // 0xFF 0xFE is not valid UTF-8
        fs::write(&path, &[0xFF, 0xFE, 0x00, 0x01]).unwrap();

        let result = super::read_manifest_file(&path);
        assert!(result.is_none());
    }

    // ── .csproj tests ─────────────────────────────────────────────────

    #[test]
    fn test_manifest_source_compatible_languages_csproj_file() {
        let langs = ManifestSource::CsprojFile.compatible_languages();
        assert_eq!(langs, vec![Language::CSharp]);
    }

    #[test]
    fn test_parse_csproj_sdk_web() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("App.csproj"),
            r#"<Project Sdk="Microsoft.NET.Sdk.Web"><PropertyGroup></PropertyGroup></Project>"#,
        )
        .unwrap();

        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_csproj(dir.path(), &mut deps, &mut sources);

        assert!(
            deps.contains("Microsoft.NET.Sdk.Web"),
            "expected SDK name in deps"
        );
    }

    #[test]
    fn test_parse_csproj_package_references() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("App.csproj"),
            r#"<Project Sdk="Microsoft.NET.Sdk.Web">
  <ItemGroup>
    <PackageReference Include="Swashbuckle.AspNetCore" Version="6.4.0" />
    <PackageReference Include="Microsoft.EntityFrameworkCore" Version="7.0.0" />
  </ItemGroup>
</Project>"#,
        )
        .unwrap();

        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_csproj(dir.path(), &mut deps, &mut sources);

        assert!(deps.contains("Swashbuckle.AspNetCore"));
        assert!(deps.contains("Microsoft.EntityFrameworkCore"));
    }

    #[test]
    fn test_parse_csproj_no_file() {
        let dir = tempdir().unwrap();
        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_csproj(dir.path(), &mut deps, &mut sources);
        assert!(deps.is_empty());
        assert!(sources.is_empty());
    }

    #[test]
    fn test_parse_csproj_source_is_csproj_file() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("App.csproj"),
            r#"<Project Sdk="Microsoft.NET.Sdk.Web">
  <ItemGroup>
    <PackageReference Include="Swashbuckle.AspNetCore" Version="6.4.0" />
  </ItemGroup>
</Project>"#,
        )
        .unwrap();

        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_csproj(dir.path(), &mut deps, &mut sources);

        assert_eq!(
            sources.get("Microsoft.NET.Sdk.Web"),
            Some(&ManifestSource::CsprojFile)
        );
        assert_eq!(
            sources.get("Swashbuckle.AspNetCore"),
            Some(&ManifestSource::CsprojFile)
        );
    }

    #[test]
    fn test_parse_csproj_no_sdk_attribute() {
        // Exercises the else-branch of `if let Some(cap) = sdk_re.captures(&content)`,
        // i.e. a .csproj with no Project Sdk attribute but with PackageReferences.
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("App.csproj"),
            r#"<Project>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#,
        )
        .unwrap();
        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_csproj(dir.path(), &mut deps, &mut sources);
        assert!(deps.contains("Newtonsoft.Json"));
        assert!(!deps.contains("Microsoft.NET.Sdk.Web"));
    }

    #[test]
    fn test_parse_csproj_nonexistent_directory() {
        // Exercises the Err(_) => return branch in parse_csproj when read_dir fails.
        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_csproj(
            Path::new("/nonexistent/path/that/does/not/exist"),
            &mut deps,
            &mut sources,
        );
        assert!(deps.is_empty());
        assert!(sources.is_empty());
    }

    #[test]
    fn test_parse_csproj_invalid_utf8() {
        // Exercises the None => return branch in parse_single_csproj when the file
        // cannot be decoded as UTF-8.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("App.csproj"), b"\xff\xfe not valid utf-8").unwrap();
        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_csproj(dir.path(), &mut deps, &mut sources);
        assert!(deps.is_empty());
        assert!(sources.is_empty());
    }

    #[test]
    fn test_parse_dependencies_includes_csproj() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("App.csproj"),
            r#"<Project Sdk="Microsoft.NET.Sdk.Web">
  <ItemGroup>
    <PackageReference Include="Microsoft.AspNetCore.Mvc" Version="2.2.0" />
  </ItemGroup>
</Project>"#,
        )
        .unwrap();

        let info = parse_dependencies(dir.path());

        assert!(info
            .dependencies
            .contains(&"Microsoft.NET.Sdk.Web".to_string()));
        assert!(info
            .dependencies
            .contains(&"Microsoft.AspNetCore.Mvc".to_string()));
        assert_eq!(
            info.sources.get("Microsoft.NET.Sdk.Web"),
            Some(&ManifestSource::CsprojFile)
        );
    }

    // ── CMakeLists.txt tests ──────────────────────────────────────────

    #[test]
    fn test_manifest_source_compatible_languages_cmake() {
        let langs = ManifestSource::CMakeListsTxt.compatible_languages();
        assert_eq!(langs, vec![Language::C, Language::Cpp]);
    }

    #[test]
    fn test_manifest_source_compatible_languages_vcpkg_json() {
        let langs = ManifestSource::VcpkgJson.compatible_languages();
        assert_eq!(langs, vec![Language::C, Language::Cpp]);
    }

    #[test]
    fn test_manifest_source_compatible_languages_conan_file_txt() {
        let langs = ManifestSource::ConanFileTxt.compatible_languages();
        assert_eq!(langs, vec![Language::C, Language::Cpp]);
    }

    #[test]
    fn test_parse_cmake_find_package() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.20)\nfind_package(Qt6 REQUIRED COMPONENTS Core)\n",
        )
        .unwrap();

        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_cmake_lists(dir.path(), &mut deps, &mut sources);

        assert!(deps.contains("Qt6"), "expected Qt6 in deps");
    }

    #[test]
    fn test_parse_cmake_fetch_content() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("CMakeLists.txt"),
            "include(FetchContent)\nFetchContent_Declare(googletest URL https://example.com)\n",
        )
        .unwrap();

        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_cmake_lists(dir.path(), &mut deps, &mut sources);

        assert!(deps.contains("googletest"), "expected googletest in deps");
    }

    #[test]
    fn test_parse_cmake_no_file() {
        let dir = tempdir().unwrap();
        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_cmake_lists(dir.path(), &mut deps, &mut sources);
        assert!(deps.is_empty());
        assert!(sources.is_empty());
    }

    #[test]
    fn test_parse_cmake_source_is_cmake() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("CMakeLists.txt"),
            "find_package(Boost REQUIRED)\n",
        )
        .unwrap();

        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_cmake_lists(dir.path(), &mut deps, &mut sources);

        assert_eq!(sources.get("Boost"), Some(&ManifestSource::CMakeListsTxt));
    }

    // ── vcpkg.json tests ─────────────────────────────────────────────

    #[test]
    fn test_parse_vcpkg_json_strings() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("vcpkg.json"),
            r#"{"name":"my-app","version":"1.0","dependencies":["zlib","openssl"]}"#,
        )
        .unwrap();

        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_vcpkg_json(dir.path(), &mut deps, &mut sources);

        assert!(deps.contains("zlib"));
        assert!(deps.contains("openssl"));
    }

    #[test]
    fn test_parse_vcpkg_json_objects() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("vcpkg.json"),
            r#"{"dependencies":[{"name":"boost-regex","features":["icu"]}]}"#,
        )
        .unwrap();

        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_vcpkg_json(dir.path(), &mut deps, &mut sources);

        assert!(deps.contains("boost-regex"), "expected boost-regex in deps");
    }

    #[test]
    fn test_parse_vcpkg_json_no_file() {
        let dir = tempdir().unwrap();
        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_vcpkg_json(dir.path(), &mut deps, &mut sources);
        assert!(deps.is_empty());
        assert!(sources.is_empty());
    }

    #[test]
    fn test_parse_vcpkg_json_source_is_vcpkg() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("vcpkg.json"),
            r#"{"dependencies":["sqlite3"]}"#,
        )
        .unwrap();

        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_vcpkg_json(dir.path(), &mut deps, &mut sources);

        assert_eq!(sources.get("sqlite3"), Some(&ManifestSource::VcpkgJson));
    }

    // ── conanfile.txt tests ──────────────────────────────────────────

    #[test]
    fn test_parse_conan_file_txt_requires() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("conanfile.txt"),
            "[requires]\nzlib/1.2.11\n",
        )
        .unwrap();

        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_conan_file_txt(dir.path(), &mut deps, &mut sources);

        assert!(deps.contains("zlib"), "expected zlib in deps");
    }

    #[test]
    fn test_parse_conan_file_txt_build_requires() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("conanfile.txt"),
            "[build_requires]\ncmake/3.25.0\n",
        )
        .unwrap();

        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_conan_file_txt(dir.path(), &mut deps, &mut sources);

        assert!(deps.contains("cmake"), "expected cmake in deps");
    }

    #[test]
    fn test_parse_conan_file_txt_no_file() {
        let dir = tempdir().unwrap();
        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_conan_file_txt(dir.path(), &mut deps, &mut sources);
        assert!(deps.is_empty());
        assert!(sources.is_empty());
    }

    #[test]
    fn test_parse_conan_file_txt_source_is_conan() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("conanfile.txt"),
            "[requires]\nboost/1.81.0\n",
        )
        .unwrap();

        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_conan_file_txt(dir.path(), &mut deps, &mut sources);

        assert_eq!(sources.get("boost"), Some(&ManifestSource::ConanFileTxt));
    }

    #[test]
    fn test_parse_dependencies_includes_cmake() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.20)\nfind_package(GTest REQUIRED)\n",
        )
        .unwrap();

        let info = parse_dependencies(dir.path());

        assert!(
            info.dependencies.contains(&"GTest".to_string()),
            "expected GTest in dependencies"
        );
        assert_eq!(
            info.sources.get("GTest"),
            Some(&ManifestSource::CMakeListsTxt)
        );
    }

    // ── csproj edge-case coverage ─────────────────────────────────────

    #[test]
    fn test_parse_csproj_nonexistent_directory() {
        // Covers `Err(_) => return` in parse_csproj (line 796).
        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_csproj(
            Path::new("/nonexistent/path/that/does/not/exist"),
            &mut deps,
            &mut sources,
        );
        assert!(deps.is_empty());
        assert!(sources.is_empty());
    }

    #[test]
    fn test_parse_csproj_invalid_utf8() {
        // Covers `None => return` in parse_single_csproj (line 816).
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("App.csproj"), b"\xff\xfe not valid utf-8").unwrap();
        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_csproj(dir.path(), &mut deps, &mut sources);
        assert!(deps.is_empty());
        assert!(sources.is_empty());
    }

    #[test]
    fn test_parse_csproj_no_sdk_attribute() {
        // Covers the else-path of `if let Some(cap) = sdk_re.captures(...)` (line 827).
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("App.csproj"),
            r#"<Project>
  <ItemGroup>
    <PackageReference Include="Newtonsoft.Json" Version="13.0.1" />
  </ItemGroup>
</Project>"#,
        )
        .unwrap();
        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_csproj(dir.path(), &mut deps, &mut sources);
        assert!(deps.contains("Newtonsoft.Json"));
        assert!(!deps.contains("Microsoft.NET.Sdk.Web"));
    }

    // ── vcpkg.json edge-case coverage ────────────────────────────────

    #[test]
    fn test_parse_vcpkg_json_invalid_json() {
        // Covers the `Err(e) => { warn; return }` branch (lines 914-916).
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("vcpkg.json"), "not valid json {{").unwrap();
        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_vcpkg_json(dir.path(), &mut deps, &mut sources);
        assert!(deps.is_empty());
        assert!(sources.is_empty());
    }

    #[test]
    fn test_parse_vcpkg_json_dep_item_skipped_if_no_name() {
        // Covers the `else { continue }` branch (line 926) for items that are
        // neither a string nor an object with a "name" key.
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("vcpkg.json"),
            r#"{"dependencies":[42, {"version":"1.0"}]}"#,
        )
        .unwrap();
        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_vcpkg_json(dir.path(), &mut deps, &mut sources);
        assert!(deps.is_empty());
        assert!(sources.is_empty());
    }

    #[test]
    fn test_parse_vcpkg_json_no_dependencies_key() {
        // Covers the else-path of `if let Some(arr) = parsed.get("dependencies")...`
        // (line 931).
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("vcpkg.json"),
            r#"{"name":"my-app","version":"1.0"}"#,
        )
        .unwrap();
        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_vcpkg_json(dir.path(), &mut deps, &mut sources);
        assert!(deps.is_empty());
        assert!(sources.is_empty());
    }

    // ── conanfile.txt edge-case coverage ─────────────────────────────

    #[test]
    fn test_parse_conan_file_txt_skips_empty_and_comment_lines() {
        // Covers `continue` at line 954: empty lines, comments, and lines
        // outside a [requires]/[build_requires] section.
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("conanfile.txt"),
            "[generators]\ncmake\n\n# a comment\n[requires]\nzlib/1.2.11\n",
        )
        .unwrap();
        let mut deps = HashSet::new();
        let mut sources = HashMap::new();
        parse_conan_file_txt(dir.path(), &mut deps, &mut sources);
        assert!(deps.contains("zlib"), "expected zlib");
        assert!(!deps.contains("cmake"), "cmake should not be a dep");
    }
}
