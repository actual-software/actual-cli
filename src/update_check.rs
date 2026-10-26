//! The once-a-day update notice, and the facts `actual version` reports.
//!
//! The running binary's own path names the channel that installed it, so the
//! notice can print that channel's upgrade command. The CLI never replaces its
//! own binary: Homebrew and npm keep records of the files they installed.
//!
//! No command waits on the network for the notice. After a command succeeds,
//! [`after_command`] prints a notice the local cache already holds, and when
//! the last lookup is more than a day old it starts the hidden
//! `__update-check` command as a detached child, which asks the npm registry
//! for the latest version and records the answer for a later run.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::cli::args::{Command, RulesAction, SessionAction};
use crate::cli::ui::theme;
use crate::config::types::Config;

/// Any non-empty value turns the update check off, as `ACTUAL_NO_TELEMETRY`
/// does for telemetry.
pub const OPT_OUT_ENV: &str = "ACTUAL_NO_UPDATE_CHECK";

/// The npm registry's `latest` document for the CLI's package: about 2 KB from
/// npm's CDN. GitHub's REST API would allow only 60 unauthenticated calls an
/// hour per IP address, which a shared office address can use up.
pub(crate) const REGISTRY_URL: &str = "https://registry.npmjs.org/@actualai/actual/latest";

/// Where a binary that no package manager owns gets its upgrade.
pub const RELEASE_URL: &str = "https://github.com/actual-software/actual-releases/releases/latest";

/// The longest one registry lookup may take.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);

/// The version this binary was built as.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

const CACHE_FILENAME: &str = "update-check.yaml";
const LOCK_FILENAME: &str = "update-check.lock";
const INTERVAL_HOURS: i64 = 24;
const REFRESH_COMMAND: &str = "__update-check";
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

/// The package manager, if any, that installed the running binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Homebrew,
    Npm,
    Other,
}

impl Channel {
    /// Classify a binary path. Homebrew keeps the binary under
    /// `Cellar/actual/<version>/`, and npm under the platform package
    /// `node_modules/@actualai/actual-<os>-<arch>/`.
    pub fn from_path(path: &Path) -> Self {
        let path = path.to_string_lossy();
        if path.contains("/Cellar/actual/") {
            Channel::Homebrew
        } else if path.contains("/node_modules/@actualai/actual-") {
            Channel::Npm
        } else {
            Channel::Other
        }
    }

    /// Classify an executable, following symlinks when its own path names no
    /// channel: Homebrew starts the binary through a link in its `bin`
    /// directory, and macOS reports the link rather than its target.
    pub fn of_executable(exe: &Path) -> Self {
        match Self::from_path(exe) {
            Channel::Other => {
                std::fs::canonicalize(exe).map_or(Channel::Other, |real| Self::from_path(&real))
            }
            found => found,
        }
    }

    /// The channel of the running binary.
    pub fn current() -> Self {
        std::env::current_exe().map_or(Channel::Other, |exe| Self::of_executable(&exe))
    }

    /// The command that upgrades this channel's install. Homebrew's needs the
    /// tap's full name, because a bare `actual` resolves to an unrelated cask.
    pub fn upgrade_command(self) -> Option<&'static str> {
        match self {
            Channel::Homebrew => Some("brew upgrade actual-software/actual/actual"),
            Channel::Npm => Some("npm install -g @actualai/actual@latest"),
            Channel::Other => None,
        }
    }

    /// How `actual version` names the channel.
    pub fn label(self) -> &'static str {
        match self {
            Channel::Homebrew => "Homebrew",
            Channel::Npm => "npm",
            Channel::Other => "not Homebrew or npm",
        }
    }
}

/// True when update checks are turned off: `ACTUAL_NO_UPDATE_CHECK` set to any
/// non-empty value, or `update_check.enabled: false` in config. Either one
/// stops every registry request, including `actual version --check`'s.
pub fn is_disabled(config: &Config) -> bool {
    env_is_set(OPT_OUT_ENV) || config.update_check.as_ref().and_then(|u| u.enabled) == Some(false)
}

fn env_is_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// True when a registry request may go out: the configuration loaded, and
/// [`is_disabled`] finds update checks on. A configuration that couldn't load
/// may be the one turning them off, so it counts as off.
fn may_check(config: Option<&Config>) -> bool {
    config.is_some_and(|config| !is_disabled(config))
}

/// Whether a command shows the notice when it finishes.
///
/// Hook modes and machine-readable output never do: the actual skill's hooks
/// start `actual` on every file read, edit, prompt and turn end, and a `--json`
/// caller parses what it gets. `mint-token` is the headless path for scripts
/// and CI, the hidden commands are internal, and `version` reports the same
/// facts itself. The match has no wildcard arm, so a new command has to be
/// placed on one side or the other.
pub fn applies_to(command: &Command) -> bool {
    match command {
        Command::AdrBot(_)
        | Command::Status(_)
        | Command::Auth(_)
        | Command::Login(_)
        | Command::Logout
        | Command::Whoami
        | Command::Advisor(_)
        | Command::Config(_)
        | Command::Runners
        | Command::Models(_)
        | Command::Cache(_)
        | Command::PlanCheckOverride(_) => true,
        Command::PlanCheck(args) => !(args.claude_hook || args.json),
        Command::ImplCheck(args) => !(args.claude_hook || args.json),
        Command::Rules(rules) => match &rules.action {
            RulesAction::Ls(args) => !args.json,
            RulesAction::Index(args) => !args.json,
            RulesAction::Select(args) => !args.json,
            RulesAction::Eval(args) => !args.json,
            RulesAction::Brief(args) => !(args.claude_hook || args.claude_session_start),
        },
        Command::Session(session) => match &session.action {
            SessionAction::Summary(args) => !(args.claude_hook || args.json),
        },
        Command::MintToken(_)
        | Command::Version(_)
        | Command::UpdateCheck
        | Command::TelemetryFlush => false,
    }
}

