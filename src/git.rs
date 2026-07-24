use std::path::Path;
use std::time::Duration;

/// Read the `origin` remote URL for the repository containing `dir`.
///
/// This is deliberately best-effort: a missing repository or remote, a
/// non-zero exit, invalid output, or a timeout all return `None`. The child is
/// killed when the timeout drops it so callers never leave an orphaned `git`
/// process behind.
pub(crate) async fn origin_remote_url(dir: &Path, timeout: Duration) -> Option<String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["remote", "get-url", "origin"]);
    cmd.current_dir(dir);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    let child = cmd.spawn().ok()?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (!stdout.is_empty()).then_some(stdout)
        }
        _ => None,
    }
}
