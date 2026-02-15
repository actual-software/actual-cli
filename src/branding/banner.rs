use console::Style;

/// ASCII art banner displayed on CLI startup.
pub const BANNER: &str = r#"
    ╔═══════════════════════════════════╗
    ║           actual CLI              ║
    ║     ADR-powered CLAUDE.md         ║
    ╚═══════════════════════════════════╝
"#;

/// Print the ASCII banner to stderr.
///
/// If `quiet` is true, the banner is suppressed entirely.
pub fn print_banner(quiet: bool) {
    if quiet {
        return;
    }

    let frame_style = Style::new().cyan();
    let text_style = Style::new().bold();

    for line in BANNER.trim_matches('\n').lines() {
        // Style frame characters (box-drawing) differently from inner text.
        // Split each line into frame and content portions.
        if let Some(inner) = line.strip_prefix("    ║").and_then(|s| s.strip_suffix('║')) {
            // Content line: frame in cyan, inner text in bold
            eprint!("    {}", frame_style.apply_to("║"));
            eprint!("{}", text_style.apply_to(inner));
            eprintln!("{}", frame_style.apply_to("║"));
        } else {
            // Pure frame line (top/bottom borders)
            eprintln!("{}", frame_style.apply_to(line));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_is_non_empty_and_contains_actual() {
        assert!(!BANNER.is_empty(), "BANNER should not be empty");
        assert!(
            BANNER.contains("actual"),
            "BANNER should contain the word 'actual'"
        );
    }

    #[test]
    fn print_banner_quiet_produces_no_panic() {
        // Calling with quiet=true should return immediately without error.
        print_banner(true);
    }
}
