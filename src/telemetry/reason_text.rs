//! The typed reason an `other` override sends as
//! `actual.override.reason_text`: secret-looking spans become `[redacted]`
//! first, then the text is cut to [`MAX_CHARS`]. Stripping runs on the whole
//! text so a cut can't leave a secret too short to recognise.

use std::sync::LazyLock;

use regex::{Captures, Regex};

/// The most characters `actual.override.reason_text` carries.
pub(crate) const MAX_CHARS: usize = 280;

const REDACTED: &str = "[redacted]";

/// What a match becomes.
type Replace = fn(&Captures) -> String;

fn redacted(_: &Captures) -> String {
    REDACTED.to_string()
}

/// Each pattern with what its matches become, applied in order.
static PATTERNS: LazyLock<Vec<(Regex, Replace)>> = LazyLock::new(|| {
    let patterns: [(&str, Replace); 7] = [
        // A PEM private key, to its END line or the end of the text.
        (
            r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|\z)",
            redacted,
        ),
        // Credentials in a URL: scheme://user:password@host, the user
        // possibly empty.
        (r"://[^/\s:@]*:[^/\s@]+@", |_| format!("://{REDACTED}@")),
        // Tokens with a well-known prefix: Actual, GitHub, Anthropic, OpenAI,
        // Slack, GitLab, AWS access keys, Google API keys.
        (
            r"\b(?:actl_pat_|gh[pousr]_|github_pat_|sk-ant-|sk-|xox[abeprs]-|glpat-|AKIA|ASIA|AIza)[A-Za-z0-9_\-]{8,}",
            redacted,
        ),
        // A JSON Web Token.
        (
            r"\beyJ[A-Za-z0-9_\-]{4,}\.[A-Za-z0-9_\-]{4,}\.[A-Za-z0-9_\-]*",
            redacted,
        ),
        // An Authorization header value, when it looks like a credential, so
        // prose such as "Basic configuration" is kept.
        (
            r"(?i)\b(bearer|basic)(\s+)([A-Za-z0-9._~+/=\-]{8,})",
            |caps| {
                let value = &caps[3];
                let token = value.trim_end_matches('.');
                if is_credential_shaped(token) {
                    format!(
                        "{}{}{REDACTED}{}",
                        &caps[1],
                        &caps[2],
                        &value[token.len()..]
                    )
                } else {
                    caps[0].to_string()
                }
            },
        ),
        // A value assigned to a key whose name says it is secret, the key
        // possibly quoted, as in JSON or YAML.
        (
            r#"(?i)\b([A-Za-z0-9_\-]*(?:password|passwd|pwd|secret|token|api[_\-]?key|access[_\-]?key|private[_\-]?key|secret[_\-]?key|credentials?|auth))(["']?\s*[:=]\s*)("[^"]*"|'[^']*'|\S+)"#,
            |caps| format!("{}{}{REDACTED}", &caps[1], &caps[2]),
        ),
        // 32 or more hex digits.
        (r"\b[0-9A-Fa-f]{32,}\b", redacted),
    ];
    patterns
        .into_iter()
        .map(|(pattern, replace)| (Regex::new(pattern).unwrap(), replace))
        .collect()
});

/// Whether an Authorization value holds a digit, `=` or `.`, or an
/// upper-case letter after its first character.
fn is_credential_shaped(value: &str) -> bool {
    value.contains(|c: char| c.is_ascii_digit() || c == '=' || c == '.')
        || value.chars().skip(1).any(|c| c.is_ascii_uppercase())
}

/// A run of 32 or more base64 or base64url characters; only one that mixes
/// digits, upper and lower case is redacted, so a long path or word is kept.
static BASE64: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/_\-]{32,}={0,2}").unwrap());

