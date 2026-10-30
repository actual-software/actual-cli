//! Signing a published release: check that both release pages carry what the
//! release run built, then sign SHA256SUMS and upload the signature to both.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};

use crate::github::{ReleaseHost, Run};
use crate::minisign::{self, Signer, TrustedKey};
use crate::sums;

pub const SOURCE_REPO: &str = "actual-software/actual-cli";
pub const MIRROR_REPO: &str = "actual-software/actual-releases";
pub const WORKFLOW: &str = "release.yml";
pub const ARTIFACT: &str = "release-assets";
pub const SIGNATURE_FILE: &str = "SHA256SUMS.minisig";

pub struct Options {
    pub tag: String,
    /// Picks one run when a tag has several.
    pub run_id: Option<u64>,
    pub upload: bool,
    /// Where SHA256SUMS.minisig is written.
    pub out_dir: PathBuf,
    /// Seconds since the Unix epoch, recorded in the trusted comment.
    pub timestamp: u64,
}

#[derive(Debug)]
pub struct Signed {
    pub run_id: u64,
    pub key_name: String,
    pub signature_path: PathBuf,
    pub uploaded_to: Vec<&'static str>,
}

/// The trusted comment on every release signature. People checking a download read the tag from it.
pub fn trusted_comment(timestamp: u64, tag: &str) -> String {
    format!("timestamp:{timestamp}\tfile:{}\ttag:{tag}", sums::FILE_NAME)
}

/// A release tag: `v`, then a version made of letters, digits, `.`, `-` and `+`.
pub fn check_tag(tag: &str) -> Result<()> {
    let version = tag.strip_prefix('v').unwrap_or_default();
    ensure!(
        version.starts_with(|c: char| c.is_ascii_digit())
            && version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+')),
        "`{tag}` isn't a release tag such as v1.2.3"
    );
    Ok(())
}

pub fn sign_release(
    host: &dyn ReleaseHost,
    signer: &dyn Signer,
    keys: &[TrustedKey],
    work_dir: &Path,
    options: &Options,
) -> Result<Signed> {
    let tag = options.tag.as_str();
    check_tag(tag)?;
    ensure!(
        !keys.is_empty(),
        "no trusted public keys to check the signature with"
    );

    let commit = host.tag_commit(SOURCE_REPO, tag)?;
    let run = pick_run(
        host.release_runs(SOURCE_REPO, WORKFLOW, tag)?,
        tag,
        &commit,
        options.run_id,
    )?;
    eprintln!(
        "{tag} is {commit}; its release run is {} ({}).",
        run.id,
        describe(&run)
    );

    let run_dir = work_dir.join("run");
    fs::create_dir_all(&run_dir)?;
    host.download_run_artifact(SOURCE_REPO, run.id, ARTIFACT, &run_dir)
        .with_context(|| format!("downloading the {ARTIFACT} artifact of run {}", run.id))?;
    let built = fs::read(run_dir.join(sums::FILE_NAME))
        .with_context(|| format!("the {ARTIFACT} artifact has no {}", sums::FILE_NAME))?;
    let entries = sums::parse(std::str::from_utf8(&built).context("SHA256SUMS isn't UTF-8")?)?;
    sums::check_dir(&run_dir, &entries, &[sums::FILE_NAME])
        .with_context(|| format!("checking the {ARTIFACT} artifact of run {}", run.id))?;

    for (index, repo) in [SOURCE_REPO, MIRROR_REPO].into_iter().enumerate() {
        let page_dir = work_dir.join(format!("page-{index}"));
        fs::create_dir_all(&page_dir)?;
        host.download_release(repo, tag, &page_dir)
            .with_context(|| format!("downloading the {tag} release from {repo}"))?;
        let published = fs::read(page_dir.join(sums::FILE_NAME))
            .with_context(|| format!("the {tag} release on {repo} has no {}", sums::FILE_NAME))?;
        ensure!(
            published == built,
            "the {} on the {tag} release in {repo} differs from the one run {} built",
            sums::FILE_NAME,
            run.id
        );
        sums::check_dir(&page_dir, &entries, &[sums::FILE_NAME, SIGNATURE_FILE])
            .with_context(|| format!("checking the {tag} release in {repo}"))?;
        if page_dir.join(SIGNATURE_FILE).exists() {
            eprintln!("{repo}: already has a {SIGNATURE_FILE}; the new one replaces it.");
        }
        eprintln!(
            "{repo}: {} matches run {}, and its {} tarballs match {}.",
            sums::FILE_NAME,
            run.id,
            entries.len(),
            sums::FILE_NAME
        );
    }

    let signature = minisign::sign(signer, &built, &trusted_comment(options.timestamp, tag))?;
    let (key, text) = minisign::encode_for_trusted_key(&signature, &built, keys)?;
    eprintln!(
        "Signed with the {} key (key id {}); the signature verifies.",
        key.name,
        key.key.key_id_hex()
    );

    fs::create_dir_all(&options.out_dir)?;
    let signature_path = options.out_dir.join(SIGNATURE_FILE);
    fs::write(&signature_path, &text)
        .with_context(|| format!("writing {}", signature_path.display()))?;

    let mut uploaded_to = Vec::new();
    if options.upload {
        for repo in [SOURCE_REPO, MIRROR_REPO] {
            host.upload_release_asset(repo, tag, &signature_path)
                .with_context(|| {
                    format!("uploading {SIGNATURE_FILE} to the {tag} release in {repo}")
                })?;
            eprintln!("{repo}: uploaded {SIGNATURE_FILE}.");
            uploaded_to.push(repo);
        }
    }
    Ok(Signed {
        run_id: run.id,
        key_name: key.name.clone(),
        signature_path,
        uploaded_to,
    })
}

