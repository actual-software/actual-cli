//! Integration tests for the static analyzer.
//!
//! These tests exercise `run_static_analysis()` end-to-end on realistic
//! project layouts created in temp directories.  They validate that the
//! full pipeline (monorepo detection → language detection → manifest parsing
//! → framework detection → path normalization) produces correct results
//! for real-world project structures.

use actual_cli::analysis::orchestrate::run_static_analysis;
use actual_cli::analysis::types::{Language, RepoAnalysis};

// ── Helpers ─────────────────────────────────────────────────────────

/// Create a directory and write a file inside a temp dir.
fn write_file(root: &std::path::Path, relative: &str, content: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

/// Find a project by path in the analysis result.
fn find_project<'a>(
    analysis: &'a RepoAnalysis,
    path: &str,
) -> Option<&'a actual_cli::analysis::types::Project> {
    analysis.projects.iter().find(|p| p.path == path)
}

/// Check that a project has a specific framework by name.
fn has_framework(analysis: &RepoAnalysis, project_path: &str, framework_name: &str) -> bool {
    find_project(analysis, project_path)
        .map(|p| p.frameworks.iter().any(|f| f.name == framework_name))
        .unwrap_or(false)
}

// ── Realistic Project Layouts ───────────────────────────────────────

#[test]
fn rust_cli_project() {
    let dir = tempfile::tempdir().unwrap();
    write_file(
        dir.path(),
        "Cargo.toml",
        r#"[package]
name = "my-cli"
version = "0.1.0"
edition = "2021"

[dependencies]
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
"#,
    );
    write_file(dir.path(), "Cargo.lock", "# lock file\n");
    write_file(
        dir.path(),
        "src/main.rs",
        r#"use clap::Parser;

#[derive(Parser)]
struct Args {
    #[arg(short, long)]
    name: String,
}

fn main() {
    let args = Args::parse();
    println!("Hello, {}!", args.name);
}
"#,
    );
    write_file(
        dir.path(),
        "src/lib.rs",
        "pub fn greet(name: &str) -> String { format!(\"Hello, {name}!\") }\n",
    );

    let result = run_static_analysis(dir.path()).unwrap();

    assert!(!result.is_monorepo);
    assert_eq!(result.projects.len(), 1);

    let project = &result.projects[0];
    assert_eq!(project.path, ".");
    assert_eq!(project.name, "my-cli");
    assert!(project.languages.contains(&Language::Rust));
    assert_eq!(project.package_manager.as_deref(), Some("cargo"));
    assert!(has_framework(&result, ".", "clap"));
    assert!(has_framework(&result, ".", "serde"));
    assert!(has_framework(&result, ".", "tokio"));
}

#[test]
fn nextjs_web_app() {
    let dir = tempfile::tempdir().unwrap();
    write_file(
        dir.path(),
        "package.json",
        r#"{
  "name": "my-nextjs-app",
  "version": "1.0.0",
  "dependencies": {
    "next": "^14.0.0",
    "react": "^18.0.0",
    "react-dom": "^18.0.0"
  },
  "devDependencies": {
    "typescript": "^5.0.0",
    "@types/react": "^18.0.0"
  }
}"#,
    );
    write_file(dir.path(), "pnpm-lock.yaml", "lockfileVersion: '6.0'\n");
    write_file(
        dir.path(),
        "next.config.js",
        "/** @type {import('next').NextConfig} */\nmodule.exports = {}\n",
    );
    write_file(
        dir.path(),
        "pages/index.tsx",
        r#"export default function Home() {
  return <div>Hello World</div>;
}
"#,
    );
    write_file(
        dir.path(),
        "pages/about.tsx",
        r#"export default function About() {
  return <div>About</div>;
}
"#,
    );

    let result = run_static_analysis(dir.path()).unwrap();

    assert!(!result.is_monorepo);
    assert_eq!(result.projects.len(), 1);

    let project = &result.projects[0];
    assert_eq!(project.path, ".");
    assert_eq!(project.package_manager.as_deref(), Some("pnpm"));
    // TypeScript files present
    assert!(
        project.languages.contains(&Language::TypeScript),
        "Expected TypeScript, got: {:?}",
        project.languages
    );
    assert!(has_framework(&result, ".", "react"));
    assert!(has_framework(&result, ".", "nextjs"));
}

