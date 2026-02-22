use std::path::Path;

use crate::api::DEFAULT_API_URL;
use crate::branding::banner::print_banner;
use crate::cli::args::StatusArgs;
use crate::cli::commands::auth::check_auth_with_timeout;
use crate::cli::commands::sync::resolve_cwd;
use crate::cli::ui::header::{render_header_bar, AuthDisplay};
use crate::cli::ui::panel::Panel;
use crate::cli::ui::term_size;
use crate::cli::ui::theme;
use crate::config;
use crate::config::types::{Config, DEFAULT_BATCH_SIZE, DEFAULT_CONCURRENCY, DEFAULT_MODEL};
use crate::error::ActualError;
use crate::generation::markers;
use crate::generation::OutputFormat;
use crate::runner::binary::find_claude_binary;

pub fn exec(args: &StatusArgs) -> Result<(), ActualError> {
    load_and_run(args)
}

fn load_and_run(args: &StatusArgs) -> Result<(), ActualError> {
    let config_path = config::paths::config_path()?;
    let config_exists = config_path.exists();
    let cfg = config::paths::load()?;
    let cwd = resolve_cwd();
    let output_format = cfg.output_format.clone().unwrap_or_default();
    run_status(
        args,
        &cfg,
        &config_path,
        config_exists,
        &cwd,
        &output_format,
    );
    Ok(())
}

/// Attempt to detect authentication status gracefully with a timeout.
///
/// Returns `None` if the Claude binary is not found, auth check fails,
/// times out (3 seconds), or any other error occurs. The status command
/// should never fail or hang because of auth detection.
fn try_detect_auth() -> Option<AuthDisplay> {
    use std::time::Duration;

    let binary = find_claude_binary().ok()?;
    check_auth_with_timeout(&binary, Duration::from_secs(3))
        .ok()
        .map(|status| AuthDisplay {
            authenticated: status.logged_in,
            email: status.email,
        })
}

fn run_status(
    args: &StatusArgs,
    cfg: &Config,
    config_path: &Path,
    config_exists: bool,
    cwd: &Path,
    output_format: &OutputFormat,
) {
    let stderr_width = term_size::terminal_width();
    let width = term_size::terminal_width();

    // Banner + header bar (written to stderr)
    print_banner(false);
    let auth = try_detect_auth();
    eprint!(
        "{}",
        render_header_bar(stderr_width, env!("CARGO_PKG_VERSION"), auth.as_ref())
    );

    // Config panel
    println!(
        "{}",
        format_config_section(cfg, config_path, config_exists, args.verbose, width)
    );

    // Output files panel (format-aware)
    println!("{}", format_output_files_section(cwd, output_format, width));

    // Verbose details panel
    if args.verbose {
        println!("{}", format_verbose_section(cfg, width));
    }
}

fn format_config_section(
    cfg: &Config,
    config_path: &Path,
    config_exists: bool,
    verbose: bool,
    width: usize,
) -> String {
    let config_annotation = if config_exists {
        format!("{} exists", theme::SUCCESS)
    } else {
        format!("{} created", theme::WARN)
    };

    let api_url = cfg.api_url.as_deref().unwrap_or(DEFAULT_API_URL);

    let panel_with_config = Panel::titled("Configuration").kv_annotated(
        "Config file",
        &config_path.display().to_string(),
        &config_annotation,
    );

    let mut panel = if cfg.api_url.is_none() {
        panel_with_config.kv_annotated("API URL", api_url, "(default)")
    } else {
        panel_with_config.kv("API URL", api_url)
    };

    // Always show the active model so users can confirm what is configured.
    panel = if cfg.model.is_none() {
        panel.kv_annotated("Model", DEFAULT_MODEL, "(default)")
    } else {
        panel.kv("Model", cfg.model.as_deref().unwrap_or(DEFAULT_MODEL))
    };

    if verbose {
        let runner = match cfg.runner.as_deref() {
            Some(r) => r.to_string(),
            None => "(inferred from model)".to_string(),
        };
        panel = panel.kv("Runner", &runner);

        if let Some(ref cursor_model) = cfg.cursor_model {
            panel = panel.kv("Cursor model", cursor_model);
        }
    }

    panel.render(width)
}