/// Whether this run shows the notice once its command succeeds. Decided before
/// the command runs, because running it consumes the parsed arguments.
pub fn should_run_after(command: &Command) -> bool {
    applies_to(command)
        && someone_is_watching(
            std::io::stdout().is_terminal(),
            std::io::stderr().is_terminal(),
        )
}

/// A person is reading this run at a terminal: stdout and stderr both go to
/// one, this is not a CI run, and this is not one of the `claude` subprocesses
/// the CLI starts. Under a pager, as in `actual rules ls | less`, stdout is a
/// pipe, and a notice on stderr would land on the screen the pager draws over.
fn someone_is_watching(stdout_is_terminal: bool, stderr_is_terminal: bool) -> bool {
    stdout_is_terminal
        && stderr_is_terminal
        && !env_is_set("CI")
        && !env_is_set(crate::runner::subprocess::SUBPROCESS_MARKER_ENV)
}

/// After a successful command [`should_run_after`] picked: print the notice
/// the cache holds, at most once a day, and start a detached refresh when the
/// last lookup is more than a day old. Never waits on the network, and never
/// fails the command.
pub fn after_command() {
    let config = crate::config::paths::load().ok();
    let refresh = after_command_with(
        config.as_ref(),
        cache_path().as_deref(),
        Utc::now(),
        Channel::current(),
        &mut std::io::stderr(),
    );
    spawn_refresh_from(refresh.then(std::env::current_exe).and_then(Result::ok));
}

/// [`after_command`] with its inputs passed in, `config` being `None` when the
/// configuration couldn't load. Returns whether to start a refresh, which is
/// true only once the attempt is recorded in the cache.
fn after_command_with(
    config: Option<&Config>,
    cache_path: Option<&Path>,
    now: DateTime<Utc>,
    channel: Channel,
    out: &mut impl Write,
) -> bool {
    let Some(cache_path) = cache_path.filter(|_| may_check(config)) else {
        return false;
    };
    let Some(claim) = take_claim(cache_path, now) else {
        return false;
    };
    if let Some(latest) = &claim.notice {
        let _ = writeln!(out, "{}", notice(latest, channel));
    }
    claim.refresh
}

/// What one run takes from the cache once its command finishes.
struct Claim {
    /// A newer release to announce.
    notice: Option<String>,
    /// Start the day's lookup.
    refresh: bool,
}

/// Decide what this run takes, under the cache lock: the notice, unless one
/// was shown within the day, and the lookup, unless one started within the
/// day. Both are recorded before the lock is released, so a command finishing
/// at the same moment reads them and takes neither. The lookup is taken only
/// once its attempt is saved, so one that fails or never finishes still waits
/// a day, and nothing starts while the cache can't be written. `None` when
/// the cache can't be locked.
fn take_claim(cache_path: &Path, now: DateTime<Utc>) -> Option<Claim> {
    let _lock = lock_cache(cache_path)?;
    let mut cache = load_cache(cache_path);
    let before = cache.clone();
    let notice = cache
        .latest_version
        .clone()
        .filter(|latest| is_newer(latest, CURRENT_VERSION))
        .filter(|_| !within_a_day(cache.notified_at, now));
    if notice.is_some() {
        cache.notified_at = Some(now);
    }
    let refresh = !within_a_day(cache.checked_at, now);
    if refresh {
        cache.checked_at = Some(now);
    }
    let refresh = cache != before && save_cache(cache_path, &cache) && refresh;
    Some(Claim { notice, refresh })
}

/// The notice, after a blank line: the new release, then the command that
/// upgrades this install, or where to download it.
fn notice(latest: &str, channel: Channel) -> String {
    let upgrade = match channel.upgrade_command() {
        Some(command) => format!("To upgrade, run: {}", theme::hint(command).for_stderr()),
        None => format!("Download it from {}", theme::hint(RELEASE_URL).for_stderr()),
    };
    format!("\nactual {latest} is available (you have {CURRENT_VERSION}).\n{upgrade}")
}

/// Start `actual __update-check` as a detached child, or do nothing without an
/// executable. Its stdio is null so it cannot touch the terminal, and nothing
/// waits for it: it outlives this command and records its answer in the cache
/// for a later run.
fn spawn_refresh_from(exe: Option<PathBuf>) {
    let Some(exe) = exe else {
        return;
    };
    let mut child = std::process::Command::new(exe);
    child
        .arg(REFRESH_COMMAND)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Its own process group: when this command leads its terminal's session,
    // as under `ssh -t` or a terminal that runs it directly, its exit hangs up
    // that session's foreground group, and the lookup would die with it.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut child, 0);
    let _ = child.spawn();
}

/// The hidden `__update-check` command's work: one lookup, recorded in the
/// cache. Does nothing when update checks are off or the configuration
/// couldn't load (`None`). A failed lookup records nothing, which leaves in
/// place the attempt [`after_command`] recorded, so the next one still waits a
/// day.
pub(crate) fn refresh(config: Option<&Config>, url: &str, cache_path: &Path) {
    if !may_check(config) {
        return;
    }
    if let Some(latest) = lookup_latest(url, LOOKUP_TIMEOUT) {
        record_latest(cache_path, &latest, Utc::now());
    }
}

/// What `actual version` learned about the latest release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Latest {
    /// `--check` wasn't given.
    NotRequested,
    /// Update checks are turned off.
    Disabled,
    /// The configuration couldn't load, and it may be what turns update checks
    /// off, so no request was made.
    ConfigNotLoaded,
    /// The registry didn't answer in time, or answered without a valid version.
    Unavailable,
    /// The latest published version.
    Found(String),
}

