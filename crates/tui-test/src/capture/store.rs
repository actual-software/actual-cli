use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::capture::config::CaptureConfig;
use crate::capture::timeline::{EventType, Timeline, TimelineEntry};
use crate::screen::ScreenSnapshot;
use crate::TuiTestError;

/// Records timeline events and screenshots during a TUI test session.
///
/// Manages numbered PNG screenshots and a structured `timeline.json` file
/// that records all session events with timestamps.
pub struct CaptureStore {
    config: CaptureConfig,
    timeline: Timeline,
    start_time: Instant,
    next_screenshot_index: u32,
    last_capture_generation: u64,
    last_capture_time: Instant,
    saved: bool,
}

impl CaptureStore {
    /// Create a new capture store with the given configuration.
    pub fn new(config: CaptureConfig, command: &str, cols: u16, rows: u16) -> Self {
        let now = Instant::now();
        Self {
            config,
            timeline: Timeline {
                terminal_size: [cols, rows],
                command: command.to_string(),
                entries: Vec::new(),
            },
            start_time: now,
            next_screenshot_index: 0,
            last_capture_generation: 0,
            last_capture_time: now,
            saved: false,
        }
    }

    /// Record the start of a session.
    pub fn record_session_start(&mut self, screen_text: &str) {
        self.timeline.entries.push(TimelineEntry {
            timestamp_ms: self.elapsed_ms(),
            event_type: EventType::SessionStart,
            generation: Some(0),
            screenshot: None,
            screen_text: Some(screen_text.to_string()),
            key: None,
            text: None,
            exit_code: None,
        });
    }

    /// Record a screen change, optionally capturing a screenshot if debounce allows.
    ///
    /// The `render_fn` closure is only called if the debounce conditions are met.
    /// Returns `true` if a screenshot was captured, `false` if debounced.
    pub fn record_screen_changed<F>(
        &mut self,
        snapshot: &ScreenSnapshot,
        render_fn: F,
    ) -> Result<bool, TuiTestError>
    where
        F: FnOnce() -> Result<Vec<u8>, TuiTestError>,
    {
        let current_generation = snapshot.generation();
        let screen_text = snapshot.contents();

        if self.should_capture(current_generation) {
            let png_bytes = render_fn()?;
            let filename = self.write_png(&png_bytes)?;
            self.last_capture_generation = current_generation;
            self.last_capture_time = Instant::now();

            self.timeline.entries.push(TimelineEntry {
                timestamp_ms: self.elapsed_ms(),
                event_type: EventType::ScreenChanged,
                generation: Some(current_generation),
                screenshot: Some(filename),
                screen_text: Some(screen_text),
                key: None,
                text: None,
                exit_code: None,
            });
            Ok(true)
        } else {
            self.timeline.entries.push(TimelineEntry {
                timestamp_ms: self.elapsed_ms(),
                event_type: EventType::ScreenChanged,
                generation: Some(current_generation),
                screenshot: None,
                screen_text: Some(screen_text),
                key: None,
                text: None,
                exit_code: None,
            });
            Ok(false)
        }
    }

    /// Record that a key was sent.
    pub fn record_key_sent(&mut self, key_name: &str) {
        self.timeline.entries.push(TimelineEntry {
            timestamp_ms: self.elapsed_ms(),
            event_type: EventType::KeySent,
            generation: None,
            screenshot: None,
            screen_text: None,
            key: Some(key_name.to_string()),
            text: None,
            exit_code: None,
        });
    }

    /// Record that text was typed.
    pub fn record_text_typed(&mut self, text: &str) {
        self.timeline.entries.push(TimelineEntry {
            timestamp_ms: self.elapsed_ms(),
            event_type: EventType::TextTyped,
            generation: None,
            screenshot: None,
            screen_text: None,
            key: None,
            text: Some(text.to_string()),
            exit_code: None,
        });
    }

    /// Record a terminal resize event.
    pub fn record_resize(&mut self, cols: u16, rows: u16) {
        self.timeline.terminal_size = [cols, rows];
        self.timeline.entries.push(TimelineEntry {
            timestamp_ms: self.elapsed_ms(),
            event_type: EventType::Resize,
            generation: None,
            screenshot: None,
            screen_text: None,
            key: None,
            text: None,
            exit_code: None,
        });
    }

