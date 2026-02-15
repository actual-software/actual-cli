use clap::{Parser, Subcommand};

/// ADR-powered CLAUDE.md generator
#[derive(Parser, Debug)]
#[command(name = "actual", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Analyze repo, fetch ADRs, tailor and write CLAUDE.md
    Sync(SyncArgs),
    /// Check CLAUDE.md state
    Status(StatusArgs),
    /// Check Claude Code authentication status
    Auth,
    /// View or edit configuration
    Config(ConfigArgs),
}

/// Arguments for the `sync` command
#[derive(Parser, Debug)]
pub struct SyncArgs {
    /// Show summary of what would change without writing files
    #[arg(long)]
    pub dry_run: bool,

    /// With --dry-run, output the full rendered CLAUDE.md to stdout
    #[arg(long)]
    pub full: bool,

    /// Skip user confirmation
    #[arg(long)]
    pub force: bool,

    /// Clear remembered ADR rejections and show all ADRs again
    #[arg(long)]
    pub reset_rejections: bool,

    /// Target a specific sub-project in a monorepo (can be repeated)
    #[arg(long = "project", value_name = "PATH")]
    pub projects: Vec<String>,

    /// Override Claude Code model (e.g., "sonnet", "opus")
    #[arg(long)]
    pub model: Option<String>,

    /// Override the ADR bank API endpoint
    #[arg(long)]
    pub api_url: Option<String>,

    /// Show detailed progress and Claude Code output
    #[arg(long)]
    pub verbose: bool,

    /// Skip the local tailoring step; use ADRs as-is from the bank
    #[arg(long)]
    pub no_tailor: bool,
}

/// Arguments for the `status` command
#[derive(Parser, Debug)]
pub struct StatusArgs {
    /// Show full cached analysis and ADR counts
    #[arg(long)]
    pub verbose: bool,
}

/// Arguments for the `config` command
#[derive(Parser, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Print current configuration
    Show,
    /// Set a configuration value (supports dotpath, e.g., options.batch_size)
    Set(ConfigSetArgs),
    /// Print config file location
    Path,
}

/// Arguments for `config set`
#[derive(Parser, Debug)]
pub struct ConfigSetArgs {
    /// Configuration key (dotpath notation)
    pub key: String,
    /// Configuration value
    pub value: String,
}

fn run(cli: Cli) -> i32 {
    match cli.command {
        Command::Sync(_) => {
            eprintln!("not implemented yet");
            0
        }
        Command::Status(_) => {
            eprintln!("not implemented yet");
            0
        }
        Command::Auth => {
            eprintln!("not implemented yet");
            0
        }
        Command::Config(config) => match config.action {
            ConfigAction::Show => {
                eprintln!("not implemented yet");
                0
            }
            ConfigAction::Set(_) => {
                eprintln!("not implemented yet");
                0
            }
            ConfigAction::Path => {
                eprintln!("not implemented yet");
                0
            }
        },
    }
}

fn main() {
    let cli = Cli::parse();
    let exit_code = run(cli);
    std::process::exit(exit_code);
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_cli_parse_sync() {
        let cli = Cli::parse_from(["actual", "sync"]);
        assert!(matches!(cli.command, Command::Sync(_)));
    }

    #[test]
    fn test_cli_parse_sync_with_flags() {
        let cli = Cli::parse_from([
            "actual",
            "sync",
            "--dry-run",
            "--full",
            "--force",
            "--reset-rejections",
            "--verbose",
            "--no-tailor",
            "--model",
            "sonnet",
            "--api-url",
            "https://example.com",
            "--project",
            "apps/web",
            "--project",
            "apps/api",
        ]);
        if let Command::Sync(args) = cli.command {
            assert!(args.dry_run);
            assert!(args.full);
            assert!(args.force);
            assert!(args.reset_rejections);
            assert!(args.verbose);
            assert!(args.no_tailor);
            assert_eq!(args.model.as_deref(), Some("sonnet"));
            assert_eq!(args.api_url.as_deref(), Some("https://example.com"));
            assert_eq!(args.projects, vec!["apps/web", "apps/api"]);
        } else {
            panic!("Expected Sync command");
        }
    }

    #[test]
    fn test_cli_parse_status() {
        let cli = Cli::parse_from(["actual", "status"]);
        assert!(matches!(cli.command, Command::Status(_)));
    }

    #[test]
    fn test_cli_parse_status_verbose() {
        let cli = Cli::parse_from(["actual", "status", "--verbose"]);
        if let Command::Status(args) = cli.command {
            assert!(args.verbose);
        } else {
            panic!("Expected Status command");
        }
    }

    #[test]
    fn test_cli_parse_auth() {
        let cli = Cli::parse_from(["actual", "auth"]);
        assert!(matches!(cli.command, Command::Auth));
    }

    #[test]
    fn test_cli_parse_config_show() {
        let cli = Cli::parse_from(["actual", "config", "show"]);
        if let Command::Config(config) = cli.command {
            assert!(matches!(config.action, ConfigAction::Show));
        } else {
            panic!("Expected Config command");
        }
    }

    #[test]
    fn test_cli_parse_config_set() {
        let cli = Cli::parse_from(["actual", "config", "set", "options.batch_size", "15"]);
        if let Command::Config(config) = cli.command {
            if let ConfigAction::Set(args) = config.action {
                assert_eq!(args.key, "options.batch_size");
                assert_eq!(args.value, "15");
            } else {
                panic!("Expected Set action");
            }
        } else {
            panic!("Expected Config command");
        }
    }

    #[test]
    fn test_cli_parse_config_path() {
        let cli = Cli::parse_from(["actual", "config", "path"]);
        if let Command::Config(config) = cli.command {
            assert!(matches!(config.action, ConfigAction::Path));
        } else {
            panic!("Expected Config command");
        }
    }

    #[test]
    fn test_run_sync() {
        let cli = Cli::parse_from(["actual", "sync"]);
        assert_eq!(run(cli), 0);
    }

    #[test]
    fn test_run_status() {
        let cli = Cli::parse_from(["actual", "status"]);
        assert_eq!(run(cli), 0);
    }

    #[test]
    fn test_run_auth() {
        let cli = Cli::parse_from(["actual", "auth"]);
        assert_eq!(run(cli), 0);
    }

    #[test]
    fn test_run_config_show() {
        let cli = Cli::parse_from(["actual", "config", "show"]);
        assert_eq!(run(cli), 0);
    }

    #[test]
    fn test_run_config_set() {
        let cli = Cli::parse_from(["actual", "config", "set", "foo", "bar"]);
        assert_eq!(run(cli), 0);
    }

    #[test]
    fn test_run_config_path() {
        let cli = Cli::parse_from(["actual", "config", "path"]);
        assert_eq!(run(cli), 0);
    }
}
