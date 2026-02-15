use console::style;

use crate::cli::args::{ConfigAction, ConfigArgs};
use crate::config;
use crate::error::ActualError;

pub fn exec(args: &ConfigArgs) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{} {}", style("Error:").red().bold(), e);
            e.exit_code()
        }
    }
}

fn run(args: &ConfigArgs) -> Result<(), ActualError> {
    match &args.action {
        ConfigAction::Path => {
            let path = config::paths::config_path()?;
            println!("{}", path.display());
            Ok(())
        }
        ConfigAction::Show => {
            let cfg = config::paths::load()?;
            let yaml = serde_yaml::to_string(&cfg).map_err(|e| {
                ActualError::ConfigError(format!("failed to serialize config: {e}"))
            })?;
            print!("{yaml}");
            Ok(())
        }
        ConfigAction::Set(args) => {
            let mut cfg = config::paths::load()?;
            config::dotpath::set(&mut cfg, &args.key, &args.value)?;
            config::paths::save(&cfg)?;
            println!("Set {} = {}", args.key, args.value);
            Ok(())
        }
    }
}
