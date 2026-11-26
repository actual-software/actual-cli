//! `actual plugin update`: update the Actual plugin in Claude Code and Codex
//! with their own plugin commands.
//!
//! Without `--agent`, it updates the plugin in each agent whose plugin list
//! has it. With `--background`, which the plugin runs when a session starts,
//! it starts that work as a detached child at most once a day and returns at
//! once, printing nothing.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use chrono::{DateTime, Utc};

use crate::cli::args::{PluginAction, PluginAgent, PluginArgs, PluginUpdateArgs};
use crate::error::ActualError;
use crate::update_check;

/// The plugin's id in Claude Code and Codex.
const PLUGIN_ID: &str = "actual-cli@actual-cli-skills";
const MARKETPLACE: &str = "actual-cli-skills";
const STAMP_FILENAME: &str = "plugin-update.stamp";
const STAMP_LOCK_FILENAME: &str = "plugin-update.lock";

/// What `actual plugin update` reads from the machine it runs on.
struct Setup {
    /// The `PATH` the agents' commands are found on.
    path: OsString,
    /// Where they run, and where the background stamp lives: the config
    /// directory, never the repository a hook started in.
    dir: PathBuf,
}

pub fn exec(args: &PluginArgs) -> Result<(), ActualError> {
    let setup = Setup {
        path: std::env::var_os("PATH").unwrap_or_default(),
        dir: crate::config::paths::config_dir()?,
    };
    dispatch(
        &setup,
        args,
        std::env::current_exe().ok(),
        &mut std::io::stdout(),
    )
}

/// [`exec`] with the running binary passed in, which `--background` starts.
fn dispatch(
    setup: &Setup,
    args: &PluginArgs,
    exe: Option<PathBuf>,
    out: &mut dyn Write,
) -> Result<(), ActualError> {
    let PluginAction::Update(args) = &args.action;
    if args.background {
        background(setup, args.agent, Utc::now(), exe);
        return Ok(());
    }
    update(setup, args, out)
}

/// Start `actual plugin update [--agent X]` as a detached child, unless one
/// started within the day. The day is claimed under a lock before the child
/// starts, so hooks starting together start one.
fn background(setup: &Setup, agent: Option<PluginAgent>, now: DateTime<Utc>, exe: Option<PathBuf>) {
    let (Some(exe), true) = (exe, claim_day(&setup.dir, now)) else {
        return;
    };
    let mut child = std::process::Command::new(exe);
    child.args(["plugin", "update"]);
    if let Some(agent) = agent {
        child.args(["--agent", agent_name(agent)]);
    }
    child
        .current_dir(&setup.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut child, 0);
    let _ = child.spawn();
}

/// True once this run has recorded today's background update. False when one
/// started within the day, or the stamp can't be locked or written.
fn claim_day(dir: &Path, now: DateTime<Utc>) -> bool {
    let Some(_lock) = update_check::lock_file(&dir.join(STAMP_LOCK_FILENAME)) else {
        return false;
    };
    let stamp = dir.join(STAMP_FILENAME);
    let last = std::fs::read_to_string(&stamp)
        .ok()
        .and_then(|text| DateTime::parse_from_rfc3339(text.trim()).ok())
        .map(|at| at.with_timezone(&Utc));
    !update_check::within_a_day(last, now)
        && crate::config::paths::write_secure(&stamp, now.to_rfc3339().as_bytes()).is_ok()
}

fn agent_name(agent: PluginAgent) -> &'static str {
    match agent {
        PluginAgent::Claude => "claude",
        PluginAgent::Codex => "codex",
    }
}

fn agent_label(agent: PluginAgent) -> &'static str {
    match agent {
        PluginAgent::Claude => "Claude Code",
        PluginAgent::Codex => "Codex",
    }
}

/// When the new version applies.
fn applies(agent: PluginAgent) -> &'static str {
    match agent {
        PluginAgent::Claude => "It applies once you restart Claude Code.",
        PluginAgent::Codex => "It applies in a new Codex thread.",
    }
}

