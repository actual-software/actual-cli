use std::path::{Path, PathBuf};

use console::style;

use crate::api::DEFAULT_API_URL;
use crate::cli::args::StatusArgs;
use crate::cli::ui::progress::{SUCCESS_SYMBOL, WARN_SYMBOL};
use crate::config;
use crate::config::types::Config;
use crate::error::ActualError;
use crate::generation::markers;

pub fn exec(args: &StatusArgs) -> i32 {
    match load_and_run(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{} {}", style("Error:").red().bold(), e);
            e.exit_code()
        }
    }
}

fn load_and_run(args: &StatusArgs) -> Result<(), ActualError> {
    let config_path = config::paths::config_path()?;
    let config_exists = config_path.exists();
    let cfg = config::paths::load()?;
    let cwd = resolve_cwd();
    run_status(args, &cfg, &config_path, config_exists, &cwd);
    Ok(())
}

/// Resolve the current working directory, falling back to `"."` if unavailable
/// (e.g. the directory was deleted while the process was running).
fn resolve_cwd() -> PathBuf {
    let fallback = PathBuf::from(".");
    std::env::current_dir().unwrap_or(fallback)
}

fn run_status(
    args: &StatusArgs,
    cfg: &Config,
    config_path: &Path,
    config_exists: bool,
    cwd: &Path,
) {
    // 1. Config section
    print_config_section(cfg, config_path, config_exists, args.verbose);

    println!();

    // 2. CLAUDE.md files section
    print_claude_md_section(cwd);

    // 3. Verbose: cached analysis
    if args.verbose {
        println!();
        print_verbose_section(cfg);
    }
}

fn print_config_section(cfg: &Config, config_path: &Path, config_exists: bool, verbose: bool) {
    println!("{}:", style("Config").bold());

    let exists_label = if config_exists {
        format!("({})", style("exists").green())
    } else {
        format!("({})", style("created").yellow())
    };
    println!(
        "  {:<14} {} {}",
        "Config file:",
        config_path.display(),
        exists_label
    );

    let api_url = cfg.api_url.as_deref().unwrap_or(DEFAULT_API_URL);
    let api_label = if cfg.api_url.is_none() {
        format!("({})", style("default").dim())
    } else {
        String::new()
    };
    println!("  {:<14} {} {}", "API URL:", api_url, api_label);

    if verbose {
        let model = cfg
            .model
            .as_deref()
            .unwrap_or("not set (will use server default)");
        println!("  {:<14} {}", "Model:", model);
    }
}

fn print_claude_md_section(cwd: &Path) {
    let files = find_claude_md_files(cwd);

    println!("{}:", style("CLAUDE.md files").bold());

    if files.is_empty() {
        println!("  No CLAUDE.md files found in current directory.");
        println!(
            "  Run {} to generate CLAUDE.md files for this repository.",
            style("`actual sync`").cyan()
        );
        return;
    }

    for file_path in &files {
        let relative = file_path.strip_prefix(cwd).unwrap_or(file_path);
        let display_path = format!("./{}", relative.display());

        let content = std::fs::read_to_string(file_path).unwrap_or_default();

        if markers::has_managed_section(&content) {
            let version = markers::extract_version(&content);
            let last_synced = markers::extract_last_synced(&content);

            let mut details = Vec::new();
            if let Some(v) = version {
                details.push(format!("v{v}"));
            }
            if let Some(ts) = last_synced {
                details.push(format!("synced {ts}"));
            }

            let detail_str = if details.is_empty() {
                String::new()
            } else {
                format!(" ({})", details.join(", "))
            };

            println!(
                "  {} {:<40} {}{}",
                style(SUCCESS_SYMBOL).green(),
                display_path,
                style("managed").green(),
                detail_str
            );
        } else {
            println!(
                "  {} {:<40} {}",
                style(WARN_SYMBOL).yellow(),
                display_path,
                style("no managed section").yellow()
            );
        }
    }
}