/// Look up the latest release for `actual version`. Without `check`, with
/// update checks off, or with a configuration that couldn't load (`None`), this
/// makes no request. A successful lookup is also recorded in the cache, which
/// the daily notice reads.
pub(crate) fn look_up(
    check: bool,
    config: Option<&Config>,
    url: &str,
    cache_path: Option<&Path>,
) -> Latest {
    if !check {
        return Latest::NotRequested;
    }
    let Some(config) = config else {
        return Latest::ConfigNotLoaded;
    };
    if is_disabled(config) {
        return Latest::Disabled;
    }
    match lookup_latest(url, LOOKUP_TIMEOUT) {
        Some(latest) => {
            if let Some(path) = cache_path {
                record_latest(path, &latest, Utc::now());
            }
            Latest::Found(latest)
        }
        None => Latest::Unavailable,
    }
}

/// True when `latest` is a newer semantic version than `current`. A version
/// that doesn't parse is never newer.
pub(crate) fn is_newer(latest: &str, current: &str) -> bool {
    match (
        semver::Version::parse(latest),
        semver::Version::parse(current),
    ) {
        (Ok(latest), Ok(current)) => latest > current,
        _ => false,
    }
}

#[derive(Deserialize)]
struct RegistryLatest {
    version: String,
}

/// Ask the registry for the latest published version. `None` on any failure:
/// offline, a timeout, an error status, or a body that names no valid version.
async fn fetch_latest(url: &str, timeout: Duration) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .https_only(!crate::net::is_loopback_http_url(url))
        .user_agent(USER_AGENT)
        .build()
        .ok()?;
    let response = client.get(url).send().await.ok()?.error_for_status().ok()?;
    let latest: RegistryLatest = response.json().await.ok()?;
    semver::Version::parse(&latest.version)
        .ok()
        .map(|version| version.to_string())
}

/// [`fetch_latest`] for callers outside an async context.
fn lookup_latest(url: &str, timeout: Duration) -> Option<String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?
        .block_on(fetch_latest(url, timeout))
}

/// What the update check remembers between runs, in
/// `~/.actualai/actual/update-check.yaml`.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
struct UpdateCache {
    /// When a lookup last started or succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checked_at: Option<DateTime<Utc>>,
    /// The latest version the last successful lookup found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest_version: Option<String>,
    /// When the notice was last printed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notified_at: Option<DateTime<Utc>>,
}

/// The cache file, beside `config.yaml`.
pub(crate) fn cache_path() -> Option<PathBuf> {
    crate::config::paths::config_dir()
        .ok()
        .map(|dir| dir.join(CACHE_FILENAME))
}

fn load_cache(path: &Path) -> UpdateCache {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|yaml| crate::yaml::from_str(&yaml).ok())
        .unwrap_or_default()
}

/// Write the cache, and report whether it landed.
fn save_cache(path: &Path, cache: &UpdateCache) -> bool {
    let yaml = crate::yaml::to_string(cache).unwrap_or_default();
    path.parent()
        .is_some_and(|dir| std::fs::create_dir_all(dir).is_ok())
        && crate::config::paths::write_secure(path, yaml.as_bytes()).is_ok()
}

/// Record a successful lookup, under the cache lock, so a notice stamp or a
/// daily claim saved at the same moment survives it. Records nothing when the
/// cache can't be locked.
fn record_latest(cache_path: &Path, latest: &str, now: DateTime<Utc>) {
    let Some(_lock) = lock_cache(cache_path) else {
        return;
    };
    let mut cache = load_cache(cache_path);
    cache.checked_at = Some(now);
    cache.latest_version = Some(latest.to_string());
    save_cache(cache_path, &cache);
}

/// Lock the cache against every other process for one read-modify-write,
/// waiting until no other holds it. The lock is on a sibling file, because
/// each save replaces the cache file. Dropping the returned file releases it,
/// as does the process exiting, so a crash leaves nothing held. `None` when
/// the lock file can't be opened or locked.
fn lock_cache(cache_path: &Path) -> Option<std::fs::File> {
    let lock_path = cache_path.with_file_name(LOCK_FILENAME);
    std::fs::create_dir_all(lock_path.parent()?).ok()?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(lock_path).ok()?;
    file.lock().ok()?;
    Some(file)
}

