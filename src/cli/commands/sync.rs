use crate::cli::args::SyncArgs;
use crate::generation::writer;

pub fn exec(_args: &SyncArgs) -> i32 {
    // TODO: implement sync pipeline (actual-cli-9ur)
    let _ = writer::write_files
        as fn(&std::path::Path, &[crate::tailoring::types::FileOutput]) -> Vec<writer::WriteResult>;
    eprintln!("not implemented yet");
    0
}
