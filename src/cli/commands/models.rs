//! Implementation of the `actual models` command.
//!
//! Prints known model names grouped by runner family so users can discover
//! valid values for `--model` / `actual config set model`.

use crate::error::ActualError;

/// A single model entry: its ID and whether it is the default for its family.
struct ModelEntry {
    id: &'static str,
    is_default: bool,
    note: Option<&'static str>,
}

impl ModelEntry {
    const fn new(id: &'static str) -> Self {
        Self {
            id,
            is_default: false,
            note: None,
        }
    }

    const fn default_model(id: &'static str) -> Self {
        Self {
            id,
            is_default: true,
            note: None,
        }
    }

    const fn with_note(id: &'static str, note: &'static str) -> Self {
        Self {
            id,
            is_default: false,
            note: Some(note),
        }
    }
}

/// A runner family with its associated models.
struct RunnerFamily {
    name: &'static str,
    runners: &'static [&'static str],
    models: &'static [ModelEntry],
}

// Last audited: 2026-02-23
// Sources:
//   OpenAI live API: https://api.openai.com/v1/models (verified via OPENAI_API_KEY)
//   OpenAI docs: https://platform.openai.com/docs/models
//   OpenAI deprecations: https://platform.openai.com/docs/deprecations
//   Anthropic docs: https://docs.anthropic.com/en/docs/about-claude/models
//
// Models removed from this list (already shut down or deprecated):
//   o1-preview: shutdown 2025-07-28 (removed)
//   chatgpt-4o-latest: shutdown 2026-02-17 (removed — past shutdown date)
//   o1-mini: shutdown 2025-10-27 (not previously listed, excluded)

/// Static list of runner families and their known models.
static RUNNER_FAMILIES: &[RunnerFamily] = &[
    RunnerFamily {
        name: "Claude / Anthropic",
        runners: &["claude-cli", "anthropic-api"],
        // Ordered newest-first within each model line, aliases at the end.
        // Source: https://docs.anthropic.com/en/docs/about-claude/models
        models: &[
            // Current / recommended models (as of 2026-02-23)
            ModelEntry::default_model("claude-sonnet-4-6"),
            ModelEntry::new("claude-opus-4-6"),
            ModelEntry::new("claude-haiku-4-5"),
            // Legacy models (still available via API)
            ModelEntry::new("claude-opus-4-5"),
            ModelEntry::new("claude-opus-4-1"),
            ModelEntry::new("claude-sonnet-4-5"),
            ModelEntry::new("claude-sonnet-4"),
            ModelEntry::new("claude-opus-4"),
            // Short aliases (claude-cli only)
            ModelEntry::with_note("sonnet", "short alias, claude-cli only"),
            ModelEntry::with_note("opus", "short alias, claude-cli only"),
            ModelEntry::with_note("haiku", "short alias, claude-cli only"),
        ],
    },
    RunnerFamily {
        name: "OpenAI",
        runners: &["openai-api"],
        // keep in sync with RUNNER_FAMILIES default for openai-api (see sync_wiring.rs unwrap_or)
        // Source: https://platform.openai.com/docs/models (verified via live API 2026-02-23)
        // Removed: o1-preview (shutdown 2025-07-28), chatgpt-4o-latest (shutdown 2026-02-17)
        models: &[
            ModelEntry::default_model("gpt-5.2"),
            ModelEntry::new("gpt-5.1"),
            ModelEntry::new("gpt-5"),
            ModelEntry::new("gpt-5-mini"),
            ModelEntry::new("gpt-5-nano"),
            ModelEntry::new("gpt-4.1"),
            ModelEntry::new("gpt-4.1-mini"),
            ModelEntry::new("gpt-4.1-nano"),
            ModelEntry::new("gpt-4o"),
            ModelEntry::new("o4-mini"),
            ModelEntry::new("o3"),
            ModelEntry::new("o3-mini"),
            ModelEntry::new("o1"),
        ],
    },
    RunnerFamily {
        name: "Codex CLI",
        runners: &["codex-cli"],
        // OpenAI-family models (gpt-*, o-series) prefer codex-cli over openai-api;
        // they also appear in the OpenAI family above for discoverability.
        // Source: https://platform.openai.com/docs/models (verified via live API 2026-02-23)
        // Removed: o1-preview (shutdown 2025-07-28), chatgpt-4o-latest (shutdown 2026-02-17)
        models: &[
            ModelEntry::default_model("gpt-5.2-codex"),
            ModelEntry::new("gpt-5.1-codex"),
            ModelEntry::new("gpt-5.1-codex-max"),
            ModelEntry::new("gpt-5.1-codex-mini"),
            ModelEntry::new("gpt-5-codex"),
            ModelEntry::new("gpt-4o"),
            ModelEntry::new("o4-mini"),
            ModelEntry::new("o3"),
            ModelEntry::new("o3-mini"),
            ModelEntry::new("o1"),
        ],
    },
    RunnerFamily {
        name: "Cursor",
        runners: &["cursor-cli"],
        models: &[
            ModelEntry::with_note("auto", "Cursor routes to best model for your tier"),
            ModelEntry::new("composer-1.5"),
            ModelEntry::default_model("opus-4.6-thinking"),
            ModelEntry::new("opus-4.6"),
            ModelEntry::new("sonnet-4.6"),
            ModelEntry::new("sonnet-4.6-thinking"),
            ModelEntry::new("gpt-5.2"),
            ModelEntry::new("gemini-3.1-pro"),
            ModelEntry::new("grok"),
            ModelEntry::with_note("kimi-k2.5", "run cursor-agent models for full list"),
        ],
    },
];

