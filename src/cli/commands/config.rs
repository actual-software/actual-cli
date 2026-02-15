use crate::cli::args::{ConfigAction, ConfigArgs};

pub fn exec(args: &ConfigArgs) -> i32 {
    match &args.action {
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
    }
}
