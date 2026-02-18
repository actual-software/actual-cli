use actual_cli::{handle_result, run, Cli};
use clap::Parser;

fn main() {
    let cli = Cli::parse();
    let exit_code = handle_result(run(cli));
    std::process::exit(exit_code);
}
