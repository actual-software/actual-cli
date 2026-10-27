//! `actual self-update`: replace a copy of `actual` someone downloaded by
//! hand with the latest release, once the release's signature checks out.
//!
//! Homebrew, npm and cargo keep a record of the files they install, so those
//! copies get that tool's command instead. It never writes where only an
//! administrator can, and it never runs under CI, inside a command `actual`
//! started, or with update checks turned off.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::config::types::Config;
use crate::error::ActualError;
use crate::release_verify::{self, ReleaseKey, SIGNATURE_FILE, SUMS_FILE};
use crate::update_check::{self, Channel, Latest, CURRENT_VERSION, RELEASE_URL};

/// Every release's files are at `<base>/v<version>/<file>`, on the mirror the
/// manual install and Homebrew download from too.
const DOWNLOAD_BASE: &str = "https://github.com/actual-software/actual-releases/releases/download";

/// The one file in each release archive.
const BINARY: &str = "actual";

/// What upgrades a copy built from source, as the README's install builds it.
const CARGO_COMMAND: &str =
    "cargo install --git https://github.com/actual-software/actual-cli.git --force";

const LOCK_FILENAME: &str = "self-update.lock";
const STAGING_PREFIX: &str = ".actual-self-update-";
/// Older than any live run's scratch directory, since every download gives
/// up at its timeout well before this.
const LEFTOVER_AGE: Duration = Duration::from_secs(3600);
const SMALL_FILE_TIMEOUT: Duration = Duration::from_secs(30);
const SMALL_FILE_LIMIT: u64 = 64 * 1024;
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(600);
const ARCHIVE_LIMIT: u64 = 256 * 1024 * 1024;

const REPORT_HINT: &str = "Nothing was replaced. Try again, and if it keeps failing, report it as https://github.com/actual-software/actual-cli/security/policy describes.";

/// What `actual self-update` reads from the machine it runs on.
struct Setup<'a> {
    config: Option<Config>,
    in_ci: bool,
    in_subprocess: bool,
    /// The running binary, with symlinks resolved.
    exe: Option<PathBuf>,
    os: &'a str,
    arch: &'a str,
    registry_url: &'a str,
    download_base: &'a str,
    keys: &'a [ReleaseKey],
    lock_path: Option<PathBuf>,
    cache_path: Option<PathBuf>,
    progress: bool,
    /// Who owns a path: [`owner_of`], or a stand-in a test can make say root.
    owner: fn(&Path) -> std::io::Result<u32>,
}

pub fn exec() -> Result<(), ActualError> {
    let keys = release_verify::committed_keys().map_err(|message| failed(message, None))?;
    let setup = Setup {
        config: crate::config::paths::load().ok(),
        in_ci: update_check::env_is_set("CI"),
        in_subprocess: update_check::env_is_set(crate::runner::subprocess::SUBPROCESS_MARKER_ENV),
        exe: std::env::current_exe().and_then(std::fs::canonicalize).ok(),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        registry_url: update_check::REGISTRY_URL,
        download_base: DOWNLOAD_BASE,
        keys: &keys,
        lock_path: crate::config::paths::config_dir()
            .ok()
            .map(|dir| dir.join(LOCK_FILENAME)),
        cache_path: update_check::cache_path(),
        progress: std::io::stderr().is_terminal(),
        owner: owner_of,
    };
    run(&setup, &mut std::io::stdout(), &mut std::io::stderr())
}

/// Check every precondition that needs no write, find the latest release,
/// then download, check and install it under the lock. The installed binary
/// doesn't change until the archive has passed both checks.
fn run(setup: &Setup, out: &mut dyn Write, err: &mut dyn Write) -> Result<(), ActualError> {
    check_allowed(setup)?;
    let exe = setup
        .exe
        .clone()
        .ok_or_else(|| failed("Couldn't find the path of the running actual binary.", None))?;
    check_unmanaged(&exe)?;
    let archive_name = archive_for(setup.os, setup.arch).ok_or_else(|| {
        refused(
            format!("There's no release build of actual for {} on {}.", setup.os, setup.arch),
            "Build it from source: cargo install --git https://github.com/actual-software/actual-cli.git",
        )
    })?;
    let latest = latest_version(setup)?;
    if !update_check::is_newer(&latest, CURRENT_VERSION) {
        writeln!(out, "actual {CURRENT_VERSION} is the latest release.")?;
        return Ok(());
    }

    let staging = staging_dir(&exe, &latest)?;
    let user = (setup.owner)(staging.path())?;
    check_not_root(user)?;
    check_owned_by(&exe, (setup.owner)(&exe)?, user, &latest)?;
    sweep_leftovers(staging.path(), SystemTime::now());
    let before = identity(&exe)?;
    let _lock = lock(setup.lock_path.as_deref(), err)?;
    if identity(&exe)? != before {
        let replaced = format!(
            "Another actual self-update replaced {} while this one waited.",
            exe.display()
        );
        writeln!(out, "{replaced}")?;
        return Ok(());
    }

    let tag = format!("v{latest}");
    let base = format!("{}/{tag}", setup.download_base);
    writeln!(err, "Downloading actual {latest} ({archive_name})…")?;
    let sums = fetch(&base, SUMS_FILE, &latest)?;
    let signature = fetch(&base, SIGNATURE_FILE, &latest)?;
    let key = release_verify::verify_sums(
        &sums,
        &String::from_utf8_lossy(&signature),
        &tag,
        setup.keys,
    )
    .map_err(|message| failed(message, Some(REPORT_HINT.to_string())))?;
    let archive = staging.path().join(archive_name);
    download(&base, archive_name, &latest, &archive, setup.progress)?;
    release_verify::check_archive(&String::from_utf8_lossy(&sums), archive_name, &archive)
        .map_err(|message| failed(message, Some(REPORT_HINT.to_string())))?;
    let binary = unpack(&archive, staging.path())?;
    install(&binary, &exe)?;
    let updated = format!(
        "Updated actual at {} from {CURRENT_VERSION} to {latest}, signed with the {} release key.",
        exe.display(),
        key.name()
    );
    writeln!(out, "{updated}")?;
    Ok(())
}