#[test]
fn python_ml_project() {
    let dir = tempfile::tempdir().unwrap();
    write_file(
        dir.path(),
        "pyproject.toml",
        r#"[project]
name = "ml-training"
version = "0.1.0"
dependencies = [
    "torch>=2.0",
    "numpy>=1.24",
    "pandas>=2.0",
]

[project.optional-dependencies]
dev = ["pytest>=7.0", "black>=23.0"]
"#,
    );
    write_file(
        dir.path(),
        "requirements.txt",
        "torch>=2.0\nnumpy>=1.24\npandas>=2.0\nscikit-learn>=1.3\n",
    );
    write_file(
        dir.path(),
        "src/train.py",
        r#"import torch
import numpy as np

def train_model(data):
    model = torch.nn.Linear(10, 1)
    optimizer = torch.optim.Adam(model.parameters())
    return model
"#,
    );
    write_file(
        dir.path(),
        "src/preprocess.py",
        "import pandas as pd\n\ndef clean(df): return df.dropna()\n",
    );

    let result = run_static_analysis(dir.path()).unwrap();

    assert!(!result.is_monorepo);
    assert_eq!(result.projects.len(), 1);

    let project = &result.projects[0];
    assert_eq!(project.path, ".");
    assert!(project.languages.contains(&Language::Python));
    // pytorch is the framework name in the registry for the "torch" package
    assert!(
        has_framework(&result, ".", "pytorch"),
        "Expected pytorch framework, got: {:?}",
        project.frameworks
    );
    assert!(has_framework(&result, ".", "numpy"));
    assert!(has_framework(&result, ".", "pandas"));
    assert!(has_framework(&result, ".", "scikit-learn"));
}

#[test]
fn go_microservice() {
    let dir = tempfile::tempdir().unwrap();
    write_file(
        dir.path(),
        "go.mod",
        r#"module github.com/example/api-service

go 1.21

require (
    github.com/gin-gonic/gin v1.9.1
    github.com/lib/pq v1.10.9
)
"#,
    );
    write_file(dir.path(), "go.sum", "# sum file\n");
    write_file(
        dir.path(),
        "main.go",
        r#"package main

import "github.com/gin-gonic/gin"

func main() {
    r := gin.Default()
    r.GET("/health", func(c *gin.Context) {
        c.JSON(200, gin.H{"status": "ok"})
    })
    r.Run()
}
"#,
    );
    write_file(
        dir.path(),
        "handlers/user.go",
        "package handlers\n\nfunc GetUser() {}\n",
    );

    let result = run_static_analysis(dir.path()).unwrap();

    assert!(!result.is_monorepo);
    assert_eq!(result.projects.len(), 1);

    let project = &result.projects[0];
    assert_eq!(project.path, ".");
    assert!(project.languages.contains(&Language::Go));
    assert_eq!(project.package_manager.as_deref(), Some("go"));
    assert!(
        has_framework(&result, ".", "gin"),
        "Expected gin framework, got: {:?}",
        project.frameworks
    );
}

