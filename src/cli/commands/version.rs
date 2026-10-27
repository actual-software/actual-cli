//! `actual version`: the running version, the channel that installed it and
//! how to upgrade it, plus the latest published version with `--check`.
//!
//! Also the hidden `__update-check` command, which the daily update notice
//! starts as a detached child to record the latest version in its cache.

use serde::Serialize;

use crate::cli::args::VersionArgs;
use crate::error::ActualError;
use crate::update_check::{self, Channel, Latest, CURRENT_VERSION, RELEASE_URL};

pub fn exec(args: &VersionArgs) -> Result<(), ActualError> {
    println!("{}", report(args, update_check::REGISTRY_URL));
    Ok(())
}

/// What `actual version` prints, asking `url` for the latest release with
/// `--check`. A configuration that couldn't load makes no request, and the
/// report still carries the local facts.
fn report(args: &VersionArgs, url: &str) -> String {
    let config = crate::config::paths::load().ok();
    let cache = update_check::cache_path();
    let latest = update_check::look_up(args.check, config.as_ref(), url, cache.as_deref());
    render(Channel::current(), &latest, args.json)
}

/// Run the hidden `__update-check` command: one lookup, recorded in the
/// update-check cache. Never fails: the command that started it has already
/// exited.
pub fn exec_refresh() -> Result<(), ActualError> {
    refresh_from(update_check::REGISTRY_URL);
    Ok(())
}

/// [`exec_refresh`]'s lookup, against `url`.
fn refresh_from(url: &str) {
    let config = crate::config::paths::load().ok();
    if let Some(path) = update_check::cache_path() {
        update_check::refresh(config.as_ref(), url, &path);
    }
}

/// The `--json` object, documented in docs/UPDATE_CHECK.md. The actual skill's
/// minimum-version check reads it, so fields are added, never renamed.
#[derive(Debug, Serialize)]
struct Report<'a> {
    current_version: &'a str,
    /// `null` unless `--check` found the latest version.
    latest_version: Option<&'a str>,
    /// `null` whenever `latest_version` is.
    update_available: Option<bool>,
    channel: Channel,
    /// `null` for a binary no package manager owns; see `release_url`.
    upgrade_command: Option<&'a str>,
    release_url: &'a str,
}

fn render(channel: Channel, latest: &Latest, json: bool) -> String {
    if json {
        render_json(channel, latest)
    } else {
        render_text(channel, latest)
    }
}

fn render_json(channel: Channel, latest: &Latest) -> String {
    let latest_version = match latest {
        Latest::Found(version) => Some(version.as_str()),
        Latest::NotRequested | Latest::Disabled | Latest::ConfigNotLoaded | Latest::Unavailable => {
            None
        }
    };
    let report = Report {
        current_version: CURRENT_VERSION,
        latest_version,
        update_available: latest_version
            .map(|latest| update_check::is_newer(latest, CURRENT_VERSION)),
        channel,
        upgrade_command: channel.upgrade_command(),
        release_url: RELEASE_URL,
    };
    serde_json::to_string_pretty(&report).expect("version report is serializable")
}

fn render_text(channel: Channel, latest: &Latest) -> String {
    let mut lines = vec![
        format!("actual {CURRENT_VERSION}"),
        row("Installed via:", channel.label()),
    ];
    let latest_line = match latest {
        Latest::NotRequested => None,
        Latest::Disabled => Some("not checked (update checks are turned off)".to_string()),
        Latest::ConfigNotLoaded => {
            Some("not checked (the config file couldn't be loaded)".to_string())
        }
        Latest::Unavailable => Some("unknown (the npm registry didn't answer)".to_string()),
        Latest::Found(version) if update_check::is_newer(version, CURRENT_VERSION) => {
            Some(format!("{version} (update available)"))
        }
        Latest::Found(version) => Some(format!("{version} (up to date)")),
    };
    if let Some(line) = latest_line {
        lines.push(row("Latest:", &line));
    }
    lines.push(row("Upgrade with:", channel.upgrade_hint()));
    lines.join("\n")
}

