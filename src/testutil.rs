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
