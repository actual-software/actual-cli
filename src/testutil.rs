use std::sync::Mutex;

/// Global mutex to serialize tests that manipulate environment variables.
///
/// Multiple test modules modify the same env vars (e.g. `ACTUAL_CONFIG`,
/// `ACTUAL_CONFIG_DIR`). Each module previously had its own `ENV_MUTEX`,
/// but those only serialized within a single module — tests across modules
/// still raced. This single shared mutex prevents that.
pub static ENV_MUTEX: Mutex<()> = Mutex::new(());

/// RAII guard that saves and restores (or removes) an environment variable.
///
/// On construction, saves the previous value and sets the new one.
/// On drop, restores the previous value (or removes the variable if it
/// was absent before). Requires `ENV_MUTEX` to be held by the caller.
pub struct EnvGuard {
    key: String,
    old: Option<String>,
}

impl EnvGuard {
    /// Set `key` to `val`, saving the previous value for restoration on drop.
    ///
    /// The caller must hold `ENV_MUTEX` (or equivalent) before calling this
    /// function to serialise access, as required by the deprecated
    /// `set_var`/`remove_var` APIs.
    pub fn set(key: &str, val: &str) -> Self {
        let old = std::env::var(key).ok();
        // SAFETY: caller holds ENV_MUTEX before calling this.
        #[allow(deprecated)]
        unsafe {
            std::env::set_var(key, val)
        };
        Self {
            key: key.to_string(),
            old,
        }
    }

    /// Remove `key`, saving the previous value for restoration on drop.
    ///
    /// The caller must hold `ENV_MUTEX` (or equivalent) before calling this
    /// function to serialise access, as required by the deprecated
    /// `set_var`/`remove_var` APIs.
    pub fn remove(key: &str) -> Self {
        let old = std::env::var(key).ok();
        // SAFETY: caller holds ENV_MUTEX before calling this.
        #[allow(deprecated)]
        unsafe {
            std::env::remove_var(key)
        };
        Self {
            key: key.to_string(),
            old,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.old {
            Some(v) => {
                // SAFETY: caller holds ENV_MUTEX for the lifetime of this guard.
                #[allow(deprecated)]
                unsafe {
                    std::env::set_var(&self.key, v)
                }
            }
            None => {
                // SAFETY: caller holds ENV_MUTEX for the lifetime of this guard.
                #[allow(deprecated)]
                unsafe {
                    std::env::remove_var(&self.key)
                }
            }
        }
    }
}

/// A config file holding `telemetry.enabled: false` beside an invalid value.
/// The whole file fails to load, the opt-out with it, so a telemetry sender
/// must treat it as opted out.
#[cfg(feature = "telemetry")]
pub const UNLOADABLE_TELEMETRY_OPT_OUT: &str = "telemetry:\n  enabled: false\nbatch_size: lots\n";

/// The positive control for [`UNLOADABLE_TELEMETRY_OPT_OUT`]: the same
/// unrelated key with a valid value, which loads with telemetry on.
#[cfg(feature = "telemetry")]
pub const LOADABLE_TELEMETRY_ON: &str = "batch_size: 10\n";

/// Two v2 rule documents with one file stem (`same-stem.md`) in different
/// directories, each a distinct decision with its own `adr_id`,
/// `candidate_group_id` and root, so only the path tells them apart. Discovery
/// does not descend into subdirectories, so this bypasses it on purpose to test
/// the index and the output over a slug collision.
pub fn colliding_index() -> crate::rules::scope::index::ScopeIndex {
    use std::path::{Path, PathBuf};

    let text = |id: &str, title: &str| {
        format!(
            "---\nversion: 2\nmetadata:\n  actual:\n    adr_id: {id}\n    candidate_group_id: grp-{id}\n  scope:\n    root: packages/{id}\n    paths:\n      - \"src/**\"\n---\n\n# {title}\n\n### Rules\n\n- **R-A-001** MUST: a rule.\n"
        )
    };
    let documents = [("one", "Alpha Rules"), ("two", "Beta Rules")]
        .iter()
        .map(|(id, title)| {
            crate::rules::parse_rule_document(
                &PathBuf::from(format!("/repo/.actual/rules/{id}/same-stem.md")),
                &text(id, title),
            )
            .unwrap()
        })
        .collect();
    crate::rules::scope::index::ScopeIndex::build(
        &crate::rules::RuleSetLoadReport {
            rules_dir: PathBuf::from("/repo/.actual/rules"),
            documents,
            errors: Vec::new(),
            digest: String::new(),
        },
        Path::new("/repo"),
        "fp".to_string(),
    )
}