/// The commands that update the plugin.
fn steps(agent: PluginAgent) -> Vec<Vec<&'static str>> {
    match agent {
        PluginAgent::Claude => vec![
            vec!["plugin", "marketplace", "update", MARKETPLACE],
            vec!["plugin", "update", PLUGIN_ID],
        ],
        PluginAgent::Codex => vec![vec!["plugin", "marketplace", "upgrade", MARKETPLACE]],
    }
}

/// Update the plugin in the agent `--agent` names, or in each one that has
/// it installed, and report each result.
fn update(setup: &Setup, args: &PluginUpdateArgs, out: &mut dyn Write) -> Result<(), ActualError> {
    let agents: Vec<(PluginAgent, PathBuf, Installed)> = match args.agent {
        Some(agent) => {
            let bin =
                find(agent_name(agent), &setup.path).ok_or_else(|| ActualError::PluginUpdate {
                    message: format!(
                        "{} isn't installed, or isn't on your PATH.",
                        agent_label(agent)
                    ),
                    hint: None,
                    code: 2,
                })?;
            let installed = installed(setup, agent, &bin);
            vec![(agent, bin, installed)]
        }
        None => [PluginAgent::Claude, PluginAgent::Codex]
            .into_iter()
            .filter_map(|agent| {
                let bin = find(agent_name(agent), &setup.path)?;
                let installed = installed(setup, agent, &bin);
                (installed != Installed::No).then_some((agent, bin, installed))
            })
            .collect(),
    };
    if agents.is_empty() {
        let found = [PluginAgent::Claude, PluginAgent::Codex]
            .into_iter()
            .any(|agent| find(agent_name(agent), &setup.path).is_some());
        if !found {
            return Err(ActualError::PluginUpdate {
                message: "Couldn't find Claude Code or Codex on your PATH.".to_string(),
                hint: None,
                code: 2,
            });
        }
        return Err(ActualError::PluginUpdate {
            message: "Neither Claude Code nor Codex has the Actual plugin installed.".to_string(),
            hint: Some(
                "See https://github.com/actual-software/actual-skill to install it.".to_string(),
            ),
            code: 2,
        });
    }
    for (agent, bin, before) in agents {
        let label = agent_label(agent);
        for step in steps(agent) {
            let status = std::process::Command::new(&bin)
                .args(&step)
                .current_dir(&setup.dir)
                .env("PATH", &setup.path)
                .stdin(Stdio::null())
                .status()
                .map_err(|e| ActualError::PluginUpdate {
                    message: format!("Couldn't run {}: {e}", bin.display()),
                    hint: None,
                    code: 1,
                })?;
            if !status.success() {
                return Err(ActualError::PluginUpdate {
                    message: format!(
                        "{} {} failed, so the Actual plugin in {label} wasn't updated.",
                        agent_name(agent),
                        step.join(" ")
                    ),
                    hint: Some("Fix the problem it reported above, then try again.".to_string()),
                    code: status.code().unwrap_or(1),
                });
            }
        }
        let after = installed(setup, agent, &bin);
        let line = match (before.version(), after.version()) {
            (Some(old), Some(new)) if old != new => {
                format!(
                    "Updated the Actual plugin in {label} from {old} to {new}. {}",
                    applies(agent)
                )
            }
            (_, Some(new)) => format!("The Actual plugin in {label} is up to date ({new})."),
            _ => format!("Updated the Actual plugin in {label}. {}", applies(agent)),
        };
        writeln!(out, "{line}")?;
    }
    Ok(())
}

/// Whether an agent has the plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Installed {
    No,
    /// Installed, at this version.
    Yes(String),
    /// Its plugin list couldn't be read, so the update is tried anyway.
    Unknown,
}

impl Installed {
    fn version(&self) -> Option<&str> {
        match self {
            Installed::Yes(version) => Some(version),
            Installed::No | Installed::Unknown => None,
        }
    }
}

