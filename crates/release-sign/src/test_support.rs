//! A local Ed25519 key in place of Cloud KMS, and a release host backed by directories.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{bail, Result};
use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};

use crate::github::{ReleaseHost, Run};
use crate::minisign::{PublicKey, Signer, TrustedKey};
use crate::release::{ARTIFACT, MIRROR_REPO, SOURCE_REPO};

pub const TAG: &str = "v1.2.3";
pub const COMMIT: &str = "768473505d1c4bfd566cb0a5339e42b23ccfdd64";
pub const RUN_ID: u64 = 39998172755;

pub struct LocalSigner {
    pair: Ed25519KeyPair,
    calls: AtomicUsize,
}

impl LocalSigner {
    pub fn generate() -> Self {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        Self {
            pair: Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap(),
            calls: AtomicUsize::new(0),
        }
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.pair.public_key().as_ref().try_into().unwrap()
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Signer for LocalSigner {
    fn sign(&self, message: &[u8]) -> Result<[u8; 64]> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.pair.sign(message).as_ref().try_into()?)
    }
}

pub fn trusted(name: &str, signer: &LocalSigner, key_id: [u8; 8]) -> TrustedKey {
    TrustedKey {
        name: name.into(),
        key: PublicKey {
            key_id,
            key: signer.public_key(),
        },
    }
}

/// A release run's artifact and the two release pages, as directories under one temp dir.
pub struct FakeHost {
    root: tempfile::TempDir,
    uploads: RefCell<Vec<String>>,
    failing_repo: RefCell<Option<String>>,
}

const TARBALLS: [&str; 4] = [
    "actual-darwin-arm64.tar.gz",
    "actual-darwin-x64.tar.gz",
    "actual-linux-x64.tar.gz",
    "actual-linux-arm64.tar.gz",
];

impl FakeHost {
    /// Both pages carry exactly what the run built.
    pub fn consistent_release() -> Self {
        let host = Self {
            root: tempfile::tempdir().unwrap(),
            uploads: RefCell::new(Vec::new()),
            failing_repo: RefCell::new(None),
        };
        let mut sums = String::new();
        for name in TARBALLS {
            let content = format!("the {name} binary");
            sums.push_str(&format!(
                "{:x}  {name}\n",
                Sha256::digest(content.as_bytes())
            ));
            for dir in host.all_dirs() {
                fs::create_dir_all(&dir).unwrap();
                fs::write(dir.join(name), &content).unwrap();
            }
        }
        for dir in host.all_dirs() {
            fs::write(dir.join("SHA256SUMS"), &sums).unwrap();
        }
        host
    }

    pub fn root(&self) -> PathBuf {
        self.root.path().to_path_buf()
    }

    pub fn sums(&self) -> Vec<u8> {
        fs::read(self.artifact_dir().join("SHA256SUMS")).unwrap()
    }

    pub fn uploads(&self) -> Vec<String> {
        self.uploads.borrow().clone()
    }

    pub fn edit_page(&self, repo: &str, name: &str, edit: impl Fn(String) -> String) {
        edit_file(&self.page_dir(repo).join(name), edit);
    }

    pub fn edit_artifact(&self, name: &str, edit: impl Fn(String) -> String) {
        edit_file(&self.artifact_dir().join(name), edit);
    }

    pub fn add_to_page(&self, repo: &str, name: &str, content: &str) {
        fs::write(self.page_dir(repo).join(name), content).unwrap();
    }

    pub fn remove_from_page(&self, repo: &str, name: &str) {
        fs::remove_file(self.page_dir(repo).join(name)).unwrap();
    }

    pub fn fail_uploads_to(&self, repo: &str) {
        *self.failing_repo.borrow_mut() = Some(repo.into());
    }

    fn artifact_dir(&self) -> PathBuf {
        self.root.path().join("artifact")
    }

    fn page_dir(&self, repo: &str) -> PathBuf {
        self.root.path().join("pages").join(repo.replace('/', "_"))
    }

    fn all_dirs(&self) -> [PathBuf; 3] {
        [
            self.artifact_dir(),
            self.page_dir(SOURCE_REPO),
            self.page_dir(MIRROR_REPO),
        ]
    }
}

fn edit_file(path: &Path, edit: impl Fn(String) -> String) {
    let text = fs::read_to_string(path).unwrap();
    fs::write(path, edit(text)).unwrap();
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    for item in fs::read_dir(from)? {
        let item = item?;
        fs::copy(item.path(), to.join(item.file_name()))?;
    }
    Ok(())
}

impl ReleaseHost for FakeHost {
    fn tag_commit(&self, repo: &str, tag: &str) -> Result<String> {
        assert_eq!((repo, tag), (SOURCE_REPO, TAG));
        Ok(COMMIT.into())
    }

    fn release_runs(&self, repo: &str, workflow: &str, tag: &str) -> Result<Vec<Run>> {
        assert_eq!((repo, workflow, tag), (SOURCE_REPO, "release.yml", TAG));
        Ok(vec![Run {
            id: RUN_ID,
            event: "push".into(),
            path: ".github/workflows/release.yml".into(),
            head_branch: Some(TAG.into()),
            head_sha: COMMIT.into(),
            status: "completed".into(),
            conclusion: Some("success".into()),
        }])
    }

    fn download_run_artifact(
        &self,
        repo: &str,
        run_id: u64,
        artifact: &str,
        dir: &Path,
    ) -> Result<()> {
        assert_eq!((repo, run_id, artifact), (SOURCE_REPO, RUN_ID, ARTIFACT));
        copy_dir(&self.artifact_dir(), dir)
    }

    fn download_release(&self, repo: &str, tag: &str, dir: &Path) -> Result<()> {
        assert_eq!(tag, TAG);
        copy_dir(&self.page_dir(repo), dir)
    }

    fn upload_release_asset(&self, repo: &str, tag: &str, file: &Path) -> Result<()> {
        assert_eq!(tag, TAG);
        assert!(file.ends_with("SHA256SUMS.minisig"));
        if self.failing_repo.borrow().as_deref() == Some(repo) {
            bail!("HTTP 403: Resource not accessible");
        }
        self.uploads.borrow_mut().push(repo.into());
        Ok(())
    }
}