fn format_output_files_section(cwd: &Path, format: &OutputFormat, width: usize) -> String {
    let files = super::find_output_files(cwd, format);
    let filename = format.filename();
    let panel_title = format!("{filename} Files");

    if files.is_empty() {
        let panel = Panel::titled(&panel_title).line(&format!(
            "No {} files found. Run {} to generate.",
            filename,
            theme::hint("`actual sync`")
        ));
        return panel.render(width);
    }

    let mut panel = Panel::titled(&panel_title);
    let mut managed_count: usize = 0;
    let mut unmanaged_count: usize = 0;

    for file_path in &files {
        let relative = file_path.strip_prefix(cwd).unwrap_or(file_path);
        let display_path = format!("./{}", relative.display());

        let content = std::fs::read_to_string(file_path).unwrap_or_default();

        if markers::has_managed_section(&content) {
            managed_count += 1;
            let version = markers::extract_version(&content);
            let version_str = match version {
                Some(v) => {
                    let ts = markers::extract_last_synced(&content);
                    match ts {
                        Some(t) => format!("v{v} · synced {t}"),
                        None => format!("v{v}"),
                    }
                }
                None => String::new(),
            };
            let label = format!("{} {display_path}", theme::SUCCESS);
            if version_str.is_empty() {
                panel = panel.kv(&label, "managed");
            } else {
                panel = panel.kv_annotated(&label, "managed", &version_str);
            }
        } else {
            unmanaged_count += 1;
            let label = format!("{} {display_path}", theme::WARN);
            panel = panel.kv(&label, "unmanaged");
        }
    }

    let footer = format!(
        "{} managed {} {} unmanaged",
        managed_count,
        theme::muted("·"),
        unmanaged_count
    );
    panel = panel.footer(&footer);

    panel.render(width)
}