/// Ask the agent's plugin list whether it has the plugin.
fn installed(setup: &Setup, agent: PluginAgent, bin: &Path) -> Installed {
    let output = std::process::Command::new(bin)
        .args(["plugin", "list", "--json"])
        .current_dir(&setup.dir)
        .env("PATH", &setup.path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match output {
        Ok(output) if output.status.success() => parse_list(agent, &output.stdout),
        _ => Installed::Unknown,
    }
}

/// Read a plugin list: Claude Code's is an array of `{id, version}`, Codex's
/// an object whose `installed` array holds `{pluginId, version}`.
fn parse_list(agent: PluginAgent, json: &[u8]) -> Installed {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(json) else {
        return Installed::Unknown;
    };
    let (list, key) = match agent {
        PluginAgent::Claude => (value.as_array(), "id"),
        PluginAgent::Codex => (
            value.get("installed").and_then(|v| v.as_array()),
            "pluginId",
        ),
    };
    let Some(list) = list else {
        return Installed::Unknown;
    };
    list.iter()
        .find(|entry| entry.get(key).and_then(|v| v.as_str()) == Some(PLUGIN_ID))
        .map_or(Installed::No, |entry| {
            entry
                .get("version")
                .and_then(|v| v.as_str())
                .map_or(Installed::Unknown, |v| Installed::Yes(v.to_string()))
        })
}

/// The executable `name` in the first absolute `PATH` entry that has it. A
/// relative entry, such as `.`, would find a file in the repository a hook
/// started in, so it is skipped.
fn find(name: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::testutil::write_executable_script;
    use tempfile::{tempdir, TempDir};

    /// A bin directory with fake `claude` and `codex` commands that log each
    /// call and answer `plugin list --json` from a file the test writes.
    struct Fake {
        bin: TempDir,
        home: TempDir,
    }

    impl Fake {
        fn new() -> Self {
            Fake {
                bin: tempdir().unwrap(),
                home: tempdir().unwrap(),
            }
        }

        /// `tool`'s plugin list says `list`; after an update step it says
        /// `updated` instead, when given. `fail` makes every update step exit
        /// with that status.
        fn tool(&self, tool: &str, list: &str, updated: Option<&str>, fail: Option<i32>) {
            let dir = self.bin.path();
            std::fs::write(dir.join(format!("{tool}.list")), list).unwrap();
            if let Some(updated) = updated {
                std::fs::write(dir.join(format!("{tool}.updated")), updated).unwrap();
            }
            let script = format!(
                r#"#!/bin/sh
d="{dir}"
echo "$PWD $*" >> "$d/{tool}.log"
if [ "$*" = "plugin list --json" ]; then /bin/cat "$d/{tool}.list"; exit 0; fi
{fail}
[ -f "$d/{tool}.updated" ] && /bin/cp "$d/{tool}.updated" "$d/{tool}.list"
exit 0
"#,
                dir = dir.display(),
                fail = fail.map_or(String::new(), |code| format!("exit {code}")),
            );
            write_executable_script(&dir.join(tool), script.as_bytes());
        }

        fn log(&self, tool: &str) -> Vec<String> {
            std::fs::read_to_string(self.bin.path().join(format!("{tool}.log")))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }

        fn setup(&self) -> Setup {
            Setup {
                path: self.bin.path().as_os_str().to_owned(),
                dir: self.home.path().to_path_buf(),
            }
        }

        fn run(&self, agent: Option<PluginAgent>) -> (Result<(), ActualError>, String) {
            let mut out = Vec::new();
            let args = PluginUpdateArgs {
                agent,
                background: false,
            };
            let result = update(&self.setup(), &args, &mut out);
            (result, String::from_utf8(out).unwrap())
        }
    }

    fn claude_list(version: &str) -> String {
        format!(r#"[{{"id":"{PLUGIN_ID}","version":"{version}","scope":"user"}}]"#)
    }

    fn codex_list(version: &str) -> String {
        format!(
            r#"{{"installed":[{{"pluginId":"{PLUGIN_ID}","version":"{version}"}}],"available":[]}}"#
        )
    }

    #[test]
    fn updates_each_agent_that_has_the_plugin() {
        let fake = Fake::new();
        fake.tool(
            "claude",
            &claude_list("1.13.2"),
            Some(&claude_list("1.14.0")),
            None,
        );
        fake.tool(
            "codex",
            &codex_list("1.13.2"),
            Some(&codex_list("1.14.0")),
            None,
        );

        let (result, out) = fake.run(None);

        result.unwrap();
        assert_eq!(
            out,
            "Updated the Actual plugin in Claude Code from 1.13.2 to 1.14.0. It applies once you restart Claude Code.\n\
             Updated the Actual plugin in Codex from 1.13.2 to 1.14.0. It applies in a new Codex thread.\n"
        );
        let home = fake.home.path().display().to_string();
        assert_eq!(
            fake.log("claude"),
            [
                format!("{home} plugin list --json"),
                format!("{home} plugin marketplace update actual-cli-skills"),
                format!("{home} plugin update {PLUGIN_ID}"),
                format!("{home} plugin list --json"),
            ]
        );
        assert_eq!(
            fake.log("codex")[1],
            format!("{home} plugin marketplace upgrade actual-cli-skills")
        );
    }

    #[test]
    fn skips_an_agent_without_the_plugin_and_reports_one_already_current() {
        let fake = Fake::new();
        fake.tool("claude", "[]", None, None);
        fake.tool("codex", &codex_list("1.14.0"), None, None);

        let (result, out) = fake.run(None);

        result.unwrap();
        assert_eq!(out, "The Actual plugin in Codex is up to date (1.14.0).\n");
        assert_eq!(
            fake.log("claude"),
            [format!("{} plugin list --json", fake.home.path().display())]
        );
    }

    #[test]
    fn tries_the_update_when_the_plugin_list_cannot_be_read() {
        let fake = Fake::new();
        fake.tool("codex", "Installed plugins:\n  actual-cli", None, None);

        let (result, out) = fake.run(None);

        result.unwrap();
        assert_eq!(
            out,
            "Updated the Actual plugin in Codex. It applies in a new Codex thread.\n"
        );
        assert_eq!(fake.log("codex").len(), 3);
    }

    #[test]
    fn says_so_when_no_agent_has_the_plugin() {
        let fake = Fake::new();
        fake.tool("claude", "[]", None, None);
        fake.tool("codex", r#"{"installed":[]}"#, None, None);
        let err = fake.run(None).0.unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.to_string().contains("Neither Claude Code nor Codex"),
            "{err}"
        );
        // Neither command is on PATH at all.
        let err = Fake::new().run(None).0.unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("on your PATH"), "{err}");
    }

    #[test]
    fn agent_names_one_and_must_be_installed() {
        let fake = Fake::new();
        fake.tool("claude", &claude_list("1.13.2"), None, None);
        fake.tool("codex", &codex_list("1.13.2"), None, None);

        let (result, out) = fake.run(Some(PluginAgent::Codex));
        result.unwrap();
        assert!(out.contains("Codex"), "{out}");
        assert!(fake.log("claude").is_empty());

        let err = Fake::new().run(Some(PluginAgent::Claude)).0.unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.to_string().contains("Claude Code isn't installed"),
            "{err}"
        );
        assert_eq!(err.hint(), None);
    }

    #[test]
    fn exits_with_the_status_of_the_step_that_failed() {
        let fake = Fake::new();
        fake.tool("claude", &claude_list("1.13.2"), None, Some(7));

        let err = fake.run(Some(PluginAgent::Claude)).0.unwrap_err();

        assert_eq!(err.exit_code(), 7);
        assert!(
            err.to_string()
                .contains("claude plugin marketplace update actual-cli-skills failed"),
            "{err}"
        );
        assert!(err.hint().is_some());
        assert_eq!(fake.log("claude").len(), 2, "stops at the first failure");
    }

    #[test]
    fn reports_a_command_that_cannot_start() {
        let fake = Fake::new();
        let broken = fake.bin.path().join("codex");
        // Executable, but no interpreter can run it.
        write_executable_script(&broken, b"#!/no/such/interpreter\n");
        let err = fake.run(Some(PluginAgent::Codex)).0.unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert!(err.to_string().starts_with("Couldn't run"), "{err}");
    }

    #[test]
    fn parse_list_reads_both_agents_formats() {
        let cases = [
            (
                PluginAgent::Claude,
                claude_list("1.2.3"),
                Installed::Yes("1.2.3".into()),
            ),
            (PluginAgent::Claude, "[]".to_string(), Installed::No),
            (
                PluginAgent::Claude,
                format!(r#"[{{"id":"{PLUGIN_ID}"}}]"#),
                Installed::Unknown,
            ),
            (
                PluginAgent::Claude,
                r#"{"installed":[]}"#.to_string(),
                Installed::Unknown,
            ),
            (
                PluginAgent::Codex,
                codex_list("1.2.3"),
                Installed::Yes("1.2.3".into()),
            ),
            (PluginAgent::Codex, "[]".to_string(), Installed::Unknown),
            (
                PluginAgent::Codex,
                "not json".to_string(),
                Installed::Unknown,
            ),
        ];
        for (agent, json, expected) in cases {
            assert_eq!(
                parse_list(agent, json.as_bytes()),
                expected,
                "{agent:?} {json}"
            );
        }
    }

    #[test]
    fn find_skips_relative_path_entries_and_files_that_are_not_executable() {
        let fake = Fake::new();
        std::fs::write(fake.bin.path().join("claude"), "not executable").unwrap();
        assert_eq!(find("claude", fake.bin.path().as_os_str()), None);
        fake.tool("claude", "[]", None, None);
        let path = std::env::join_paths([Path::new("."), fake.bin.path()]).unwrap();
        assert_eq!(find("claude", &path), Some(fake.bin.path().join("claude")));
        assert_eq!(find("claude", OsStr::new(".")), None);
    }

    #[test]
    fn background_starts_one_child_a_day_from_the_config_directory() {
        let fake = Fake::new();
        let exe = fake.bin.path().join("actual");
        let log = fake.bin.path().join("actual.log");
        write_executable_script(
            &exe,
            format!("#!/bin/sh\necho \"$PWD $*\" >> {}\n", log.display()).as_bytes(),
        );
        let now = Utc::now();
        let setup = fake.setup();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope
                    .spawn(|| background(&setup, Some(PluginAgent::Codex), now, Some(exe.clone())));
            }
        });
        background(
            &setup,
            None,
            now + chrono::Duration::hours(23),
            Some(exe.clone()),
        );
        background(&setup, None, now, None);
        let started = std::time::Instant::now();
        while !log.exists() && started.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        let home = fake.home.path().display();
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            format!("{home} plugin update --agent codex\n")
        );

        // A day later it starts again, with no agent named.
        background(
            &setup,
            None,
            now + chrono::Duration::hours(25),
            Some(exe.clone()),
        );
        let started = std::time::Instant::now();
        while std::fs::read_to_string(&log).unwrap().lines().count() < 2
            && started.elapsed() < std::time::Duration::from_secs(10)
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(std::fs::read_to_string(&log)
            .unwrap()
            .ends_with(&format!("{home} plugin update\n")));
    }

    #[test]
    fn background_starts_nothing_when_the_stamp_cannot_be_written() {
        let fake = Fake::new();
        let file = fake.home.path().join("not-a-dir");
        std::fs::write(&file, "").unwrap();
        assert!(!claim_day(&file, Utc::now()));
    }

    #[test]
    fn exec_and_dispatch_pick_foreground_or_background() {
        use crate::testutil::{EnvGuard, ENV_MUTEX};
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let fake = Fake::new();
        let _config = EnvGuard::set("ACTUAL_CONFIG_DIR", fake.home.path().to_str().unwrap());
        let _path = EnvGuard::set("PATH", "");
        let update = |background| PluginArgs {
            action: PluginAction::Update(PluginUpdateArgs {
                agent: None,
                background,
            }),
        };
        assert_eq!(exec(&update(false)).unwrap_err().exit_code(), 2);
        // Without a binary to start, the background run only takes the day.
        let mut out = Vec::new();
        dispatch(&fake.setup(), &update(true), None, &mut out).unwrap();
        assert!(out.is_empty());
        assert!(fake.home.path().join(STAMP_FILENAME).exists());
    }
}
