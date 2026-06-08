//! `actual login` — sign in to the Actual AI platform via browser OAuth.
//!
//! Distinct from the `auth` command (which checks the underlying coding-agent
//! runner). This establishes the user's Actual AI platform identity and
//! persists OAuth credentials locally via [`crate::auth::store`].

use crate::auth::oauth::{self, OAuthConfig, DEFAULT_LOGIN_TIMEOUT};
use crate::auth::store::{self, StoredCredentials};
use crate::cli::args::LoginArgs;
use crate::cli::ui::theme;
use crate::error::ActualError;

pub fn exec(args: &LoginArgs) -> Result<(), ActualError> {
    let base_url = oauth::resolve_auth_url(args.api_url.as_deref())?;
    let cfg = OAuthConfig::new(base_url);

    let creds = build_runtime()?.block_on(oauth::login(
        &cfg,
        args.org.clone(),
        !args.no_browser,
        &|url| {
            let _ = open::that(url);
        },
        DEFAULT_LOGIN_TIMEOUT,
    ))?;

    store::save(&creds)?;
    print_success(&creds);
    Ok(())
}

/// Build a single-threaded tokio runtime so the sync CLI dispatch path can
/// drive the async login flow (mirrors `commands::auth`).
fn build_runtime() -> Result<tokio::runtime::Runtime, ActualError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ActualError::InternalError(format!("failed to build tokio runtime: {e}")))
}

/// Print a success summary. **Never prints token material** — only the
/// non-sensitive identity fields.
fn print_success(creds: &StoredCredentials) {
    println!(
        "{} Signed in to {}",
        theme::success(&theme::SUCCESS),
        theme::success("Actual AI")
    );
    println!("  {:<14} {}", "Organization:", creds.organization_id);
    if let Some(email) = &creds.email {
        println!("  {:<14} {}", "Account:", email);
    } else if let Some(subject) = &creds.subject {
        println!("  {:<14} {}", "Account:", subject);
    }
    println!("  {:<14} {}", "Member:", creds.member_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};

    #[test]
    fn test_exec_rejects_non_https_url() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::remove("ACTUAL_AUTH_URL");
        // resolve_auth_url now defaults to the prod HTTPS endpoint, so a missing
        // URL no longer errors. A non-HTTPS, non-loopback --api-url is still
        // rejected with a clear error before any browser/network work, so
        // credentials are never sent in clear text.
        let args = LoginArgs {
            org: None,
            api_url: Some("http://example.com".to_string()),
            no_browser: true,
        };
        let err = exec(&args).unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("HTTPS")),
            "expected a clear HTTPS config error, got: {err:?}"
        );
    }

    #[test]
    fn test_print_success_does_not_panic_with_minimal_creds() {
        let creds = StoredCredentials {
            access_token: "secret".to_string(),
            refresh_token: "secret".to_string(),
            token_type: "Bearer".to_string(),
            expires_at: None,
            scope: None,
            organization_id: "org-1".to_string(),
            member_id: "member-1".to_string(),
            email: None,
            subject: None,
            auth_url: Some("http://localhost:4000".to_string()),
        };
        print_success(&creds);
    }

    #[test]
    fn test_build_runtime_succeeds() {
        assert!(build_runtime().is_ok());
    }
}
