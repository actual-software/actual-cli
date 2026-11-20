use std::path::PathBuf;

use actual_cli::{handle_result, run, update_check, Cli, Command};
use clap::Parser;

fn main() {
    // First, before parsing, so `--help`, `--version` and a rejected
    // invocation report it too.
    #[cfg(debug_assertions)]
    write_debug_invocation_line();
    let cli = Cli::parse();
    init_logging(&cli);
    let update_notice = update_check::should_run_after(&cli.command);
    let exit_code = handle_result(run(cli));
    if update_notice && exit_code == 0 {
        update_check::after_command();
    }
    std::process::exit(exit_code);
}

/// Debug builds only: name which binary is running, its version, where it is
/// and the directory it was started in, as the first line on stderr.
///
/// Several `actual` binaries are often on one machine (a development build
/// and released ones), and a hook runs whichever `PATH` finds first. Release
/// builds compile none of this. It goes to stderr, never stdout, which carries
/// `--json` and `--claude-hook` output. A failed write is ignored, so a closed
/// stderr cannot stop the command.
#[cfg(debug_assertions)]
fn write_debug_invocation_line() {
    use std::io::Write;

    fn shown(path: std::io::Result<PathBuf>) -> String {
        path.map_or_else(
            |err| format!("<unknown: {err}>"),
            |p| p.display().to_string(),
        )
    }

    let _ = writeln!(
        std::io::stderr(),
        "actual debug build: name={} version={} path={} cwd={}",
        env!("CARGO_BIN_NAME"),
        env!("CARGO_PKG_VERSION"),
        shown(std::env::current_exe()),
        shown(std::env::current_dir()),
    );
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

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn,tokei=error"));

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
    // Disable ANSI escape codes so tracing output does not interleave raw
    // escape sequences with the pipeline's own plain-mode `eprintln!` output.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
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