/// True when `at` lies within a day of `now`, on either side: a timestamp far
/// in the future, left by a clock that was later set back, counts as stale
/// rather than holding the check off until the clock catches up.
fn within_a_day(at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    at.is_some_and(|at| (now - at).num_hours().abs() < INTERVAL_HOURS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::Cli;
    use crate::config::types::UpdateCheckConfig;
    use crate::runner::subprocess::SUBPROCESS_MARKER_ENV;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use clap::Parser;
    use tempfile::tempdir;

    /// A version newer than any this crate will be built as.
    const FUTURE: &str = "999.0.0";

    fn command(args: &[&str]) -> Command {
        Cli::try_parse_from(std::iter::once("actual").chain(args.iter().copied()))
            .unwrap_or_else(|e| panic!("{args:?} should parse: {e}"))
            .command
    }

    fn config_with(enabled: Option<bool>) -> Config {
        Config {
            update_check: Some(UpdateCheckConfig { enabled }),
            ..Config::default()
        }
    }

    fn hours_ago(now: DateTime<Utc>, hours: i64) -> Option<DateTime<Utc>> {
        Some(now - chrono::Duration::hours(hours))
    }

    // ---- channel ----

    #[test]
    fn channel_from_path_reads_the_install_location() {
        let cases = [
            ("/opt/homebrew/Cellar/actual/0.8.0/bin/actual", Channel::Homebrew),
            ("/usr/local/Cellar/actual/0.8.0/bin/actual", Channel::Homebrew),
            (
                "/home/linuxbrew/.linuxbrew/Cellar/actual/0.8.0/bin/actual",
                Channel::Homebrew,
            ),
            (
                "/usr/local/lib/node_modules/@actualai/actual/node_modules/@actualai/actual-darwin-arm64/bin/actual",
                Channel::Npm,
            ),
            (
                "/home/me/.nvm/versions/node/v20.0.0/lib/node_modules/@actualai/actual-linux-x64/bin/actual",
                Channel::Npm,
            ),
            ("/usr/local/bin/actual", Channel::Other),
            ("/home/me/.cargo/bin/actual", Channel::Other),
            ("/opt/homebrew/bin/actual", Channel::Other),
            // A different formula's Cellar is not ours.
            ("/opt/homebrew/Cellar/actual-budget/1.0/bin/actual", Channel::Other),
        ];
        for (path, expected) in cases {
            assert_eq!(Channel::from_path(Path::new(path)), expected, "{path}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn channel_of_executable_follows_a_homebrew_bin_link() {
        let prefix = tempdir().unwrap();
        let real = prefix.path().join("Cellar/actual/0.8.0/bin/actual");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, b"").unwrap();
        let link = prefix.path().join("bin/actual");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert_eq!(Channel::from_path(&link), Channel::Other);
        assert_eq!(Channel::of_executable(&link), Channel::Homebrew);
    }

    #[test]
    fn channel_of_executable_keeps_a_channel_its_own_path_names() {
        // No canonicalize call is needed, so a path that doesn't exist still
        // classifies.
        let npm = Path::new("/nowhere/node_modules/@actualai/actual-linux-x64/bin/actual");
        assert_eq!(Channel::of_executable(npm), Channel::Npm);
    }

    #[test]
    fn channel_of_executable_is_other_when_the_path_cannot_be_resolved() {
        assert_eq!(
            Channel::of_executable(Path::new("/no/such/dir/actual")),
            Channel::Other
        );
    }

    #[test]
    fn channel_current_classifies_the_test_binary_as_other() {
        assert_eq!(Channel::current(), Channel::Other);
    }

    #[test]
    fn channel_upgrade_commands_and_labels() {
        assert_eq!(
            Channel::Homebrew.upgrade_command(),
            Some("brew upgrade actual-software/actual/actual")
        );
        assert_eq!(
            Channel::Npm.upgrade_command(),
            Some("npm install -g @actualai/actual@latest")
        );
        assert_eq!(Channel::Other.upgrade_command(), None);
        assert_eq!(Channel::Homebrew.label(), "Homebrew");
        assert_eq!(Channel::Npm.label(), "npm");
        assert_eq!(Channel::Other.label(), "not Homebrew or npm");
    }

    #[test]
    fn channel_serializes_in_lowercase() {
        let names: Vec<String> = [Channel::Homebrew, Channel::Npm, Channel::Other]
            .iter()
            .map(|c| serde_json::to_string(c).unwrap())
            .collect();
        assert_eq!(names, ["\"homebrew\"", "\"npm\"", "\"other\""]);
    }

    // ---- opt-out ----

    #[test]
    fn is_disabled_by_env_var_or_config() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        {
            let _off = EnvGuard::remove(OPT_OUT_ENV);
            assert!(!is_disabled(&Config::default()));
            assert!(!is_disabled(&config_with(None)));
            assert!(!is_disabled(&config_with(Some(true))));
            assert!(is_disabled(&config_with(Some(false))));
        }
        {
            let _on = EnvGuard::set(OPT_OUT_ENV, "1");
            assert!(is_disabled(&Config::default()));
            assert!(is_disabled(&config_with(Some(true))));
        }
        {
            let _empty = EnvGuard::set(OPT_OUT_ENV, "");
            assert!(!is_disabled(&Config::default()));
        }
    }

    // ---- which commands show the notice ----

    #[test]
    fn applies_to_commands_a_person_reads() {
        let shown: &[&[&str]] = &[
            &["adr-bot"],
            &["status"],
            &["auth"],
            &["login"],
            &["logout"],
            &["whoami"],
            &["advisor", "why?"],
            &["config", "show"],
            &["runners"],
            &["models"],
            &["cache", "clear"],
            &[
                "check-override",
                "--session",
                "s",
                "--rule",
                "doc::r1",
                "--reason",
                "why",
            ],
            &["plan-check", "a plan"],
            &["impl-check"],
            &["rules", "ls"],
            &["rules", "index"],
            &["rules", "select", "a plan"],
            &["rules", "eval", "--golden", "golden.json"],
            &["rules", "brief", "--file", "src/lib.rs"],
            &["session", "summary", "--session", "s1"],
        ];
        for args in shown {
            assert!(
                applies_to(&command(args)),
                "{args:?} should show the notice"
            );
        }
    }

    #[test]
    fn applies_to_skips_hook_modes_json_and_internal_commands() {
        let skipped: &[&[&str]] = &[
            &["plan-check", "--claude-hook"],
            &["plan-check", "--json", "a plan"],
            &["impl-check", "--claude-hook"],
            &["impl-check", "--json"],
            &["rules", "ls", "--json"],
            &["rules", "index", "--json"],
            &["rules", "select", "--json", "a plan"],
            &["rules", "eval", "--golden", "golden.json", "--json"],
            &["rules", "brief", "--claude-hook"],
            &["rules", "brief", "--claude-session-start"],
            &["session", "summary", "--claude-hook"],
            &["session", "summary", "--session", "s1", "--json"],
            &[
                "mint-token",
                "--service-account-id",
                "00000000-0000-0000-0000-000000000000",
                "--kid",
                "k",
            ],
            &["version"],
            &["version", "--check", "--json"],
            &["__update-check"],
            &["__telemetry-flush"],
        ];
        for args in skipped {
            assert!(!applies_to(&command(args)), "{args:?} should stay quiet");
        }
    }

    #[test]
    fn someone_is_watching_needs_both_streams_at_a_terminal_outside_ci_and_subprocesses() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _ci = EnvGuard::remove("CI");
        let _sub = EnvGuard::remove(SUBPROCESS_MARKER_ENV);
        assert!(someone_is_watching(true, true));
        assert!(
            !someone_is_watching(false, true),
            "stdout piped to a pager or a file"
        );
        assert!(!someone_is_watching(true, false), "stderr piped");
        assert!(!someone_is_watching(false, false));
        {
            let _ci = EnvGuard::set("CI", "true");
            assert!(!someone_is_watching(true, true));
        }
        let _sub = EnvGuard::set(SUBPROCESS_MARKER_ENV, "1");
        assert!(!someone_is_watching(true, true));
    }

    #[test]
    fn should_run_after_is_false_for_skipped_commands_and_under_ci() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _ci = EnvGuard::set("CI", "true");
        assert!(!should_run_after(&command(&["version"])));
        assert!(!should_run_after(&command(&["runners"])));
    }

    // ---- versions and time ----

    #[test]
    fn is_newer_compares_semantic_versions() {
        assert!(is_newer("0.9.0", "0.8.0"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(is_newer("0.8.1", "0.8.0"));
        assert!(is_newer("0.9.0", "0.9.0-rc.1"));
        assert!(!is_newer("0.8.0", "0.8.0"));
        assert!(!is_newer("0.7.9", "0.8.0"));
        assert!(!is_newer("not-a-version", "0.8.0"));
        assert!(!is_newer("0.9.0", "garbage"));
    }

    #[test]
    fn within_a_day_on_either_side_of_now() {
        let now = Utc::now();
        assert!(!within_a_day(None, now));
        assert!(within_a_day(hours_ago(now, 1), now));
        assert!(!within_a_day(hours_ago(now, 25), now));
        assert!(within_a_day(hours_ago(now, -2), now));
        assert!(!within_a_day(hours_ago(now, -72), now));
    }

    // ---- cache ----

    #[test]
    fn cache_round_trips_and_degrades_to_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join(CACHE_FILENAME);
        assert_eq!(load_cache(&path), UpdateCache::default());

        let cache = UpdateCache {
            checked_at: Some(Utc::now()),
            latest_version: Some("0.9.0".to_string()),
            notified_at: None,
        };
        assert!(save_cache(&path, &cache));
        assert_eq!(load_cache(&path), cache);

        std::fs::write(&path, "checked_at: [not, a, time\n").unwrap();
        assert_eq!(load_cache(&path), UpdateCache::default());
    }

    #[test]
    fn save_cache_reports_a_write_that_cannot_land() {
        let dir = tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, b"").unwrap();
        // A path under a regular file cannot be created.
        assert!(!save_cache(
            &blocker.join(CACHE_FILENAME),
            &UpdateCache::default()
        ));
        assert!(!save_cache(Path::new(""), &UpdateCache::default()));
    }

    #[test]
    fn cache_path_sits_beside_the_config_file() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());
        assert_eq!(cache_path(), Some(dir.path().join(CACHE_FILENAME)));
    }

    // ---- the notice after a command ----

    /// Run [`after_command_with`] against a cache file, returning what it
    /// printed and whether it asked for a refresh.
    fn after(config: &Config, path: &Path, now: DateTime<Utc>, channel: Channel) -> (String, bool) {
        let mut out = Vec::new();
        let refresh = after_command_with(Some(config), Some(path), now, channel, &mut out);
        (String::from_utf8(out).unwrap(), refresh)
    }

    fn seed(path: &Path, cache: &UpdateCache) {
        assert!(save_cache(path, cache));
    }

    #[test]
    fn after_command_does_nothing_when_turned_off() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove(OPT_OUT_ENV);
        let dir = tempdir().unwrap();
        let path = dir.path().join(CACHE_FILENAME);
        let (out, refresh) = after(&config_with(Some(false)), &path, Utc::now(), Channel::Npm);
        assert_eq!(out, "");
        assert!(!refresh);
        assert!(!path.exists(), "an opted-out run must not write the cache");
    }

    #[test]
    fn after_command_starts_a_refresh_when_nothing_was_checked_today() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove(OPT_OUT_ENV);
        let dir = tempdir().unwrap();
        let path = dir.path().join(CACHE_FILENAME);
        let now = Utc::now();

        let (out, refresh) = after(&Config::default(), &path, now, Channel::Npm);
        assert_eq!(out, "");
        assert!(refresh);
        assert_eq!(load_cache(&path).checked_at, Some(now));

        // The attempt is recorded, so a second command the same day neither
        // looks up again nor prints.
        let (out, refresh) = after(&Config::default(), &path, now, Channel::Npm);
        assert_eq!(out, "");
        assert!(!refresh);
    }

    #[test]
    fn after_command_prints_a_newer_version_once_a_day() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove(OPT_OUT_ENV);
        let dir = tempdir().unwrap();
        let path = dir.path().join(CACHE_FILENAME);
        let now = Utc::now();
        seed(
            &path,
            &UpdateCache {
                checked_at: hours_ago(now, 1),
                latest_version: Some(FUTURE.to_string()),
                notified_at: hours_ago(now, 30),
            },
        );

        let (out, refresh) = after(&Config::default(), &path, now, Channel::Homebrew);
        assert!(
            out.contains(&format!(
                "actual {FUTURE} is available (you have {CURRENT_VERSION})."
            )),
            "{out}"
        );
        assert!(out.contains("To upgrade, run: "), "{out}");
        assert!(
            out.contains("brew upgrade actual-software/actual/actual"),
            "{out}"
        );
        assert!(!refresh, "a lookup ran an hour ago");
        assert_eq!(load_cache(&path).notified_at, Some(now));

        let (out, _) = after(&Config::default(), &path, now, Channel::Homebrew);
        assert_eq!(out, "", "the notice prints at most once a day");
    }

    #[test]
    fn after_command_links_the_release_for_a_binary_no_package_manager_owns() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove(OPT_OUT_ENV);
        let dir = tempdir().unwrap();
        let path = dir.path().join(CACHE_FILENAME);
        let now = Utc::now();
        seed(
            &path,
            &UpdateCache {
                checked_at: hours_ago(now, 30),
                latest_version: Some(FUTURE.to_string()),
                notified_at: None,
            },
        );

        let (out, refresh) = after(&Config::default(), &path, now, Channel::Other);
        assert!(out.contains("Download it from "), "{out}");
        assert!(out.contains(RELEASE_URL), "{out}");
        assert!(refresh, "the cached answer is more than a day old");
    }

    #[test]
    fn after_command_is_quiet_when_the_cached_version_is_not_newer() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove(OPT_OUT_ENV);
        let dir = tempdir().unwrap();
        let path = dir.path().join(CACHE_FILENAME);
        let now = Utc::now();
        let cache = UpdateCache {
            checked_at: hours_ago(now, 2),
            latest_version: Some(CURRENT_VERSION.to_string()),
            notified_at: None,
        };
        seed(&path, &cache);

        let (out, refresh) = after(&Config::default(), &path, now, Channel::Npm);
        assert_eq!(out, "");
        assert!(!refresh);
        assert_eq!(load_cache(&path), cache, "nothing changed, nothing written");
    }

    #[test]
    fn after_command_starts_nothing_when_the_cache_cannot_be_written() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove(OPT_OUT_ENV);
        let dir = tempdir().unwrap();
        // A directory where the cache file belongs lets the lock be taken and
        // refuses the save.
        let path = dir.path().join(CACHE_FILENAME);
        std::fs::create_dir(&path).unwrap();

        let (_, refresh) = after(&Config::default(), &path, Utc::now(), Channel::Npm);
        assert!(!refresh, "an unrecorded attempt would repeat on every run");
    }

    /// Commands that finish together each read the same stale cache. The lock
    /// lets one of them take the day's lookup and notice, and the rest read
    /// what it recorded. Each call opens the lock file itself, so these threads
    /// contend for it exactly as separate processes do: the lock belongs to the
    /// open file, not to the process.
    #[test]
    fn concurrent_commands_take_the_lookup_and_the_notice_once() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove(OPT_OUT_ENV);
        let dir = tempdir().unwrap();
        let path = dir.path().join(CACHE_FILENAME);
        let now = Utc::now();
        seed(
            &path,
            &UpdateCache {
                checked_at: hours_ago(now, 30),
                latest_version: Some(FUTURE.to_string()),
                notified_at: hours_ago(now, 30),
            },
        );

        let runs = 8;
        let start = std::sync::Arc::new(std::sync::Barrier::new(runs));
        let threads: Vec<_> = (0..runs)
            .map(|_| {
                let (start, path) = (start.clone(), path.clone());
                std::thread::spawn(move || {
                    start.wait();
                    after(&Config::default(), &path, now, Channel::Npm)
                })
            })
            .collect();
        let results: Vec<(String, bool)> = threads.into_iter().map(|t| t.join().unwrap()).collect();

        let refreshes = results.iter().filter(|(_, refresh)| *refresh).count();
        let notices = results.iter().filter(|(out, _)| !out.is_empty()).count();
        assert_eq!((refreshes, notices), (1, 1), "{results:?}");
        let cache = load_cache(&path);
        assert_eq!(
            (cache.checked_at, cache.notified_at),
            (Some(now), Some(now))
        );
    }

    /// A lookup's answer and a command's notice stamp, saved at the same
    /// moment, both survive. Without the lock, each could save over the other
    /// with what it read before the other's save landed.
    #[test]
    fn a_lookup_and_a_notice_saved_together_both_survive() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove(OPT_OUT_ENV);
        let dir = tempdir().unwrap();
        let path = dir.path().join(CACHE_FILENAME);
        let now = Utc::now();
        let newer = "999.1.0";
        // A race doesn't lose an update every time it runs, so run it often.
        for _ in 0..10 {
            seed(
                &path,
                &UpdateCache {
                    checked_at: hours_ago(now, 1),
                    latest_version: Some(FUTURE.to_string()),
                    notified_at: hours_ago(now, 30),
                },
            );
            let start = std::sync::Arc::new(std::sync::Barrier::new(2));
            let lookup = std::thread::spawn({
                let (start, path) = (start.clone(), path.clone());
                move || {
                    start.wait();
                    record_latest(&path, newer, now);
                }
            });
            let command = std::thread::spawn({
                let (start, path) = (start.clone(), path.clone());
                move || {
                    start.wait();
                    after(&Config::default(), &path, now, Channel::Npm)
                }
            });
            lookup.join().unwrap();
            let (out, _) = command.join().unwrap();

            assert!(!out.is_empty(), "the notice was due");
            let cache = load_cache(&path);
            assert_eq!(cache.latest_version.as_deref(), Some(newer), "lookup lost");
            assert_eq!(cache.notified_at, Some(now), "notice stamp lost");
        }
    }

    /// A directory where the lock file belongs defeats the lock, and a run that
    /// can't lock the cache neither takes from it nor records in it.
    #[test]
    fn nothing_is_taken_or_recorded_without_the_lock() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove(OPT_OUT_ENV);
        let dir = tempdir().unwrap();
        let path = dir.path().join(CACHE_FILENAME);
        let now = Utc::now();
        let stale = UpdateCache {
            checked_at: hours_ago(now, 30),
            latest_version: Some(FUTURE.to_string()),
            notified_at: hours_ago(now, 30),
        };
        seed(&path, &stale);
        std::fs::create_dir(dir.path().join(LOCK_FILENAME)).unwrap();

        let taken = after(&Config::default(), &path, now, Channel::Npm);
        assert_eq!(taken, (String::new(), false));
        record_latest(&path, "999.1.0", now);
        assert_eq!(load_cache(&path), stale);
    }

    /// A writer that records whether the cache lock was held each time the
    /// notice reached it.
    struct LockProbe {
        lock_path: PathBuf,
        held: bool,
    }

    impl Write for LockProbe {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let lock = std::fs::File::open(&self.lock_path)?;
            self.held |= lock.try_lock().is_err();
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_notice_prints_after_the_lock_is_released() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove(OPT_OUT_ENV);
        let dir = tempdir().unwrap();
        let path = dir.path().join(CACHE_FILENAME);
        let now = Utc::now();
        seed(
            &path,
            &UpdateCache {
                checked_at: hours_ago(now, 1),
                latest_version: Some(FUTURE.to_string()),
                notified_at: None,
            },
        );
        let mut probe = LockProbe {
            lock_path: dir.path().join(LOCK_FILENAME),
            held: false,
        };

        after_command_with(
            Some(&Config::default()),
            Some(&path),
            now,
            Channel::Npm,
            &mut probe,
        );
        probe.flush().unwrap();
        assert_eq!(
            load_cache(&path).notified_at,
            Some(now),
            "the notice is due"
        );
        assert!(
            !probe.held,
            "a stalled terminal would hold up every other command"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_lock_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        assert!(lock_cache(&dir.path().join(CACHE_FILENAME)).is_some());
        let lock_file = std::fs::metadata(dir.path().join(LOCK_FILENAME)).unwrap();
        assert_eq!(lock_file.permissions().mode() & 0o777, 0o600);
    }

    /// A configuration that couldn't load (`None`) may be the one turning
    /// update checks off, so nothing reaches the registry or the cache.
    #[test]
    fn nothing_reaches_the_registry_when_the_config_cannot_load() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove(OPT_OUT_ENV);
        let dir = tempdir().unwrap();
        let path = dir.path().join(CACHE_FILENAME);
        let now = Utc::now();
        let stale = UpdateCache {
            checked_at: hours_ago(now, 30),
            latest_version: Some(FUTURE.to_string()),
            notified_at: hours_ago(now, 30),
        };
        seed(&path, &stale);
        let (server, mock) = registry(r#"{"version":"0.9.0"}"#, 200, 0);
        let url = latest_url(&server);

        let mut out = Vec::new();
        assert!(!after_command_with(
            None,
            Some(&path),
            now,
            Channel::Npm,
            &mut out
        ));
        assert!(
            out.is_empty(),
            "the notice is part of what the opt-out stops"
        );
        refresh(None, &url, &path);
        assert_eq!(
            look_up(true, None, &url, Some(&path)),
            Latest::ConfigNotLoaded
        );
        assert_eq!(
            look_up(false, None, &url, Some(&path)),
            Latest::NotRequested
        );
        mock.assert();
        assert_eq!(load_cache(&path), stale);
    }

    /// `update_check.enabled: false` beside an invalid value: the whole file
    /// fails to load, the opt-out with it.
    const BROKEN_CONFIG: &str = "update_check:\n  enabled: false\nbatch_size: lots\n";

    #[test]
    fn after_command_takes_nothing_when_the_config_cannot_load() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());
        let _env = EnvGuard::remove(OPT_OUT_ENV);
        std::fs::write(dir.path().join("config.yaml"), BROKEN_CONFIG).unwrap();
        let now = Utc::now();
        // A notice is due and a lookup isn't, so a run that ignored the
        // failure would stamp the notice without starting a child.
        let cache = UpdateCache {
            checked_at: hours_ago(now, 1),
            latest_version: Some(FUTURE.to_string()),
            notified_at: hours_ago(now, 30),
        };
        let path = dir.path().join(CACHE_FILENAME);
        seed(&path, &cache);

        after_command();
        assert_eq!(load_cache(&path), cache);
    }

    #[test]
    fn after_command_reads_the_real_config_and_respects_the_opt_out() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());
        let _off = EnvGuard::set(OPT_OUT_ENV, "1");
        after_command();
        assert!(!dir.path().join(CACHE_FILENAME).exists());
    }

    #[test]
    fn after_command_needs_a_cache_path() {
        let mut out = Vec::new();
        let refresh = after_command_with(
            Some(&Config::default()),
            None,
            Utc::now(),
            Channel::Npm,
            &mut out,
        );
        assert!(!refresh);
        assert!(out.is_empty());
    }

    #[test]
    fn spawn_refresh_from_tolerates_no_executable_or_a_missing_one() {
        spawn_refresh_from(None);
        spawn_refresh_from(Some(PathBuf::from("/no/such/actual-binary")));
    }

    /// A process that leads its own group has a group id equal to its pid. A
    /// child left in this test's group would not, and would die of the hangup
    /// a session-leading parent's exit sends that group.
    #[cfg(unix)]
    #[test]
    fn spawn_refresh_from_starts_the_child_in_its_own_process_group() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let out = dir.path().join("ids");
        let script = dir.path().join("fake-actual");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$$ $(ps -o pgid= -p $$)\" > '{0}.tmp' && mv '{0}.tmp' '{0}'\n",
                out.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        spawn_refresh_from(Some(script));

        let started = std::time::Instant::now();
        while !out.exists() && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let ids = std::fs::read_to_string(&out).expect("the child should have run");
        let ids: Vec<&str> = ids.split_whitespace().collect();
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert_eq!(ids[0], ids[1], "pid and process group id should match");
    }

    // ---- registry lookups ----

    /// A registry answering `/@actualai/actual/latest`, which must be asked
    /// exactly `hits` times.
    fn registry(body: &str, status: usize, hits: usize) -> (mockito::ServerGuard, mockito::Mock) {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/@actualai/actual/latest")
            .with_status(status)
            .with_header("content-type", "application/json")
            .with_body(body)
            .expect(hits)
            .create();
        (server, mock)
    }

    fn latest_url(server: &mockito::ServerGuard) -> String {
        format!("{}/@actualai/actual/latest", server.url())
    }

    /// A loopback URL nothing listens on.
    fn closed_url() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{addr}/@actualai/actual/latest")
    }

    #[test]
    fn lookup_latest_reads_the_registry_version() {
        let (server, mock) = registry(r#"{"name":"@actualai/actual","version":"0.9.0"}"#, 200, 1);
        assert_eq!(
            lookup_latest(&latest_url(&server), LOOKUP_TIMEOUT),
            Some("0.9.0".to_string())
        );
        mock.assert();
    }

    #[test]
    fn lookup_latest_sends_the_cli_user_agent() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/@actualai/actual/latest")
            .match_header("user-agent", USER_AGENT)
            .with_body(r#"{"version":"0.9.0"}"#)
            .create();
        assert!(lookup_latest(&latest_url(&server), LOOKUP_TIMEOUT).is_some());
        mock.assert();
    }

    #[test]
    fn lookup_latest_is_none_on_a_bad_answer() {
        let cases = [
            (r#"{"version":"0.9.0"}"#, 503),
            (r#"{"version":"latest-and-greatest"}"#, 200),
            (r#"{"name":"@actualai/actual"}"#, 200),
            ("<html>not json</html>", 200),
        ];
        for (body, status) in cases {
            let (server, mock) = registry(body, status, 1);
            assert_eq!(
                lookup_latest(&latest_url(&server), LOOKUP_TIMEOUT),
                None,
                "{status} {body}"
            );
            mock.assert();
        }
    }

    #[test]
    fn lookup_latest_is_none_offline() {
        assert_eq!(lookup_latest(&closed_url(), LOOKUP_TIMEOUT), None);
    }

    #[test]
    fn lookup_latest_gives_up_at_the_timeout() {
        // Accepts the connection (the OS completes the handshake from the
        // backlog) and never answers.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/@actualai/actual/latest",
            listener.local_addr().unwrap()
        );
        let started = std::time::Instant::now();
        assert_eq!(lookup_latest(&url, Duration::from_millis(200)), None);
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(listener);
    }

    #[test]
    fn refresh_records_the_latest_version() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove(OPT_OUT_ENV);
        let dir = tempdir().unwrap();
        let path = dir.path().join(CACHE_FILENAME);
        let (server, mock) = registry(r#"{"version":"0.9.0"}"#, 200, 1);

        refresh(Some(&Config::default()), &latest_url(&server), &path);
        mock.assert();
        let cache = load_cache(&path);
        assert_eq!(cache.latest_version.as_deref(), Some("0.9.0"));
        assert!(cache.checked_at.is_some());
    }

    #[test]
    fn refresh_records_nothing_when_the_lookup_fails() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove(OPT_OUT_ENV);
        let dir = tempdir().unwrap();
        let path = dir.path().join(CACHE_FILENAME);
        let claimed = UpdateCache {
            checked_at: Some(Utc::now()),
            latest_version: None,
            notified_at: None,
        };
        seed(&path, &claimed);

        refresh(Some(&Config::default()), &closed_url(), &path);
        assert_eq!(load_cache(&path), claimed);
    }

    #[test]
    fn refresh_makes_no_request_when_turned_off() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set(OPT_OUT_ENV, "1");
        let dir = tempdir().unwrap();
        let path = dir.path().join(CACHE_FILENAME);
        let (server, mock) = registry(r#"{"version":"0.9.0"}"#, 200, 0);

        refresh(Some(&Config::default()), &latest_url(&server), &path);
        mock.assert();
        assert!(!path.exists());
    }

    #[test]
    fn look_up_reports_each_outcome() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove(OPT_OUT_ENV);
        let dir = tempdir().unwrap();
        let path = dir.path().join(CACHE_FILENAME);
        let (server, mock) = registry(r#"{"version":"0.9.0"}"#, 200, 2);
        let url = latest_url(&server);

        assert_eq!(
            look_up(false, Some(&Config::default()), &url, Some(&path)),
            Latest::NotRequested
        );
        assert_eq!(
            look_up(true, Some(&config_with(Some(false))), &url, Some(&path)),
            Latest::Disabled
        );
        assert!(
            !path.exists(),
            "neither form above may touch the registry or the cache"
        );

        assert_eq!(
            look_up(true, Some(&Config::default()), &url, Some(&path)),
            Latest::Found("0.9.0".to_string())
        );
        assert_eq!(load_cache(&path).latest_version.as_deref(), Some("0.9.0"));
        assert_eq!(
            look_up(true, Some(&Config::default()), &url, None),
            Latest::Found("0.9.0".to_string())
        );
        mock.assert();

        assert_eq!(
            look_up(true, Some(&Config::default()), &closed_url(), Some(&path)),
            Latest::Unavailable
        );
    }

    /// An update-check cache serde_yml 0.0.12 wrote reads back unchanged.
    #[test]
    fn test_reads_update_cache_written_by_serde_yml() {
        let yaml = include_str!("../tests/fixtures/serde_yml/update_cache.yaml");
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/serde_yml/update_cache.json"
        ))
        .unwrap();
        let cache: UpdateCache = crate::yaml::from_str(yaml).expect("parse serde_yml output");
        assert_eq!(serde_json::to_value(&cache).unwrap(), expected);

        let rewritten = crate::yaml::to_string(&cache).unwrap();
        assert_eq!(
            crate::yaml::from_str::<UpdateCache>(&rewritten).unwrap(),
            cache
        );
    }
}
