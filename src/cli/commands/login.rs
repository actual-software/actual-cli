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
    if args.device {
        return exec_device(base_url);
    }
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

/// Browserless device-authorization login (`actual login --device`, RFC 8628).
/// Prints a short code + URL for the user to approve on any device, polls for
/// the session, then persists it exactly like the browser flow.
fn exec_device(base_url: String) -> Result<(), ActualError> {
    let cfg = OAuthConfig::new_device(base_url);
    let creds = build_runtime()?.block_on(oauth::device_login(&cfg, &print_device_prompt))?;
    store::save(&creds)?;
    print_success(&creds);
    Ok(())
}

/// Show the user where to approve the device request. Written to **stderr** so
/// the answer channel (stdout) stays clean for scripting. Never prints token
/// material — only the verification URL and the short user code.
fn print_device_prompt(device: &oauth::DeviceCodeResponse) {
    eprintln!();
    eprintln!("{} To sign in, open this URL on any device:", theme::BULLET);
    eprintln!("    {}", theme::accent(&device.verification_uri));
    eprintln!("  and enter the code:");
    eprintln!("    {}", theme::accent_secondary(&device.user_code));
    if let Some(complete) = &device.verification_uri_complete {
        eprintln!();
        eprintln!("  Or open this URL to skip entering the code:");
        eprintln!("    {}", theme::muted(complete));
    }
    eprintln!();
    eprintln!(
        "  {}",
        theme::muted(format!(
            "Waiting for approval… (code expires in {}s)",
            device.expires_in
        ))
    );
    eprintln!();
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
            device: false,
        };
        let err = exec(&args).unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("HTTPS")),
            "expected a clear HTTPS config error, got: {err:?}"
        );
    }

    #[test]
    fn test_exec_device_rejects_non_https_url() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::remove("ACTUAL_AUTH_URL");
        // The device flow applies the same HTTPS/loopback guard before any
        // network work, so a non-HTTPS override is rejected up front.
        let args = LoginArgs {
            org: None,
            api_url: Some("http://example.com".to_string()),
            no_browser: true,
            device: true,
        };
        let err = exec(&args).unwrap_err();
        assert!(
            matches!(err, ActualError::ConfigError(ref m) if m.contains("HTTPS")),
            "expected a clear HTTPS config error, got: {err:?}"
        );
    }

    #[test]
    fn test_print_device_prompt_does_not_panic() {
        // With the optional complete URL present…
        let with_complete = oauth::DeviceCodeResponse {
            device_code: "d".to_string(),
            user_code: "WXYZ-1234".to_string(),
            verification_uri: "https://auth.example/device".to_string(),
            verification_uri_complete: Some(
                "https://auth.example/device?user_code=WXYZ-1234".to_string(),
            ),
            interval: Some(5),
            expires_in: 300,
        };
        print_device_prompt(&with_complete);

        // …and without it (exercises the `None` branch).
        let without_complete = oauth::DeviceCodeResponse {
            device_code: "d".to_string(),
            user_code: "WXYZ-1234".to_string(),
            verification_uri: "https://auth.example/device".to_string(),
            verification_uri_complete: None,
            interval: None,
            expires_in: 120,
        };
        print_device_prompt(&without_complete);
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