fn row(label: &str, value: &str) -> String {
    format!("  {label:<15} {value}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    const FUTURE: &str = "999.0.0";

    #[test]
    fn json_without_a_lookup_carries_the_local_facts() {
        let value: serde_json::Value =
            serde_json::from_str(&render(Channel::Homebrew, &Latest::NotRequested, true)).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "current_version": CURRENT_VERSION,
                "latest_version": null,
                "update_available": null,
                "channel": "homebrew",
                "upgrade_command": "brew upgrade actual-software/actual/actual",
                "release_url": RELEASE_URL,
            })
        );
    }

    #[test]
    fn json_reports_a_found_version_and_whether_it_is_newer() {
        let newer: serde_json::Value = serde_json::from_str(&render(
            Channel::Npm,
            &Latest::Found(FUTURE.to_string()),
            true,
        ))
        .unwrap();
        assert_eq!(newer["latest_version"], FUTURE);
        assert_eq!(newer["update_available"], true);
        assert_eq!(newer["channel"], "npm");
        assert_eq!(
            newer["upgrade_command"],
            "npm install -g @actualai/actual@latest"
        );

        let same: serde_json::Value = serde_json::from_str(&render(
            Channel::Other,
            &Latest::Found(CURRENT_VERSION.to_string()),
            true,
        ))
        .unwrap();
        assert_eq!(same["update_available"], false);
        assert_eq!(same["channel"], "other");
        assert!(same["upgrade_command"].is_null());
    }

    #[test]
    fn json_leaves_the_latest_unknown_when_disabled_or_unanswered() {
        for latest in [
            Latest::Disabled,
            Latest::ConfigNotLoaded,
            Latest::Unavailable,
        ] {
            let value: serde_json::Value =
                serde_json::from_str(&render(Channel::Npm, &latest, true)).unwrap();
            assert!(value["latest_version"].is_null(), "{latest:?}");
            assert!(value["update_available"].is_null(), "{latest:?}");
        }
    }

    #[test]
    fn text_names_the_channel_and_its_upgrade() {
        let text = render(Channel::Homebrew, &Latest::NotRequested, false);
        assert_eq!(
            text,
            format!(
                "actual {CURRENT_VERSION}\n  \
                 Installed via:  Homebrew\n  \
                 Upgrade with:   brew upgrade actual-software/actual/actual"
            )
        );
        let text = render(Channel::Other, &Latest::NotRequested, false);
        assert!(
            text.contains("Installed via:  not Homebrew or npm"),
            "{text}"
        );
        assert!(
            text.contains("Upgrade with:   actual self-update"),
            "{text}"
        );
    }

    #[test]
    fn text_describes_each_lookup_outcome() {
        let cases = [
            (
                Latest::Disabled,
                "not checked (update checks are turned off)".to_string(),
            ),
            (
                Latest::ConfigNotLoaded,
                "not checked (the config file couldn't be loaded)".to_string(),
            ),
            (
                Latest::Unavailable,
                "unknown (the npm registry didn't answer)".to_string(),
            ),
            (
                Latest::Found(FUTURE.to_string()),
                format!("{FUTURE} (update available)"),
            ),
            (
                Latest::Found(CURRENT_VERSION.to_string()),
                format!("{CURRENT_VERSION} (up to date)"),
            ),
        ];
        for (latest, expected) in cases {
            let text = render(Channel::Npm, &latest, false);
            assert!(
                text.contains(&format!("  Latest:         {expected}\n")),
                "{text}"
            );
        }
    }

    #[test]
    fn exec_answers_offline_without_check() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());
        for json in [false, true] {
            exec(&VersionArgs { check: false, json }).unwrap();
        }
        assert!(
            !dir.path().join("update-check.yaml").exists(),
            "an offline answer must not touch the update-check cache"
        );
    }

    #[test]
    fn exec_with_check_makes_no_request_when_turned_off() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());
        let _off = EnvGuard::set(update_check::OPT_OUT_ENV, "1");
        exec(&VersionArgs {
            check: true,
            json: true,
        })
        .unwrap();
        assert!(!dir.path().join("update-check.yaml").exists());
    }

    #[test]
    fn exec_refresh_makes_no_request_when_turned_off() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());
        let _off = EnvGuard::set(update_check::OPT_OUT_ENV, "1");
        assert!(exec_refresh().is_ok());
        assert!(!dir.path().join("update-check.yaml").exists());
    }

    /// `update_check.enabled: false` beside an invalid value: the whole file
    /// fails to load, the opt-out with it.
    const BROKEN_CONFIG: &str = "update_check:\n  enabled: false\nbatch_size: lots\n";

    /// A registry that must be asked exactly `hits` times, and its URL.
    fn registry(hits: usize) -> (mockito::ServerGuard, mockito::Mock, String) {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/latest")
            .with_header("content-type", "application/json")
            .with_body(r#"{"version":"0.9.0"}"#)
            .expect(hits)
            .create();
        let url = format!("{}/latest", server.url());
        (server, mock, url)
    }

    fn report_json(check: bool, url: &str) -> serde_json::Value {
        serde_json::from_str(&report(&VersionArgs { check, json: true }, url)).unwrap()
    }

    #[test]
    fn report_makes_no_request_when_the_config_cannot_load() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());
        let _env = EnvGuard::remove(update_check::OPT_OUT_ENV);
        std::fs::write(dir.path().join("config.yaml"), BROKEN_CONFIG).unwrap();
        let (_server, mock, url) = registry(0);

        let local_facts = serde_json::json!({
            "current_version": CURRENT_VERSION,
            "latest_version": null,
            "update_available": null,
            "channel": "other",
            "upgrade_command": null,
            "release_url": RELEASE_URL,
        });
        assert_eq!(report_json(true, &url), local_facts);
        assert_eq!(report_json(false, &url), local_facts);
        let text = report(
            &VersionArgs {
                check: true,
                json: false,
            },
            &url,
        );
        assert!(
            text.contains("not checked (the config file couldn't be loaded)"),
            "{text}"
        );
        mock.assert();
        assert!(!dir.path().join("update-check.yaml").exists());
    }

    #[test]
    fn report_asks_the_registry_when_the_config_loads() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());
        let _env = EnvGuard::remove(update_check::OPT_OUT_ENV);
        let (_server, mock, url) = registry(1);

        assert_eq!(report_json(true, &url)["latest_version"], "0.9.0");
        mock.assert();
    }

    #[test]
    fn exec_refresh_makes_no_request_when_the_config_cannot_load() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());
        let _env = EnvGuard::remove(update_check::OPT_OUT_ENV);
        std::fs::write(dir.path().join("config.yaml"), BROKEN_CONFIG).unwrap();
        let (_server, mock, url) = registry(0);

        refresh_from(&url);
        mock.assert();
        assert!(!dir.path().join("update-check.yaml").exists());
    }

    #[test]
    fn exec_refresh_records_the_latest_version_when_the_config_loads() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", dir.path().to_str().unwrap());
        let _env = EnvGuard::remove(update_check::OPT_OUT_ENV);
        let (_server, mock, url) = registry(1);

        refresh_from(&url);
        mock.assert();
        let cache = std::fs::read_to_string(dir.path().join("update-check.yaml")).unwrap();
        assert!(cache.contains("0.9.0"), "{cache}");
    }
}