fn format_verbose_section(cfg: &Config, width: usize) -> String {
    let telemetry_enabled = cfg
        .telemetry
        .as_ref()
        .and_then(|t| t.enabled)
        .unwrap_or(true);
    let telemetry_value = if telemetry_enabled {
        format!("{}", theme::success("enabled"))
    } else {
        format!("{}", theme::warning("disabled"))
    };

    let batch_size = cfg.batch_size.unwrap_or(DEFAULT_BATCH_SIZE);
    let concurrency = cfg.concurrency.unwrap_or(DEFAULT_CONCURRENCY);

    let rejected_adrs_value = if let Some(ref rejected) = cfg.rejected_adrs {
        let total: usize = rejected.values().map(|v| v.len()).sum();
        format!("{total} across {} repos", rejected.len())
    } else {
        "none".to_string()
    };

    let mut panel = Panel::titled("Details")
        .kv("Telemetry", &telemetry_value)
        .kv("Batch size", &batch_size.to_string())
        .kv("Concurrency", &concurrency.to_string())
        .kv("Rejected ADRs", &rejected_adrs_value);

    if let Some(ref analysis) = cfg.cached_analysis {
        let mut details = vec![format!("repo: {}", analysis.repo_path)];
        if let Some(ref commit) = analysis.head_commit {
            let short = if commit.len() > 7 {
                &commit[..7]
            } else {
                commit
            };
            details.push(format!("HEAD: {short}"));
        }
        details.push(format!("at: {}", analysis.analyzed_at));
        panel = panel.kv("Cached analysis", &format!("{}", theme::success("present")));
        for detail in &details {
            panel = panel.line(&format!("  {detail}"));
        }
    } else {
        panel = panel.kv("Cached analysis", &format!("{}", theme::muted("none")));
    }

    panel.render(width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::types::RepoAnalysis;
    use crate::cli::commands::handle_result;
    use crate::config::types::{CachedAnalysis, TelemetryConfig};
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tempfile::tempdir;

    // Re-import shared utilities from the parent commands module for tests.
    use super::super::{find_claude_md_files, SKIP_DIRS};

    fn with_temp_config<F: FnOnce()>(f: F) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("config.yaml");
        let _guard = EnvGuard::set("ACTUAL_CONFIG", config_file.to_str().unwrap());
        f();
    }

    /// Strip ANSI codes from rendered output for assertion.
    fn plain(rendered: &str) -> String {
        console::strip_ansi_codes(rendered).into_owned()
    }

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
        with_temp_config(|| {
            let args = StatusArgs { verbose: false };
            let code = handle_result(exec(&args));
            assert_eq!(code, 0);
        });
    }

    #[test]
    fn test_exec_verbose_returns_zero() {
        with_temp_config(|| {
            let args = StatusArgs { verbose: true };
            let code = handle_result(exec(&args));
            assert_eq!(code, 0);
        });
    }

    #[test]
    fn test_exec_error_path() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        // Set ACTUAL_CONFIG to empty string which will cause config_path to fail
        let _guard = EnvGuard::set("ACTUAL_CONFIG", "");
        let args = StatusArgs { verbose: false };
        let code = handle_result(exec(&args));
        assert_ne!(code, 0);
    }

    #[test]
    fn test_env_guard_both_branches() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let key = "ACTUAL_TEST_STATUS_RESTORE";

        // Exercise set-then-restore cycle
        {
            let _g = EnvGuard::set(key, "original");
            assert_eq!(std::env::var(key).unwrap(), "original");
        }
        // Guard dropped: variable should be removed (was absent before)
        assert!(std::env::var(key).is_err());

        // Exercise set-over-existing then restore
        {
            let _g1 = EnvGuard::set(key, "first");
            let _g2 = EnvGuard::set(key, "second");
            assert_eq!(std::env::var(key).unwrap(), "second");
        }
        // Both guards dropped: variable should be removed
        assert!(std::env::var(key).is_err());
    }

    // ─── format_config_section ───────────────────────────────────

    #[test]
    fn test_format_config_section_default_config() {
        let cfg = Config::default();
        let path = PathBuf::from("/tmp/test/config.yaml");
        let output = format_config_section(&cfg, &path, true, false, 80);
        let p = plain(&output);
        assert!(p.contains("Configuration"), "should have panel title");
        assert!(
            p.contains("/tmp/test/config.yaml"),
            "should show config path"
        );
        assert!(p.contains("exists"), "should show 'exists' annotation");
        assert!(p.contains("API URL"), "should show API URL label");
        assert!(
            p.contains("(default)"),
            "should show '(default)' for API or Model"
        );
        assert!(p.contains("Model"), "should show Model row always");
        assert!(
            p.contains("claude-sonnet-4-6"),
            "should show default model name"
        );
    }

    #[test]
    fn test_format_config_section_created_config() {
        let cfg = Config::default();
        let path = PathBuf::from("/tmp/test/config.yaml");
        let output = format_config_section(&cfg, &path, false, false, 80);
        let p = plain(&output);
        assert!(p.contains("created"), "should show 'created' annotation");
    }

    #[test]
    fn test_format_config_section_custom_api_url() {
        let cfg = Config {
            api_url: Some("https://custom.api.com".to_string()),
            ..Config::default()
        };
        let path = PathBuf::from("/tmp/test/config.yaml");
        let output = format_config_section(&cfg, &path, true, false, 80);
        let p = plain(&output);
        assert!(
            p.contains("https://custom.api.com"),
            "should show custom API URL"
        );
        // API URL row must not show "(default)" since it's explicitly set
        let api_line = p
            .lines()
            .find(|l| l.contains("API URL"))
            .unwrap_or_default();
        assert!(
            !api_line.contains("(default)"),
            "should not show '(default)' for custom URL: {api_line}"
        );
    }

    #[test]
    fn test_format_config_section_normal_shows_configured_model() {
        let cfg = Config {
            model: Some("claude-opus-4-5".to_string()),
            ..Config::default()
        };
        let path = PathBuf::from("/tmp/test/config.yaml");
        let output = format_config_section(&cfg, &path, true, false, 80);
        let p = plain(&output);
        assert!(p.contains("Model"), "should show Model row: {p}");
        assert!(
            p.contains("claude-opus-4-5"),
            "should show configured model name: {p}"
        );
        // Model row must not show "(default)" since model is explicitly set
        let model_line = p.lines().find(|l| l.contains("Model")).unwrap_or_default();
        assert!(
            !model_line.contains("(default)"),
            "should not show (default) on Model line when model is explicitly set: {model_line}"
        );
    }

    #[test]
    fn test_format_config_section_normal_shows_default_model_when_not_set() {
        let cfg = Config::default();
        let path = PathBuf::from("/tmp/test/config.yaml");
        let output = format_config_section(&cfg, &path, true, false, 80);
        let p = plain(&output);
        assert!(p.contains("Model"), "should show Model row: {p}");
        assert!(
            p.contains("claude-sonnet-4-6"),
            "should show default model name: {p}"
        );
        assert!(
            p.contains("(default)"),
            "should annotate with (default) when no model is set: {p}"
        );
    }

    #[test]
    fn test_format_config_section_verbose_with_model() {
        let cfg = Config {
            model: Some("opus".to_string()),
            ..Config::default()
        };
        let path = PathBuf::from("/tmp/test/config.yaml");
        let output = format_config_section(&cfg, &path, true, true, 80);
        let p = plain(&output);
        assert!(p.contains("Model"), "should show Model always");
        assert!(p.contains("opus"), "should show model name");
    }

    #[test]
    fn test_format_config_section_verbose_no_model() {
        let cfg = Config::default();
        let path = PathBuf::from("/tmp/test/config.yaml");
        let output = format_config_section(&cfg, &path, true, true, 80);
        let p = plain(&output);
        assert!(p.contains("Model"), "should show Model always");
        assert!(
            p.contains("claude-sonnet-4-6"),
            "should show default model name when model not set"
        );
        assert!(
            p.contains("(default)"),
            "should annotate with (default) when model not set"
        );
    }

    #[test]
    fn test_format_config_section_verbose_shows_runner_inferred() {
        let cfg = Config::default();
        let path = PathBuf::from("/tmp/test/config.yaml");
        let output = format_config_section(&cfg, &path, true, true, 80);
        let p = plain(&output);
        assert!(p.contains("Runner"), "should show Runner in verbose mode");
        assert!(
            p.contains("(inferred from model)"),
            "should show inferred runner when not set: {p}"
        );
    }

    #[test]
    fn test_format_config_section_verbose_shows_explicit_runner() {
        let cfg = Config {
            runner: Some("anthropic-api".to_string()),
            ..Config::default()
        };
        let path = PathBuf::from("/tmp/test/config.yaml");
        let output = format_config_section(&cfg, &path, true, true, 80);
        let p = plain(&output);
        assert!(p.contains("Runner"), "should show Runner in verbose mode");
        assert!(
            p.contains("anthropic-api"),
            "should show explicit runner value: {p}"
        );
    }

    #[test]
    fn test_format_config_section_verbose_shows_gpt_model_in_model_row() {
        // A gpt model set via config.model shows in the Model row.
        let cfg = Config {
            model: Some("gpt-5".to_string()),
            ..Config::default()
        };
        let path = PathBuf::from("/tmp/test/config.yaml");
        let output = format_config_section(&cfg, &path, true, true, 80);
        let p = plain(&output);
        assert!(
            p.contains("Model"),
            "should show Model row in verbose mode: {p}"
        );
        assert!(
            p.contains("gpt-5"),
            "should show model value in Model row: {p}"
        );
        assert!(
            !p.contains("OpenAI model"),
            "should NOT show a separate OpenAI model row: {p}"
        );
    }

    #[test]
    fn test_format_config_section_verbose_shows_cursor_model() {
        let cfg = Config {
            cursor_model: Some("cursor-fast".to_string()),
            ..Config::default()
        };
        let path = PathBuf::from("/tmp/test/config.yaml");
        let output = format_config_section(&cfg, &path, true, true, 80);
        let p = plain(&output);
        assert!(
            p.contains("Cursor model"),
            "should show Cursor model in verbose mode: {p}"
        );
        assert!(
            p.contains("cursor-fast"),
            "should show cursor model value: {p}"
        );
    }

    #[test]
    fn test_format_config_section_verbose_hides_cursor_model_when_not_set() {
        let cfg = Config::default();
        let path = PathBuf::from("/tmp/test/config.yaml");
        let output = format_config_section(&cfg, &path, true, true, 80);
        let p = plain(&output);
        assert!(
            !p.contains("Cursor model"),
            "should not show Cursor model when not set: {p}"
        );
    }

    #[test]
    fn test_format_config_section_verbose_all_fields() {
        let cfg = Config {
            runner: Some("openai-api".to_string()),
            model: Some("sonnet".to_string()),
            cursor_model: Some("cursor-small".to_string()),
            ..Config::default()
        };
        let path = PathBuf::from("/tmp/test/config.yaml");
        let output = format_config_section(&cfg, &path, true, true, 80);
        let p = plain(&output);
        assert!(p.contains("Runner"), "should show Runner: {p}");
        assert!(p.contains("openai-api"), "should show runner value: {p}");
        assert!(p.contains("Model"), "should show Model: {p}");
        assert!(p.contains("sonnet"), "should show model value: {p}");
        assert!(
            !p.contains("OpenAI model"),
            "should NOT show separate OpenAI model row: {p}"
        );
        assert!(p.contains("Cursor model"), "should show Cursor model: {p}");
        assert!(
            p.contains("cursor-small"),
            "should show cursor model value: {p}"
        );
    }

    #[test]
    fn test_format_config_section_verbose_not_shown_when_not_verbose() {
        let cfg = Config {
            runner: Some("anthropic-api".to_string()),
            cursor_model: Some("cursor-fast".to_string()),
            ..Config::default()
        };
        let path = PathBuf::from("/tmp/test/config.yaml");
        let output = format_config_section(&cfg, &path, true, false, 80);
        let p = plain(&output);
        assert!(
            p.contains("Model"),
            "should show Model even when not verbose: {p}"
        );
        assert!(
            !p.contains("Runner"),
            "should not show Runner when not verbose: {p}"
        );
        assert!(
            !p.contains("OpenAI model"),
            "should not show OpenAI model row: {p}"
        );
        assert!(
            !p.contains("Cursor model"),
            "should not show Cursor model when not verbose: {p}"
        );
    }

    #[test]
    fn test_format_config_section_has_box_borders() {
        let cfg = Config::default();
        let path = PathBuf::from("/tmp/test/config.yaml");
        let output = format_config_section(&cfg, &path, true, false, 80);
        let p = plain(&output);
        assert!(
            p.contains('╭'),
            "should contain top-left corner box character"
        );
        assert!(
            p.contains('╰'),
            "should contain bottom-left corner box character"
        );
        assert!(p.contains('│'), "should contain vertical border character");
    }

    // ─── format_output_files_section ────────────────────────────────

    #[test]
    fn test_format_output_files_section_empty() {
        let dir = tempdir().unwrap();
        let output = format_output_files_section(dir.path(), &OutputFormat::ClaudeMd, 80);
        let p = plain(&output);
        assert!(
            p.contains("CLAUDE.md Files"),
            "should have panel title: {p}"
        );
        assert!(
            p.contains("No CLAUDE.md files found"),
            "should show empty message: {p}"
        );
        assert!(
            p.contains("actual sync"),
            "should show hint to run sync: {p}"
        );
    }

    #[test]
    fn test_format_output_files_section_agents_md_empty() {
        let dir = tempdir().unwrap();
        let output = format_output_files_section(dir.path(), &OutputFormat::AgentsMd, 80);
        let p = plain(&output);
        assert!(
            p.contains("AGENTS.md Files"),
            "should have panel title for agents-md: {p}"
        );
        assert!(
            p.contains("No AGENTS.md files found"),
            "should show empty message for agents-md: {p}"
        );
    }

    #[test]
    fn test_format_output_files_section_unmanaged() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# No markers").unwrap();
        let output = format_output_files_section(dir.path(), &OutputFormat::ClaudeMd, 80);
        let p = plain(&output);
        assert!(p.contains("unmanaged"), "should show 'unmanaged': {p}");
        assert!(p.contains("0 managed"), "should show 0 managed count: {p}");
        assert!(
            p.contains("1 unmanaged"),
            "should show 1 unmanaged count: {p}"
        );
    }

    #[test]
    fn test_format_output_files_section_managed() {
        let dir = tempdir().unwrap();
        let managed = markers::wrap_in_markers("test content", 2, &[]);
        std::fs::write(dir.path().join("CLAUDE.md"), &managed).unwrap();
        let output = format_output_files_section(dir.path(), &OutputFormat::ClaudeMd, 80);
        let p = plain(&output);
        assert!(p.contains("managed"), "should show 'managed': {p}");
        assert!(p.contains("v2"), "should show version: {p}");
        assert!(p.contains("synced"), "should show synced timestamp: {p}");
        assert!(p.contains("1 managed"), "should show 1 managed count: {p}");
        assert!(
            p.contains("0 unmanaged"),
            "should show 0 unmanaged count: {p}"
        );
    }

    #[test]
    fn test_format_output_files_section_managed_version_without_last_synced() {
        let dir = tempdir().unwrap();
        // Managed section with version but without last-synced metadata
        let content = format!(
            "{}\n<!-- version: 3 -->\ncontent\n{}",
            markers::START_MARKER,
            markers::END_MARKER
        );
        std::fs::write(dir.path().join("CLAUDE.md"), &content).unwrap();
        let output = format_output_files_section(dir.path(), &OutputFormat::ClaudeMd, 80);
        let p = plain(&output);
        assert!(p.contains("v3"), "should show version: {p}");
        assert!(
            !p.contains("synced"),
            "should not show synced without timestamp: {p}"
        );
    }

    #[test]
    fn test_format_output_files_section_managed_no_metadata() {
        let dir = tempdir().unwrap();
        // Managed section but without version/last-synced metadata
        let content = format!(
            "{}\ncontent only\n{}",
            markers::START_MARKER,
            markers::END_MARKER
        );
        std::fs::write(dir.path().join("CLAUDE.md"), &content).unwrap();
        let output = format_output_files_section(dir.path(), &OutputFormat::ClaudeMd, 80);
        let p = plain(&output);
        assert!(
            p.contains("managed"),
            "should show 'managed' for file with markers: {p}"
        );
        assert!(p.contains("1 managed"), "should show 1 managed count: {p}");
    }

    #[test]
    fn test_format_output_files_section_summary_counts() {
        let dir = tempdir().unwrap();
        // Create one managed and one unmanaged file
        let managed = markers::wrap_in_markers("test content", 1, &[]);
        std::fs::write(dir.path().join("CLAUDE.md"), &managed).unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("CLAUDE.md"), "# Unmanaged").unwrap();
        let output = format_output_files_section(dir.path(), &OutputFormat::ClaudeMd, 80);
        let p = plain(&output);
        assert!(p.contains("1 managed"), "should show 1 managed count: {p}");
        assert!(
            p.contains("1 unmanaged"),
            "should show 1 unmanaged count: {p}"
        );
    }

    // ─── format_verbose_section ──────────────────────────────────

    #[test]
    fn test_format_verbose_section_defaults() {
        let cfg = Config::default();
        let output = format_verbose_section(&cfg, 80);
        let p = plain(&output);
        assert!(p.contains("Details"), "should have panel title: {p}");
        assert!(p.contains("Telemetry"), "should show Telemetry: {p}");
        assert!(p.contains("enabled"), "should show 'enabled': {p}");
        assert!(p.contains("Batch size"), "should show Batch size: {p}");
        assert!(p.contains("15"), "should show default batch size: {p}");
        assert!(p.contains("Concurrency"), "should show Concurrency: {p}");
        assert!(p.contains("10"), "should show default concurrency: {p}");
        assert!(
            p.contains("Rejected ADRs"),
            "should show Rejected ADRs: {p}"
        );
        assert!(
            p.contains("none"),
            "should show 'none' for rejected ADRs: {p}"
        );
    }

    #[test]
    fn test_format_verbose_section_telemetry_disabled() {
        let cfg = Config {
            telemetry: Some(TelemetryConfig {
                enabled: Some(false),
            }),
            ..Config::default()
        };
        let output = format_verbose_section(&cfg, 80);
        let p = plain(&output);
        assert!(p.contains("disabled"), "should show 'disabled': {p}");
    }

    #[test]
    fn test_format_verbose_section_telemetry_enabled_explicit() {
        let cfg = Config {
            telemetry: Some(TelemetryConfig {
                enabled: Some(true),
            }),
            ..Config::default()
        };
        let output = format_verbose_section(&cfg, 80);
        let p = plain(&output);
        assert!(p.contains("enabled"), "should show 'enabled': {p}");
    }

    #[test]
    fn test_format_verbose_section_telemetry_none_in_struct() {
        let cfg = Config {
            telemetry: Some(TelemetryConfig { enabled: None }),
            ..Config::default()
        };
        let output = format_verbose_section(&cfg, 80);
        let p = plain(&output);
        // defaults to enabled
        assert!(
            p.contains("enabled"),
            "should show 'enabled' when None: {p}"
        );
    }

    #[test]
    fn test_format_verbose_section_with_custom_batch_and_concurrency() {
        let cfg = Config {
            batch_size: Some(25),
            concurrency: Some(8),
            ..Config::default()
        };
        let output = format_verbose_section(&cfg, 80);
        let p = plain(&output);
        assert!(p.contains("25"), "should show custom batch size: {p}");
        assert!(p.contains("8"), "should show custom concurrency: {p}");
    }

    #[test]
    fn test_format_verbose_section_with_rejected_adrs() {
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
        let output = format_verbose_section(&cfg, 80);
        let p = plain(&output);
        assert!(p.contains("3"), "should show total rejected count: {p}");
        assert!(
            p.contains("2 repos"),
            "should show repo count for rejected: {p}"
        );
    }

    #[test]
    fn test_format_verbose_section_with_cached_analysis_long_commit() {
        let cfg = Config {
            cached_analysis: Some(CachedAnalysis {
                repo_path: "/home/user/project".to_string(),
                head_commit: Some("abc123def456789".to_string()),
                config_hash: None,
                analysis: RepoAnalysis {
                    is_monorepo: false,
                    workspace_type: None,
                    projects: vec![],
                },
                analyzed_at: chrono::Utc::now(),
            }),
            ..Config::default()
        };
        let output = format_verbose_section(&cfg, 80);
        let p = plain(&output);
        assert!(
            p.contains("present"),
            "should show 'present' for cached analysis: {p}"
        );
        assert!(
            p.contains("/home/user/project"),
            "should show repo path: {p}"
        );
        assert!(p.contains("abc123d"), "should show short commit hash: {p}");
        assert!(
            !p.contains("abc123def456789"),
            "should not show full commit hash: {p}"
        );
    }

    #[test]
    fn test_format_verbose_section_with_cached_analysis_short_commit() {
        let cfg = Config {
            cached_analysis: Some(CachedAnalysis {
                repo_path: "/home/user/project".to_string(),
                head_commit: Some("abc".to_string()),
                config_hash: None,
                analysis: RepoAnalysis {
                    is_monorepo: false,
                    workspace_type: None,
                    projects: vec![],
                },
                analyzed_at: chrono::Utc::now(),
            }),
            ..Config::default()
        };
        let output = format_verbose_section(&cfg, 80);
        let p = plain(&output);
        assert!(p.contains("abc"), "should show short commit: {p}");
    }

    #[test]
    fn test_format_verbose_section_with_cached_analysis_no_commit() {
        let cfg = Config {
            cached_analysis: Some(CachedAnalysis {
                repo_path: "/home/user/project".to_string(),
                head_commit: None,
                config_hash: None,
                analysis: RepoAnalysis {
                    is_monorepo: false,
                    workspace_type: None,
                    projects: vec![],
                },
                analyzed_at: chrono::Utc::now(),
            }),
            ..Config::default()
        };
        let output = format_verbose_section(&cfg, 80);
        let p = plain(&output);
        assert!(
            p.contains("present"),
            "should show 'present' for cached analysis: {p}"
        );
        assert!(
            p.contains("/home/user/project"),
            "should show repo path: {p}"
        );
    }

    #[test]
    fn test_format_verbose_section_with_cached_analysis_exactly_7_char_commit() {
        let cfg = Config {
            cached_analysis: Some(CachedAnalysis {
                repo_path: "/home/user/project".to_string(),
                head_commit: Some("abcdefg".to_string()),
                config_hash: None,
                analysis: RepoAnalysis {
                    is_monorepo: false,
                    workspace_type: None,
                    projects: vec![],
                },
                analyzed_at: chrono::Utc::now(),
            }),
            ..Config::default()
        };
        let output = format_verbose_section(&cfg, 80);
        let p = plain(&output);
        assert!(
            p.contains("abcdefg"),
            "should show exactly 7-char commit: {p}"
        );
    }

    #[test]
    fn test_format_verbose_section_has_box_borders() {
        let cfg = Config::default();
        let output = format_verbose_section(&cfg, 80);
        let p = plain(&output);
        assert!(p.contains('╭'), "should contain top-left corner");
        assert!(p.contains('╰'), "should contain bottom-left corner");
        assert!(p.contains('│'), "should contain vertical border");
    }

    // ─── try_detect_auth ──────────────────────────────────────────

    #[test]
    fn test_try_detect_auth_no_binary() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::set("CLAUDE_BINARY", "/nonexistent/path/to/claude");
        let auth = try_detect_auth();
        assert!(auth.is_none(), "should return None when binary not found");
    }

    #[cfg(unix)]
    #[test]
    fn test_try_detect_auth_authenticated() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let script = dir.path().join("fake-claude");
        let content = "#!/bin/sh\nprintf '%s\\n' '{\"loggedIn\": true, \"authMethod\": \"claude.ai\", \"email\": \"user@example.com\"}'\nexit 0\n";
        std::fs::write(&script, content).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
        let auth = try_detect_auth();
        let auth = auth.expect("should return Some for authenticated binary");
        assert!(auth.authenticated, "should be authenticated");
        assert_eq!(auth.email.as_deref(), Some("user@example.com"));
    }

    #[cfg(unix)]
    #[test]
    fn test_try_detect_auth_not_authenticated() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let script = dir.path().join("fake-claude");
        let content = "#!/bin/sh\nprintf '%s\\n' '{\"loggedIn\": false}'\nexit 0\n";
        std::fs::write(&script, content).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
        let auth = try_detect_auth();
        let auth = auth.expect("should return Some even when not logged in");
        assert!(!auth.authenticated, "should not be authenticated");
        assert!(auth.email.is_none(), "should have no email");
    }

    #[cfg(unix)]
    #[test]
    fn test_try_detect_auth_subprocess_failure() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let script = dir.path().join("fake-claude");
        let content = "#!/bin/sh\nexit 1\n";
        std::fs::write(&script, content).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
        let auth = try_detect_auth();
        assert!(auth.is_none(), "should return None when subprocess fails");
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
        with_temp_config(|| {
            let result = load_and_run(&StatusArgs { verbose: false });
            assert!(result.is_ok());
        });
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
            &OutputFormat::ClaudeMd,
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
            &OutputFormat::ClaudeMd,
        );
    }
}
