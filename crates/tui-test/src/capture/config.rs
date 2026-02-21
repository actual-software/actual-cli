use std::path::PathBuf;

/// Configuration for capture persistence during TUI test sessions.
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Directory where screenshots and timeline are saved.
    pub output_dir: PathBuf,
    /// Minimum generation gap between auto-captures during screen changes.
    pub generation_debounce: u64,
    /// Minimum milliseconds between auto-captures.
    pub min_interval_ms: u64,
    /// Whether to automatically capture after `wait_for` succeeds.
    pub auto_capture_on_wait: bool,
    /// Whether to save the timeline on session drop.
    pub save_on_drop: bool,
    /// Whether to take a final screenshot on session drop.
    pub capture_on_drop: bool,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            output_dir: PathBuf::from("tui-test-captures"),
            generation_debounce: 1,
            min_interval_ms: 0,
            auto_capture_on_wait: true,
            save_on_drop: true,
            capture_on_drop: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = CaptureConfig::default();
        assert_eq!(config.output_dir, PathBuf::from("tui-test-captures"));
        assert_eq!(config.generation_debounce, 1);
        assert_eq!(config.min_interval_ms, 0);
        assert!(config.auto_capture_on_wait);
        assert!(config.save_on_drop);
        assert!(config.capture_on_drop);
    }

    #[test]
    fn test_custom_config() {
        let config = CaptureConfig {
            output_dir: PathBuf::from("/tmp/captures"),
            generation_debounce: 5,
            min_interval_ms: 100,
            auto_capture_on_wait: false,
            save_on_drop: false,
            capture_on_drop: false,
        };
        assert_eq!(config.output_dir, PathBuf::from("/tmp/captures"));
        assert_eq!(config.generation_debounce, 5);
        assert_eq!(config.min_interval_ms, 100);
        assert!(!config.auto_capture_on_wait);
        assert!(!config.save_on_drop);
        assert!(!config.capture_on_drop);
    }

    #[test]
    fn test_config_clone() {
        let config = CaptureConfig::default();
        let cloned = config.clone();
        assert_eq!(config.output_dir, cloned.output_dir);
        assert_eq!(config.generation_debounce, cloned.generation_debounce);
        assert_eq!(config.min_interval_ms, cloned.min_interval_ms);
        assert_eq!(config.auto_capture_on_wait, cloned.auto_capture_on_wait);
        assert_eq!(config.save_on_drop, cloned.save_on_drop);
        assert_eq!(config.capture_on_drop, cloned.capture_on_drop);
    }

    #[test]
    fn test_config_debug() {
        let config = CaptureConfig::default();
        let debug_str = format!("{config:?}");
        assert!(debug_str.contains("CaptureConfig"));
        assert!(debug_str.contains("tui-test-captures"));
    }
}