/// Returns the list of known model name strings from all runner families.
///
/// Used by `config set model` to warn when an unrecognised name is provided,
/// and by `model_compatibility_warning()` to check cursor model names.
pub fn known_model_names() -> Vec<&'static str> {
    RUNNER_FAMILIES
        .iter()
        .flat_map(|f| f.models.iter())
        .map(|m| m.id)
        .collect()
}

/// Returns the list of known Cursor model name strings.
///
/// Used by `model_compatibility_warning()` to issue a soft warning when a
/// configured cursor model is not in the known list.
pub fn known_cursor_model_names() -> Vec<&'static str> {
    RUNNER_FAMILIES
        .iter()
        .find(|f| f.runners.contains(&"cursor-cli"))
        .map(|f| f.models.iter().map(|m| m.id).collect())
        .unwrap_or_default()
}

pub fn exec() -> Result<(), ActualError> {
    run_models();

    // Attempt live fetch (no-op if no API key; graceful on error)
    let live_openai = crate::model_cache::get_openai_models(
        crate::config::paths::load()
            .ok()
            .as_ref()
            .and_then(|c: &crate::config::types::Config| c.openai_api_key.as_deref()),
        None,
    );
    if !live_openai.is_empty() {
        // Show supplementary live models not already in the static list
        let static_ids: std::collections::HashSet<&str> =
            known_model_names().iter().copied().collect();
        let new_models: Vec<&String> = live_openai
            .iter()
            .filter(|id| !static_ids.contains(id.as_str()))
            .collect();
        if !new_models.is_empty() {
            println!();
            println!("Additional live models from OpenAI API:");
            for id in new_models {
                println!("  {id}");
            }
        }
    }

    Ok(())
}

