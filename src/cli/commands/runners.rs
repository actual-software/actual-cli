use crate::cli::args::RunnerChoice;
use crate::cli::ui::panel::Panel;
use crate::cli::ui::term_size;
use crate::error::ActualError;

/// Metadata describing a single runner option for display purposes.
struct RunnerInfo {
    name: &'static str,
    requirement: &'static str,
    description: &'static str,
}

/// Returns display metadata for all supported runners, in the same order as
/// [`RunnerChoice::value_variants`].
fn runner_infos() -> Vec<RunnerInfo> {
    vec![
        RunnerInfo {
            name: RunnerChoice::ClaudeCli.display_name(),
            requirement: "requires `claude` binary",
            description: "Claude Code CLI subprocess",
        },
        RunnerInfo {
            name: RunnerChoice::AnthropicApi.display_name(),
            requirement: "requires ANTHROPIC_API_KEY",
            description: "Anthropic Messages API",
        },
        RunnerInfo {
            name: RunnerChoice::OpenAiApi.display_name(),
            requirement: "requires OPENAI_API_KEY",
            description: "OpenAI Responses API",
        },
        RunnerInfo {
            name: RunnerChoice::CodexCli.display_name(),
            requirement: "requires `codex` binary",
            description: "Codex CLI subprocess (default)",
        },
        RunnerInfo {
            name: RunnerChoice::CursorCli.display_name(),
            requirement: "requires `agent` binary",
            description: "Cursor CLI subprocess",
        },
    ]
}

pub fn exec() -> Result<(), ActualError> {
    let width = term_size::terminal_width();
    println!("{}", format_runners_panel(width));
    Ok(())
}

fn format_runners_panel(width: usize) -> String {
    let infos = runner_infos();
    let mut panel = Panel::titled("Available Runners");

    for info in &infos {
        let label = format!("{} ({})", info.name, info.requirement);
        panel = panel.kv(&label, info.description);
    }

    panel.render(width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;

    fn plain(rendered: &str) -> String {
        console::strip_ansi_codes(rendered).into_owned()
    }

    #[test]
    fn test_format_runners_panel_contains_title() {
        let output = format_runners_panel(80);
        let p = plain(&output);
        assert!(
            p.contains("Available Runners"),
            "should have panel title: {p}"
        );
    }

    #[test]
    fn test_format_runners_panel_contains_all_runner_names() {
        let output = format_runners_panel(80);
        let p = plain(&output);
        assert!(p.contains("claude-cli"), "should contain claude-cli: {p}");
        assert!(
            p.contains("anthropic-api"),
            "should contain anthropic-api: {p}"
        );
        assert!(p.contains("openai-api"), "should contain openai-api: {p}");
        assert!(p.contains("codex-cli"), "should contain codex-cli: {p}");
        assert!(p.contains("cursor-cli"), "should contain cursor-cli: {p}");
    }

    #[test]
    fn test_format_runners_panel_contains_requirements() {
        let output = format_runners_panel(80);
        let p = plain(&output);
        assert!(
            p.contains("claude` binary"),
            "should mention claude binary: {p}"
        );
        assert!(
            p.contains("ANTHROPIC_API_KEY"),
            "should mention ANTHROPIC_API_KEY: {p}"
        );
        assert!(
            p.contains("OPENAI_API_KEY"),
            "should mention OPENAI_API_KEY: {p}"
        );
        assert!(
            p.contains("codex` binary"),
            "should mention codex binary: {p}"
        );
        assert!(
            p.contains("agent` binary"),
            "should mention agent binary: {p}"
        );
    }

    #[test]
    fn test_format_runners_panel_contains_descriptions() {
        let output = format_runners_panel(80);
        let p = plain(&output);
        assert!(
            p.contains("Claude Code CLI subprocess"),
            "should have claude-cli description: {p}"
        );
        assert!(
            p.contains("Anthropic Messages API"),
            "should have anthropic-api description: {p}"
        );
        assert!(
            p.contains("OpenAI Responses API"),
            "should have openai-api description: {p}"
        );
        assert!(
            p.contains("Codex CLI subprocess (default)"),
            "should have codex-cli description with (default): {p}"
        );
        assert!(
            p.contains("Cursor CLI subprocess"),
            "should have cursor-cli description: {p}"
        );
    }

    #[test]
    fn test_format_runners_panel_has_box_borders() {
        let output = format_runners_panel(80);
        let p = plain(&output);
        assert!(p.contains('╭'), "should contain top-left corner: {p}");
        assert!(p.contains('╰'), "should contain bottom-left corner: {p}");
        assert!(p.contains('│'), "should contain vertical border: {p}");
    }

    #[test]
    fn test_runner_infos_count_matches_variants() {
        let infos = runner_infos();
        let variants = RunnerChoice::value_variants();
        assert_eq!(
            infos.len(),
            variants.len(),
            "runner_infos() must have one entry per RunnerChoice variant"
        );
    }

    #[test]
    fn test_runner_infos_names_match_display_names() {
        let infos = runner_infos();
        let variants = RunnerChoice::value_variants();
        for (info, variant) in infos.iter().zip(variants.iter()) {
            assert_eq!(
                info.name,
                variant.display_name(),
                "runner_infos name must match RunnerChoice::display_name()"
            );
        }
    }

    #[test]
    fn test_exec_returns_ok() {
        let result = exec();
        assert!(result.is_ok(), "exec() should return Ok(())");
    }
}
