use serde::{Deserialize, Serialize};

/// The complete timeline of a test session, including metadata and events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Timeline {
    /// Terminal size as `[cols, rows]`.
    pub terminal_size: [u16; 2],
    /// The command that was run.
    pub command: String,
    /// Ordered list of timeline entries.
    pub entries: Vec<TimelineEntry>,
}

/// A single event in the session timeline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimelineEntry {
    /// Milliseconds elapsed since session start.
    pub timestamp_ms: u64,
    /// The type of event.
    #[serde(rename = "type")]
    pub event_type: EventType,
    /// Screen generation counter (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    /// Path to screenshot file (if one was taken).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    /// Plain text of the screen at this point.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen_text: Option<String>,
    /// Key name that was sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Text that was typed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Process exit code (for session end events).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<u32>,
}

/// Types of events that can be recorded in the timeline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    /// Session started.
    SessionStart,
    /// Screen content changed.
    ScreenChanged,
    /// A key was sent.
    KeySent,
    /// Text was typed.
    TextTyped,
    /// An explicit screenshot was taken.
    Screenshot,
    /// Terminal was resized.
    Resize,
    /// Session ended.
    SessionEnd,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_type_serialization() {
        assert_eq!(
            serde_json::to_string(&EventType::SessionStart).unwrap(),
            "\"session_start\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::ScreenChanged).unwrap(),
            "\"screen_changed\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::KeySent).unwrap(),
            "\"key_sent\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::TextTyped).unwrap(),
            "\"text_typed\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::Screenshot).unwrap(),
            "\"screenshot\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::Resize).unwrap(),
            "\"resize\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::SessionEnd).unwrap(),
            "\"session_end\""
        );
    }

    #[test]
    fn test_event_type_deserialization() {
        assert_eq!(
            serde_json::from_str::<EventType>("\"session_start\"").unwrap(),
            EventType::SessionStart
        );
        assert_eq!(
            serde_json::from_str::<EventType>("\"screen_changed\"").unwrap(),
            EventType::ScreenChanged
        );
        assert_eq!(
            serde_json::from_str::<EventType>("\"key_sent\"").unwrap(),
            EventType::KeySent
        );
        assert_eq!(
            serde_json::from_str::<EventType>("\"text_typed\"").unwrap(),
            EventType::TextTyped
        );
        assert_eq!(
            serde_json::from_str::<EventType>("\"screenshot\"").unwrap(),
            EventType::Screenshot
        );
        assert_eq!(
            serde_json::from_str::<EventType>("\"resize\"").unwrap(),
            EventType::Resize
        );
        assert_eq!(
            serde_json::from_str::<EventType>("\"session_end\"").unwrap(),
            EventType::SessionEnd
        );
    }

    #[test]
    fn test_timeline_entry_serialization_minimal() {
        let entry = TimelineEntry {
            timestamp_ms: 100,
            event_type: EventType::KeySent,
            generation: None,
            screenshot: None,
            screen_text: None,
            key: Some("Enter".to_string()),
            text: None,
            exit_code: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"timestamp_ms\":100"));
        assert!(json.contains("\"type\":\"key_sent\""));
        assert!(json.contains("\"key\":\"Enter\""));
        // None fields should be skipped
        assert!(!json.contains("generation"));
        assert!(!json.contains("screenshot"));
        assert!(!json.contains("screen_text"));
        assert!(!json.contains("text"));
        assert!(!json.contains("exit_code"));
    }

    #[test]
    fn test_timeline_entry_serialization_full() {
        let entry = TimelineEntry {
            timestamp_ms: 500,
            event_type: EventType::ScreenChanged,
            generation: Some(3),
            screenshot: Some("003.png".to_string()),
            screen_text: Some("hello world".to_string()),
            key: None,
            text: None,
            exit_code: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"generation\":3"));
        assert!(json.contains("\"screenshot\":\"003.png\""));
        assert!(json.contains("\"screen_text\":\"hello world\""));
    }

    #[test]
    fn test_timeline_entry_deserialization() {
        let json = r#"{"timestamp_ms":100,"type":"key_sent","key":"Enter"}"#;
        let entry: TimelineEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.timestamp_ms, 100);
        assert_eq!(entry.event_type, EventType::KeySent);
        assert_eq!(entry.key, Some("Enter".to_string()));
        assert_eq!(entry.generation, None);
        assert_eq!(entry.screenshot, None);
    }

    #[test]
    fn test_timeline_serialization_roundtrip() {
        let timeline = Timeline {
            terminal_size: [80, 24],
            command: "test-app".to_string(),
            entries: vec![
                TimelineEntry {
                    timestamp_ms: 0,
                    event_type: EventType::SessionStart,
                    generation: Some(0),
                    screenshot: None,
                    screen_text: Some("$ ".to_string()),
                    key: None,
                    text: None,
                    exit_code: None,
                },
                TimelineEntry {
                    timestamp_ms: 100,
                    event_type: EventType::KeySent,
                    generation: None,
                    screenshot: None,
                    screen_text: None,
                    key: Some("Enter".to_string()),
                    text: None,
                    exit_code: None,
                },
                TimelineEntry {
                    timestamp_ms: 1000,
                    event_type: EventType::SessionEnd,
                    generation: None,
                    screenshot: None,
                    screen_text: None,
                    key: None,
                    text: None,
                    exit_code: Some(0),
                },
            ],
        };

        let json = serde_json::to_string_pretty(&timeline).unwrap();
        let deserialized: Timeline = serde_json::from_str(&json).unwrap();
        assert_eq!(timeline, deserialized);
    }

    #[test]
    fn test_timeline_empty_entries() {
        let timeline = Timeline {
            terminal_size: [120, 40],
            command: "empty".to_string(),
            entries: vec![],
        };
        let json = serde_json::to_string(&timeline).unwrap();
        let deserialized: Timeline = serde_json::from_str(&json).unwrap();
        assert_eq!(timeline, deserialized);
        assert!(deserialized.entries.is_empty());
    }

    #[test]
    fn test_timeline_entry_session_end_with_exit_code() {
        let entry = TimelineEntry {
            timestamp_ms: 5000,
            event_type: EventType::SessionEnd,
            generation: None,
            screenshot: None,
            screen_text: Some("done".to_string()),
            key: None,
            text: None,
            exit_code: Some(42),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"exit_code\":42"));
        let deserialized: TimelineEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.exit_code, Some(42));
    }

    #[test]
    fn test_timeline_entry_text_typed() {
        let entry = TimelineEntry {
            timestamp_ms: 200,
            event_type: EventType::TextTyped,
            generation: None,
            screenshot: None,
            screen_text: None,
            key: None,
            text: Some("hello world".to_string()),
            exit_code: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"text\":\"hello world\""));
        assert!(json.contains("\"type\":\"text_typed\""));
    }

    #[test]
    fn test_timeline_entry_resize() {
        let entry = TimelineEntry {
            timestamp_ms: 300,
            event_type: EventType::Resize,
            generation: None,
            screenshot: None,
            screen_text: Some("resized".to_string()),
            key: None,
            text: None,
            exit_code: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"type\":\"resize\""));
    }

    #[test]
    fn test_event_type_equality() {
        assert_eq!(EventType::SessionStart, EventType::SessionStart);
        assert_ne!(EventType::SessionStart, EventType::SessionEnd);
    }

    #[test]
    fn test_event_type_clone() {
        let event = EventType::Screenshot;
        let cloned = event.clone();
        assert_eq!(event, cloned);
    }

    #[test]
    fn test_timeline_entry_clone() {
        let entry = TimelineEntry {
            timestamp_ms: 100,
            event_type: EventType::KeySent,
            generation: Some(1),
            screenshot: Some("000.png".to_string()),
            screen_text: Some("text".to_string()),
            key: Some("Enter".to_string()),
            text: Some("typed".to_string()),
            exit_code: Some(0),
        };
        let cloned = entry.clone();
        assert_eq!(entry, cloned);
    }

    #[test]
    fn test_timeline_clone() {
        let timeline = Timeline {
            terminal_size: [80, 24],
            command: "test".to_string(),
            entries: vec![],
        };
        let cloned = timeline.clone();
        assert_eq!(timeline, cloned);
    }

    #[test]
    fn test_event_type_debug() {
        let debug_str = format!("{:?}", EventType::SessionStart);
        assert_eq!(debug_str, "SessionStart");
    }

    #[test]
    fn test_timeline_entry_debug() {
        let entry = TimelineEntry {
            timestamp_ms: 0,
            event_type: EventType::SessionStart,
            generation: None,
            screenshot: None,
            screen_text: None,
            key: None,
            text: None,
            exit_code: None,
        };
        let debug_str = format!("{entry:?}");
        assert!(debug_str.contains("TimelineEntry"));
        assert!(debug_str.contains("SessionStart"));
    }

    #[test]
    fn test_timeline_debug() {
        let timeline = Timeline {
            terminal_size: [80, 24],
            command: "test".to_string(),
            entries: vec![],
        };
        let debug_str = format!("{timeline:?}");
        assert!(debug_str.contains("Timeline"));
    }
}
