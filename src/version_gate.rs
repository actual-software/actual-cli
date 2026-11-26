//! The version gate: whether this copy of actual, and the plugin that started
//! it, may still run a command.
//!
//! The npm registry's `minimum` dist-tag names the oldest supported release.
//! Before a command runs, the gate reads it from the update-check cache,
//! looking it up first when the cache is a week old or says this release is
//! below it (see [`update_check::supported_minimum`]). Below the minimum, a
//! release build refuses with [`EXIT_UNSUPPORTED`] and a build from source
//! warns. No cache and no network means no known minimum, so the command runs.
//! `ACTUAL_NO_UPDATE_CHECK` turns off the notice, not the gate.
//!
//! A hook mode also checks the plugin that started it: the Claude Code and
//! Codex plugins export [`PLUGIN_VERSION_ENV`], and one older than
//! [`MIN_PLUGIN_VERSION`] is refused the same way. A plugin from before the
//! gate sets no version and treats [`EXIT_UNSUPPORTED`] as a check that didn't
//! run, so it would say nothing; for that plugin the hook answers in the shape
//! it already shows instead, then exits 0.
//!
//! The commands that fix a refusal (`self-update`, `plugin update`), report
//! the version (`version`; clap answers `--version` and `help` itself), and
//! the internal ones never wait on the gate.

use std::io::{Read, Write};

use chrono::Utc;

use crate::cli::args::{AdrAction, Command, RulesAction, SessionAction};
use crate::cli::commands::hook_host::HookHost;
use crate::update_check::{self, CURRENT_VERSION, RELEASE_BUILD};

/// The exit status of a refused run. No other error uses it (it is
/// `EX_CONFIG` in `sysexits.h`), so the plugin and scripts can tell a version
/// refusal from a failure.
pub const EXIT_UNSUPPORTED: i32 = 78;

/// The oldest plugin release this CLI works with.
pub const MIN_PLUGIN_VERSION: &str = "1.13.2";

/// The plugin's version, which its hooks export for every `actual` they run.
pub const PLUGIN_VERSION_ENV: &str = "ACTUAL_PLUGIN_VERSION";

/// How the gate treats a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Never refused.
    Exempt,
    /// Refused below the minimum.
    Command,
    /// A plugin's hook: also refused for an old plugin, answered as `Reply`
    /// for a plugin that sets no version.
    Hook(HookHost, Reply),
}

/// What a hook answers a plugin that sets no version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reply {
    /// A denial on Claude Code, where it blocks the plan; nothing on Codex,
    /// whose plan gate runs on every `Stop` beside impl-check's notice.
    Deny,
    /// A notice the user sees.
    Notice,
    /// Nothing: a hook that runs on every read, or whose output isn't shown.
    Silent,
}

/// What the gate decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Run the command.
    Run,
    /// Run it after printing this on stderr.
    Warn(String),
    /// Print this on stderr and exit with [`EXIT_UNSUPPORTED`].
    Refuse(String),
    /// Print this on stdout, if anything, and exit 0 without running.
    Answer(Option<String>),
}

/// The gate's verdict on this run.
pub fn check(command: &Command) -> Verdict {
    let role = role(command);
    if role == Role::Exempt {
        return Verdict::Run;
    }
    let url = update_check::registry_url();
    let plugin = std::env::var(PLUGIN_VERSION_ENV).ok();
    decide(
        role,
        CURRENT_VERSION,
        RELEASE_BUILD,
        plugin.as_deref(),
        || {
            update_check::supported_minimum(
                update_check::cache_path().as_deref(),
                &url,
                CURRENT_VERSION,
                Utc::now(),
                update_check::GATE_TIMEOUT,
                RELEASE_BUILD,
            )
        },
    )
}

