use std::io::Write;
use std::process::{Command, Stdio};

/// Copy text to the system clipboard using platform-specific commands.
///
/// Returns `true` if the text was successfully copied, `false` otherwise.
/// Silently returns `false` if no clipboard command is available.
pub fn copy_to_clipboard(text: &str) -> bool {
    copy_to_clipboard_impl(text)
}

#[cfg(target_os = "macos")]
fn copy_to_clipboard_impl(text: &str) -> bool {
    pipe_to_command("pbcopy", &[], text)
}

#[cfg(target_os = "windows")]
fn copy_to_clipboard_impl(text: &str) -> bool {
    pipe_to_command("clip", &[], text)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn copy_to_clipboard_impl(text: &str) -> bool {
    pipe_to_command("xclip", &["-selection", "clipboard"], text)
        || pipe_to_command("xsel", &["--clipboard", "--input"], text)
}

/// Pipe `text` to a command's stdin. Returns `true` on success.
fn pipe_to_command(cmd: &str, args: &[&str], text: &str) -> bool {
    let child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    match child {
        Ok(mut child) => {
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(text.as_bytes());
            }
            child.wait().map(|s| s.success()).unwrap_or(false)
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_to_nonexistent_command_returns_false() {
        assert!(!pipe_to_command(
            "nonexistent_clipboard_cmd_12345",
            &[],
            "test"
        ));
    }

    #[test]
    fn pipe_to_true_returns_true() {
        // `true` is a standard Unix command that exits 0
        assert!(pipe_to_command("true", &[], "hello"));
    }
}
