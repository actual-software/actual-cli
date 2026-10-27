//! Reading and writing GitHub releases through the signer's own `gh` login.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{ensure, Context, Result};
use serde::Deserialize;

use crate::spawn::spawn_with_etxtbsy_retry;

/// A workflow run, with the fields that tie it to a tag.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Run {
    pub id: u64,
    pub event: String,
    pub path: String,
    pub head_branch: Option<String>,
    pub head_sha: String,
    pub status: String,
    pub conclusion: Option<String>,
}

pub trait ReleaseHost {
    /// The commit a tag points at.
    fn tag_commit(&self, repo: &str, tag: &str) -> Result<String>;
    /// The release workflow's runs for a tag push.
    fn release_runs(&self, repo: &str, workflow: &str, tag: &str) -> Result<Vec<Run>>;
    fn download_run_artifact(
        &self,
        repo: &str,
        run_id: u64,
        artifact: &str,
        dir: &Path,
    ) -> Result<()>;
    /// Downloads every asset of a release into `dir`.
    fn download_release(&self, repo: &str, tag: &str, dir: &Path) -> Result<()>;
    /// Uploads `file` to a release, replacing an asset of the same name.
    fn upload_release_asset(&self, repo: &str, tag: &str, file: &Path) -> Result<()>;
}

pub struct Gh {
    program: PathBuf,
}

#[derive(Deserialize)]
struct Runs {
    workflow_runs: Vec<Run>,
}

impl Gh {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    fn run<I, S>(&self, args: I) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<_> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
        let shown = args
            .iter()
            .map(|a| a.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        let output = spawn_with_etxtbsy_retry(|| Command::new(&self.program).args(&args).output())
            .with_context(|| {
                format!(
                    "running {} (is the GitHub CLI installed?)",
                    self.program.display()
                )
            })?;
        ensure!(
            output.status.success(),
            "`gh {shown}` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(output.stdout)
    }
}

impl ReleaseHost for Gh {
    fn tag_commit(&self, repo: &str, tag: &str) -> Result<String> {
        let out = self.run([
            "api".to_owned(),
            format!("repos/{repo}/commits/refs/tags/{tag}"),
            "--jq".to_owned(),
            ".sha".to_owned(),
        ])?;
        let sha = String::from_utf8(out)?.trim().to_owned();
        ensure!(
            sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
            "GitHub didn't return a commit for tag {tag} in {repo}"
        );
        Ok(sha)
    }

    fn release_runs(&self, repo: &str, workflow: &str, tag: &str) -> Result<Vec<Run>> {
        let out = self.run([
            "api".to_owned(),
            "--method".to_owned(),
            "GET".to_owned(),
            format!("repos/{repo}/actions/workflows/{workflow}/runs"),
            "-f".to_owned(),
            "event=push".to_owned(),
            "-f".to_owned(),
            format!("branch={tag}"),
            "-f".to_owned(),
            "per_page=100".to_owned(),
        ])?;
        let runs: Runs = serde_json::from_slice(&out).context("reading the workflow runs")?;
        Ok(runs.workflow_runs)
    }

    fn download_run_artifact(
        &self,
        repo: &str,
        run_id: u64,
        artifact: &str,
        dir: &Path,
    ) -> Result<()> {
        self.run([
            OsStr::new("run"),
            OsStr::new("download"),
            OsStr::new(&run_id.to_string()),
            OsStr::new("--repo"),
            OsStr::new(repo),
            OsStr::new("--name"),
            OsStr::new(artifact),
            OsStr::new("--dir"),
            dir.as_os_str(),
        ])?;
        Ok(())
    }

    fn download_release(&self, repo: &str, tag: &str, dir: &Path) -> Result<()> {
        self.run([
            OsStr::new("release"),
            OsStr::new("download"),
            OsStr::new(tag),
            OsStr::new("--repo"),
            OsStr::new(repo),
            OsStr::new("--dir"),
            dir.as_os_str(),
        ])?;
        Ok(())
    }