/// Carry out a verdict: print its message, and return the status to exit
/// with when the command must not run. A hook's envelope, `input`, is read to
/// the end first, so the plugin writing it never sees a closed pipe.
pub fn apply(
    verdict: &Verdict,
    command: &Command,
    input: &mut impl Read,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Option<i32> {
    let code = match verdict {
        Verdict::Run => return None,
        Verdict::Warn(message) => {
            let _ = writeln!(err, "{message}");
            return None;
        }
        Verdict::Refuse(message) => {
            let _ = writeln!(err, "{message}");
            EXIT_UNSUPPORTED
        }
        Verdict::Answer(reply) => {
            if let Some(reply) = reply {
                let _ = writeln!(out, "{reply}");
            }
            0
        }
    };
    if matches!(role(command), Role::Hook(..)) {
        let _ = std::io::copy(&mut input.take(1 << 24), &mut std::io::sink());
    }
    Some(code)
}

/// [`check`] with its inputs passed in, for a command that isn't exempt.
fn decide(
    role: Role,
    current: &str,
    release_build: bool,
    plugin: Option<&str>,
    minimum: impl FnOnce() -> Option<String>,
) -> Verdict {
    let plugin = plugin.filter(|version| !version.is_empty());
    let message = minimum()
        .filter(|minimum| update_check::is_newer(minimum, current))
        .map(|minimum| {
            format!(
                "actual {current} is no longer supported (minimum {minimum}). Run \"actual self-update\", then try again."
            )
        })
        .or_else(|| match role {
            Role::Hook(host, _) if !plugin.is_some_and(plugin_supported) => {
                Some(plugin_message(plugin, current, host))
            }
            _ => None,
        });
    let Some(message) = message else {
        return Verdict::Run;
    };
    if !release_build {
        return Verdict::Warn(format!("warning: {message}"));
    }
    match role {
        Role::Hook(host, reply) if plugin.is_none() => Verdict::Answer(match reply {
            Reply::Deny if host == HookHost::Claude => Some(host.deny(&message)),
            Reply::Notice => Some(host.notice(&message)),
            // Codex runs the plan gate on every `Stop`, beside impl-check's
            // notice, which already says it once.
            Reply::Deny | Reply::Silent => None,
        }),
        _ => Verdict::Refuse(message),
    }
}

/// A plugin version at or above [`MIN_PLUGIN_VERSION`]. One that doesn't
/// parse is too old.
fn plugin_supported(version: &str) -> bool {
    match (
        semver::Version::parse(version),
        semver::Version::parse(MIN_PLUGIN_VERSION),
    ) {
        (Ok(version), Ok(minimum)) => version >= minimum,
        _ => false,
    }
}

fn plugin_message(plugin: Option<&str>, current: &str, host: HookHost) -> String {
    let plugin = plugin.map_or_else(String::new, |version| format!(" {version}"));
    let then = match host {
        HookHost::Claude => "then restart Claude Code",
        HookHost::Codex => "then start a new Codex thread",
    };
    format!(
        "Your Actual plugin{plugin} is too old for actual {current}. Run \"actual plugin update\", {then}."
    )
}

/// The flags that make a command a plugin's hook. The match has no wildcard
/// arm, so a new command has to be placed.
fn role(command: &Command) -> Role {
    match command {
        Command::PlanCheck(args) => hook(args.claude_hook, args.codex_hook, Reply::Deny),
        Command::ImplCheck(args) => hook(args.claude_hook, args.codex_hook, Reply::Notice),
        Command::PlanCheckOverride(args) => hook(args.claude_hook, false, Reply::Silent),
        Command::Session(session) => match &session.action {
            SessionAction::Summary(args) => hook(args.claude_hook, args.codex_hook, Reply::Silent),
        },
        Command::Adr(adr) => match &adr.action {
            // The plugin's hooks run it, and it exits 0 whatever happens.
            AdrAction::Refresh(_) => Role::Exempt,
            _ => Role::Command,
        },
        Command::Rules(rules) => match &rules.action {
            RulesAction::Brief(args) => hook(
                args.claude_hook || args.claude_session_start,
                args.codex_hook || args.codex_session_start,
                Reply::Silent,
            ),
            // The agent runs it to answer a gate's question; refusing it
            // would leave the gate waiting.
            RulesAction::ReportUse(_) => Role::Exempt,
            RulesAction::Ls(_)
            | RulesAction::Index(_)
            | RulesAction::Select(_)
            | RulesAction::Eval(_) => Role::Command,
        },
        Command::AdrBot(_)
        | Command::Status(_)
        | Command::Auth(_)
        | Command::Login(_)
        | Command::Logout
        | Command::Whoami(_)
        | Command::Advisor(_)
        | Command::MintToken(_)
        | Command::Config(_)
        | Command::Runners
        | Command::Models(_)
        | Command::Cache(_)
        | Command::Stats(_) => Role::Command,
        Command::Version(_)
        | Command::SelfUpdate(_)
        | Command::Plugin(_)
        | Command::TelemetryFlush
        | Command::SessionEnded
        | Command::UpdateCheck => Role::Exempt,
    }
}

fn hook(claude: bool, codex: bool, reply: Reply) -> Role {
    HookHost::select(claude, codex).map_or(Role::Command, |host| Role::Hook(host, reply))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::Cli;
    use clap::Parser;

    fn command(args: &[&str]) -> Command {
        Cli::try_parse_from(std::iter::once("actual").chain(args.iter().copied()))
            .expect("the arguments should parse")
            .command
    }

    fn min(version: &str) -> impl FnOnce() -> Option<String> {
        let version = version.to_string();
        move || Some(version)
    }

    const CLI_REFUSAL: &str = "actual 0.16.0 is no longer supported (minimum 0.17.0). Run \"actual self-update\", then try again.";

    #[test]
    fn places_each_command() {
        let claude = |reply| Role::Hook(HookHost::Claude, reply);
        let codex = |reply| Role::Hook(HookHost::Codex, reply);
        let cases: &[(&[&str], Role)] = &[
            (&["plan-check", "--claude-hook"], claude(Reply::Deny)),
            (&["plan-check", "--codex-hook"], codex(Reply::Deny)),
            (&["plan-check", "plan.md"], Role::Command),
            (&["impl-check", "--claude-hook"], claude(Reply::Notice)),
            (&["impl-check", "--codex-hook"], codex(Reply::Notice)),
            (&["impl-check"], Role::Command),
            (&["check-override", "--claude-hook"], claude(Reply::Silent)),
            (
                &["session", "summary", "--claude-hook"],
                claude(Reply::Silent),
            ),
            (
                &["session", "summary", "--codex-hook"],
                codex(Reply::Silent),
            ),
            (&["rules", "brief", "--claude-hook"], claude(Reply::Silent)),
            (
                &["rules", "brief", "--claude-session-start"],
                claude(Reply::Silent),
            ),
            (&["rules", "brief", "--codex-hook"], codex(Reply::Silent)),
            (
                &["rules", "brief", "--codex-session-start"],
                codex(Reply::Silent),
            ),
            (&["rules", "brief", "--file", "src/a.rs"], Role::Command),
            (&["rules", "ls"], Role::Command),
            (
                &[
                    "rules",
                    "report-use",
                    "--claude-session",
                    "s",
                    "--turn",
                    "t",
                    "--none",
                ],
                Role::Exempt,
            ),
            (&["whoami"], Role::Command),
            (&["adr", "refresh"], Role::Exempt),
            (&["adr", "list"], Role::Command),
            (&["version"], Role::Exempt),
            (&["self-update"], Role::Exempt),
            (
                &["plugin", "update", "--background", "--agent", "codex"],
                Role::Exempt,
            ),
            (&["__update-check"], Role::Exempt),
            (&["__telemetry-flush"], Role::Exempt),
            (&["__session-ended"], Role::Exempt),
        ];
        for (args, expected) in cases {
            assert_eq!(role(&command(args)), *expected, "{args:?}");
        }
    }

    #[test]
    fn runs_at_or_above_the_minimum_and_without_one() {
        for minimum in [Some("0.16.0"), Some("0.15.2"), Some("not-a-version"), None] {
            let minimum = minimum.map(str::to_string);
            assert_eq!(
                decide(Role::Command, "0.16.0", true, None, || minimum.clone()),
                Verdict::Run,
                "{minimum:?}"
            );
        }
    }

    #[test]
    fn a_release_build_below_the_minimum_refuses_and_one_from_source_warns() {
        assert_eq!(
            decide(Role::Command, "0.16.0", true, None, min("0.17.0")),
            Verdict::Refuse(CLI_REFUSAL.to_string())
        );
        assert_eq!(
            decide(Role::Command, "0.16.0", false, None, min("0.17.0")),
            Verdict::Warn(format!("warning: {CLI_REFUSAL}"))
        );
    }

    #[test]
    fn exempt_commands_never_look_up_the_minimum() {
        assert_eq!(check(&command(&["version"])), Verdict::Run);
    }

    #[test]
    fn a_hook_refuses_an_old_or_unreadable_plugin_version() {
        let none = || None;
        for (host, then) in [
            (HookHost::Claude, "then restart Claude Code."),
            (HookHost::Codex, "then start a new Codex thread."),
        ] {
            for plugin in ["1.13.1", "1.2.99", "one-point-fourteen"] {
                assert_eq!(
                    decide(Role::Hook(host, Reply::Notice), "0.18.0", true, Some(plugin), none),
                    Verdict::Refuse(format!(
                        "Your Actual plugin {plugin} is too old for actual 0.18.0. Run \"actual plugin update\", {then}"
                    )),
                );
            }
            for plugin in [MIN_PLUGIN_VERSION, "1.14.0", "2.0.0"] {
                assert_eq!(
                    decide(
                        Role::Hook(host, Reply::Notice),
                        "0.18.0",
                        true,
                        Some(plugin),
                        none
                    ),
                    Verdict::Run,
                    "{plugin}"
                );
            }
        }
        // A command the agent runs itself carries no plugin version, and
        // isn't asked for one.
        assert_eq!(
            decide(Role::Command, "0.18.0", true, None, none),
            Verdict::Run
        );
    }

    #[test]
    fn the_cli_minimum_comes_before_the_plugin() {
        assert_eq!(
            decide(
                Role::Hook(HookHost::Claude, Reply::Notice),
                "0.16.0",
                true,
                Some("1.0.0"),
                min("0.17.0")
            ),
            Verdict::Refuse(CLI_REFUSAL.to_string())
        );
    }

    #[test]
    fn a_plugin_that_sets_no_version_gets_an_answer_it_shows() {
        let message = "Your Actual plugin is too old for actual 0.18.0. Run \"actual plugin update\", then restart Claude Code.";
        let codex = "Your Actual plugin is too old for actual 0.18.0. Run \"actual plugin update\", then start a new Codex thread.";
        let cases = [
            (
                HookHost::Claude,
                Reply::Deny,
                Some(HookHost::Claude.deny(message)),
            ),
            (HookHost::Codex, Reply::Deny, None),
            (
                HookHost::Claude,
                Reply::Notice,
                Some(HookHost::Claude.notice(message)),
            ),
            (
                HookHost::Codex,
                Reply::Notice,
                Some(HookHost::Codex.notice(codex)),
            ),
            (HookHost::Claude, Reply::Silent, None),
        ];
        for (host, reply, expected) in cases {
            for plugin in [None, Some("")] {
                assert_eq!(
                    decide(Role::Hook(host, reply), "0.18.0", true, plugin, || None),
                    Verdict::Answer(expected.clone()),
                    "{host:?} {reply:?} {plugin:?}"
                );
            }
        }
        // Below the minimum too: an old plugin can't show the refusal either.
        assert_eq!(
            decide(
                Role::Hook(HookHost::Claude, Reply::Notice),
                "0.16.0",
                true,
                None,
                min("0.17.0"),
            ),
            Verdict::Answer(Some(HookHost::Claude.notice(CLI_REFUSAL)))
        );
        // A build from source warns and runs the hook.
        assert!(matches!(
            decide(
                Role::Hook(HookHost::Claude, Reply::Deny),
                "0.18.0",
                false,
                None,
                || None
            ),
            Verdict::Warn(_)
        ));
    }

    #[test]
    fn apply_prints_the_verdict_and_returns_the_status() {
        let cmd = command(&["whoami"]);
        let run = |verdict: &Verdict| {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = apply(verdict, &cmd, &mut std::io::empty(), &mut out, &mut err);
            (
                code,
                String::from_utf8(out).unwrap(),
                String::from_utf8(err).unwrap(),
            )
        };
        assert_eq!(run(&Verdict::Run), (None, String::new(), String::new()));
        assert_eq!(
            run(&Verdict::Warn("careful".into())),
            (None, String::new(), "careful\n".into())
        );
        assert_eq!(
            run(&Verdict::Refuse("no".into())),
            (Some(EXIT_UNSUPPORTED), String::new(), "no\n".into())
        );
        assert_eq!(
            run(&Verdict::Answer(Some("{}".into()))),
            (Some(0), "{}\n".into(), String::new())
        );
        assert_eq!(
            run(&Verdict::Answer(None)),
            (Some(0), String::new(), String::new())
        );
        // A hook reads its envelope to the end first.
        let hook = command(&["impl-check", "--claude-hook"]);
        let mut envelope = std::io::Cursor::new(b"{\"session_id\":\"s\"}".to_vec());
        assert_eq!(
            apply(
                &Verdict::Answer(None),
                &hook,
                &mut envelope,
                &mut Vec::new(),
                &mut Vec::new()
            ),
            Some(0)
        );
        assert_eq!(envelope.position(), envelope.get_ref().len() as u64);
    }
}
