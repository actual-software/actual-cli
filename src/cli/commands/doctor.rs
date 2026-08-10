//! `actual doctor` — verify that this repository is set up correctly for
//! Actual AI. Checks CLI authentication, observer hooks, repo onboarding,
//! and API connectivity. Outputs copy-pasteable fix commands for any issues.

use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;

use crate::api::client::DEFAULT_API_URL;
use crate::auth::store;
use crate::cli::ui::panel::Panel;
use crate::cli::ui::term_size;
use crate::cli::ui::theme;
use crate::error::ActualError;
use crate::observe::setup;

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

fn check_auth() -> Vec<Check> {
    let creds = match store::load() {
        Ok(Some(c)) => c,
        Ok(None) => {
            return vec![Check::fail("Login", "not signed in", "actual login")];
        }
        Err(_) => {
            return vec![Check::fail(
                "Login",
                "credentials file corrupted",
                "actual logout && actual login",
            )];
        }
    };

    let mut checks = Vec::new();

    let now = Utc::now();
    if creds.is_expired(now) {
        checks.push(Check::fail("Login", "access token expired", "actual login"));
    } else {
        let identity = creds.email.as_deref().unwrap_or(&creds.member_id);
        let org_short = if creds.organization_id.len() > 8 {
            &creds.organization_id[..8]
        } else {
            &creds.organization_id
        };
        checks.push(Check::pass("Login", format!("{identity} (org {org_short}...)")));
    }

    if let Some(scope) = &creds.scope {
        let has_observe = scope.contains("observe:events");
        let has_adr = scope.contains("adr:query");
        if has_observe && has_adr {
            checks.push(Check::pass("Permissions", "observe + query scopes granted"));
        } else if !has_observe {
            checks.push(Check::fail(
                "Permissions",
                "missing observe:events scope",
                "actual logout && actual login",
            ));
        } else {
            checks.push(Check::fail(
                "Permissions",
                "missing adr:query scope",
                "actual logout && actual login",
            ));
        }
    }

    checks
}

fn check_git_repo() -> Vec<Check> {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(_) => return vec![Check::fail("Git repo", "cannot read current directory", "cd into your project")],
    };

    let git_dir = find_git_root(&cwd);
    if git_dir.is_none() {
        return vec![Check::fail(
            "Git repo",
            "not inside a git repository",
            "cd into a git repository",
        )];
    }

    let origin = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output();

    match origin {
        Ok(out) if out.status.success() => {
            let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let short = if url.len() > 50 {
                format!("...{}", &url[url.len() - 47..])
            } else {
                url
            };
            vec![Check::pass("Git repo", short)]
        }
        _ => vec![Check::fail(
            "Git remote",
            "no 'origin' remote configured",
            "git remote add origin <url>",
        )],
    }
}

fn check_observer_hooks() -> Vec<Check> {
    let project_settings = setup::default_settings_path();
    let home_settings = dirs::home_dir().map(|h| h.join(".claude").join("settings.json"));

    let project_has_hooks = has_observer_hooks(&project_settings);
    let global_has_hooks = home_settings.as_ref().map_or(false, |p| has_observer_hooks(p));

    if project_has_hooks {
        vec![Check::pass("Observer hooks", "installed (project)")]
    } else if global_has_hooks {
        vec![Check::pass("Observer hooks", "installed (global)")]
    } else {
        vec![Check::fail(
            "Observer hooks",
            "not installed — advisor won't receive events",
            "actual observe setup",
        )]
    }
}

fn has_observer_hooks(path: &PathBuf) -> bool {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    content.contains("actual observe pre-tool")
}

fn check_api_reachable() -> Check {
    let api_url = std::env::var("ACTUAL_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string());

    let display_url = if api_url == DEFAULT_API_URL {
        "production".to_string()
    } else {
        api_url.clone()
    };

    let parsed = match url::Url::parse(&api_url) {
        Ok(u) => u,
        Err(_) => return Check::fail("API", format!("invalid URL: {api_url}"), "check ACTUAL_API_URL"),
    };

    let host = parsed.host_str().unwrap_or("localhost");
    let port = parsed.port().unwrap_or(if parsed.scheme() == "https" { 443 } else { 80 });

    let addr_str = format!("{host}:{port}");
    let reachable = std::net::ToSocketAddrs::to_socket_addrs(&addr_str)
        .ok()
        .and_then(|mut addrs| addrs.next())
        .map(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(3)).is_ok())
        .unwrap_or(false);

    if reachable {
        Check::pass("API", format!("reachable ({display_url})"))
    } else {
        Check::fail(
            "API",
            format!("not reachable ({display_url})"),
            if api_url == DEFAULT_API_URL {
                "check your internet connection".to_string()
            } else {
                format!("start the API service or check ACTUAL_API_URL={api_url}")
            },
        )
    }
}

fn find_git_root(start: &std::path::Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

fn render_section(panel: Panel, title: &str, checks: &[Check]) -> Panel {
    let mut p = panel.separator().line(&format!("  {} {title}", theme::DIAMOND));
    for c in checks {
        let icon = if c.passed { theme::SUCCESS.to_string() } else { theme::ERROR.to_string() };
        let styled = if c.passed {
            theme::success(&c.detail).to_string()
        } else {
            theme::error(&c.detail).to_string()
        };
        p = p.kv(&format!("  {icon} {}", c.name), &styled);
    }
    p
}

pub fn exec() -> Result<(), ActualError> {
    let width = term_size::terminal_width();

    let auth_checks = check_auth();
    let repo_checks = check_git_repo();
    let hook_checks = check_observer_hooks();
    let api_check = vec![check_api_reachable()];

    let all: Vec<&[Check]> = vec![&auth_checks, &repo_checks, &hook_checks, &api_check];
    let total: usize = all.iter().map(|s| s.len()).sum();
    let failed: Vec<&Check> = all.iter().flat_map(|s| s.iter()).filter(|c| !c.passed).collect();

    let mut panel = Panel::titled("Actual Doctor");
    panel = panel.line("");

    panel = render_section(panel, "Authentication", &auth_checks);
    panel = render_section(panel, "Repository", &repo_checks);
    panel = render_section(panel, "Observer", &hook_checks);
    panel = render_section(panel, "Connectivity", &api_check);

    let passed = total - failed.len();
    let summary = if failed.is_empty() {
        format!("{} All {total} checks passed", theme::SUCCESS)
    } else {
        format!("{} {passed}/{total} passed, {} failed", theme::WARN, failed.len())
    };
    panel = panel.separator().line("").line(&format!("  {summary}"));

    if !failed.is_empty() {
        panel = panel.line("").line(&format!("  {} Fix commands:", theme::BULLET));
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
    fn has_observer_hooks_false_for_missing_file() {
        let p = PathBuf::from("/nonexistent/settings.json");
        assert!(!has_observer_hooks(&p));
    }

    #[test]
    fn has_observer_hooks_true_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"hooks":{"PreToolUse":[{"hooks":[{"command":"actual observe pre-tool"}]}]}}"#).unwrap();
        assert!(has_observer_hooks(&path.to_path_buf()));
    }

    #[test]
    fn has_observer_hooks_false_when_no_observe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"hooks":{"PreToolUse":[{"hooks":[{"command":"other command"}]}]}}"#).unwrap();
        assert!(!has_observer_hooks(&path.to_path_buf()));
    }

    #[test]
    fn exec_runs_without_panic() {
        assert!(exec().is_ok());
    }
}
