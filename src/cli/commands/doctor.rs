//! `actual doctor` — diagnose local development environment health.
//!
//! Checks that required services are reachable, env files exist, and key
//! environment variables are set. Outputs copy-pasteable fix commands for
//! any issues found.

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cli::ui::panel::Panel;
use crate::cli::ui::term_size;
use crate::cli::ui::theme;
use crate::error::ActualError;

/// A single diagnostic check result.
struct Check {
    name: String,
    passed: bool,
    detail: String,
    fix: Option<String>,
}

impl Check {
    fn pass(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { name: name.into(), passed: true, detail: detail.into(), fix: None }
    }

    fn fail(name: impl Into<String>, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self { name: name.into(), passed: false, detail: detail.into(), fix: Some(fix.into()) }
    }
}

fn probe_tcp(host: &str, port: u16) -> bool {
    TcpStream::connect_timeout(
        &format!("{host}:{port}").parse().unwrap(),
        Duration::from_secs(2),
    )
    .is_ok()
}

fn check_service(name: &'static str, host: &str, port: u16, start_cmd: &str) -> Check {
    if probe_tcp(host, port) {
        Check::pass(name, format!("listening on {host}:{port}"))
    } else {
        Check::fail(
            name,
            format!("not reachable at {host}:{port}"),
            start_cmd.to_string(),
        )
    }
}

