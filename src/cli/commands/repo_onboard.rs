//! `actual repo onboard <url>` — onboard a public repository via the CLI.
//!
//! Authenticates using ephemeral credentials (access token only, no refresh
//! token) and calls `POST /repos/onboard-public`.

use chrono::Utc;

use crate::api::client::ActualApiClient;
use crate::auth::{ephemeral, pending};
use crate::auth::ephemeral::EphemeralCredentials;
use crate::auth::oauth;
use crate::auth::pending::PendingOperation;
use crate::cli::args::RepoOnboardArgs;
use crate::error::ActualError;

const DEFAULT_AUTH_URL: &str = "https://app.actual.ai";

fn resolve_auth_url(args: &RepoOnboardArgs) -> String {
    args.auth_url
        .clone()
        .or_else(|| std::env::var("ACTUAL_AUTH_URL").ok())
        .unwrap_or_else(|| DEFAULT_AUTH_URL.to_string())
}

fn resolve_api_url(args: &RepoOnboardArgs) -> String {
    args.api_url
        .clone()
        .or_else(|| std::env::var("ACTUAL_API_URL").ok())
        .unwrap_or_else(|| crate::api::client::DEFAULT_API_URL.to_string())
}

pub fn exec(args: &RepoOnboardArgs) -> Result<(), ActualError> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| ActualError::ConfigError(format!("Failed to create async runtime: {e}")))?;
    rt.block_on(exec_async(args))
}

async fn exec_async(args: &RepoOnboardArgs) -> Result<(), ActualError> {
    let git_url = &args.url;

    // Try existing ephemeral credentials first
    if let Some(creds) = ephemeral::load()? {
        if !creds.is_expired(Utc::now()) {
            return call_onboard_api(args, &creds, git_url).await;
        }
        ephemeral::clear()?;
    }

    // No valid credentials — save the pending operation and authenticate
    let pending_op = PendingOperation::RepoOnboard {
        url: git_url.clone(),
    };
    pending::save(&pending_op)?;

    let creds = authenticate(args).await?;
    ephemeral::save(&creds)?;
    pending::clear()?;

    call_onboard_api(args, &creds, git_url).await
}

async fn authenticate(args: &RepoOnboardArgs) -> Result<EphemeralCredentials, ActualError> {
    let auth_url = resolve_auth_url(args);

    println!("Actual needs to connect your account.");
    if args.device {
        println!("Use the device code flow to authenticate.");
    } else {
        println!("A browser window will open — enter your email and click the verification link to continue.");
    }

    let login_result = oauth::login_ephemeral(&auth_url, args.device).await?;
    Ok(login_result)
}

async fn call_onboard_api(
    args: &RepoOnboardArgs,
    creds: &EphemeralCredentials,
    git_url: &str,
) -> Result<(), ActualError> {
    let api_url = resolve_api_url(args);
    let client = ActualApiClient::new(&api_url)?.with_bearer(&creds.access_token);

    match client.onboard_public_repo(git_url).await {
        Ok(response) => {
            match response.status.as_str() {
                "queued" => {
                    println!("Repository queued for analysis: {}", response.repository_id);
                }
                "exists" => {
                    println!("Repository already onboarded: {}", response.repository_id);
                }
                other => {
                    println!("Repository {}: {}", other, response.repository_id);
                }
            }
            Ok(())
        }
        Err(ActualError::NotLoggedIn) => {
            ephemeral::clear()?;
            Err(ActualError::NotLoggedIn)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_auth_url_uses_default() {
        let args = RepoOnboardArgs {
            url: "https://github.com/acme/widgets".to_string(),
            device: false,
            api_url: None,
            auth_url: None,
        };
        assert_eq!(resolve_auth_url(&args), "https://app.actual.ai");
    }

    #[test]
    fn resolve_auth_url_uses_arg_override() {
        let args = RepoOnboardArgs {
            url: "https://github.com/acme/widgets".to_string(),
            device: false,
            api_url: None,
            auth_url: Some("https://custom.auth.ai".to_string()),
        };
        assert_eq!(resolve_auth_url(&args), "https://custom.auth.ai");
    }
}