    fn upload_release_asset(&self, repo: &str, tag: &str, file: &Path) -> Result<()> {
        self.run([
            OsStr::new("release"),
            OsStr::new("upload"),
            OsStr::new(tag),
            file.as_os_str(),
            OsStr::new("--repo"),
            OsStr::new(repo),
            OsStr::new("--clobber"),
        ])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A `gh` that records its arguments and answers from `script`.
    fn fake_gh(script: &str) -> (tempfile::TempDir, Gh, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("calls");
        let path = dir.path().join("gh");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n{script}\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let gh = Gh::new(&path);
        (dir, gh, log)
    }

    fn calls(log: &Path) -> String {
        std::fs::read_to_string(log).unwrap()
    }

    const SHA: &str = "768473505d1c4bfd566cb0a5339e42b23ccfdd64";

    #[test]
    fn resolves_a_tag_to_its_commit() {
        let (_dir, gh, log) = fake_gh(&format!("echo {SHA}"));
        assert_eq!(gh.tag_commit("o/r", "v1.2.3").unwrap(), SHA);
        assert_eq!(
            calls(&log),
            "api repos/o/r/commits/refs/tags/v1.2.3 --jq .sha\n"
        );
    }

    #[test]
    fn refuses_a_tag_lookup_that_returns_no_commit() {
        let (_dir, gh, _log) = fake_gh("echo null");
        assert!(gh.tag_commit("o/r", "v1.2.3").is_err());
    }

    #[test]
    fn lists_the_release_runs_for_a_tag() {
        let body = format!(
            r#"{{"total_count":1,"workflow_runs":[{{"id":39998172755,"name":"Release","event":"push","path":".github/workflows/release.yml","head_branch":"v1.2.3","head_sha":"{SHA}","status":"completed","conclusion":"success"}}]}}"#
        );
        let (_dir, gh, log) = fake_gh(&format!("echo '{body}'"));
        let runs = gh.release_runs("o/r", "release.yml", "v1.2.3").unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, 39998172755);
        assert_eq!(runs[0].head_branch.as_deref(), Some("v1.2.3"));
        assert_eq!(
            calls(&log),
            "api --method GET repos/o/r/actions/workflows/release.yml/runs -f event=push -f branch=v1.2.3 -f per_page=100\n"
        );
    }

    #[test]
    fn downloads_and_uploads_with_the_expected_arguments() {
        let (dir, gh, log) = fake_gh("exit 0");
        let target = dir.path().join("out");
        gh.download_run_artifact("o/r", 42, "release-assets", &target)
            .unwrap();
        gh.download_release("o/m", "v1.2.3", &target).unwrap();
        gh.upload_release_asset("o/m", "v1.2.3", &target.join("SHA256SUMS.minisig"))
            .unwrap();
        let t = target.display();
        assert_eq!(
            calls(&log),
            format!(
                "run download 42 --repo o/r --name release-assets --dir {t}\n\
                 release download v1.2.3 --repo o/m --dir {t}\n\
                 release upload v1.2.3 {t}/SHA256SUMS.minisig --repo o/m --clobber\n"
            )
        );
    }

    #[test]
    fn surfaces_gh_errors() {
        let (_dir, gh, _log) = fake_gh("echo 'HTTP 404: Not Found' >&2; exit 1");
        let err = gh
            .download_release("o/m", "v9.9.9", Path::new("/tmp"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("gh release download v9.9.9") && err.contains("404"),
            "{err}"
        );
        let missing = Gh::new("/nonexistent/gh");
        assert!(missing
            .tag_commit("o/r", "v1")
            .unwrap_err()
            .to_string()
            .contains("GitHub CLI"));
    }

    #[test]
    fn refuses_unreadable_run_listings() {
        let (_dir, gh, _log) = fake_gh("echo '{}'");
        assert!(gh.release_runs("o/r", "release.yml", "v1").is_err());
    }
}
