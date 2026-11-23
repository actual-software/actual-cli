//! The local enforcement log: one JSON line per gate round, in
//! `<config dir>/enforcement.jsonl`, for `actual stats` to read.
//!
//! The log is written whatever the telemetry setting and is never uploaded.
//! `ACTUAL_ENFORCEMENT_LOG=off` stops new lines being written. The file is
//! readable by its owner only and keeps at most 90 days and 10 MB of rounds.
//! Writing is best-effort: a gate never fails because its line could not be
//! recorded.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

use crate::api::types::PlanGovernanceDecision;
use crate::config::paths::{append_secure, config_dir, write_secure};
use crate::rules::check::{ArtifactKind, CheckedRule, Verdict};

/// The log's file name, directly under the config directory.
pub const LOG_NAME: &str = "enforcement.jsonl";

/// Serializes appends against compaction across processes.
const LOCK_NAME: &str = "enforcement.lock";

/// Set to `off` (any case) to stop writing the log.
pub const OFF_SWITCH: &str = "ACTUAL_ENFORCEMENT_LOG";

/// Written as `v` on every line.
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy)]
struct Limits {
    max_days: i64,
    max_bytes: u64,
}

const LIMITS: Limits = Limits {
    max_days: 90,
    max_bytes: 10 * 1024 * 1024,
};

impl Limits {
    /// Compaction keeps a day and a tenth of the size cap in hand, so the
    /// next one is that far away rather than one append.
    fn keep_after(self, now: DateTime<Utc>) -> DateTime<Utc> {
        now - TimeDelta::days(self.max_days - 1)
    }

    fn keep_bytes(self) -> u64 {
        self.max_bytes / 10 * 9
    }
}

/// Which gate ran the round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate {
    Plan,
    Impl,
}

impl From<ArtifactKind> for Gate {
    fn from(kind: ArtifactKind) -> Self {
        match kind {
            ArtifactKind::Plan => Gate::Plan,
            ArtifactKind::Diff => Gate::Impl,
        }
    }
}

/// Whether the gate ran from an agent hook or from a direct command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Hook,
    Direct,
}

/// What the round decided, in the gate's own allow / warn / block terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Warn,
    Block,
}

impl From<PlanGovernanceDecision> for Decision {
    fn from(decision: PlanGovernanceDecision) -> Self {
        match decision {
            PlanGovernanceDecision::Allow => Decision::Allow,
            PlanGovernanceDecision::Warn => Decision::Warn,
            PlanGovernanceDecision::Block => Decision::Block,
        }
    }
}

/// One ADR judged in a round: its rule file's `doc_slug` (the same key the
/// override log prefixes its `doc_slug::rule_id` entries with) and the
/// strongest verdict any of its rules got.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdrVerdict {
    pub adr: String,
    pub verdict: Verdict,
}

/// One line of the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Round {
    pub v: u32,
    pub at: DateTime<Utc>,
    pub gate: Gate,
    pub mode: Mode,
    /// Absent for a run outside an agent session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The rules directory as the gate resolved it.
    pub rules_dir: String,
    pub decision: Decision,
    /// The session's round counter for this gate; absent with `session_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u32>,
    #[serde(default)]
    pub adrs: Vec<AdrVerdict>,
}

impl Round {
    /// A round stamped now, outside any session, with `verdicts` grouped per
    /// ADR by [`adr_verdicts`].
    pub fn new(
        gate: Gate,
        mode: Mode,
        rules_dir: &Path,
        decision: Decision,
        verdicts: &[CheckedRule],
    ) -> Self {
        Self {
            v: FORMAT_VERSION,
            at: Utc::now(),
            gate,
            mode,
            session_id: None,
            rules_dir: rules_dir.display().to_string(),
            decision,
            round: None,
            adrs: adr_verdicts(verdicts),
        }
    }

    pub fn in_session(mut self, session_id: &str, round: u32) -> Self {
        self.session_id = Some(session_id.to_string());
        self.round = Some(round);
        self
    }
}

/// The strongest verdict per `doc_slug`, ordered by `doc_slug`.
pub fn adr_verdicts(verdicts: &[CheckedRule]) -> Vec<AdrVerdict> {
    let mut strongest: BTreeMap<&str, Verdict> = BTreeMap::new();
    for checked in verdicts {
        strongest
            .entry(&checked.doc_slug)
            .and_modify(|held| {
                if checked.verdict.strength() > held.strength() {
                    *held = checked.verdict;
                }
            })
            .or_insert(checked.verdict);
    }
    strongest
        .into_iter()
        .map(|(adr, verdict)| AdrVerdict {
            adr: adr.to_string(),
            verdict,
        })
        .collect()
}