/// `text` fit to send, or `None` when nothing is left once it is trimmed.
pub(crate) fn shareable(text: &str) -> Option<String> {
    let mut stripped = text.trim().to_string();
    for (pattern, replace) in PATTERNS.iter() {
        stripped = pattern.replace_all(&stripped, *replace).into_owned();
    }
    let stripped = BASE64.replace_all(&stripped, |caps: &Captures| {
        let run = &caps[0];
        let mixed = run.chars().any(|c| c.is_ascii_digit())
            && run.chars().any(|c| c.is_ascii_uppercase())
            && run.chars().any(|c| c.is_ascii_lowercase());
        if mixed {
            REDACTED.to_string()
        } else {
            run.to_string()
        }
    });
    let cut: String = stripped.chars().take(MAX_CHARS).collect();
    let cut = cut.trim_end();
    (!cut.is_empty()).then(|| cut.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fake credentials, assembled at run time so no scanner reads the
    /// source as holding a real one. `xY7` is not hex, so the hex rule can't
    /// stand in for the one under test.
    fn fake(prefix: &str, body_len: usize) -> String {
        format!("{prefix}{}", "xY7".repeat(body_len.div_ceil(3)))
    }

    #[test]
    fn test_every_pattern_compiles() {
        assert_eq!(PATTERNS.len(), 7);
        LazyLock::force(&BASE64);
    }

    #[test]
    fn test_plain_text_is_kept() {
        assert_eq!(
            shareable("  the rule is about the old auth flow, see src/cli/commands/override_telemetry.rs  ")
                .as_deref(),
            Some("the rule is about the old auth flow, see src/cli/commands/override_telemetry.rs")
        );
    }

    #[test]
    fn test_nothing_left_is_none() {
        assert_eq!(shareable("   "), None);
        assert_eq!(shareable(""), None);
    }

    #[test]
    fn test_prefixed_tokens_are_redacted() {
        for prefix in [
            "actl_pat_",
            "ghp_",
            "github_pat_",
            "sk-ant-",
            "sk-",
            "xoxb-",
            "glpat-",
            "AKIA",
            "AIza",
        ] {
            let token = fake(prefix, 24);
            assert_eq!(
                shareable(&format!("used {token} to test")).as_deref(),
                Some("used [redacted] to test"),
                "{prefix}"
            );
        }
    }

    #[test]
    fn test_a_jwt_is_redacted() {
        let jwt = format!("eyJ{}.eyJ{}.{}", fake("", 12), fake("", 20), fake("", 16));
        assert_eq!(
            shareable(&format!("jwt {jwt} here")).as_deref(),
            Some("jwt [redacted] here")
        );
    }

    #[test]
    fn test_an_authorization_value_keeps_its_scheme() {
        assert_eq!(
            shareable("sent Bearer abcdefgh.ijkl and Basic dXNlcjpwYXNz").as_deref(),
            Some("sent Bearer [redacted] and Basic [redacted]")
        );
    }

    #[test]
    fn test_prose_after_bearer_or_basic_is_kept() {
        for prose in [
            "Basic configuration is intentional here",
            "the bearer requirement is handled upstream.",
            "Basic Configuration only.",
        ] {
            assert_eq!(shareable(prose).as_deref(), Some(prose));
        }
        assert_eq!(
            shareable("it was Bearer abc123defg.").as_deref(),
            Some("it was Bearer [redacted].")
        );
        assert_eq!(
            shareable("token: Bearer abc123defg").as_deref(),
            Some("token: [redacted] [redacted]")
        );
    }

    #[test]
    fn test_a_secret_key_keeps_its_name_and_loses_its_value() {
        assert_eq!(
            shareable(r#"password=hunter2 DB_PASSWORD: "two words" api-key='k' token = abc"#)
                .as_deref(),
            Some(
                r#"password=[redacted] DB_PASSWORD: [redacted] api-key=[redacted] token = [redacted]"#
            )
        );
    }

    #[test]
    fn test_a_quoted_key_loses_its_value() {
        assert_eq!(
            shareable(r#"{"password": "hunter2", "token":"abc", "user": "me"}"#).as_deref(),
            Some(r#"{"password": [redacted], "token":[redacted], "user": "me"}"#)
        );
        assert_eq!(
            shareable("'client_secret': hunter2").as_deref(),
            Some("'client_secret': [redacted]")
        );
    }

    #[test]
    fn test_a_secret_key_name_loses_its_value() {
        assert_eq!(
            shareable("SECRET_KEY=hunter2 and secret-key: hunter3").as_deref(),
            Some("SECRET_KEY=[redacted] and secret-key: [redacted]")
        );
    }

    #[test]
    fn test_url_credentials_are_redacted() {
        assert_eq!(
            shareable("cloned https://bot:s3cr3t@example.com/repo.git").as_deref(),
            Some("cloned https://[redacted]@example.com/repo.git")
        );
    }

    #[test]
    fn test_url_credentials_with_no_user_are_redacted() {
        assert_eq!(
            shareable("cache at redis://:hunter2@cache:6379/0").as_deref(),
            Some("cache at redis://[redacted]@cache:6379/0")
        );
    }

    #[test]
    fn test_a_private_key_is_redacted_ended_or_not() {
        let marker = |edge: &str, kind: &str| format!("-----{edge} {kind}PRIVATE KEY-----");
        let body = fake("", 12);
        let key = format!(
            "{}\n{body}\n{}",
            marker("BEGIN", "RSA "),
            marker("END", "RSA ")
        );
        assert_eq!(
            shareable(&format!("key {key} done")).as_deref(),
            Some("key [redacted] done")
        );
        assert_eq!(
            shareable(&format!("key {}\n{body}", marker("BEGIN", ""))).as_deref(),
            Some("key [redacted]")
        );
    }

    #[test]
    fn test_long_hex_is_redacted_and_short_hex_kept() {
        let long = "0123456789abcdef".repeat(2);
        assert_eq!(
            shareable(&format!("hash {long} and abc123")).as_deref(),
            Some("hash [redacted] and abc123")
        );
    }

    #[test]
    fn test_long_mixed_base64_is_redacted_and_a_long_word_kept() {
        let blob = fake("", 40);
        let word = "x".repeat(40);
        assert_eq!(
            shareable(&format!("{blob} {word}")),
            Some(format!("[redacted] {word}"))
        );
    }

    #[test]
    fn test_text_is_cut_to_the_limit() {
        let long = "word ".repeat(100);
        let cut = shareable(&long).unwrap();
        assert_eq!(cut.chars().count(), MAX_CHARS - 1);
        assert!(long.starts_with(&cut));
        // Characters, not bytes.
        assert_eq!(
            shareable(&"é".repeat(300)).unwrap().chars().count(),
            MAX_CHARS
        );
    }

    /// A token that straddles the limit is stripped before the cut, so no
    /// part of it is sent.
    #[test]
    fn test_a_token_at_the_limit_is_stripped_before_the_cut() {
        let token = fake("ghp_", 36);
        let text = format!("{} {token}", "x".repeat(MAX_CHARS - 10));
        let sent = shareable(&text).unwrap();
        assert!(!sent.contains("ghp_"), "{sent}");
        assert!(!sent.contains("xY7"), "{sent}");
    }
}