fn print_verbose_section(cfg: &Config) {
    println!("{}:", style("Details").bold());

    // Telemetry
    let telemetry_enabled = cfg
        .telemetry
        .as_ref()
        .and_then(|t| t.enabled)
        .unwrap_or(true);
    println!(
        "  {:<20} {}",
        "Telemetry:",
        if telemetry_enabled {
            style("enabled").green().to_string()
        } else {
            style("disabled").yellow().to_string()
        }
    );

    // Batch size
    let batch_size = cfg.batch_size.unwrap_or(15);
    println!("  {:<20} {}", "Batch size:", batch_size);

    // Concurrency
    let concurrency = cfg.concurrency.unwrap_or(3);
    println!("  {:<20} {}", "Concurrency:", concurrency);

    // Rejected ADRs
    if let Some(ref rejected) = cfg.rejected_adrs {
        let total: usize = rejected.values().map(|v| v.len()).sum();
        println!(
            "  {:<20} {} across {} repos",
            "Rejected ADRs:",
            total,
            rejected.len()
        );
    } else {
        println!("  {:<20} none", "Rejected ADRs:");
    }

    // Cached analysis
    if let Some(ref analysis) = cfg.cached_analysis {
        println!("  {:<20} {}", "Cached analysis:", style("present").green());
        println!("    {:<18} {}", "Repo:", analysis.repo_path);
        if let Some(ref commit) = analysis.head_commit {
            let short = if commit.len() > 7 {
                &commit[..7]
            } else {
                commit
            };
            println!("    {:<18} {}", "HEAD:", short);
        }
        println!("    {:<18} {}", "Analyzed at:", analysis.analyzed_at);
    } else {
        println!("  {:<20} {}", "Cached analysis:", style("none").dim());
    }
}

/// Recursively find all files named `CLAUDE.md` under the given root directory.
///
/// Skips hidden directories (starting with `.`) and common ignore directories
/// (`node_modules`, `target`, `vendor`, `.git`).
pub fn find_claude_md_files(root: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    walk_for_claude_md(root, &mut results);
    results.sort();
    results
}

const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    ".git",
    ".hg",
    ".svn",
    "__pycache__",
    "dist",
    "build",
    ".worktrees",
];