#[test]
fn cargo_workspace_monorepo() {
    let dir = tempfile::tempdir().unwrap();
    write_file(
        dir.path(),
        "Cargo.toml",
        r#"[workspace]
members = ["crates/*"]
"#,
    );
    write_file(dir.path(), "Cargo.lock", "# lock file\n");

    // Crate 1: CLI binary
    write_file(
        dir.path(),
        "crates/cli/Cargo.toml",
        r#"[package]
name = "my-cli"
version = "0.1.0"

[dependencies]
clap = "4"
"#,
    );
    write_file(
        dir.path(),
        "crates/cli/src/main.rs",
        "fn main() { println!(\"hello\"); }\n",
    );

    // Crate 2: library
    write_file(
        dir.path(),
        "crates/core/Cargo.toml",
        r#"[package]
name = "my-core"
version = "0.1.0"

[dependencies]
serde = "1"
"#,
    );
    write_file(
        dir.path(),
        "crates/core/src/lib.rs",
        "pub fn core_fn() {}\n",
    );

    // Crate 3: utils
    write_file(
        dir.path(),
        "crates/utils/Cargo.toml",
        r#"[package]
name = "my-utils"
version = "0.1.0"
"#,
    );
    write_file(
        dir.path(),
        "crates/utils/src/lib.rs",
        "pub fn util_fn() {}\n",
    );

    let result = run_static_analysis(dir.path()).unwrap();

    assert!(result.is_monorepo, "Expected monorepo, got single project");
    assert!(
        result.projects.len() >= 3,
        "Expected at least 3 projects, got {}",
        result.projects.len()
    );

    // Check CLI crate
    let cli = find_project(&result, "crates/cli");
    assert!(cli.is_some(), "Expected crates/cli project");
    let cli = cli.unwrap();
    assert!(cli.languages.contains(&Language::Rust));
    assert!(has_framework(&result, "crates/cli", "clap"));

    // Check core crate
    let core = find_project(&result, "crates/core");
    assert!(core.is_some(), "Expected crates/core project");
    assert!(has_framework(&result, "crates/core", "serde"));

    // Check utils crate
    assert!(
        find_project(&result, "crates/utils").is_some(),
        "Expected crates/utils project"
    );
}

#[test]
fn pnpm_workspace_monorepo() {
    let dir = tempfile::tempdir().unwrap();
    write_file(
        dir.path(),
        "pnpm-workspace.yaml",
        "packages:\n  - \"apps/*\"\n  - \"libs/*\"\n",
    );
    write_file(
        dir.path(),
        "package.json",
        r#"{"name": "my-monorepo", "private": true}"#,
    );
    write_file(dir.path(), "pnpm-lock.yaml", "lockfileVersion: '6.0'\n");

    // App 1: Next.js frontend
    write_file(
        dir.path(),
        "apps/web/package.json",
        r#"{"name": "web", "dependencies": {"next": "^14.0.0", "react": "^18.0.0"}}"#,
    );
    write_file(
        dir.path(),
        "apps/web/pages/index.tsx",
        "export default function Home() { return <div/>; }\n",
    );

    // App 2: Express API
    write_file(
        dir.path(),
        "apps/api/package.json",
        r#"{"name": "api", "dependencies": {"express": "^4.18.0"}}"#,
    );
    write_file(
        dir.path(),
        "apps/api/src/index.ts",
        "import express from 'express';\nconst app = express();\n",
    );

    // Shared lib
    write_file(
        dir.path(),
        "libs/shared/package.json",
        r#"{"name": "shared", "dependencies": {}}"#,
    );
    write_file(
        dir.path(),
        "libs/shared/src/utils.ts",
        "export function add(a: number, b: number) { return a + b; }\n",
    );

    let result = run_static_analysis(dir.path()).unwrap();

    assert!(result.is_monorepo);
    assert!(
        result.projects.len() >= 3,
        "Expected at least 3 projects, got {}",
        result.projects.len()
    );

    // Check web app
    let web = find_project(&result, "apps/web");
    assert!(web.is_some(), "Expected apps/web project");
    assert!(has_framework(&result, "apps/web", "react"));
    assert!(has_framework(&result, "apps/web", "nextjs"));

    // Check API app
    let api = find_project(&result, "apps/api");
    assert!(api.is_some(), "Expected apps/api project");
    assert!(has_framework(&result, "apps/api", "express"));

    // Check shared lib
    assert!(
        find_project(&result, "libs/shared").is_some(),
        "Expected libs/shared project"
    );
}