    /// Take an explicit screenshot, ignoring debounce settings.
    ///
    /// Returns the filename of the saved screenshot.
    pub fn capture_screenshot<F>(
        &mut self,
        snapshot: &ScreenSnapshot,
        render_fn: F,
    ) -> Result<String, TuiTestError>
    where
        F: FnOnce() -> Result<Vec<u8>, TuiTestError>,
    {
        let png_bytes = render_fn()?;
        let filename = self.write_png(&png_bytes)?;
        let screen_text = snapshot.contents();

        self.timeline.entries.push(TimelineEntry {
            timestamp_ms: self.elapsed_ms(),
            event_type: EventType::Screenshot,
            generation: Some(snapshot.generation()),
            screenshot: Some(filename.clone()),
            screen_text: Some(screen_text),
            key: None,
            text: None,
            exit_code: None,
        });

        Ok(filename)
    }

    /// Record the end of a session.
    pub fn record_session_end(&mut self, exit_code: Option<u32>, screen_text: Option<&str>) {
        self.timeline.entries.push(TimelineEntry {
            timestamp_ms: self.elapsed_ms(),
            event_type: EventType::SessionEnd,
            generation: None,
            screenshot: None,
            screen_text: screen_text.map(String::from),
            key: None,
            text: None,
            exit_code,
        });
    }

    /// Save the timeline to `timeline.json` in the output directory.
    ///
    /// Returns the path to the saved timeline file.
    pub fn save(&mut self) -> Result<PathBuf, TuiTestError> {
        self.ensure_dir()?;
        let path = self.config.output_dir.join("timeline.json");
        let json = serde_json::to_string_pretty(&self.timeline)
            .map_err(|e| TuiTestError::Capture(format!("Failed to serialize timeline: {e}")))?;
        std::fs::write(&path, json)
            .map_err(|e| TuiTestError::Capture(format!("Failed to write timeline: {e}")))?;
        self.saved = true;
        Ok(path)
    }

    /// Return a reference to the timeline.
    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    /// Return the output directory path.
    pub fn output_dir(&self) -> &Path {
        &self.config.output_dir
    }

    /// Return the number of screenshots taken so far.
    pub fn screenshot_count(&self) -> u32 {
        self.next_screenshot_index
    }

    /// Return a reference to the capture configuration.
    pub fn config(&self) -> &CaptureConfig {
        &self.config
    }

    /// Return whether the timeline has been saved.
    pub fn is_saved(&self) -> bool {
        self.saved
    }

    /// Return the elapsed time in milliseconds since the store was created.
    fn elapsed_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    /// Check whether a new auto-capture should be taken based on debounce settings.
    fn should_capture(&self, current_generation: u64) -> bool {
        let generation_ok =
            current_generation >= self.last_capture_generation + self.config.generation_debounce;
        let time_ok =
            self.last_capture_time.elapsed().as_millis() as u64 >= self.config.min_interval_ms;
        generation_ok && time_ok
    }

    /// Write PNG bytes to a numbered file in the output directory.
    fn write_png(&mut self, png_bytes: &[u8]) -> Result<String, TuiTestError> {
        self.ensure_dir()?;
        let filename = format!("{:03}.png", self.next_screenshot_index);
        let path = self.config.output_dir.join(&filename);
        std::fs::write(&path, png_bytes)
            .map_err(|e| TuiTestError::Capture(format!("Failed to write PNG {filename}: {e}")))?;
        self.next_screenshot_index += 1;
        Ok(filename)
    }