fn run_models() {
    println!("Known models by runner\n");
    println!("  (default) marks the model used when none is configured");
    println!("  Set with: actual config set model <name>\n");

    for family in RUNNER_FAMILIES {
        let runners_str = family.runners.join(", ");
        println!("  {} ({})", family.name, runners_str);

        for model in family.models {
            let default_tag = if model.is_default { " (default)" } else { "" };
            match model.note {
                Some(note) => {
                    println!("    {}{}  — {}", model.id, default_tag, note);
                }
                None => {
                    println!("    {}{}", model.id, default_tag);
                }
            }
        }

        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exec_returns_ok() {
        assert!(exec().is_ok());
    }

    #[test]
    fn test_runner_families_nonempty() {
        assert!(
            !RUNNER_FAMILIES.is_empty(),
            "should have at least one family"
        );
    }

    #[test]
    fn test_each_family_has_runners_and_models() {
        for family in RUNNER_FAMILIES {
            assert!(
                !family.runners.is_empty(),
                "family '{}' should have runners",
                family.name
            );
            assert!(
                !family.models.is_empty(),
                "family '{}' should have models",
                family.name
            );
        }
    }

    #[test]
    fn test_exactly_one_default_per_claude_family() {
        let claude_family = RUNNER_FAMILIES
            .iter()
            .find(|f| f.name == "Claude / Anthropic")
            .expect("should have Claude family");
        let defaults: Vec<_> = claude_family
            .models
            .iter()
            .filter(|m| m.is_default)
            .collect();
        assert_eq!(
            defaults.len(),
            1,
            "Claude family should have exactly one default"
        );
        assert_eq!(
            defaults[0].id, "claude-sonnet-4-6",
            "default Claude model should be claude-sonnet-4-6"
        );
    }

    #[test]
    fn test_exactly_one_default_per_openai_family() {
        let openai_family = RUNNER_FAMILIES
            .iter()
            .find(|f| f.name == "OpenAI")
            .expect("should have OpenAI family");
        let defaults: Vec<_> = openai_family
            .models
            .iter()
            .filter(|m| m.is_default)
            .collect();
        assert_eq!(
            defaults.len(),
            1,
            "OpenAI family should have exactly one default"
        );
        assert_eq!(
            defaults[0].id, "gpt-5.2",
            "default OpenAI model should be gpt-5.2"
        );
    }

    #[test]
    fn test_exactly_one_default_per_codex_family() {
        let codex_family = RUNNER_FAMILIES
            .iter()
            .find(|f| f.name == "Codex CLI")
            .expect("should have Codex CLI family");
        let defaults: Vec<_> = codex_family
            .models
            .iter()
            .filter(|m| m.is_default)
            .collect();
        assert_eq!(
            defaults.len(),
            1,
            "Codex CLI family should have exactly one default"
        );
        assert_eq!(
            defaults[0].id, "gpt-5.2-codex",
            "default Codex model should be gpt-5.2-codex"
        );
    }

    #[test]
    fn test_model_entry_new_not_default() {
        let entry = ModelEntry::new("some-model");
        assert!(!entry.is_default);
        assert!(entry.note.is_none());
    }

    #[test]
    fn test_model_entry_default_model() {
        let entry = ModelEntry::default_model("my-model");
        assert!(entry.is_default);
        assert!(entry.note.is_none());
    }

    #[test]
    fn test_model_entry_with_note() {
        let entry = ModelEntry::with_note("alias", "short alias only");
        assert!(!entry.is_default);
        assert_eq!(entry.note, Some("short alias only"));
    }

    #[test]
    fn test_short_aliases_present_in_claude_family() {
        let claude_family = RUNNER_FAMILIES
            .iter()
            .find(|f| f.name == "Claude / Anthropic")
            .expect("should have Claude family");
        let ids: Vec<&str> = claude_family.models.iter().map(|m| m.id).collect();
        assert!(
            ids.contains(&"sonnet"),
            "should include 'sonnet' short alias"
        );
        assert!(ids.contains(&"opus"), "should include 'opus' short alias");
        assert!(ids.contains(&"haiku"), "should include 'haiku' short alias");
    }

    #[test]
    fn test_claude_full_ids_present() {
        let claude_family = RUNNER_FAMILIES
            .iter()
            .find(|f| f.name == "Claude / Anthropic")
            .expect("should have Claude family");
        let ids: Vec<&str> = claude_family.models.iter().map(|m| m.id).collect();
        assert!(
            ids.contains(&"claude-sonnet-4-6"),
            "should include claude-sonnet-4-6"
        );
        assert!(
            ids.contains(&"claude-opus-4-6"),
            "should include claude-opus-4-6"
        );
        assert!(
            ids.contains(&"claude-haiku-4-5"),
            "should include claude-haiku-4-5"
        );
        assert!(
            ids.contains(&"claude-opus-4-5"),
            "should include claude-opus-4-5"
        );
        // Legacy models still available
        assert!(
            ids.contains(&"claude-opus-4"),
            "should include claude-opus-4 (legacy)"
        );
    }

    #[test]
    fn test_openai_models_present() {
        let openai_family = RUNNER_FAMILIES
            .iter()
            .find(|f| f.name == "OpenAI")
            .expect("should have OpenAI family");
        let ids: Vec<&str> = openai_family.models.iter().map(|m| m.id).collect();
        // GPT-5 family (current)
        assert!(ids.contains(&"gpt-5.2"), "should include gpt-5.2");
        assert!(ids.contains(&"gpt-5.1"), "should include gpt-5.1");
        assert!(ids.contains(&"gpt-5"), "should include gpt-5");
        assert!(ids.contains(&"gpt-5-mini"), "should include gpt-5-mini");
        assert!(ids.contains(&"gpt-5-nano"), "should include gpt-5-nano");
        // GPT-4.1 family
        assert!(ids.contains(&"gpt-4.1"), "should include gpt-4.1");
        assert!(ids.contains(&"gpt-4.1-mini"), "should include gpt-4.1-mini");
        assert!(ids.contains(&"gpt-4.1-nano"), "should include gpt-4.1-nano");
        // GPT-4o (still live)
        assert!(ids.contains(&"gpt-4o"), "should include gpt-4o");
        // o-series (current)
        assert!(ids.contains(&"o4-mini"), "should include o4-mini");
        assert!(ids.contains(&"o3"), "should include o3");
        assert!(ids.contains(&"o3-mini"), "should include o3-mini");
        assert!(ids.contains(&"o1"), "should include o1");
        // Removed: o1-preview (shutdown 2025-07-28), chatgpt-4o-latest (shutdown 2026-02-17)
        assert!(
            !ids.contains(&"o1-preview"),
            "o1-preview must NOT be listed (shutdown 2025-07-28)"
        );
        assert!(
            !ids.contains(&"chatgpt-4o-latest"),
            "chatgpt-4o-latest must NOT be listed (shutdown 2026-02-17)"
        );
    }

    #[test]
    fn test_codex_cli_models_present() {
        let codex_family = RUNNER_FAMILIES
            .iter()
            .find(|f| f.name == "Codex CLI")
            .expect("should have Codex CLI family");
        let ids: Vec<&str> = codex_family.models.iter().map(|m| m.id).collect();
        assert!(
            ids.contains(&"gpt-5.2-codex"),
            "should include gpt-5.2-codex"
        );
        assert!(
            ids.contains(&"gpt-5.1-codex"),
            "should include gpt-5.1-codex"
        );
        assert!(
            ids.contains(&"gpt-5.1-codex-max"),
            "should include gpt-5.1-codex-max"
        );
        assert!(
            ids.contains(&"gpt-5.1-codex-mini"),
            "should include gpt-5.1-codex-mini"
        );
        assert!(ids.contains(&"gpt-5-codex"), "should include gpt-5-codex");
        // o-series and gpt-4o models route to codex-cli (preferred runner)
        assert!(ids.contains(&"gpt-4o"), "should include gpt-4o");
        assert!(ids.contains(&"o4-mini"), "should include o4-mini");
        assert!(ids.contains(&"o3"), "should include o3");
        assert!(ids.contains(&"o3-mini"), "should include o3-mini");
        assert!(ids.contains(&"o1"), "should include o1");
        // Removed: o1-preview (shutdown 2025-07-28), chatgpt-4o-latest (shutdown 2026-02-17)
        assert!(
            !ids.contains(&"o1-preview"),
            "o1-preview must NOT be listed (shutdown 2025-07-28)"
        );
        assert!(
            !ids.contains(&"chatgpt-4o-latest"),
            "chatgpt-4o-latest must NOT be listed (shutdown 2026-02-17)"
        );
    }

    // --- known_model_names() tests ---

    #[test]
    fn test_known_model_names_nonempty() {
        let names = known_model_names();
        assert!(!names.is_empty(), "known_model_names() should not be empty");
    }

    #[test]
    fn test_known_model_names_includes_claude_models() {
        let names = known_model_names();
        assert!(names.contains(&"claude-sonnet-4-6"));
        assert!(names.contains(&"claude-opus-4-6"));
        assert!(names.contains(&"claude-haiku-4-5"));
        assert!(names.contains(&"claude-opus-4-5"));
        assert!(names.contains(&"claude-opus-4"));
        assert!(names.contains(&"sonnet"));
        assert!(names.contains(&"opus"));
        assert!(names.contains(&"haiku"));
    }

    #[test]
    fn test_known_model_names_includes_openai_models() {
        let names = known_model_names();
        // GPT-5 family
        assert!(names.contains(&"gpt-5.2"));
        assert!(names.contains(&"gpt-5.1"));
        assert!(names.contains(&"gpt-5"));
        assert!(names.contains(&"gpt-5-mini"));
        assert!(names.contains(&"gpt-5-nano"));
        // GPT-4.1 family
        assert!(names.contains(&"gpt-4.1"));
        assert!(names.contains(&"gpt-4.1-mini"));
        assert!(names.contains(&"gpt-4.1-nano"));
        // GPT-4o and o-series (current)
        assert!(names.contains(&"gpt-4o"));
        assert!(names.contains(&"o4-mini"));
        assert!(names.contains(&"o3"));
        assert!(names.contains(&"o3-mini"));
        assert!(names.contains(&"o1"));
        // Deprecated/removed models must NOT appear
        assert!(
            !names.contains(&"o1-preview"),
            "o1-preview was shut down 2025-07-28"
        );
        assert!(
            !names.contains(&"chatgpt-4o-latest"),
            "chatgpt-4o-latest was shut down 2026-02-17"
        );
    }

    #[test]
    fn test_known_model_names_includes_codex_models() {
        let names = known_model_names();
        assert!(names.contains(&"gpt-5.2-codex"));
        assert!(names.contains(&"gpt-5.1-codex"));
        assert!(names.contains(&"gpt-5.1-codex-max"));
        assert!(names.contains(&"gpt-5.1-codex-mini"));
        assert!(names.contains(&"gpt-5-codex"));
    }

    #[test]
    fn test_known_model_names_includes_cursor_models() {
        let names = known_model_names();
        assert!(names.contains(&"auto"), "should include 'auto'");
        assert!(names.contains(&"opus-4.6"), "should include 'opus-4.6'");
        assert!(names.contains(&"sonnet-4.6"), "should include 'sonnet-4.6'");
        assert!(names.contains(&"gpt-5.2"), "should include 'gpt-5.2'");
        assert!(names.contains(&"grok"), "should include 'grok'");
    }

    #[test]
    fn test_known_cursor_model_names_nonempty() {
        let names = known_cursor_model_names();
        assert!(
            !names.is_empty(),
            "known_cursor_model_names() should not be empty"
        );
    }

    #[test]
    fn test_known_cursor_model_names_contains_expected() {
        let names = known_cursor_model_names();
        assert!(names.contains(&"auto"), "should include 'auto'");
        assert!(
            names.contains(&"opus-4.6-thinking"),
            "should include 'opus-4.6-thinking'"
        );
        assert!(names.contains(&"sonnet-4.6"), "should include 'sonnet-4.6'");
        assert!(names.contains(&"grok"), "should include 'grok'");
        assert!(names.contains(&"kimi-k2.5"), "should include 'kimi-k2.5'");
    }

    #[test]
    fn test_cursor_family_has_default_model() {
        let cursor_family = RUNNER_FAMILIES
            .iter()
            .find(|f| f.name == "Cursor")
            .expect("should have Cursor family");
        let defaults: Vec<_> = cursor_family
            .models
            .iter()
            .filter(|m| m.is_default)
            .collect();
        assert_eq!(
            defaults.len(),
            1,
            "Cursor family should have exactly one default"
        );
        assert_eq!(
            defaults[0].id, "opus-4.6-thinking",
            "default Cursor model should be opus-4.6-thinking"
        );
    }
}