#[test]
fn polyglot_rust_and_typescript() {
    let dir = tempfile::tempdir().unwrap();
    // Rust backend
    write_file(
        dir.path(),
        "Cargo.toml",
        r#"[package]
name = "polyglot-app"
version = "0.1.0"

[dependencies]
actix-web = "4"
"#,
    );
    write_file(dir.path(), "Cargo.lock", "# lock\n");
    write_file(
        dir.path(),
        "src/main.rs",
        "fn main() { println!(\"server\"); }\n",
    );

    // TypeScript frontend (co-located)
    write_file(
        dir.path(),
        "package.json",
        r#"{"name": "frontend", "dependencies": {"react": "^18.0.0"}}"#,
    );
    write_file(
        dir.path(),
        "frontend/App.tsx",
        "export default function App() { return <div/>; }\n",
    );
    write_file(
        dir.path(),
        "frontend/utils.ts",
        "export function helper() { return 42; }\n",
    );

    let result = run_static_analysis(dir.path()).unwrap();

    // Single project (no workspace config → not a monorepo)
    assert!(!result.is_monorepo);
    assert_eq!(result.projects.len(), 1);

    let project = &result.projects[0];
    // Both languages should be detected
    assert!(
        project.languages.contains(&Language::Rust),
        "Expected Rust, got: {:?}",
        project.languages
    );
    assert!(
        project.languages.contains(&Language::TypeScript),
        "Expected TypeScript, got: {:?}",
        project.languages
    );
    // Frameworks from both ecosystems
    assert!(has_framework(&result, ".", "actix-web"));
    assert!(has_framework(&result, ".", "react"));
}

#[test]
fn minimal_repo_no_manifests() {
    let dir = tempfile::tempdir().unwrap();
    // Source files only — no package manager, no manifests
    write_file(
        dir.path(),
        "main.py",
        "print('hello')\nfor i in range(10):\n    print(i)\n",
    );
    write_file(
        dir.path(),
        "utils.py",
        "def helper():\n    return 42\n\ndef other():\n    return 0\n",
    );

    let result = run_static_analysis(dir.path()).unwrap();

    assert!(!result.is_monorepo);
    assert_eq!(result.projects.len(), 1);

    let project = &result.projects[0];
    assert_eq!(project.path, ".");
    assert!(
        project.languages.contains(&Language::Python),
        "Expected Python, got: {:?}",
        project.languages
    );
    // No package manager or frameworks without manifests
    assert_eq!(project.package_manager, None);
    assert!(
        project.frameworks.is_empty(),
        "Expected no frameworks, got: {:?}",
        project.frameworks
    );
}

// ── Edge Cases ──────────────────────────────────────────────────────

#[test]
fn empty_directory() {
    let dir = tempfile::tempdir().unwrap();

    let result = run_static_analysis(dir.path()).unwrap();

    assert!(!result.is_monorepo);
    assert_eq!(result.projects.len(), 1);

    let project = &result.projects[0];
    assert_eq!(project.path, ".");
    assert!(project.languages.is_empty());
    assert!(project.frameworks.is_empty());
    assert_eq!(project.package_manager, None);
}

#[test]
fn binary_files_only() {
    let dir = tempfile::tempdir().unwrap();
    // Write some binary content
    std::fs::write(dir.path().join("image.png"), &[0x89, 0x50, 0x4E, 0x47]).unwrap();
    std::fs::write(dir.path().join("data.bin"), &[0x00, 0xFF, 0xFE, 0xFD]).unwrap();

    let result = run_static_analysis(dir.path()).unwrap();

    assert!(!result.is_monorepo);
    assert_eq!(result.projects.len(), 1);

    let project = &result.projects[0];
    assert!(
        project.languages.is_empty(),
        "Expected no languages for binary files, got: {:?}",
        project.languages
    );
}

