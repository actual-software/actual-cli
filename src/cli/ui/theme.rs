use console::{style, Emoji, StyledObject};

// ─── Emoji constants with ASCII fallbacks ───

/// Success indicator (checkmark or "ok" on unsupported terminals).
pub static SUCCESS: Emoji<'_, '_> = Emoji("✔", "ok");

/// Error indicator (cross or "err" on unsupported terminals).
pub static ERROR: Emoji<'_, '_> = Emoji("✖", "err");

/// Warning indicator (triangle or "!!" on unsupported terminals).
pub static WARN: Emoji<'_, '_> = Emoji("⚠", "!!");

/// Diamond bullet for section headers.
pub static DIAMOND: Emoji<'_, '_> = Emoji("◆", "*");

/// Arrow bullet for list items.
pub static BULLET: Emoji<'_, '_> = Emoji("▸", ">");

// ─── Semantic styling helpers ───

/// Style a value as a success (green).
pub fn success<D: std::fmt::Display>(val: D) -> StyledObject<D> {
    style(val).green()
}

/// Style a value as an error (red).
pub fn error<D: std::fmt::Display>(val: D) -> StyledObject<D> {
    style(val).red()
}

/// Style a value as a warning (yellow).
pub fn warning<D: std::fmt::Display>(val: D) -> StyledObject<D> {
    style(val).yellow()
}

/// Style a value as a heading (bold).
pub fn heading<D: std::fmt::Display>(val: D) -> StyledObject<D> {
    style(val).bold()
}

/// Style a value as muted/secondary (dim).
pub fn muted<D: std::fmt::Display>(val: D) -> StyledObject<D> {
    style(val).dim()
}

/// Style a value as a hint/command (cyan).
pub fn hint<D: std::fmt::Display>(val: D) -> StyledObject<D> {
    style(val).cyan()
}

/// Format the "Error:" prefix in red bold.
pub fn error_prefix() -> StyledObject<&'static str> {
    style("Error:").red().bold()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_success_contains_checkmark() {
        let rendered = format!("{SUCCESS}");
        // On terminals with emoji support, contains the checkmark;
        // on others, the ASCII fallback "ok".
        assert!(
            rendered.contains('✔') || rendered.contains("ok"),
            "expected checkmark or 'ok' in: {rendered}"
        );
    }

    #[test]
    fn emoji_error_contains_cross() {
        let rendered = format!("{ERROR}");
        assert!(
            rendered.contains('✖') || rendered.contains("err"),
            "expected cross or 'err' in: {rendered}"
        );
    }

    #[test]
    fn emoji_warn_contains_triangle() {
        let rendered = format!("{WARN}");
        assert!(
            rendered.contains('⚠') || rendered.contains("!!"),
            "expected triangle or '!!' in: {rendered}"
        );
    }

    #[test]
    fn emoji_diamond_renders() {
        let rendered = format!("{DIAMOND}");
        assert!(
            rendered.contains('◆') || rendered.contains('*'),
            "expected diamond or '*' in: {rendered}"
        );
    }

    #[test]
    fn emoji_bullet_renders() {
        let rendered = format!("{BULLET}");
        assert!(
            rendered.contains('▸') || rendered.contains('>'),
            "expected bullet or '>' in: {rendered}"
        );
    }

    #[test]
    fn style_helpers_produce_non_empty_output() {
        assert!(!format!("{}", success("ok")).is_empty());
        assert!(!format!("{}", error("fail")).is_empty());
        assert!(!format!("{}", warning("warn")).is_empty());
        assert!(!format!("{}", heading("title")).is_empty());
        assert!(!format!("{}", muted("dim")).is_empty());
        assert!(!format!("{}", hint("cmd")).is_empty());
    }

    #[test]
    fn error_prefix_contains_error() {
        let rendered = format!("{}", error_prefix());
        assert!(
            rendered.contains("Error:"),
            "expected 'Error:' in: {rendered}"
        );
    }
}