/// False when [`OFF_SWITCH`] is set to `off`.
pub fn enabled() -> bool {
    !std::env::var(OFF_SWITCH).is_ok_and(|value| value.trim().eq_ignore_ascii_case("off"))
}

/// Where the log lives, or `None` when the config directory can't be resolved.
pub fn log_path() -> Option<PathBuf> {
    config_dir().ok().map(|dir| dir.join(LOG_NAME))
}

/// Append `round` to the log, unless the log is switched off.
pub fn record(round: &Round) {
    if let Some(path) = log_path().filter(|_| enabled()) {
        append(&path, round, LIMITS);
    }
}

/// Every readable round at or after `since`, oldest first. Lines that don't
/// parse, such as one torn by a crash mid-append, are skipped.
pub fn read(since: Option<DateTime<Utc>>) -> Vec<Round> {
    log_path()
        .map(|path| read_from(&path, since))
        .unwrap_or_default()
}

fn read_from(path: &Path, since: Option<DateTime<Utc>>) -> Vec<Round> {
    std::fs::read(path)
        .unwrap_or_default()
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Round>(line).ok())
        .filter(|round| since.is_none_or(|since| round.at >= since))
        .collect()
}

fn append(path: &Path, round: &Round, limits: Limits) {
    let mut line = serde_json::to_vec(round).unwrap_or_default();
    line.push(b'\n');
    let _guard = path.parent().and_then(lock);
    let incoming = line.len() as u64;
    if needs_compaction(path, incoming, round.at, limits) {
        compact(path, incoming, round.at, limits);
    }
    let _ = append_secure(path, &line);
}

fn lock(dir: &Path) -> Option<std::fs::File> {
    std::fs::create_dir_all(dir).ok()?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(dir.join(LOCK_NAME)).ok()?;
    file.lock().ok()?;
    Some(file)
}

#[derive(Deserialize)]
struct Stamp {
    at: DateTime<Utc>,
}

fn stamp(line: &[u8]) -> Option<DateTime<Utc>> {
    serde_json::from_slice::<Stamp>(line)
        .ok()
        .map(|stamp| stamp.at)
}

/// Lines are appended in time order, so the first one is the oldest.
fn oldest(path: &Path) -> Option<DateTime<Utc>> {
    let mut first = Vec::new();
    BufReader::new(std::fs::File::open(path).ok()?)
        .read_until(b'\n', &mut first)
        .ok()?;
    stamp(&first)
}

fn needs_compaction(path: &Path, incoming: u64, now: DateTime<Utc>, limits: Limits) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() + incoming > limits.max_bytes => true,
        Ok(_) => oldest(path).is_none_or(|at| now - at > TimeDelta::days(limits.max_days)),
        Err(_) => false,
    }
}

