use std::path::PathBuf;

use actual_cli::{handle_result, run, Cli, Command};
use clap::Parser;

fn main() {
    let cli = Cli::parse();
    init_logging(&cli);
    let exit_code = handle_result(run(cli));
    std::process::exit(exit_code);
}

/// Initialize the tracing subscriber.
///
/// In TUI mode (`actual adr-bot` without `--no-tui`), traces are written to a log
/// file (`~/.actual/logs/actual-cli.log`) so they do not interfere with the
/// alternate-screen terminal UI.  In plain/non-TUI mode, traces go to stderr.
///
/// The default filter level is WARN; override with the `RUST_LOG` environment
/// variable (e.g. `RUST_LOG=debug actual adr-bot`).
fn init_logging(cli: &Cli) {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));

    // Detect TUI mode: adr-bot command with --no-tui not set.
    let is_tui = matches!(&cli.command, Command::AdrBot(a) if !a.no_tui);

    if is_tui {
        // In TUI mode, write to a log file so tracing output does not appear
        // on the normal screen buffer when the alternate screen exits.
        if let Some(log_path) = tui_log_path() {
            if let Ok(file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_writer(std::sync::Mutex::new(file))
                    .with_ansi(false)
                    .init();
                return;
            }
        }
    }

    // Non-TUI mode (or log file creation failed): write to stderr.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// Resolve the path for the TUI-mode tracing log file.
///
/// Creates the log directory if it does not exist.  Returns `None` if the
/// home directory cannot be determined or the directory cannot be created.
fn tui_log_path() -> Option<PathBuf> {
    let log_dir = dirs::home_dir()?.join(".actual").join("logs");
    std::fs::create_dir_all(&log_dir).ok()?;
    Some(log_dir.join("actual-cli.log"))
}