fn walk_for_claude_md(dir: &Path, results: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let path = entry.path();
        if path.is_dir() {
            // Skip hidden directories and common ignore dirs
            if name_str.starts_with('.') || SKIP_DIRS.contains(&name_str.as_ref()) {
                continue;
            }
            walk_for_claude_md(&path, results);
        } else if name_str == "CLAUDE.md" {
            results.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{CachedAnalysis, TelemetryConfig};
    use std::collections::HashMap;
    use tempfile::tempdir;

    // ─── find_claude_md_files ────────────────────────────────────

    #[test]
    fn test_find_claude_md_files_empty_dir() {
        let dir = tempdir().unwrap();
        let files = find_claude_md_files(dir.path());
        assert!(files.is_empty(), "expected no files in empty dir");
    }

    #[test]
    fn test_find_claude_md_files_finds_root_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# Test").unwrap();
        let files = find_claude_md_files(dir.path());
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("CLAUDE.md"));
    }

    #[test]
    fn test_find_claude_md_files_finds_nested() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# Root").unwrap();
        let sub = dir.path().join("src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("CLAUDE.md"), "# Src").unwrap();
        let files = find_claude_md_files(dir.path());
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn test_find_claude_md_files_skips_hidden_dirs() {
        let dir = tempdir().unwrap();
        let hidden = dir.path().join(".hidden");
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::write(hidden.join("CLAUDE.md"), "# Hidden").unwrap();
        let files = find_claude_md_files(dir.path());
        assert!(files.is_empty());
    }

    #[test]
    fn test_find_claude_md_files_skips_node_modules() {
        let dir = tempdir().unwrap();
        let nm = dir.path().join("node_modules");
        std::fs::create_dir_all(&nm).unwrap();
        std::fs::write(nm.join("CLAUDE.md"), "# NM").unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# Root").unwrap();
        let files = find_claude_md_files(dir.path());
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn test_find_claude_md_files_ignores_other_md_files() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "# Readme").unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# Claude").unwrap();
        let files = find_claude_md_files(dir.path());
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("CLAUDE.md"));
    }

    #[test]
    fn test_find_claude_md_files_sorted() {
        let dir = tempdir().unwrap();
        let b_dir = dir.path().join("b");
        let a_dir = dir.path().join("a");
        std::fs::create_dir_all(&b_dir).unwrap();
        std::fs::create_dir_all(&a_dir).unwrap();
        std::fs::write(b_dir.join("CLAUDE.md"), "# B").unwrap();
        std::fs::write(a_dir.join("CLAUDE.md"), "# A").unwrap();
        let files = find_claude_md_files(dir.path());
        assert_eq!(files.len(), 2);
        assert!(files[0] < files[1]);
    }

    #[test]
    fn test_find_claude_md_files_nonexistent_dir() {
        let files = find_claude_md_files(Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(files.is_empty());
    }

    #[test]
    fn test_find_claude_md_files_skips_all_ignored_dirs() {
        let dir = tempdir().unwrap();
        for skip in SKIP_DIRS {
            let d = dir.path().join(skip);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("CLAUDE.md"), "# Skip").unwrap();
        }
        let files = find_claude_md_files(dir.path());
        assert!(files.is_empty());
    }

    // ─── exec ────────────────────────────────────────────────────

    #[test]
    fn test_exec_returns_zero() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");
        std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());

        let args = StatusArgs { verbose: false };
        let code = exec(&args);
        assert_eq!(code, 0);

        std::env::remove_var("ACTUAL_CONFIG");
    }

    #[test]
    fn test_exec_verbose_returns_zero() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");
        std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());

        let args = StatusArgs { verbose: true };
        let code = exec(&args);
        assert_eq!(code, 0);

        std::env::remove_var("ACTUAL_CONFIG");
    }

    #[test]
    fn test_exec_error_path() {
        // Set ACTUAL_CONFIG to empty string which will cause config_path to fail
        std::env::set_var("ACTUAL_CONFIG", "");
        let args = StatusArgs { verbose: false };
        let code = exec(&args);
        assert_ne!(code, 0);
        std::env::remove_var("ACTUAL_CONFIG");
    }

    // ─── print_config_section ────────────────────────────────────

    #[test]
    fn test_print_config_section_default_config() {
        let cfg = Config::default();
        let path = PathBuf::from("/tmp/test/config.yaml");
        // Exercises the config_exists=true branch and api_url=None branch
        print_config_section(&cfg, &path, true, false);
    }

    #[test]
    fn test_print_config_section_created_config() {
        let cfg = Config::default();
        let path = PathBuf::from("/tmp/test/config.yaml");
        // Exercises the config_exists=false (created) branch
        print_config_section(&cfg, &path, false, false);
    }

    #[test]
    fn test_print_config_section_custom_api_url() {
        let cfg = Config {
            api_url: Some("https://custom.api.com".to_string()),
            ..Config::default()
        };
        let path = PathBuf::from("/tmp/test/config.yaml");
        // Exercises the api_url.is_some() branch
        print_config_section(&cfg, &path, true, false);
    }

    #[test]
    fn test_print_config_section_verbose_with_model() {
        let cfg = Config {
            model: Some("opus".to_string()),
            ..Config::default()
        };
        let path = PathBuf::from("/tmp/test/config.yaml");
        // Exercises verbose=true with model set
        print_config_section(&cfg, &path, true, true);
    }

    #[test]
    fn test_print_config_section_verbose_no_model() {
        let cfg = Config::default();
        let path = PathBuf::from("/tmp/test/config.yaml");
        // Exercises verbose=true with model=None
        print_config_section(&cfg, &path, true, true);
    }

    // ─── print_claude_md_section ─────────────────────────────────

    #[test]
    fn test_print_claude_md_section_empty() {
        let dir = tempdir().unwrap();
        print_claude_md_section(dir.path());
    }

    #[test]
    fn test_print_claude_md_section_unmanaged() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# No markers").unwrap();
        print_claude_md_section(dir.path());
    }

    #[test]
    fn test_print_claude_md_section_managed() {
        let dir = tempdir().unwrap();
        let managed = markers::wrap_in_markers("test content", 2, &[]);
        std::fs::write(dir.path().join("CLAUDE.md"), &managed).unwrap();
        print_claude_md_section(dir.path());
    }

    #[test]
    fn test_print_claude_md_section_managed_no_metadata() {
        let dir = tempdir().unwrap();
        // Managed section but without version/last-synced metadata
        let content = format!(
            "{}\ncontent only\n{}",
            markers::START_MARKER,
            markers::END_MARKER
        );
        std::fs::write(dir.path().join("CLAUDE.md"), &content).unwrap();
        print_claude_md_section(dir.path());
    }

    // ─── print_verbose_section ───────────────────────────────────

    #[test]
    fn test_print_verbose_section_defaults() {
        let cfg = Config::default();
        print_verbose_section(&cfg);
    }

    #[test]
    fn test_print_verbose_section_telemetry_disabled() {
        let cfg = Config {
            telemetry: Some(TelemetryConfig {
                enabled: Some(false),
            }),
            ..Config::default()
        };
        print_verbose_section(&cfg);
    }

    #[test]
    fn test_print_verbose_section_telemetry_enabled_explicit() {
        let cfg = Config {
            telemetry: Some(TelemetryConfig {
                enabled: Some(true),
            }),
            ..Config::default()
        };
        print_verbose_section(&cfg);
    }

    #[test]
    fn test_print_verbose_section_telemetry_none_in_struct() {
        let cfg = Config {
            telemetry: Some(TelemetryConfig { enabled: None }),
            ..Config::default()
        };
        print_verbose_section(&cfg);
    }

    #[test]
    fn test_print_verbose_section_with_custom_batch_and_concurrency() {
        let cfg = Config {
            batch_size: Some(25),
            concurrency: Some(8),
            ..Config::default()
        };
        print_verbose_section(&cfg);
    }

    #[test]
    fn test_print_verbose_section_with_rejected_adrs() {
        let mut rejected = HashMap::new();
        rejected.insert(
            "repo1".to_string(),
            vec!["adr-001".to_string(), "adr-002".to_string()],
        );
        rejected.insert("repo2".to_string(), vec!["adr-003".to_string()]);
        let cfg = Config {
            rejected_adrs: Some(rejected),
            ..Config::default()
        };
        print_verbose_section(&cfg);
    }

    #[test]
    fn test_print_verbose_section_with_cached_analysis_long_commit() {
        let cfg = Config {
            cached_analysis: Some(CachedAnalysis {
                repo_path: "/home/user/project".to_string(),
                head_commit: Some("abc123def456789".to_string()),
                analysis: serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
                analyzed_at: chrono::Utc::now(),
            }),
            ..Config::default()
        };
        print_verbose_section(&cfg);
    }

    #[test]
    fn test_print_verbose_section_with_cached_analysis_short_commit() {
        let cfg = Config {
            cached_analysis: Some(CachedAnalysis {
                repo_path: "/home/user/project".to_string(),
                head_commit: Some("abc".to_string()),
                analysis: serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
                analyzed_at: chrono::Utc::now(),
            }),
            ..Config::default()
        };
        print_verbose_section(&cfg);
    }

    #[test]
    fn test_print_verbose_section_with_cached_analysis_no_commit() {
        let cfg = Config {
            cached_analysis: Some(CachedAnalysis {
                repo_path: "/home/user/project".to_string(),
                head_commit: None,
                analysis: serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
                analyzed_at: chrono::Utc::now(),
            }),
            ..Config::default()
        };
        print_verbose_section(&cfg);
    }

    #[test]
    fn test_print_verbose_section_with_cached_analysis_exactly_7_char_commit() {
        let cfg = Config {
            cached_analysis: Some(CachedAnalysis {
                repo_path: "/home/user/project".to_string(),
                head_commit: Some("abcdefg".to_string()),
                analysis: serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
                analyzed_at: chrono::Utc::now(),
            }),
            ..Config::default()
        };
        print_verbose_section(&cfg);
    }

    // ─── resolve_cwd ──────────────────────────────────────────────

    #[test]
    fn test_resolve_cwd_returns_path() {
        let cwd = resolve_cwd();
        // resolve_cwd always returns a valid path (either real CWD or ".")
        assert!(!cwd.as_os_str().is_empty());
    }

    // ─── load_and_run / run_status ─────────────────────────────────

    #[test]
    fn test_load_and_run_succeeds() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");
        std::env::set_var("ACTUAL_CONFIG", config_file.to_str().unwrap());

        let result = load_and_run(&StatusArgs { verbose: false });
        assert!(result.is_ok());

        std::env::remove_var("ACTUAL_CONFIG");
    }

    // ─── run_status ──────────────────────────────────────────────

    #[test]
    fn test_run_status_normal_mode() {
        let dir = tempdir().unwrap();
        let cfg = Config::default();
        let config_path = dir.path().join("config.yaml");
        run_status(
            &StatusArgs { verbose: false },
            &cfg,
            &config_path,
            true,
            dir.path(),
        );
    }

    #[test]
    fn test_run_status_verbose_mode() {
        let dir = tempdir().unwrap();
        let cfg = Config::default();
        let config_path = dir.path().join("config.yaml");
        run_status(
            &StatusArgs { verbose: true },
            &cfg,
            &config_path,
            true,
            dir.path(),
        );
    }
}