/// The one push run of the release workflow that built `tag` at `commit`.
fn pick_run(runs: Vec<Run>, tag: &str, commit: &str, wanted: Option<u64>) -> Result<Run> {
    let mut matching: Vec<Run> = runs
        .into_iter()
        .filter(|run| {
            run.event == "push"
                && run.path == format!(".github/workflows/{WORKFLOW}")
                && run.head_branch.as_deref() == Some(tag)
                && run.head_sha == commit
        })
        .collect();
    if let Some(id) = wanted {
        matching.retain(|run| run.id == id);
        ensure!(
            !matching.is_empty(),
            "run {id} isn't a {WORKFLOW} run for the push of {tag} at {commit}"
        );
    }
    match matching.len() {
        0 => bail!("no {WORKFLOW} run built {tag} at {commit}"),
        1 => Ok(matching.remove(0)),
        _ => {
            let ids: Vec<String> = matching.iter().map(|run| run.id.to_string()).collect();
            bail!(
                "{WORKFLOW} ran more than once for {tag} ({}); pass --run with the one that published the release",
                ids.join(", ")
            )
        }
    }
}

fn describe(run: &Run) -> String {
    match &run.conclusion {
        Some(conclusion) => format!("{}, {conclusion}", run.status),
        None => run.status.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{release_key, FakeHost, LocalSigner, COMMIT, TAG};

    fn options(dir: &Path, upload: bool) -> Options {
        Options {
            tag: TAG.into(),
            run_id: None,
            upload,
            out_dir: dir.join("out"),
            timestamp: 1_792_800_000,
        }
    }

    fn sign_with(
        host: &FakeHost,
        signer: &LocalSigner,
        keys: &[TrustedKey],
        upload: bool,
    ) -> Result<Signed> {
        let work = tempfile::tempdir().unwrap();
        sign_release(
            host,
            signer,
            keys,
            work.path(),
            &options(&host.root(), upload),
        )
    }

    #[test]
    fn signs_a_consistent_release_and_uploads_to_both_pages() {
        let host = FakeHost::consistent_release();
        let signer = LocalSigner::generate();
        let keys = [release_key("primary", &signer, [1; 8])];
        let signed = sign_with(&host, &signer, &keys, true).unwrap();

        assert_eq!(signed.key_name, "primary");
        assert_eq!(signed.uploaded_to, [SOURCE_REPO, MIRROR_REPO]);
        assert_eq!(host.uploads(), [SOURCE_REPO, MIRROR_REPO]);
        let text = fs::read_to_string(&signed.signature_path).unwrap();
        let comment = minisign::verify(&text, &host.sums(), &keys[0].key).unwrap();
        assert_eq!(comment, "timestamp:1792800000\tfile:SHA256SUMS\ttag:v1.2.3");
    }

    #[test]
    fn without_upload_it_only_writes_the_signature() {
        let host = FakeHost::consistent_release();
        let signer = LocalSigner::generate();
        let keys = [release_key("primary", &signer, [1; 8])];
        let signed = sign_with(&host, &signer, &keys, false).unwrap();
        assert!(signed.signature_path.exists());
        assert!(signed.uploaded_to.is_empty());
        assert!(host.uploads().is_empty());
    }

    #[test]
    fn a_page_whose_sums_differ_from_the_run_is_not_signed() {
        let host = FakeHost::consistent_release();
        host.edit_page(MIRROR_REPO, "SHA256SUMS", |sums| sums.replacen('a', "b", 1));
        let signer = LocalSigner::generate();
        let keys = [release_key("primary", &signer, [1; 8])];
        let err = sign_with(&host, &signer, &keys, true).unwrap_err();
        assert!(
            format!("{err:#}").contains("differs from the one run"),
            "{err:#}"
        );
        assert_eq!(signer.calls(), 0);
        assert!(host.uploads().is_empty());
    }

    #[test]
    fn a_swapped_tarball_on_either_page_is_not_signed() {
        for repo in [SOURCE_REPO, MIRROR_REPO] {
            let host = FakeHost::consistent_release();
            host.edit_page(repo, "actual-linux-x64.tar.gz", |_| "tampered".into());
            let signer = LocalSigner::generate();
            let keys = [release_key("primary", &signer, [1; 8])];
            let err = sign_with(&host, &signer, &keys, true).unwrap_err();
            assert!(
                format!("{err:#}").contains("actual-linux-x64.tar.gz has SHA-256"),
                "{err:#}"
            );
            assert_eq!(signer.calls(), 0);
            assert!(host.uploads().is_empty());
        }
    }

    #[test]
    fn an_extra_or_missing_asset_is_not_signed() {
        let host = FakeHost::consistent_release();
        host.add_to_page(SOURCE_REPO, "actual-windows-x64.zip", "unlisted");
        let signer = LocalSigner::generate();
        let keys = [release_key("primary", &signer, [1; 8])];
        let err = format!("{:#}", sign_with(&host, &signer, &keys, true).unwrap_err());
        assert!(err.contains("actual-windows-x64.zip"), "{err}");

        let host = FakeHost::consistent_release();
        host.remove_from_page(MIRROR_REPO, "actual-darwin-x64.tar.gz");
        let err = format!("{:#}", sign_with(&host, &signer, &keys, true).unwrap_err());
        assert!(err.contains("actual-darwin-x64.tar.gz is listed"), "{err}");
        assert_eq!(signer.calls(), 0);
    }

    #[test]
    fn an_existing_signature_on_a_page_is_replaced() {
        let host = FakeHost::consistent_release();
        host.add_to_page(MIRROR_REPO, SIGNATURE_FILE, "an older signature");
        let signer = LocalSigner::generate();
        let keys = [release_key("primary", &signer, [1; 8])];
        sign_with(&host, &signer, &keys, true).unwrap();
        assert_eq!(host.uploads(), [SOURCE_REPO, MIRROR_REPO]);
    }

    #[test]
    fn a_tampered_run_artifact_is_not_signed() {
        let host = FakeHost::consistent_release();
        host.edit_artifact("actual-darwin-arm64.tar.gz", |_| "tampered".into());
        let signer = LocalSigner::generate();
        let keys = [release_key("primary", &signer, [1; 8])];
        let err = format!("{:#}", sign_with(&host, &signer, &keys, true).unwrap_err());
        assert!(
            err.contains("checking the release-assets artifact"),
            "{err}"
        );
    }

    #[test]
    fn a_key_the_repository_does_not_trust_uploads_nothing() {
        let host = FakeHost::consistent_release();
        let committed = LocalSigner::generate();
        let other = LocalSigner::generate();
        let keys = [release_key("primary", &committed, [1; 8])];
        let err = sign_with(&host, &other, &keys, true).unwrap_err();
        assert!(err.to_string().contains("doesn't verify"), "{err}");
        assert!(host.uploads().is_empty());
    }

    #[test]
    fn a_failed_upload_is_reported() {
        let host = FakeHost::consistent_release();
        host.fail_uploads_to(MIRROR_REPO);
        let signer = LocalSigner::generate();
        let keys = [release_key("primary", &signer, [1; 8])];
        let err = format!("{:#}", sign_with(&host, &signer, &keys, true).unwrap_err());
        assert!(err.contains("uploading SHA256SUMS.minisig to the v1.2.3 release in actual-software/actual-releases"), "{err}");
        assert_eq!(host.uploads(), [SOURCE_REPO]);
    }

    fn run(id: u64) -> Run {
        Run {
            id,
            event: "push".into(),
            path: ".github/workflows/release.yml".into(),
            head_branch: Some(TAG.into()),
            head_sha: COMMIT.into(),
            status: "completed".into(),
            conclusion: Some("success".into()),
        }
    }

    #[test]
    fn picks_the_one_run_that_built_the_tag() {
        let other_commit = Run {
            head_sha: "0".repeat(40),
            ..run(1)
        };
        let other_workflow = Run {
            path: ".github/workflows/npm-publish.yml".into(),
            ..run(2)
        };
        let not_a_push = Run {
            event: "workflow_dispatch".into(),
            ..run(3)
        };
        let other_tag = Run {
            head_branch: Some("v1.2.4".into()),
            ..run(4)
        };
        let runs = vec![other_commit, other_workflow, not_a_push, other_tag, run(5)];
        assert_eq!(pick_run(runs, TAG, COMMIT, None).unwrap().id, 5);
    }

    #[test]
    fn needs_a_choice_when_a_tag_ran_twice() {
        let err = pick_run(vec![run(5), run(6)], TAG, COMMIT, None).unwrap_err();
        assert!(err.to_string().contains("5, 6"), "{err}");
        assert_eq!(
            pick_run(vec![run(5), run(6)], TAG, COMMIT, Some(6))
                .unwrap()
                .id,
            6
        );
        assert!(pick_run(vec![run(5)], TAG, COMMIT, Some(7)).is_err());
        assert!(pick_run(vec![], TAG, COMMIT, None).is_err());
    }

    #[test]
    fn needs_a_release_tag() {
        for good in ["v1.2.3", "v0.10.0", "v1.2.3-rc.1", "v1.2.3+build.5"] {
            check_tag(good).unwrap();
        }
        for bad in ["1.2.3", "v", "vx", "v1.2.3\tx", "v1/2", "main", "v1.2.3 "] {
            assert!(check_tag(bad).is_err(), "accepted {bad:?}");
        }
        let host = FakeHost::consistent_release();
        let signer = LocalSigner::generate();
        let keys = [release_key("primary", &signer, [1; 8])];
        let work = tempfile::tempdir().unwrap();
        let mut bad = options(work.path(), true);
        bad.tag = "main".into();
        assert!(sign_release(&host, &signer, &keys, work.path(), &bad).is_err());
        assert!(sign_release(
            &host,
            &signer,
            &[],
            work.path(),
            &options(work.path(), true)
        )
        .is_err());
    }

    #[test]
    fn describes_runs_with_and_without_a_conclusion() {
        assert_eq!(describe(&run(1)), "completed, success");
        let running = Run {
            status: "in_progress".into(),
            conclusion: None,
            ..run(1)
        };
        assert_eq!(describe(&running), "in_progress");
    }
}