#[test]
fn deeply_nested_monorepo_project() {
    let dir = tempfile::tempdir().unwrap();
    write_file(
        dir.path(),
        "pnpm-workspace.yaml",
        "packages:\n  - \"org/team/area/apps/*\"\n",
    );
    write_file(
        dir.path(),
        "package.json",
        r#"{"name": "deep-mono", "private": true}"#,
    );

    // Deeply nested project
    write_file(
        dir.path(),
        "org/team/area/apps/service/package.json",
        r#"{"name": "deep-service", "dependencies": {"express": "^4.0.0"}}"#,
    );
    write_file(
        dir.path(),
        "org/team/area/apps/service/src/index.ts",
        "import express from 'express';\n",
    );

    let result = run_static_analysis(dir.path()).unwrap();

    assert!(result.is_monorepo);
    let service = find_project(&result, "org/team/area/apps/service");
    assert!(
        service.is_some(),
        "Expected deeply nested project at org/team/area/apps/service, projects: {:?}",
        result.projects.iter().map(|p| &p.path).collect::<Vec<_>>()
    );
    assert!(has_framework(
        &result,
        "org/team/area/apps/service",
        "express"
    ));
}

// ── Self-Analysis ───────────────────────────────────────────────────

#[test]
fn self_analysis_detects_actual_cli() {
    let dir = tempfile::tempdir().unwrap();
    // Write a minimal Cargo.toml that mimics actual-cli
    std::fs::write(
        dir.path().join("Cargo.toml"),
        r#"[package]
name = "actual-cli"
version = "0.1.0"
edition = "2021"

[dependencies]
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
reqwest = "0.12"
"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    // Cargo.lock is required for package manager detection
    std::fs::write(dir.path().join("Cargo.lock"), "# lock file\n").unwrap();

    let result = run_static_analysis(dir.path()).unwrap();

    assert!(
        !result.is_monorepo,
        "single Cargo.toml should not be a monorepo"
    );
    assert_eq!(result.projects.len(), 1);
    let project = &result.projects[0];
    assert_eq!(project.path, ".");
    assert!(
        project.languages.contains(&Language::Rust),
        "Expected Rust, got: {:?}",
        project.languages
    );
    assert_eq!(project.package_manager.as_deref(), Some("cargo"));
    assert!(has_framework(&result, ".", "clap"), "Expected clap");
    assert!(has_framework(&result, ".", "serde"), "Expected serde");
    assert!(has_framework(&result, ".", "tokio"), "Expected tokio");
    assert!(project.description.is_some(), "Expected a description");
    let desc = project.description.as_ref().unwrap();
    assert!(
        desc.contains("rust") || desc.contains("Rust"),
        "Description should mention rust, got: {desc}"
    );
}

#[test]
fn bundle_context_excludes_sensitive_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Helper to write files with directories
    let write = |path: &str, content: &str| {
        let full = root.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, content).unwrap();
    };

    // Legitimate files — should appear
    write("Cargo.toml", "[package]\nname = \"test\"\n");
    write("src/main.rs", "fn main() {}\n");

    // Sensitive-named files — must NOT appear
    write(".env", "API_KEY=super_secret\n");
    write("api_key.env", "KEY=abc123\n");
    write("credentials.json", r#"{"token":"secret"}"#);
    write("secret.key", "-----BEGIN PRIVATE KEY-----\n");
    write(".secrets", "password=hunter2\n");
    write("config/local.env", "DB_PASSWORD=foo\n");
    write("deploy/prod.pem", "-----BEGIN CERTIFICATE-----\n");
    write("auth/id_rsa", "-----BEGIN RSA PRIVATE KEY-----\n");

    let ctx = actual_cli::tailoring::bundle_context(root).unwrap();

    let sensitive_names = [
        ".env",
        "api_key.env",
        "credentials.json",
        "secret.key",
        ".secrets",
        "local.env",
        "prod.pem",
        "id_rsa",
    ];
    for name in &sensitive_names {
        assert!(
            !ctx.file_tree.contains(name),
            "file_tree must not include sensitive file '{}', got:\n{}",
            name,
            ctx.file_tree
        );
    }

    // Legitimate files should still appear
    assert!(
        ctx.file_tree.contains("Cargo.toml"),
        "Cargo.toml should appear in file_tree"
    );
}
