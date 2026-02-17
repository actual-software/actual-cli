//! Header bar rendered below the banner gradient.
//!
//! Shows version info on the left and authentication status on the right,
//! with a horizontal separator line beneath.
//!
//! ```text
//!  actual v0.4.2                                    ✔ authenticated (user@example.com)
//! ╶──────────────────────────────────────────────────────────────────────────────────────╴
//! ```
//!
//! All output goes to **stderr** (same stream as the banner), so styled
//! objects use `.for_stderr()`.

use std::fmt::Write;

use super::theme;

/// Authentication display info for the header bar.
pub struct AuthDisplay {
    /// Whether the user is authenticated.
    pub authenticated: bool,
    /// Email address, if known.
    pub email: Option<String>,
}

/// Render a header bar with version and optional auth status.
///
/// - Left side: `actual v{version}` in accent colour
/// - Right side: auth status (green checkmark / red cross / dim dash)
/// - Separator: `╶` + `─` repeated + `╴`
///
/// `width` is the available terminal width (capped by the caller).
pub fn render_header_bar(width: usize, version: &str, auth: Option<&AuthDisplay>) -> String {
    let mut buf = String::new();

    // ── Content line ──

    let left = format!(" actual v{version}");
    let right = format_auth_status(auth);

    let left_len = console::measure_text_width(&left);
    let right_len = console::measure_text_width(&right);

    // Pad between left and right; if the line is too narrow, just concatenate.
    let gap = width.saturating_sub(left_len + right_len);

    let _ = write!(
        buf,
        "{}{}{}",
        theme::accent(&left).for_stderr(),
        " ".repeat(gap),
        right,
    );
    buf.push('\n');

    // ── Separator line ──

    let inner = width.saturating_sub(2);
    let separator = format!("╶{}╴", "─".repeat(inner));
    let _ = write!(buf, "{}", theme::muted(&separator).for_stderr());
    buf.push('\n');

    buf
}

/// Format the right-hand auth status string (already styled for stderr).
fn format_auth_status(auth: Option<&AuthDisplay>) -> String {
    match auth {
        Some(a) if a.authenticated => {
            let label = match &a.email {
                Some(email) => format!("{} authenticated ({email})", theme::SUCCESS),
                None => format!("{} authenticated", theme::SUCCESS),
            };
            format!("{} ", theme::success(&label).for_stderr())
        }
        Some(_) => {
            let label = format!("{} not authenticated", theme::ERROR);
            format!("{} ", theme::error(&label).for_stderr())
        }
        None => {
            let label = "─ auth unknown".to_string();
            format!("{} ", theme::muted(&label).for_stderr())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_authenticated_with_email() {
        let auth = AuthDisplay {
            authenticated: true,
            email: Some("user@example.com".to_string()),
        };
        let output = render_header_bar(80, "0.4.2", Some(&auth));
        let plain = console::strip_ansi_codes(&output).to_string();
        assert!(
            plain.contains("actual v0.4.2"),
            "expected version on left: {plain}"
        );
        assert!(
            plain.contains("authenticated"),
            "expected 'authenticated' on right: {plain}"
        );
        assert!(
            plain.contains("user@example.com"),
            "expected email on right: {plain}"
        );
    }

    #[test]
    fn test_authenticated_without_email() {
        let auth = AuthDisplay {
            authenticated: true,
            email: None,
        };
        let output = render_header_bar(80, "1.0.0", Some(&auth));
        let plain = console::strip_ansi_codes(&output).to_string();
        assert!(
            plain.contains("authenticated"),
            "expected 'authenticated': {plain}"
        );
        assert!(
            !plain.contains('('),
            "should not contain parentheses without email: {plain}"
        );
    }

    #[test]
    fn test_not_authenticated() {
        let auth = AuthDisplay {
            authenticated: false,
            email: None,
        };
        let output = render_header_bar(80, "0.1.0", Some(&auth));
        let plain = console::strip_ansi_codes(&output).to_string();
        assert!(
            plain.contains("not authenticated"),
            "expected 'not authenticated': {plain}"
        );
    }

    #[test]
    fn test_unknown_auth() {
        let output = render_header_bar(80, "0.1.0", None);
        let plain = console::strip_ansi_codes(&output).to_string();
        assert!(
            plain.contains("auth unknown"),
            "expected 'auth unknown': {plain}"
        );
    }

    #[test]
    fn test_width_80_separator_length() {
        let output = render_header_bar(80, "0.1.0", None);
        let plain = console::strip_ansi_codes(&output).to_string();
        let sep_line = plain.lines().nth(1).expect("expected separator line");
        // Separator is: ╶ + 78 × ─ + ╴  (each char is one display column)
        // The Unicode chars ╶, ─, ╴ are each 3 bytes but 1 column
        assert!(
            sep_line.contains('╶') && sep_line.contains('╴'),
            "expected separator endpoints: {sep_line}"
        );
    }

    #[test]
    fn test_width_120_separator_longer() {
        let output = render_header_bar(120, "0.1.0", None);
        let plain = console::strip_ansi_codes(&output).to_string();
        let sep_line = plain.lines().nth(1).expect("expected separator line");
        // Count the ─ characters
        let dash_count = sep_line.chars().filter(|c| *c == '─').count();
        assert_eq!(
            dash_count, 118,
            "expected 118 dashes for width 120, got {dash_count}"
        );
    }

    #[test]
    fn test_narrow_width_does_not_panic() {
        // Width 10 is very narrow — should not panic
        let output = render_header_bar(10, "0.1.0", None);
        assert!(!output.is_empty(), "should produce some output");
    }

    #[test]
    fn test_zero_width_does_not_panic() {
        let output = render_header_bar(0, "0.1.0", None);
        assert!(
            !output.is_empty(),
            "should produce some output even at width 0"
        );
    }

    #[test]
    fn test_version_string_present() {
        let output = render_header_bar(
            80,
            "99.88.77",
            Some(&AuthDisplay {
                authenticated: true,
                email: Some("test@test.com".to_string()),
            }),
        );
        let plain = console::strip_ansi_codes(&output).to_string();
        assert!(
            plain.contains("actual v99.88.77"),
            "expected version string: {plain}"
        );
    }

    #[test]
    fn test_output_has_two_lines() {
        let output = render_header_bar(80, "0.1.0", None);
        let plain = console::strip_ansi_codes(&output).to_string();
        let line_count = plain.lines().count();
        assert_eq!(
            line_count, 2,
            "expected 2 lines (content + separator), got {line_count}"
        );
    }
}