fn refused(message: impl Into<String>, hint: impl Into<String>) -> ActualError {
    ActualError::SelfUpdateRefused {
        message: message.into(),
        hint: hint.into(),
    }
}

fn failed(message: impl Into<String>, hint: Option<String>) -> ActualError {
    ActualError::SelfUpdateFailed {
        message: message.into(),
        hint,
    }
}

/// Refuse when no person is the one asking, or update checks are off. A
/// config file that couldn't load may be the one turning them off.
fn check_allowed(setup: &Setup) -> Result<(), ActualError> {
    if setup.in_ci {
        return Err(refused(
            "actual self-update doesn't run under CI, and CI is set.",
            "Install a pinned release in the pipeline instead.",
        ));
    }
    if setup.in_subprocess {
        return Err(refused(
            "actual self-update doesn't run inside a command actual started itself.",
            "Run it from your own terminal.",
        ));
    }
    match &setup.config {
        None => Err(refused(
            "The config file couldn't be loaded, and it may be what turns update checks off, so actual self-update won't run.",
            "Run actual config show to see the problem.",
        )),
        Some(config) if update_check::is_disabled(config) => Err(refused(
            "Update checks are turned off, so actual self-update won't run.",
            format!(
                "Unset {} and set update_check.enabled to true, or download the release yourself",
                update_check::OPT_OUT_ENV
            ),
        )),
        Some(_) => Ok(()),
    }
}

/// Homebrew, npm and cargo keep a record of the files they install, and
/// their next upgrade overwrites a binary replaced behind their back anyway.
/// A cargo build may also be one made for a system the release builds don't
/// run on, such as a Linux with a glibc older than theirs.
fn check_unmanaged(exe: &Path) -> Result<(), ActualError> {
    let channel = Channel::of_executable(exe);
    let manager = match channel.upgrade_command() {
        Some(command) => Some((channel.label(), command)),
        None if exe.to_string_lossy().contains("/.cargo/bin/") => Some(("cargo", CARGO_COMMAND)),
        None => None,
    };
    match manager {
        Some((name, command)) => Err(refused(
            format!(
                "This copy of actual was installed with {name}, which keeps track of the files it installs, so it does the upgrade."
            ),
            command,
        )),
        None => Ok(()),
    }
}

/// The release archive built for `os` and `arch`, as `std::env::consts`
/// names them.
fn archive_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("actual-darwin-arm64.tar.gz"),
        ("macos", "x86_64") => Some("actual-darwin-x64.tar.gz"),
        ("linux", "aarch64") => Some("actual-linux-arm64.tar.gz"),
        ("linux", "x86_64") => Some("actual-linux-x64.tar.gz"),
        _ => None,
    }
}

/// The latest version npm has, the same lookup the daily notice makes, so
/// the release this installs is the one the notice announced.
fn latest_version(setup: &Setup) -> Result<String, ActualError> {
    match update_check::look_up(
        true,
        setup.config.as_ref(),
        setup.registry_url,
        setup.cache_path.as_deref(),
    ) {
        Latest::Found(version) => Ok(version),
        _ => Err(failed(
            "Couldn't find the latest release: the npm registry didn't answer.",
            Some("Check your connection and try again.".to_string()),
        )),
    }
}

/// A scratch directory beside the binary, so the new one is renamed into
/// place within one filesystem. Making it is also the check that this user
/// can write there: a directory only an administrator can change, such as
/// `/usr/local/bin`, is left alone.
fn staging_dir(exe: &Path, latest: &str) -> Result<tempfile::TempDir, ActualError> {
    let dir = exe.parent().unwrap_or(exe);
    tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(dir)
        .map_err(|e| {
            refused(
                format!(
                    "actual {latest} is out, but actual self-update can't write to {} ({e}).",
                    dir.display()
                ),
                format!("Download it from {RELEASE_URL}, or move actual to a directory you own, such as ~/.local/bin, and run actual self-update from there."),
            )
        })
}