/// Rewrite the log without lines older than the age budget or unparseable,
/// then drop the oldest until `incoming` fits in the size budget.
fn compact(path: &Path, incoming: u64, now: DateTime<Utc>, limits: Limits) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let keep_after = limits.keep_after(now);
    let lines: Vec<&[u8]> = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| stamp(line).is_some_and(|at| at >= keep_after))
        .collect();
    let budget = limits.keep_bytes().saturating_sub(incoming);
    let mut size: u64 = lines.iter().map(|line| line.len() as u64 + 1).sum();
    let mut start = 0;
    while size > budget && start < lines.len() {
        size -= lines[start].len() as u64 + 1;
        start += 1;
    }
    let mut out = Vec::with_capacity(size as usize);
    for line in &lines[start..] {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    let _ = write_secure(path, &out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::types::RuleLevel;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    struct ConfigDir {
        home: tempfile::TempDir,
        _guards: [EnvGuard; 3],
    }

    impl ConfigDir {
        fn path(&self) -> &Path {
            self.home.path()
        }
    }

    fn with_config_dir() -> ConfigDir {
        let home = tempdir().unwrap();
        let _guards = [
            EnvGuard::set("ACTUAL_CONFIG_DIR", home.path().to_str().unwrap()),
            EnvGuard::remove("ACTUAL_CONFIG"),
            EnvGuard::remove(OFF_SWITCH),
        ];
        ConfigDir { home, _guards }
    }

    fn checked(doc_slug: &str, rule_id: &str, verdict: Verdict) -> CheckedRule {
        CheckedRule {
            doc_slug: doc_slug.to_string(),
            rule_id: rule_id.to_string(),
            level: RuleLevel::Must,
            statement: "statement".to_string(),
            verdict,
            span: String::new(),
            reason: String::new(),
        }
    }

    fn sample() -> Round {
        Round::new(
            Gate::Plan,
            Mode::Hook,
            Path::new("/repo/.actual/rules"),
            Decision::Block,
            &[
                checked("adr-a", "R-001", Verdict::Followed),
                checked("adr-a", "R-002", Verdict::Conflicting),
                checked("adr-b", "R-001", Verdict::NotApplicable),
            ],
        )
        .in_session("session-1", 2)
    }

    fn at(round: Round, at: DateTime<Utc>) -> Round {
        Round { at, ..round }
    }

    fn line_count(path: &Path) -> usize {
        std::fs::read_to_string(path).unwrap().lines().count()
    }

    #[test]
    fn test_record_appends_a_round_that_reads_back() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = with_config_dir();

        let round = sample();
        record(&round);
        record(&round);

        assert_eq!(log_path().unwrap(), home.path().join(LOG_NAME));
        assert_eq!(read(None), vec![round.clone(), round]);
    }

    #[test]
    fn test_round_records_the_fields_the_plan_names() {
        let value = serde_json::to_value(sample()).unwrap();
        assert_eq!(value["v"], 1);
        assert_eq!(value["gate"], "plan");
        assert_eq!(value["mode"], "hook");
        assert_eq!(value["session_id"], "session-1");
        assert_eq!(value["rules_dir"], "/repo/.actual/rules");
        assert_eq!(value["decision"], "block");
        assert_eq!(value["round"], 2);
        assert_eq!(
            value["adrs"],
            serde_json::json!([
                {"adr": "adr-a", "verdict": "conflicting"},
                {"adr": "adr-b", "verdict": "not_applicable"},
            ])
        );
    }

    #[test]
    fn test_direct_round_omits_session_and_round() {
        let round = Round::new(
            Gate::from(ArtifactKind::Diff),
            Mode::Direct,
            Path::new("rules"),
            Decision::Allow,
            &[],
        );
        let value = serde_json::to_value(&round).unwrap();
        assert_eq!(value["gate"], "impl");
        assert_eq!(value["mode"], "direct");
        assert_eq!(value["decision"], "allow");
        assert!(value.get("session_id").is_none());
        assert!(value.get("round").is_none());
        assert_eq!(value["adrs"], serde_json::json!([]));
    }

    #[test]
    fn test_gate_names_both_artifact_kinds() {
        assert_eq!(Gate::from(ArtifactKind::Plan), Gate::Plan);
        assert_eq!(Gate::from(ArtifactKind::Diff), Gate::Impl);
    }

    #[test]
    fn test_decision_mirrors_the_governance_decision() {
        for (governance, decision) in [
            (PlanGovernanceDecision::Allow, Decision::Allow),
            (PlanGovernanceDecision::Warn, Decision::Warn),
            (PlanGovernanceDecision::Block, Decision::Block),
        ] {
            assert_eq!(Decision::from(governance), decision);
        }
    }

    #[test]
    fn test_adr_verdicts_keep_the_strongest_verdict_per_adr() {
        let adrs = adr_verdicts(&[
            checked("adr-b", "R-001", Verdict::RequiresDecision),
            checked("adr-a", "R-001", Verdict::Conflicting),
            checked("adr-a", "R-002", Verdict::Followed),
            checked("adr-b", "R-002", Verdict::Followed),
        ]);
        assert_eq!(
            adrs,
            vec![
                AdrVerdict {
                    adr: "adr-a".to_string(),
                    verdict: Verdict::Conflicting,
                },
                AdrVerdict {
                    adr: "adr-b".to_string(),
                    verdict: Verdict::RequiresDecision,
                },
            ]
        );
    }

    #[test]
    fn test_off_switch_stops_writing() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _home = with_config_dir();

        for value in ["off", " OFF "] {
            let _off = EnvGuard::set(OFF_SWITCH, value);
            assert!(!enabled());
            record(&sample());
        }
        assert!(!log_path().unwrap().exists());

        for value in ["on", "", "0"] {
            let _on = EnvGuard::set(OFF_SWITCH, value);
            assert!(enabled());
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_log_and_lock_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = with_config_dir();

        record(&sample());
        for name in [LOG_NAME, LOCK_NAME] {
            let mode = std::fs::metadata(home.path().join(name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{name}");
        }
    }

    #[test]
    fn test_record_creates_a_missing_config_dir() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().unwrap();
        let nested = home.path().join("a").join("b");
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", nested.to_str().unwrap());
        let _config = EnvGuard::remove("ACTUAL_CONFIG");
        let _switch = EnvGuard::remove(OFF_SWITCH);

        record(&sample());
        assert_eq!(read(None).len(), 1);
    }

    #[test]
    fn test_unresolvable_config_dir_writes_and_reads_nothing() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _dir = EnvGuard::set("ACTUAL_CONFIG_DIR", "relative/dir");
        let _config = EnvGuard::remove("ACTUAL_CONFIG");
        let _switch = EnvGuard::remove(OFF_SWITCH);

        assert!(log_path().is_none());
        record(&sample());
        assert!(read(None).is_empty());
    }

    #[test]
    fn test_read_skips_unparseable_lines_and_filters_by_time() {
        let home = tempdir().unwrap();
        let path = home.path().join(LOG_NAME);
        let now = Utc::now();
        let old = at(sample(), now - TimeDelta::days(3));
        let new = at(sample(), now);
        let text = format!(
            "{}\nnot json\n\n{}\n{{\"at\":\"torn",
            serde_json::to_string(&old).unwrap(),
            serde_json::to_string(&new).unwrap(),
        );
        std::fs::write(&path, text).unwrap();

        assert_eq!(read_from(&path, None), vec![old, new.clone()]);
        assert_eq!(read_from(&path, Some(now - TimeDelta::days(1))), vec![new]);
        assert!(read_from(&home.path().join("missing"), None).is_empty());
    }

    #[test]
    fn test_size_cap_drops_the_oldest_rounds() {
        let home = tempdir().unwrap();
        let path = home.path().join(LOG_NAME);
        let limits = Limits {
            max_days: 90,
            max_bytes: 2_000,
        };
        let now = Utc::now();
        for i in 0..100 {
            let round = at(sample(), now + TimeDelta::seconds(i));
            append(&path, &round.in_session(&format!("s-{i}"), 1), limits);
            assert!(std::fs::metadata(&path).unwrap().len() <= limits.max_bytes);
        }

        let rounds = read_from(&path, None);
        assert_eq!(rounds.len(), line_count(&path));
        assert!(rounds.len() > 1);
        assert_eq!(rounds.last().unwrap().session_id.as_deref(), Some("s-99"));
        assert!(rounds.windows(2).all(|pair| pair[0].at < pair[1].at));
    }

    #[test]
    fn test_age_cap_drops_rounds_past_the_budget() {
        let home = tempdir().unwrap();
        let path = home.path().join(LOG_NAME);
        let now = Utc::now();
        let ancient = at(sample(), now - TimeDelta::days(120));
        let edge = at(sample(), now - TimeDelta::days(89) - TimeDelta::hours(1));
        let recent = at(sample(), now - TimeDelta::days(30));
        for round in [&ancient, &edge, &recent] {
            append(&path, round, LIMITS);
        }
        assert_eq!(line_count(&path), 3);

        let current = at(sample(), now);
        append(&path, &current, LIMITS);
        assert_eq!(read_from(&path, None), vec![recent, current]);
    }

    #[test]
    fn test_rounds_inside_the_age_budget_are_not_rewritten() {
        let home = tempdir().unwrap();
        let path = home.path().join(LOG_NAME);
        let now = Utc::now();
        let first = at(sample(), now - TimeDelta::days(89));
        append(&path, &first, LIMITS);
        assert!(!needs_compaction(&path, 1, now, LIMITS));
        assert!(needs_compaction(&path, 1, now + TimeDelta::days(2), LIMITS));
    }

    #[test]
    fn test_a_torn_first_line_is_compacted_away() {
        let home = tempdir().unwrap();
        let path = home.path().join(LOG_NAME);
        std::fs::write(&path, "{\"at\":\"torn\n").unwrap();

        let round = sample();
        append(&path, &round, LIMITS);
        assert_eq!(read_from(&path, None), vec![round]);
        assert_eq!(line_count(&path), 1);
    }

    #[test]
    fn test_a_directory_in_the_logs_place_is_left_alone() {
        let home = tempdir().unwrap();
        let path = home.path().join(LOG_NAME);
        std::fs::create_dir(&path).unwrap();

        append(&path, &sample(), LIMITS);
        assert!(path.is_dir());
        assert!(read_from(&path, None).is_empty());
    }

    #[test]
    fn test_concurrent_appends_keep_every_line_whole() {
        let home = tempdir().unwrap();
        let path = home.path().join(LOG_NAME);
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for i in 0..25 {
                        let round = sample().in_session(&format!("t{t}-{i}"), i);
                        append(&path, &round, LIMITS);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }

        assert_eq!(line_count(&path), 200);
        assert_eq!(read_from(&path, None).len(), 200);
    }
}