    /// Ensure the output directory exists.
    fn ensure_dir(&self) -> Result<(), TuiTestError> {
        if !self.config.output_dir.exists() {
            std::fs::create_dir_all(&self.config.output_dir).map_err(|e| {
                TuiTestError::Capture(format!(
                    "Failed to create output directory {:?}: {e}",
                    self.config.output_dir
                ))
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a test ScreenSnapshot from text.
    fn make_snapshot(text: &str, generation: u64) -> ScreenSnapshot {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(text.as_bytes());
        ScreenSnapshot::new_for_test(parser.screen().clone(), generation)
    }

    /// Fake PNG data (valid enough for our write tests).
    fn fake_png() -> Vec<u8> {
        vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
    }

    #[test]
    fn test_new_store() {
        let config = CaptureConfig::default();
        let store = CaptureStore::new(config, "test-cmd", 80, 24);
        assert_eq!(store.timeline().terminal_size, [80, 24]);
        assert_eq!(store.timeline().command, "test-cmd");
        assert!(store.timeline().entries.is_empty());
        assert_eq!(store.screenshot_count(), 0);
        assert!(!store.is_saved());
    }

    #[test]
    fn test_record_session_start() {
        let config = CaptureConfig::default();
        let mut store = CaptureStore::new(config, "test", 80, 24);
        store.record_session_start("initial screen");
        assert_eq!(store.timeline().entries.len(), 1);
        let entry = &store.timeline().entries[0];
        assert_eq!(entry.event_type, EventType::SessionStart);
        assert_eq!(entry.generation, Some(0));
        assert_eq!(entry.screen_text.as_deref(), Some("initial screen"));
    }

    #[test]
    fn test_record_key_sent() {
        let config = CaptureConfig::default();
        let mut store = CaptureStore::new(config, "test", 80, 24);
        store.record_key_sent("Enter");
        assert_eq!(store.timeline().entries.len(), 1);
        let entry = &store.timeline().entries[0];
        assert_eq!(entry.event_type, EventType::KeySent);
        assert_eq!(entry.key.as_deref(), Some("Enter"));
    }

    #[test]
    fn test_record_text_typed() {
        let config = CaptureConfig::default();
        let mut store = CaptureStore::new(config, "test", 80, 24);
        store.record_text_typed("hello world");
        assert_eq!(store.timeline().entries.len(), 1);
        let entry = &store.timeline().entries[0];
        assert_eq!(entry.event_type, EventType::TextTyped);
        assert_eq!(entry.text.as_deref(), Some("hello world"));
    }

    #[test]
    fn test_record_resize() {
        let config = CaptureConfig::default();
        let mut store = CaptureStore::new(config, "test", 80, 24);
        store.record_resize(120, 40);
        assert_eq!(store.timeline().terminal_size, [120, 40]);
        assert_eq!(store.timeline().entries.len(), 1);
        assert_eq!(store.timeline().entries[0].event_type, EventType::Resize);
    }

    #[test]
    fn test_record_session_end() {
        let config = CaptureConfig::default();
        let mut store = CaptureStore::new(config, "test", 80, 24);
        store.record_session_end(Some(0), Some("final screen"));
        assert_eq!(store.timeline().entries.len(), 1);
        let entry = &store.timeline().entries[0];
        assert_eq!(entry.event_type, EventType::SessionEnd);
        assert_eq!(entry.exit_code, Some(0));
        assert_eq!(entry.screen_text.as_deref(), Some("final screen"));
    }

    #[test]
    fn test_record_session_end_no_exit_code() {
        let config = CaptureConfig::default();
        let mut store = CaptureStore::new(config, "test", 80, 24);
        store.record_session_end(None, None);
        let entry = &store.timeline().entries[0];
        assert_eq!(entry.exit_code, None);
        assert_eq!(entry.screen_text, None);
    }

    #[test]
    fn test_record_screen_changed_captures() {
        let tmp = TempDir::new().unwrap();
        let config = CaptureConfig {
            output_dir: tmp.path().to_path_buf(),
            generation_debounce: 1,
            min_interval_ms: 0,
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "test", 80, 24);
        let snap = make_snapshot("hello", 1);

        let captured = store
            .record_screen_changed(&snap, || Ok(fake_png()))
            .unwrap();
        assert!(captured);
        assert_eq!(store.screenshot_count(), 1);
        assert!(tmp.path().join("000.png").exists());

        let entry = &store.timeline().entries[0];
        assert_eq!(entry.event_type, EventType::ScreenChanged);
        assert_eq!(entry.screenshot.as_deref(), Some("000.png"));
        assert_eq!(entry.generation, Some(1));
    }

    #[test]
    fn test_record_screen_changed_debounced() {
        let tmp = TempDir::new().unwrap();
        let config = CaptureConfig {
            output_dir: tmp.path().to_path_buf(),
            generation_debounce: 5,
            min_interval_ms: 0,
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "test", 80, 24);

        // Generation 1 is not enough (debounce is 5, last was 0, need >= 5)
        let snap = make_snapshot("hello", 1);
        let captured = store
            .record_screen_changed(&snap, || Ok(fake_png()))
            .unwrap();
        assert!(!captured);
        assert_eq!(store.screenshot_count(), 0);

        // Entry still recorded, just no screenshot
        let entry = &store.timeline().entries[0];
        assert_eq!(entry.event_type, EventType::ScreenChanged);
        assert!(entry.screenshot.is_none());
    }

    #[test]
    fn test_record_screen_changed_time_debounce() {
        let tmp = TempDir::new().unwrap();
        let config = CaptureConfig {
            output_dir: tmp.path().to_path_buf(),
            generation_debounce: 1,
            min_interval_ms: 10_000, // 10 seconds — won't be met in test
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "test", 80, 24);

        let snap = make_snapshot("hello", 1);
        let captured = store
            .record_screen_changed(&snap, || Ok(fake_png()))
            .unwrap();
        // Generation debounce passes (1 >= 0 + 1) but time debounce fails
        assert!(!captured);
    }

    #[test]
    fn test_capture_screenshot_ignores_debounce() {
        let tmp = TempDir::new().unwrap();
        let config = CaptureConfig {
            output_dir: tmp.path().to_path_buf(),
            generation_debounce: 100, // high debounce
            min_interval_ms: 100_000, // high time debounce
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "test", 80, 24);
        let snap = make_snapshot("hello", 1);

        let filename = store.capture_screenshot(&snap, || Ok(fake_png())).unwrap();
        assert_eq!(filename, "000.png");
        assert_eq!(store.screenshot_count(), 1);
        assert!(tmp.path().join("000.png").exists());

        let entry = &store.timeline().entries[0];
        assert_eq!(entry.event_type, EventType::Screenshot);
        assert_eq!(entry.screenshot.as_deref(), Some("000.png"));
    }

    #[test]
    fn test_screenshot_numbering() {
        let tmp = TempDir::new().unwrap();
        let config = CaptureConfig {
            output_dir: tmp.path().to_path_buf(),
            generation_debounce: 1,
            min_interval_ms: 0,
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "test", 80, 24);

        for i in 0..3 {
            let snap = make_snapshot(&format!("gen{i}"), (i + 1) as u64);
            store.capture_screenshot(&snap, || Ok(fake_png())).unwrap();
        }

        assert_eq!(store.screenshot_count(), 3);
        assert!(tmp.path().join("000.png").exists());
        assert!(tmp.path().join("001.png").exists());
        assert!(tmp.path().join("002.png").exists());
    }

    #[test]
    fn test_save_creates_timeline_json() {
        let tmp = TempDir::new().unwrap();
        let config = CaptureConfig {
            output_dir: tmp.path().to_path_buf(),
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "test-cmd", 80, 24);
        store.record_session_start("start");
        store.record_key_sent("Enter");
        store.record_session_end(Some(0), Some("end"));

        let path = store.save().unwrap();
        assert!(path.exists());
        assert!(store.is_saved());

        let content = std::fs::read_to_string(&path).unwrap();
        let timeline: Timeline = serde_json::from_str(&content).unwrap();
        assert_eq!(timeline.command, "test-cmd");
        assert_eq!(timeline.terminal_size, [80, 24]);
        assert_eq!(timeline.entries.len(), 3);
    }

    #[test]
    fn test_save_creates_output_dir() {
        let tmp = TempDir::new().unwrap();
        let nested = tmp.path().join("nested").join("dir");
        let config = CaptureConfig {
            output_dir: nested.clone(),
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "test", 80, 24);
        store.record_session_start("start");

        let path = store.save().unwrap();
        assert!(path.exists());
        assert!(nested.exists());
    }

    #[test]
    fn test_output_dir() {
        let config = CaptureConfig {
            output_dir: PathBuf::from("/tmp/test-captures"),
            ..CaptureConfig::default()
        };
        let store = CaptureStore::new(config, "test", 80, 24);
        assert_eq!(store.output_dir(), Path::new("/tmp/test-captures"));
    }

    #[test]
    fn test_config_accessor() {
        let config = CaptureConfig {
            generation_debounce: 42,
            ..CaptureConfig::default()
        };
        let store = CaptureStore::new(config, "test", 80, 24);
        assert_eq!(store.config().generation_debounce, 42);
    }

    #[test]
    fn test_record_screen_changed_render_fn_error() {
        let tmp = TempDir::new().unwrap();
        let config = CaptureConfig {
            output_dir: tmp.path().to_path_buf(),
            generation_debounce: 1,
            min_interval_ms: 0,
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "test", 80, 24);
        let snap = make_snapshot("hello", 1);

        let result = store.record_screen_changed(&snap, || {
            Err(TuiTestError::Render("render failed".to_string()))
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_capture_screenshot_render_fn_error() {
        let tmp = TempDir::new().unwrap();
        let config = CaptureConfig {
            output_dir: tmp.path().to_path_buf(),
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "test", 80, 24);
        let snap = make_snapshot("hello", 1);

        let result = store.capture_screenshot(&snap, || {
            Err(TuiTestError::Render("render failed".to_string()))
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_full_lifecycle() {
        let tmp = TempDir::new().unwrap();
        let config = CaptureConfig {
            output_dir: tmp.path().to_path_buf(),
            generation_debounce: 1,
            min_interval_ms: 0,
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "my-app", 80, 24);

        store.record_session_start("$ ");
        store.record_key_sent("h");
        store.record_text_typed("ello");

        let snap1 = make_snapshot("hello", 1);
        store
            .record_screen_changed(&snap1, || Ok(fake_png()))
            .unwrap();

        store.record_resize(120, 40);

        let snap2 = make_snapshot("resized", 2);
        store.capture_screenshot(&snap2, || Ok(fake_png())).unwrap();

        store.record_session_end(Some(0), Some("done"));

        let path = store.save().unwrap();
        assert!(path.exists());
        assert_eq!(store.screenshot_count(), 2);
        assert_eq!(store.timeline().entries.len(), 7);

        // Verify timeline file is valid JSON
        let content = std::fs::read_to_string(&path).unwrap();
        let timeline: Timeline = serde_json::from_str(&content).unwrap();
        assert_eq!(timeline.entries.len(), 7);
        assert_eq!(timeline.entries[0].event_type, EventType::SessionStart);
        assert_eq!(timeline.entries[1].event_type, EventType::KeySent);
        assert_eq!(timeline.entries[2].event_type, EventType::TextTyped);
        assert_eq!(timeline.entries[3].event_type, EventType::ScreenChanged);
        assert_eq!(timeline.entries[4].event_type, EventType::Resize);
        assert_eq!(timeline.entries[5].event_type, EventType::Screenshot);
        assert_eq!(timeline.entries[6].event_type, EventType::SessionEnd);
    }

    #[test]
    fn test_debounce_generation_boundary() {
        let tmp = TempDir::new().unwrap();
        let config = CaptureConfig {
            output_dir: tmp.path().to_path_buf(),
            generation_debounce: 2,
            min_interval_ms: 0,
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "test", 80, 24);

        // Generation 1: not enough (need >= 0 + 2 = 2)
        let snap1 = make_snapshot("g1", 1);
        let captured1 = store
            .record_screen_changed(&snap1, || Ok(fake_png()))
            .unwrap();
        assert!(!captured1);

        // Generation 2: exactly at boundary
        let snap2 = make_snapshot("g2", 2);
        let captured2 = store
            .record_screen_changed(&snap2, || Ok(fake_png()))
            .unwrap();
        assert!(captured2);

        // Generation 3: not enough (need >= 2 + 2 = 4)
        let snap3 = make_snapshot("g3", 3);
        let captured3 = store
            .record_screen_changed(&snap3, || Ok(fake_png()))
            .unwrap();
        assert!(!captured3);

        // Generation 4: exactly at boundary
        let snap4 = make_snapshot("g4", 4);
        let captured4 = store
            .record_screen_changed(&snap4, || Ok(fake_png()))
            .unwrap();
        assert!(captured4);
    }

    #[test]
    fn test_write_png_to_nonexistent_deeply_nested_dir() {
        let tmp = TempDir::new().unwrap();
        let deep_dir = tmp.path().join("a").join("b").join("c");
        let config = CaptureConfig {
            output_dir: deep_dir.clone(),
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "test", 80, 24);
        let snap = make_snapshot("hello", 1);

        let filename = store.capture_screenshot(&snap, || Ok(fake_png())).unwrap();
        assert_eq!(filename, "000.png");
        assert!(deep_dir.join("000.png").exists());
    }

    #[test]
    fn test_timestamps_are_non_decreasing() {
        let config = CaptureConfig::default();
        let mut store = CaptureStore::new(config, "test", 80, 24);
        store.record_session_start("start");
        store.record_key_sent("a");
        store.record_text_typed("bc");
        store.record_session_end(None, None);

        let timestamps: Vec<u64> = store
            .timeline()
            .entries
            .iter()
            .map(|e| e.timestamp_ms)
            .collect();
        for window in timestamps.windows(2) {
            assert!(window[0] <= window[1], "Timestamps must be non-decreasing");
        }
    }

    #[test]
    fn test_multiple_saves() {
        let tmp = TempDir::new().unwrap();
        let config = CaptureConfig {
            output_dir: tmp.path().to_path_buf(),
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "test", 80, 24);
        store.record_session_start("start");

        let path1 = store.save().unwrap();
        store.record_key_sent("x");
        let path2 = store.save().unwrap();

        assert_eq!(path1, path2);
        // Second save should have more entries
        let content = std::fs::read_to_string(&path2).unwrap();
        let timeline: Timeline = serde_json::from_str(&content).unwrap();
        assert_eq!(timeline.entries.len(), 2);
    }

    #[test]
    fn test_save_to_invalid_path() {
        // Use a path that can't be created (nested under a file, not a dir)
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("not_a_dir");
        std::fs::write(&file_path, b"i am a file").unwrap();
        // Try to use a path nested under this file (which isn't a directory)
        let config = CaptureConfig {
            output_dir: file_path.join("subdir"),
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "test", 80, 24);
        store.record_session_start("start");

        let result = store.save();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, TuiTestError::Capture(ref msg) if msg.contains("Failed to create output directory")),
            "Expected Capture error about directory creation, got: {err:?}"
        );
    }

    #[test]
    fn test_write_png_to_invalid_path() {
        // Create an output dir that's actually a file so write fails
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("not_a_dir");
        std::fs::write(&file_path, b"i am a file").unwrap();

        let config = CaptureConfig {
            output_dir: file_path.join("subdir"),
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "test", 80, 24);
        let snap = make_snapshot("hello", 1);

        let result = store.capture_screenshot(&snap, || Ok(fake_png()));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, TuiTestError::Capture(ref msg) if msg.contains("Failed to create output directory")),
            "Expected Capture error, got: {err:?}"
        );
    }

    #[test]
    fn test_save_write_failure() {
        // Create the output dir, then make timeline.json a directory so write fails
        let tmp = TempDir::new().unwrap();
        let output_dir = tmp.path().join("output");
        std::fs::create_dir_all(&output_dir).unwrap();
        // Create timeline.json as a directory so fs::write fails
        std::fs::create_dir_all(output_dir.join("timeline.json")).unwrap();

        let config = CaptureConfig {
            output_dir,
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "test", 80, 24);
        store.record_session_start("start");

        let result = store.save();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, TuiTestError::Capture(ref msg) if msg.contains("Failed to write timeline")),
            "Expected Capture error about write failure, got: {err:?}"
        );
    }

    #[test]
    fn test_write_png_file_write_failure() {
        // Create the output dir, then make the PNG filename a directory so write fails
        let tmp = TempDir::new().unwrap();
        let output_dir = tmp.path().join("output");
        std::fs::create_dir_all(&output_dir).unwrap();
        // Create 000.png as a directory so fs::write fails
        std::fs::create_dir_all(output_dir.join("000.png")).unwrap();

        let config = CaptureConfig {
            output_dir,
            generation_debounce: 1,
            min_interval_ms: 0,
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "test", 80, 24);
        let snap = make_snapshot("hello", 1);

        let result = store.capture_screenshot(&snap, || Ok(fake_png()));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, TuiTestError::Capture(ref msg) if msg.contains("Failed to write PNG")),
            "Expected Capture error about PNG write failure, got: {err:?}"
        );
    }

    #[test]
    fn test_ensure_dir_already_exists() {
        // ensure_dir should succeed when dir already exists
        let tmp = TempDir::new().unwrap();
        let config = CaptureConfig {
            output_dir: tmp.path().to_path_buf(),
            generation_debounce: 1,
            min_interval_ms: 0,
            ..CaptureConfig::default()
        };
        let mut store = CaptureStore::new(config, "test", 80, 24);

        // Directory already exists (created by TempDir), should not fail
        let snap = make_snapshot("hello", 1);
        let result = store.capture_screenshot(&snap, || Ok(fake_png()));
        assert!(result.is_ok());
        assert!(tmp.path().join("000.png").exists());
    }
}
