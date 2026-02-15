use actual_cli::{run, Cli};
use clap::Parser;

fn main() {
    let cli = Cli::parse();
    let exit_code = run(cli);
    std::process::exit(exit_code);
}
