use std::sync::Mutex;

/// Global mutex to serialize tests that manipulate environment variables.
///
/// Multiple test modules modify the same env vars (e.g. `ACTUAL_CONFIG`,
/// `ACTUAL_CONFIG_DIR`). Each module previously had its own `ENV_MUTEX`,
/// but those only serialized within a single module — tests across modules
/// still raced. This single shared mutex prevents that.
pub static ENV_MUTEX: Mutex<()> = Mutex::new(());
