use serde::Deserialize;

/// Parsed output of `claude auth status --json`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAuthStatus {
    pub logged_in: bool,
    #[serde(default)]
    pub auth_method: Option<String>,
    #[serde(default)]
    pub api_provider: Option<String>,
    #[serde(default)]
    pub api_key_source: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub org_id: Option<String>,
    #[serde(default)]
    pub org_name: Option<String>,
    #[serde(default)]
    pub subscription_type: Option<String>,
}

impl ClaudeAuthStatus {
    /// Whether the authentication is sufficient to use Claude Code.
    pub fn is_usable(&self) -> bool {
        self.logged_in
    }

    /// Minimal authenticated status for use when the CLI does not provide
    /// structured JSON output (e.g. older versions without `--json` support).
    pub(crate) fn minimal_authenticated() -> Self {
        Self {
            logged_in: true,
            auth_method: None,
            api_provider: None,
            api_key_source: None,
            email: None,
            org_id: None,
            org_name: None,
            subscription_type: None,
        }
    }

    /// Human-readable description of the authentication method.
    pub fn display_method(&self) -> &str {
        match self.auth_method.as_deref() {
            Some("claude.ai") => "Claude subscription (OAuth)",
            _ if self.api_key_source.is_some() => "API key (ANTHROPIC_API_KEY)",
            _ => "Unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_oauth_auth() {
        let json = r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "user@example.com"}"#;
        let status: ClaudeAuthStatus = serde_json::from_str(json).unwrap();
        assert!(status.logged_in);
        assert_eq!(status.auth_method.as_deref(), Some("claude.ai"));
        assert_eq!(status.email.as_deref(), Some("user@example.com"));
        assert!(status.api_key_source.is_none());
    }

    #[test]
    fn test_deserialize_api_key_auth() {
        let json = r#"{"loggedIn": true, "apiKeySource": "ANTHROPIC_API_KEY"}"#;
        let status: ClaudeAuthStatus = serde_json::from_str(json).unwrap();
        assert!(status.logged_in);
        assert_eq!(status.api_key_source.as_deref(), Some("ANTHROPIC_API_KEY"));
        assert!(status.auth_method.is_none());
    }

    #[test]
    fn test_deserialize_unauthenticated() {
        let json = r#"{"loggedIn": false}"#;
        let status: ClaudeAuthStatus = serde_json::from_str(json).unwrap();
        assert!(!status.logged_in);
        assert!(status.auth_method.is_none());
        assert!(status.email.is_none());
        assert!(status.api_key_source.is_none());
    }

    #[test]
    fn test_display_method_oauth() {
        let json = r#"{"loggedIn": true, "authMethod": "claude.ai"}"#;
        let status: ClaudeAuthStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status.display_method(), "Claude subscription (OAuth)");
    }

    #[test]
    fn test_display_method_api_key() {
        let json = r#"{"loggedIn": true, "apiKeySource": "ANTHROPIC_API_KEY"}"#;
        let status: ClaudeAuthStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status.display_method(), "API key (ANTHROPIC_API_KEY)");
    }

    #[test]
    fn test_display_method_unknown() {
        let json = r#"{"loggedIn": false}"#;
        let status: ClaudeAuthStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status.display_method(), "Unknown");
    }

    #[test]
    fn test_is_usable_when_logged_in() {
        let json = r#"{"loggedIn": true, "authMethod": "claude.ai"}"#;
        let status: ClaudeAuthStatus = serde_json::from_str(json).unwrap();
        assert!(status.is_usable());
    }

    #[test]
    fn test_is_usable_when_not_logged_in() {
        let json = r#"{"loggedIn": false}"#;
        let status: ClaudeAuthStatus = serde_json::from_str(json).unwrap();
        assert!(!status.is_usable());
    }
}
