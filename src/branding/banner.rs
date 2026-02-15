use console::Style;

/// ASCII art banner based on the actual logo.
///
/// The logo is a stylized "a" shaped like a bird with speed lines and
/// circuit nodes extending from the right, representing AI-powered
/// code management.
pub const BANNER: &str = r#"
           ▄███████▄
      ▄▄  ███▀   ▀███    ━━━━━━━
    ▄████ ██   ▄█▄  ██  ●━━━━━━━
     ▀▀▀  ███ ▀█▀  ██▀ ●━━━━━━━
          ▀████▄▄███▀
       ███▀▀▀████▀     actual
        ▀██▄▄█▀▀
          ▀▀▀
"#;

/// Print the ASCII banner to stderr.
///
/// If `quiet` is true, the banner is suppressed entirely.
pub fn print_banner(quiet: bool) {
    if quiet {
        return;
    }

    let logo_style = Style::new().cyan().bold();
    let lines_style = Style::new().blue();
    let label_style = Style::new().white().bold();

    for line in BANNER.trim_matches('\n').lines() {
        // Lines containing circuit nodes get split styling
        if let Some(idx) = line.find('●') {
            // Circuit node + speed lines in blue, logo part in cyan
            eprint!("{}", logo_style.apply_to(&line[..idx]));
            eprintln!("{}", lines_style.apply_to(&line[idx..]));
        } else if let Some(idx) = line.find("actual") {
            // The "actual" label line: logo part in cyan, label in bold white
            eprint!("{}", logo_style.apply_to(&line[..idx]));
            eprintln!("{}", label_style.apply_to(&line[idx..]));
        } else if let Some(idx) = line.find("━━━") {
            // Top speed line (no circuit node)
            eprint!("{}", logo_style.apply_to(&line[..idx]));
            eprintln!("{}", lines_style.apply_to(&line[idx..]));
        } else {
            eprintln!("{}", logo_style.apply_to(line));
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
    fn print_banner_quiet_produces_no_output() {
        // Calling with quiet=true should return immediately without error.
        print_banner(true);
    }

    #[test]
    fn print_banner_non_quiet_produces_no_panic() {
        // Calling with quiet=false should print the banner to stderr without error.
        // This exercises all the styling and printing code paths.
        print_banner(false);
    }

    #[test]
    fn banner_contains_logo_elements() {
        // The banner should contain block characters for the logo shape
        assert!(
            BANNER.contains('█'),
            "BANNER should contain block characters for the logo"
        );
        // Speed lines
        assert!(
            BANNER.contains('━'),
            "BANNER should contain speed line characters"
        );
        // Circuit nodes
        assert!(
            BANNER.contains('●'),
            "BANNER should contain circuit node characters"
        );
    }
}