/// Remove the scratch directories that runs killed mid-download, as by Ctrl-C,
/// left beside the binary. Each stays until it is [`LEFTOVER_AGE`] old, so a
/// run still downloading next to this one keeps its own.
fn sweep_leftovers(staging: &Path, now: SystemTime) {
    let dir = staging.parent().unwrap_or(staging);
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if is_leftover(&entry, staging, now) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn is_leftover(entry: &std::fs::DirEntry, staging: &Path, now: SystemTime) -> bool {
    let age = entry
        .metadata()
        .and_then(|meta| meta.modified())
        .map(|modified| now.duration_since(modified).unwrap_or_default());
    entry
        .file_name()
        .to_string_lossy()
        .starts_with(STAGING_PREFIX)
        && entry.path() != staging
        && entry.file_type().is_ok_and(|kind| kind.is_dir())
        && age.is_ok_and(|age| age > LEFTOVER_AGE)
}

/// A directory this process just made belongs to its effective user.
#[cfg(unix)]
fn owner_of(path: &Path) -> std::io::Result<u32> {
    let meta = std::fs::metadata(path)?;
    Ok(std::os::unix::fs::MetadataExt::uid(&meta))
}

#[cfg(not(unix))]
fn owner_of(_path: &Path) -> std::io::Result<u32> {
    Ok(u32::MAX)
}

/// Root can write anywhere, including where only an administrator should.
fn check_not_root(owner: u32) -> Result<(), ActualError> {
    if owner == 0 {
        return Err(refused(
            "actual self-update doesn't replace a binary as root.",
            format!("Run it as the user who installed actual, or download the release from {RELEASE_URL}"),
        ));
    }
    Ok(())
}

/// A binary another user installed, typically root through `sudo mv`, stays
/// theirs to replace, even in a directory this user can write.
fn check_owned_by(exe: &Path, owner: u32, user: u32, latest: &str) -> Result<(), ActualError> {
    if owner != user {
        return Err(refused(
            format!(
                "actual {latest} is out, but {} belongs to another user, so actual self-update leaves it alone.",
                exe.display()
            ),
            format!("Download it from {RELEASE_URL} and install it the way this copy was installed, or move actual to a directory you own, such as ~/.local/bin."),
        ));
    }
    Ok(())
}

/// Tells this binary apart from one another run renamed over it, since the
/// rename gives the path a new inode.
#[cfg(unix)]
fn identity(path: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path)?;
    Ok((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn identity(path: &Path) -> std::io::Result<(u64, Option<std::time::SystemTime>)> {
    let meta = std::fs::metadata(path)?;
    Ok((meta.len(), meta.modified().ok()))
}

/// Hold the self-update lock until the returned file drops, waiting for
/// another run to finish first. The process exiting releases it too, so a
/// crash leaves nothing held.
fn lock(path: Option<&Path>, err: &mut dyn Write) -> Result<std::fs::File, ActualError> {
    let path = path.ok_or_else(|| {
        failed(
            "Couldn't find the config directory, where actual self-update keeps its lock.",
            None,
        )
    })?;
    std::fs::create_dir_all(path.parent().unwrap_or(path))?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(path)?;
    if file.try_lock().is_err() {
        writeln!(err, "Waiting for another actual self-update to finish…")?;
        file.lock()?;
    }
    Ok(file)
}

/// One small file from the release page, read into memory.
fn fetch(base: &str, file: &str, latest: &str) -> Result<Vec<u8>, ActualError> {
    let mut body = Vec::new();
    ::self_update::Download::from_url(format!("{base}/{file}"))
        .timeout(SMALL_FILE_TIMEOUT)
        .max_download_size(SMALL_FILE_LIMIT)
        .download_to(&mut body)
        .map_err(|e| download_error(e, file, latest))?;
    Ok(body)
}

/// The archive, streamed to `dest`, with a progress bar when `progress`.
fn download(
    base: &str,
    file: &str,
    latest: &str,
    dest: &Path,
    progress: bool,
) -> Result<(), ActualError> {
    let out = std::fs::File::create(dest)?;
    ::self_update::Download::from_url(format!("{base}/{file}"))
        .timeout(ARCHIVE_TIMEOUT)
        .max_download_size(ARCHIVE_LIMIT)
        .show_download_progress(progress)
        .download_to(out)
        .map_err(|e| download_error(e, file, latest))
}

/// A file missing from the release page means the release isn't finished:
/// someone signs it by hand once the release workflow has published the rest.
fn download_error(e: ::self_update::errors::Error, file: &str, latest: &str) -> ActualError {
    match e {
        ::self_update::errors::Error::NotFound { .. } => failed(
            format!("The release page for actual {latest} has no {file} yet, so actual self-update can't check that release."),
            Some(format!("Try again later, or download it from {RELEASE_URL}")),
        ),
        other => failed(format!("Couldn't download {file} for actual {latest}: {other}"), None),
    }
}

/// Unpack the binary from the archive, already checked against its signed
/// SHA-256, and make sure what came out is a regular file.
fn unpack(archive: &Path, dir: &Path) -> Result<PathBuf, ActualError> {
    ::self_update::Extract::from_source(archive)
        .archive(::self_update::ArchiveKind::Tar(Some(
            ::self_update::Compression::Gz,
        )))
        .extract_file(dir, BINARY)
        .map_err(|e| {
            failed(
                format!("Couldn't unpack {BINARY} from the release archive: {e}"),
                None,
            )
        })?;
    let binary = dir.join(BINARY);
    if !std::fs::symlink_metadata(&binary)?.is_file() {
        return Err(failed(
            format!("The release archive's {BINARY} isn't a regular file."),
            None,
        ));
    }
    Ok(binary)
}

/// Rename the new binary over the old one, with the old one's permissions,
/// so a process starting `actual` at that moment runs one or the other and
/// never a file still being written.
fn install(new: &Path, exe: &Path) -> Result<(), ActualError> {
    std::fs::set_permissions(new, std::fs::metadata(exe)?.permissions())?;
    std::fs::File::open(new)?.sync_all()?;
    ::self_update::Move::from_source(new)
        .to_dest(exe)
        .map_err(|e| {
            failed(
                format!(
                    "Couldn't move the new binary into place at {}: {e}",
                    exe.display()
                ),
                None,
            )
        })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::config::types::UpdateCheckConfig;
    use crate::release_verify::test_key::{comment, TestKey};
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex, MutexGuard};
    use tempfile::{tempdir, TempDir};

    /// A version newer than any this crate will be built as.
    const FUTURE: &str = "999.0.0";
    const ARCHIVE: &str = "actual-linux-x64.tar.gz";
    const NEW_BINARY: &[u8] = b"the new actual";

    /// Hold the env lock with update checks on, for any test that reaches
    /// the opt-out check. The guard restores the variable before the lock
    /// drops, since a tuple drops its first element first.
    fn quiet_env() -> (EnvGuard, MutexGuard<'static, ()>) {
        let lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        (EnvGuard::remove(update_check::OPT_OUT_ENV), lock)
    }

    /// What a release archive holds where the binary belongs.
    enum Entry {
        File(&'static [u8]),
        Link,
        Missing,
    }

    /// A gzipped tar archive like the ones the release workflow builds.
    fn archive(entry: Entry) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tar = tar::Builder::new(gz);
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        match entry {
            Entry::File(content) => {
                header.set_size(content.len() as u64);
                tar.append_data(&mut header, BINARY, content).unwrap();
            }
            Entry::Link => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                tar.append_link(&mut header, BINARY, "/etc/hosts").unwrap();
            }
            Entry::Missing => {
                header.set_size(3);
                tar.append_data(&mut header, "README", &b"abc"[..]).unwrap();
            }
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    /// The files a release page serves, `None` for one it doesn't have yet.
    struct Page {
        sums: Vec<u8>,
        signature: Option<String>,
        archive: Vec<u8>,
    }

    impl Page {
        /// A release of [`FUTURE`] signed by `key`, listing `archive` under
        /// [`ARCHIVE`] beside another platform's archive.
        fn signed_by(key: &TestKey, archive: Vec<u8>) -> Self {
            let sums = format!(
                "{:x}  actual-darwin-arm64.tar.gz\n{:x}  {ARCHIVE}\n",
                Sha256::digest(b"another platform's archive"),
                Sha256::digest(&archive)
            )
            .into_bytes();
            let signature = Some(key.sign(&sums, &comment(&format!("v{FUTURE}"))));
            Self {
                sums,
                signature,
                archive,
            }
        }
    }

    struct Fixture {
        server: mockito::ServerGuard,
        install: TempDir,
        home: TempDir,
        exe: PathBuf,
        primary: TestKey,
        spare: TestKey,
        keys: Vec<ReleaseKey>,
        registry_url: String,
        download_base: String,
    }

    impl Fixture {
        /// A hand-downloaded `actual` in a directory its user owns.
        fn new() -> Self {
            let server = mockito::Server::new();
            let install = tempdir().unwrap();
            let exe = install.path().join(BINARY);
            std::fs::write(&exe, b"the old actual").unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
            let primary = TestKey::generate(1);
            let spare = TestKey::generate(2);
            let keys = vec![primary.release_key("primary"), spare.release_key("spare")];
            Self {
                registry_url: format!("{}/registry", server.url()),
                download_base: server.url(),
                server,
                exe: std::fs::canonicalize(exe).unwrap(),
                install,
                home: tempdir().unwrap(),
                primary,
                spare,
                keys,
            }
        }

        fn setup(&self) -> Setup<'_> {
            Setup {
                config: Some(Config::default()),
                in_ci: false,
                in_subprocess: false,
                exe: Some(self.exe.clone()),
                os: "linux",
                arch: "x86_64",
                registry_url: &self.registry_url,
                download_base: &self.download_base,
                keys: &self.keys,
                lock_path: Some(self.lock_path()),
                cache_path: Some(self.home.path().join("update-check.yaml")),
                progress: false,
                owner: owner_of,
            }
        }

        fn lock_path(&self) -> PathBuf {
            self.home.path().join(LOCK_FILENAME)
        }

        /// npm's latest version, asked for `hits` times.
        fn latest(&mut self, version: &str, hits: usize) -> mockito::Mock {
            self.server
                .mock("GET", "/registry")
                .with_body(format!(r#"{{"version":"{version}"}}"#))
                .expect(hits)
                .create()
        }

        /// Serve `page` as the release page of [`FUTURE`], expecting each of
        /// `SHA256SUMS`, its signature and the archive `hits` times.
        fn publish(&mut self, page: &Page, hits: [usize; 3]) -> Vec<mockito::Mock> {
            vec![
                self.serve(SUMS_FILE, Some(&page.sums), 200, hits[0]),
                self.serve(
                    SIGNATURE_FILE,
                    page.signature.as_deref().map(str::as_bytes),
                    200,
                    hits[1],
                ),
                self.serve(ARCHIVE, Some(&page.archive), 200, hits[2]),
            ]
        }

        /// One file of the release page of [`FUTURE`], a 404 when `body` is
        /// `None`.
        fn serve(
            &mut self,
            name: &str,
            body: Option<&[u8]>,
            status: usize,
            hits: usize,
        ) -> mockito::Mock {
            self.server
                .mock("GET", format!("/v{FUTURE}/{name}").as_str())
                .with_status(if body.is_some() { status } else { 404 })
                .with_body(body.unwrap_or_default())
                .expect(hits)
                .create()
        }

        /// The names in the install directory, which a finished run leaves
        /// holding only the binary.
        fn installed(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(self.install.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    fn update(setup: &Setup) -> (Result<(), ActualError>, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let result = run(setup, &mut out, &mut err);
        (
            result,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn assert_all(mocks: Vec<mockito::Mock>) {
        for mock in mocks {
            mock.assert();
        }
    }

    // ---- a release that checks out ----

    #[test]
    fn replaces_a_hand_downloaded_binary_with_the_signed_release() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        let registry = f.latest(FUTURE, 1);
        let page = Page::signed_by(&f.primary, archive(Entry::File(NEW_BINARY)));
        let files = f.publish(&page, [1, 1, 1]);
        let before = identity(&f.exe).unwrap();

        let (result, out, err) = update(&f.setup());

        result.unwrap();
        assert_eq!(std::fs::read(&f.exe).unwrap(), NEW_BINARY);
        let mode = std::fs::metadata(&f.exe).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "the old binary's permissions");
        assert_ne!(identity(&f.exe).unwrap(), before, "renamed into place");
        assert_eq!(
            out,
            format!(
                "Updated actual at {} from {CURRENT_VERSION} to {FUTURE}, signed with the primary release key.\n",
                f.exe.display()
            )
        );
        assert_eq!(err, format!("Downloading actual {FUTURE} ({ARCHIVE})…\n"));
        assert_eq!(f.installed(), [BINARY]);
        registry.assert();
        assert_all(files);
    }

    /// A scratch directory another run left, as if it was made `hours_ago`.
    fn leftover(dir: &Path, name: &str, hours_ago: u64) -> PathBuf {
        let path = dir.join(format!("{STAGING_PREFIX}{name}"));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join(ARCHIVE), b"half an archive").unwrap();
        let then = SystemTime::now() - Duration::from_secs(hours_ago * 3600);
        std::fs::File::open(&path)
            .unwrap()
            .set_modified(then)
            .unwrap();
        path
    }

    #[test]
    fn a_run_removes_what_an_interrupted_run_left_behind() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        f.latest(FUTURE, 1);
        let page = Page::signed_by(&f.primary, archive(Entry::File(NEW_BINARY)));
        f.publish(&page, [1, 1, 1]);
        let dir = f.exe.parent().unwrap().to_path_buf();
        let stale = leftover(&dir, "killed", 2);
        let recent = leftover(&dir, "running", 0);

        update(&f.setup()).0.unwrap();

        assert!(!stale.exists(), "an hour-old leftover is removed");
        assert!(recent.exists(), "a recent one may belong to a live run");
        assert_eq!(std::fs::read(&f.exe).unwrap(), NEW_BINARY);
    }

    #[test]
    fn sweep_leftovers_removes_only_old_scratch_directories_beside_the_binary() {
        let dir = tempdir().unwrap();
        let ours = leftover(dir.path(), "ours", 2);
        let old = leftover(dir.path(), "old", 2);
        let fresh = leftover(dir.path(), "fresh", 0);
        let file = dir.path().join(format!("{STAGING_PREFIX}file"));
        std::fs::write(&file, b"").unwrap();
        let unrelated = dir.path().join("other");
        std::fs::create_dir(&unrelated).unwrap();

        sweep_leftovers(&ours, SystemTime::now());

        assert!(!old.exists());
        for kept in [&ours, &fresh, &file, &unrelated] {
            assert!(kept.exists(), "{}", kept.display());
        }
        // A clock set back before the directory's time counts it as new.
        sweep_leftovers(&ours, SystemTime::UNIX_EPOCH);
        assert!(fresh.exists());
    }

    #[test]
    fn accepts_a_release_the_spare_key_signed() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        f.latest(FUTURE, 1);
        let page = Page::signed_by(&f.spare, archive(Entry::File(NEW_BINARY)));
        f.publish(&page, [1, 1, 1]);

        let (result, out, _) = update(&f.setup());

        result.unwrap();
        assert!(
            out.ends_with("signed with the spare release key.\n"),
            "{out}"
        );
        assert_eq!(std::fs::read(&f.exe).unwrap(), NEW_BINARY);
    }

    #[test]
    fn concurrent_runs_leave_one_complete_binary() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        // Unasserted: how many runs download depends on how they interleave.
        f.latest(FUTURE, 4);
        let page = Page::signed_by(&f.primary, archive(Entry::File(NEW_BINARY)));
        f.publish(&page, [1, 1, 1]);

        let setup = f.setup();
        let outcomes: Vec<(Result<(), ActualError>, String, String)> =
            std::thread::scope(|scope| {
                let runs: Vec<_> = (0..4).map(|_| scope.spawn(|| update(&setup))).collect();
                runs.into_iter().map(|run| run.join().unwrap()).collect()
            });

        assert_eq!(std::fs::read(&f.exe).unwrap(), NEW_BINARY);
        assert_eq!(f.installed(), [BINARY]);
        for (result, out, _) in outcomes {
            assert!(result.is_ok(), "{result:?}");
            assert!(
                out.starts_with("Updated actual at ")
                    || out.starts_with("Another actual self-update replaced "),
                "{out}"
            );
        }
    }

    // ---- a release that doesn't check out ----

    /// Run against `page`, expect `message` in the error, and check the old
    /// binary is untouched and nothing is left beside it.
    fn assert_refused_release(
        f: &mut Fixture,
        page: Page,
        hits: [usize; 3],
        message: &str,
    ) -> ActualError {
        f.latest(FUTURE, 1);
        let files = f.publish(&page, hits);

        let (result, out, _) = update(&f.setup());

        let err = result.unwrap_err();
        assert!(err.to_string().contains(message), "{err}");
        assert_eq!(err.exit_code(), 1);
        assert_eq!(out, "");
        assert_eq!(std::fs::read(&f.exe).unwrap(), b"the old actual");
        assert_eq!(f.installed(), [BINARY]);
        assert_all(files);
        err
    }

    #[test]
    fn refuses_a_signature_from_a_key_it_does_not_trust() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        let stranger = TestKey::generate(1);
        let page = Page::signed_by(&stranger, archive(Entry::File(NEW_BINARY)));
        let _ = assert_refused_release(
            &mut f,
            page,
            [1, 1, 0],
            "doesn't verify against the release keys",
        );
    }

    #[test]
    fn refuses_another_release_signature_replayed_for_this_one() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        let mut page = Page::signed_by(&f.primary, archive(Entry::File(NEW_BINARY)));
        page.signature = Some(f.primary.sign(&page.sums, &comment("v0.1.0")));
        let _ = assert_refused_release(&mut f, page, [1, 1, 0], "is for another release");
    }

    #[test]
    fn refuses_an_archive_that_does_not_match_its_signed_digest() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        let mut page = Page::signed_by(&f.primary, archive(Entry::File(NEW_BINARY)));
        page.archive = archive(Entry::File(b"something else"));
        let _ = assert_refused_release(&mut f, page, [1, 1, 1], "but SHA256SUMS lists");
    }

    #[test]
    fn refuses_an_archive_without_the_binary_or_with_a_link_in_its_place() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        let page = Page::signed_by(&f.primary, archive(Entry::Missing));
        let _ = assert_refused_release(&mut f, page, [1, 1, 1], "Couldn't unpack actual");

        let mut f = Fixture::new();
        let page = Page::signed_by(&f.primary, archive(Entry::Link));
        let _ = assert_refused_release(&mut f, page, [1, 1, 1], "isn't a regular file");
    }

    #[test]
    fn waits_for_a_release_nobody_has_signed_yet() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        let mut page = Page::signed_by(&f.primary, archive(Entry::File(NEW_BINARY)));
        page.signature = None;
        let err = assert_refused_release(&mut f, page, [1, 1, 0], "has no SHA256SUMS.minisig yet");
        let hint = err.hint().unwrap().into_owned();
        assert_eq!(
            hint,
            format!("Try again later, or download it from {RELEASE_URL}")
        );
    }

    #[test]
    fn reports_a_download_that_fails() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        f.latest(FUTURE, 1);
        let page = Page::signed_by(&f.primary, archive(Entry::File(NEW_BINARY)));
        let sums = f.serve(SUMS_FILE, Some(&page.sums), 200, 1);
        let signature = page.signature.unwrap();
        let signed = f.serve(SIGNATURE_FILE, Some(signature.as_bytes()), 200, 1);
        let broken = f.serve(ARCHIVE, Some(&page.archive), 500, 1);

        let err = update(&f.setup()).0.unwrap_err();

        assert!(
            err.to_string().starts_with(&format!(
                "Couldn't download {ARCHIVE} for actual {FUTURE}: "
            )),
            "{err}"
        );
        assert_eq!(err.hint(), None);
        assert_eq!(std::fs::read(&f.exe).unwrap(), b"the old actual");
        assert_all(vec![sums, signed, broken]);
    }

    #[test]
    fn reports_a_binary_it_cannot_move_into_place() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        f.latest(FUTURE, 1);
        let page = Page::signed_by(&f.primary, archive(Entry::File(NEW_BINARY)));
        f.publish(&page, [1, 1, 1]);
        // A non-empty directory where the binary should be can't be renamed over.
        let occupied = f.install.path().join("occupied");
        std::fs::create_dir(&occupied).unwrap();
        std::fs::write(occupied.join("file"), b"").unwrap();
        let mut setup = f.setup();
        setup.exe = Some(std::fs::canonicalize(&occupied).unwrap());

        let err = update(&setup).0.unwrap_err();

        assert!(
            err.to_string()
                .starts_with("Couldn't move the new binary into place at "),
            "{err}"
        );
        assert!(occupied.join("file").exists());
    }

    // ---- nothing to do ----

    #[test]
    fn stops_when_this_is_the_latest_release() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        let registry = f.latest(CURRENT_VERSION, 1);

        let (result, out, err) = update(&f.setup());

        result.unwrap();
        assert_eq!(
            out,
            format!("actual {CURRENT_VERSION} is the latest release.\n")
        );
        assert_eq!(err, "");
        assert_eq!(f.installed(), [BINARY]);
        registry.assert();
    }

    #[test]
    fn reports_a_registry_that_does_not_answer() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        let registry = f.server.mock("GET", "/registry").with_status(500).create();

        let err = update(&f.setup()).0.unwrap_err();

        assert_eq!(
            err.to_string(),
            "Couldn't find the latest release: the npm registry didn't answer."
        );
        assert_eq!(err.exit_code(), 1);
        registry.assert();
    }

    // ---- where it won't run ----

    /// Run with `change` applied and expect a refusal naming `message`,
    /// before any request.
    fn assert_refused(change: impl FnOnce(&mut Setup), message: &str) -> ActualError {
        let mut f = Fixture::new();
        let registry = f.latest(FUTURE, 0);
        let mut setup = f.setup();
        change(&mut setup);

        let err = update(&setup).0.unwrap_err();

        assert!(err.to_string().contains(message), "{err}");
        assert_eq!(err.exit_code(), 2);
        assert_eq!(std::fs::read(&f.exe).unwrap(), b"the old actual");
        registry.assert();
        err
    }

    #[test]
    fn refuses_under_ci_and_inside_its_own_subprocesses() {
        let _env = quiet_env();
        assert_refused(|s| s.in_ci = true, "doesn't run under CI");
        assert_refused(
            |s| s.in_subprocess = true,
            "inside a command actual started",
        );
    }

    #[test]
    fn refuses_with_update_checks_off_or_a_config_that_cannot_load() {
        let _env = quiet_env();
        let off = || Config {
            update_check: Some(UpdateCheckConfig {
                enabled: Some(false),
            }),
            ..Config::default()
        };
        let err = assert_refused(|s| s.config = Some(off()), "Update checks are turned off");
        assert!(err.hint().unwrap().contains("ACTUAL_NO_UPDATE_CHECK"));
        assert_refused(|s| s.config = None, "config file couldn't be loaded");
        let _on = EnvGuard::set(update_check::OPT_OUT_ENV, "1");
        assert_refused(|_| {}, "Update checks are turned off");
    }

    #[test]
    fn gives_homebrew_npm_and_cargo_installs_their_own_command() {
        let _env = quiet_env();
        for (path, command) in [
            ("/home/me/.cargo/bin/actual", CARGO_COMMAND),
            (
                "/opt/homebrew/Cellar/actual/0.9.0/bin/actual",
                "brew upgrade actual-software/actual/actual",
            ),
            (
                "/usr/local/lib/node_modules/@actualai/actual-linux-x64/bin/actual",
                "npm install -g @actualai/actual@latest",
            ),
        ] {
            let err = assert_refused(
                |s| s.exe = Some(PathBuf::from(path)),
                "which keeps track of the files it installs",
            );
            assert_eq!(err.hint().as_deref(), Some(command));
        }
    }

    #[test]
    fn refuses_a_platform_with_no_release_build() {
        let _env = quiet_env();
        let err = assert_refused(
            |s| s.os = "windows",
            "There's no release build of actual for windows on x86_64.",
        );
        assert!(err.hint().unwrap().contains("cargo install --git"));
    }

    #[test]
    fn fails_without_the_path_of_the_running_binary() {
        let _env = quiet_env();
        let f = Fixture::new();
        let mut setup = f.setup();
        setup.exe = None;
        let err = update(&setup).0.unwrap_err();
        assert_eq!(
            err.to_string(),
            "Couldn't find the path of the running actual binary."
        );
    }

    #[test]
    fn leaves_a_directory_this_user_cannot_write() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        let registry = f.latest(FUTURE, 1);
        let dir = f.exe.parent().unwrap().to_path_buf();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let result = update(&f.setup()).0;

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = result.unwrap_err();
        assert!(
            err.to_string().starts_with(&format!(
                "actual {FUTURE} is out, but actual self-update can't write to {} (",
                dir.display()
            )),
            "{err}"
        );
        assert_eq!(err.exit_code(), 2);
        assert!(err.hint().unwrap().contains("~/.local/bin"));
        assert_eq!(f.installed(), [BINARY]);
        registry.assert();
    }

    /// Reports the installed binary, and only it, as another user's.
    fn binary_owned_by_someone_else(path: &Path) -> std::io::Result<u32> {
        let real = owner_of(path)?;
        Ok(if path.ends_with(BINARY) {
            real + 1
        } else {
            real
        })
    }

    #[test]
    fn leaves_a_binary_another_user_installed() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        let registry = f.latest(FUTURE, 1);
        let mut setup = f.setup();
        setup.owner = binary_owned_by_someone_else;

        let err = update(&setup).0.unwrap_err();

        assert_eq!(
            err.to_string(),
            format!(
                "actual {FUTURE} is out, but {} belongs to another user, so actual self-update leaves it alone.",
                f.exe.display()
            )
        );
        assert_eq!(err.exit_code(), 2);
        assert!(err.hint().unwrap().contains("~/.local/bin"));
        assert_eq!(std::fs::read(&f.exe).unwrap(), b"the old actual");
        assert_eq!(f.installed(), [BINARY]);
        registry.assert();
    }

    #[test]
    fn refuses_to_run_as_root() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        let registry = f.latest(FUTURE, 1);
        let mut setup = f.setup();
        setup.owner = |_| Ok(0);

        let err = update(&setup).0.unwrap_err();

        assert_eq!(
            err.to_string(),
            "actual self-update doesn't replace a binary as root."
        );
        assert_eq!(err.exit_code(), 2);
        assert!(err
            .hint()
            .unwrap()
            .starts_with("Run it as the user who installed actual"));
        assert_eq!(f.installed(), [BINARY]);
        registry.assert();
    }

    // ---- the lock ----

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl SharedBuf {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }

        /// Whether `text` shows up within 30 seconds.
        fn shows(&self, text: &str) -> bool {
            (0..3000).any(|_| {
                std::thread::sleep(Duration::from_millis(10));
                self.text().contains(text)
            })
        }
    }

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn hold_lock(path: &Path) -> std::fs::File {
        let file = std::fs::File::create(path).unwrap();
        file.lock().unwrap();
        file
    }

    /// Start a run while another holds the lock, wait for it to say so, do
    /// `meanwhile`, then let it go.
    fn run_after_waiting(
        f: &Fixture,
        meanwhile: impl FnOnce(),
    ) -> (Result<(), ActualError>, SharedBuf) {
        let held = hold_lock(&f.lock_path());
        let (mut out, mut err) = (SharedBuf::default(), SharedBuf::default());
        let watch = err.clone();
        let result = std::thread::scope(|scope| {
            let setup = f.setup();
            let output = out.clone();
            let run = scope.spawn(move || run(&setup, &mut output.clone(), &mut err));
            assert!(watch.shows("Waiting for another actual self-update to finish…"));
            meanwhile();
            drop(held);
            run.join().unwrap()
        });
        out.flush().unwrap();
        (result, out)
    }

    #[test]
    fn waits_for_another_run_then_updates() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        f.latest(FUTURE, 1);
        let page = Page::signed_by(&f.primary, archive(Entry::File(NEW_BINARY)));
        let files = f.publish(&page, [1, 1, 1]);

        let (result, out) = run_after_waiting(&f, || {});

        result.unwrap();
        let printed = out.text();
        assert!(printed.starts_with("Updated actual at "), "{printed}");
        assert_eq!(std::fs::read(&f.exe).unwrap(), NEW_BINARY);
        assert_all(files);
    }

    #[test]
    fn stops_when_another_run_replaced_the_binary_while_it_waited() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        f.latest(FUTURE, 1);
        let page = Page::signed_by(&f.primary, archive(Entry::File(NEW_BINARY)));
        let files = f.publish(&page, [0, 0, 0]);

        let (result, out) = run_after_waiting(&f, || {
            let other = f.install.path().join("from-another-run");
            std::fs::write(&other, b"another run's actual").unwrap();
            std::fs::rename(&other, &f.exe).unwrap();
        });

        result.unwrap();
        assert_eq!(
            out.text(),
            format!(
                "Another actual self-update replaced {} while this one waited.\n",
                f.exe.display()
            )
        );
        assert_eq!(std::fs::read(&f.exe).unwrap(), b"another run's actual");
        assert_all(files);
    }

    #[test]
    fn fails_without_a_place_for_the_lock() {
        let _env = quiet_env();
        let mut f = Fixture::new();
        f.latest(FUTURE, 2);
        let mut setup = f.setup();
        setup.lock_path = None;
        let err = update(&setup).0.unwrap_err();
        assert_eq!(
            err.to_string(),
            "Couldn't find the config directory, where actual self-update keeps its lock."
        );

        // A lock path under a regular file can't be created.
        let blocker = f.home.path().join("file");
        std::fs::write(&blocker, b"").unwrap();
        let mut setup = f.setup();
        setup.lock_path = Some(blocker.join(LOCK_FILENAME));
        assert!(matches!(update(&setup).0, Err(ActualError::IoError(_))));
        assert_eq!(std::fs::read(&f.exe).unwrap(), b"the old actual");
    }

    // ---- the glue ----

    #[test]
    fn archive_for_names_the_four_release_builds() {
        assert_eq!(
            archive_for("macos", "aarch64"),
            Some("actual-darwin-arm64.tar.gz")
        );
        assert_eq!(
            archive_for("macos", "x86_64"),
            Some("actual-darwin-x64.tar.gz")
        );
        assert_eq!(
            archive_for("linux", "aarch64"),
            Some("actual-linux-arm64.tar.gz")
        );
        assert_eq!(
            archive_for("linux", "x86_64"),
            Some("actual-linux-x64.tar.gz")
        );
        assert_eq!(archive_for("windows", "x86_64"), None);
        assert_eq!(archive_for("linux", "riscv64"), None);
    }

    #[test]
    fn exec_reads_ci_its_subprocess_marker_and_the_opt_out_from_the_environment() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let _config = EnvGuard::remove("ACTUAL_CONFIG");
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap());
        let marker = crate::runner::subprocess::SUBPROCESS_MARKER_ENV;
        let cases = [
            ("CI", "doesn't run under CI"),
            (marker, "inside a command actual started"),
            (update_check::OPT_OUT_ENV, "Update checks are turned off"),
        ];
        for (name, message) in cases {
            let _clear: Vec<EnvGuard> = cases.iter().map(|(n, _)| EnvGuard::remove(n)).collect();
            let _set = EnvGuard::set(name, "1");
            let err = exec().unwrap_err();
            assert!(err.to_string().contains(message), "{name}: {err}");
        }
        assert!(!home.path().join(LOCK_FILENAME).exists());
    }
}