fn find_project_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let mut dir = cwd.as_path();
    loop {
        if dir.join("pnpm-workspace.yaml").exists() || dir.join("supabase").join("config.toml").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

fn check_env_file(root: &Path, rel_path: &str) -> Check {
    let full = root.join(rel_path);
    let example = root.join(format!("{rel_path}.example"));
    if full.exists() {
        Check::pass(rel_path, "exists")
    } else if example.exists() {
        Check::fail(
            "env file",
            format!("{rel_path} missing"),
            format!("cp {rel_path}.example {rel_path}"),
        )
    } else {
        Check::fail(
            "env file",
            format!("{rel_path} missing (no .example template found)"),
            format!("touch {rel_path}"),
        )
    }
}

fn check_env_var(name: &'static str, hint: &str) -> Check {
    match std::env::var(name) {
        Ok(val) if !val.is_empty() => Check::pass(name, "set"),
        _ => Check::fail(name, "not set", hint.to_string()),
    }
}

fn check_next_cache(root: &Path) -> Check {
    let next_dir = root.join("apps/actual/.next");
    if !next_dir.exists() {
        return Check::pass(".next cache", "clean (no cache dir)");
    }
    let manifest = next_dir.join("static/development/_buildManifest.js");
    if manifest.exists() {
        Check::pass(".next cache", "OK")
    } else {
        Check::fail(
            ".next cache",
            "corrupted — missing _buildManifest.js",
            "rm -rf apps/actual/.next",
        )
    }
}

fn check_python_venv(root: &Path) -> Check {
    let venv = root.join(".venv");
    if !venv.exists() {
        return Check::fail(
            "Python venv",
            ".venv directory missing",
            "pnpm python:build:dev",
        );
    }
    let lock = root.join("uv.lock");
    if !lock.exists() {
        return Check::fail(
            "Python venv",
            "uv.lock missing",
            "pnpm python:build:dev",
        );
    }
    Check::pass("Python venv", "exists")
}

pub fn exec() -> Result<(), ActualError> {
    let width = term_size::terminal_width();
    let root = find_project_root();

    // ── Services ──
    let services = vec![
        check_service("Supabase DB", "127.0.0.1", 54322, "pnpm supabase start"),
        check_service("Supabase API", "127.0.0.1", 54321, "pnpm supabase start"),
        check_service("Temporal", "127.0.0.1", 7233, "cd backend && docker compose -f docker-compose.dev.yaml up -d"),
        check_service("Redis", "127.0.0.1", 6380, "pnpm redis:start"),
        check_service("API Service", "127.0.0.1", 3002, "pnpm --filter api-service dev"),
        check_service("Next.js App", "127.0.0.1", 3000, "pnpm --filter actual-ai-app dev"),
    ];

    // ── Env files ──
    let env_checks: Vec<Check> = if let Some(ref r) = root {
        vec![
            check_env_file(r, "apps/actual/.env.local"),
            check_env_file(r, "apps/api-service/.env"),
            check_env_file(r, "backend/.env"),
        ]
    } else {
        vec![Check::fail("project root", "could not locate monorepo root", "cd into the sprintreview directory")]
    };

    // ── Project health ──
    let project_checks: Vec<Check> = if let Some(ref r) = root {
        vec![
            check_next_cache(r),
            check_python_venv(r),
        ]
    } else {
        vec![]
    };

    // ── Env vars ──
    let env_var_checks = vec![
        check_env_var("NEXT_PUBLIC_SUPABASE_URL", "Add to apps/actual/.env.local: NEXT_PUBLIC_SUPABASE_URL=http://127.0.0.1:54321"),
        check_env_var("TEMPORAL_ADDRESS", "Add to backend/.env: TEMPORAL_ADDRESS=localhost:7233"),
    ];

    // ── Render ──
    let all_checks: Vec<&[Check]> = vec![&services, &env_checks, &project_checks, &env_var_checks];
    let total: usize = all_checks.iter().map(|s| s.len()).sum();
    let failed: Vec<&Check> = all_checks.iter().flat_map(|s| s.iter()).filter(|c| !c.passed).collect();

    // Summary panel
    let mut panel = Panel::titled("Actual Doctor");

    // Services
    panel = panel.line("").line(&format!("  {} Services", theme::DIAMOND));
    for c in &services {
        let icon = if c.passed { theme::SUCCESS.to_string() } else { theme::ERROR.to_string() };
        let styled_detail = if c.passed {
            format!("{}", theme::success(&c.detail))
        } else {
            format!("{}", theme::error(&c.detail))
        };
        panel = panel.kv(&format!("  {icon} {}", c.name), &styled_detail);
    }

    // Env files
    panel = panel.separator().line(&format!("  {} Environment Files", theme::DIAMOND));
    for c in &env_checks {
        let icon = if c.passed { theme::SUCCESS.to_string() } else { theme::ERROR.to_string() };
        let styled_detail = if c.passed {
            format!("{}", theme::success(&c.detail))
        } else {
            format!("{}", theme::error(&c.detail))
        };
        panel = panel.kv(&format!("  {icon} {}", c.name), &styled_detail);
    }

    // Project health
    if !project_checks.is_empty() {
        panel = panel.separator().line(&format!("  {} Project Health", theme::DIAMOND));
        for c in &project_checks {
            let icon = if c.passed { theme::SUCCESS.to_string() } else { theme::ERROR.to_string() };
            let styled_detail = if c.passed {
                format!("{}", theme::success(&c.detail))
            } else {
                format!("{}", theme::error(&c.detail))
            };
            panel = panel.kv(&format!("  {icon} {}", c.name), &styled_detail);
        }
    }

    // Env vars
    panel = panel.separator().line(&format!("  {} Environment Variables", theme::DIAMOND));
    for c in &env_var_checks {
        let icon = if c.passed { theme::SUCCESS.to_string() } else { theme::ERROR.to_string() };
        let styled_detail = if c.passed {
            format!("{}", theme::success(&c.detail))
        } else {
            format!("{}", theme::error(&c.detail))
        };
        panel = panel.kv(&format!("  {icon} {}", c.name), &styled_detail);
    }

    // Summary line
    let passed = total - failed.len();
    let summary = if failed.is_empty() {
        format!("{} All {total} checks passed", theme::SUCCESS)
    } else {
        format!("{} {passed}/{total} passed, {} failed", theme::WARN, failed.len())
    };
    panel = panel.separator().line("").line(&format!("  {summary}"));

    // Fix commands
    if !failed.is_empty() {
        panel = panel.line("").line(&format!("  {} Fix commands (copy & paste):", theme::BULLET));
        panel = panel.line("");
        for (i, c) in failed.iter().enumerate() {
            if let Some(ref fix) = c.fix {
                panel = panel.line(&format!("  {}. {} — {}", i + 1, c.name, c.detail));
                panel = panel.line(&format!("     {}", theme::accent(fix)));
                panel = panel.line("");
            }
        }
    }

    panel = panel.line("");
    println!("{}", panel.render(width));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_tcp_fails_on_unused_port() {
        assert!(!probe_tcp("127.0.0.1", 19999));
    }

    #[test]
    fn check_pass_is_passed() {
        let c = Check::pass("test", "ok");
        assert!(c.passed);
        assert!(c.fix.is_none());
    }

    #[test]
    fn check_fail_has_fix() {
        let c = Check::fail("test", "bad", "fix it");
        assert!(!c.passed);
        assert_eq!(c.fix.as_deref(), Some("fix it"));
    }

    #[test]
    fn check_env_file_missing_returns_fail() {
        let dir = tempfile::tempdir().unwrap();
        let c = check_env_file(dir.path(), "nonexistent/.env");
        assert!(!c.passed);
    }

    #[test]
    fn check_env_file_present_returns_pass() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env.local"), "KEY=val").unwrap();
        let c = check_env_file(dir.path(), ".env.local");
        assert!(c.passed);
    }

    #[test]
    fn exec_runs_without_panic() {
        assert!(exec().is_ok());
    }
}
