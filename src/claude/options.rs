/// Options for a Claude Code subprocess invocation.
///
/// Encapsulates the CLI flags passed to `claude --print`. Use the
/// factory method [`InvocationOptions::for_tailoring`] to create a
/// pre-configured instance for the tailoring invocation profile.
#[derive(Debug, Clone)]
pub struct InvocationOptions {
    /// Model name (e.g., "sonnet", "opus").
    pub model: String,
    /// Maximum number of agentic turns.
    pub max_turns: u32,
    /// Comma-separated tool names for --tools.
    pub tools: String,
    /// Permission patterns for --allowedTools (one entry per pattern).
    pub allowed_tools: Vec<String>,
    /// Optional JSON schema for --json-schema.
    pub json_schema: Option<String>,
    /// Optional spending cap for --max-budget-usd.
    pub max_budget_usd: Option<f64>,
    /// Whether to pass --allow-dangerously-skip-permissions.
    ///
    /// Profiles that use only read-only tools (e.g., tailoring) set this to
    /// `true`. Profiles with write or exec tools must leave it `false` so that
    /// the user is prompted for permission.
    pub skip_permissions: bool,
}

const DEFAULT_MODEL: &str = "sonnet";

impl InvocationOptions {
    /// Create options for ADR tailoring.
    ///
    /// Tailoring is restricted to read-only tools (no Bash), 15 max turns.
    pub fn for_tailoring(model_override: Option<&str>) -> Self {
        Self {
            model: model_override.unwrap_or(DEFAULT_MODEL).to_string(),
            max_turns: 15,
            tools: "Read,Glob,Grep".to_string(),
            allowed_tools: vec![
                "Read".to_string(),
                "Glob(*)".to_string(),
                "Grep(*)".to_string(),
            ],
            json_schema: None,
            max_budget_usd: None,
            skip_permissions: true,
        }
    }

    /// Convert to CLI argument list for `ClaudeRunner::run()`.
    ///
    /// The `--print` flag is NOT included — it is added by
    /// `CliClaudeRunner` automatically.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = vec![
            "--model".to_string(),
            self.model.clone(),
            "--max-turns".to_string(),
            self.max_turns.to_string(),
            "--output-format".to_string(),
            "json".to_string(),
        ];

        if self.skip_permissions {
            args.push("--allow-dangerously-skip-permissions".to_string());
        }

        args.push("--no-session-persistence".to_string());
        args.push("--tools".to_string());
        args.push(self.tools.clone());

        for tool in &self.allowed_tools {
            args.push("--allowedTools".to_string());
            args.push(tool.clone());
        }

        if let Some(ref schema) = self.json_schema {
            args.push("--json-schema".to_string());
            args.push(schema.clone());
        }

        if let Some(budget) = self.max_budget_usd {
            args.push("--max-budget-usd".to_string());
            args.push(budget.to_string());
        }

        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_for_tailoring_no_bash() {
        let opts = InvocationOptions::for_tailoring(None);
        let args = opts.to_args();

        // Max turns is 15
        assert_arg_value(&args, "--max-turns", "15");

        // No Bash anywhere in the args
        assert_eq!(args.iter().filter(|a| a.contains("Bash")).count(), 0);
    }

    #[test]
    fn test_to_args_omits_none_fields() {
        let opts = InvocationOptions::for_tailoring(None);
        let args = opts.to_args();

        assert!(!args.contains(&"--json-schema".to_string()));
        assert!(!args.contains(&"--max-budget-usd".to_string()));
    }

    #[test]
    fn test_for_tailoring_model_override() {
        let opts = InvocationOptions::for_tailoring(Some("opus"));
        let args = opts.to_args();

        assert_arg_value(&args, "--model", "opus");
    }

    #[test]
    fn test_for_tailoring_default_model() {
        let opts = InvocationOptions::for_tailoring(None);
        let args = opts.to_args();

        assert_arg_value(&args, "--model", "sonnet");
    }

    #[test]
    fn test_to_args_includes_json_schema_when_set() {
        let mut opts = InvocationOptions::for_tailoring(None);
        opts.json_schema = Some(r#"{"type":"object"}"#.to_string());
        let args = opts.to_args();

        assert_arg_value(&args, "--json-schema", r#"{"type":"object"}"#);
    }

    #[test]
    fn test_to_args_includes_max_budget_when_set() {
        let mut opts = InvocationOptions::for_tailoring(None);
        opts.max_budget_usd = Some(0.5);
        let args = opts.to_args();

        assert_arg_value(&args, "--max-budget-usd", "0.5");
    }

    #[test]
    fn test_to_args_always_includes_common_flags() {
        let opts = InvocationOptions::for_tailoring(None);
        let args = opts.to_args();

        assert_arg_value(&args, "--output-format", "json");
        assert!(args.contains(&"--no-session-persistence".to_string()));
    }

    #[test]
    fn test_tailoring_includes_skip_permissions() {
        let opts = InvocationOptions::for_tailoring(None);
        assert!(opts.skip_permissions);
        let args = opts.to_args();
        assert!(args.contains(&"--allow-dangerously-skip-permissions".to_string()));
    }

    /// Guard: `skip_permissions = true` must never be combined with tools that
    /// can execute code or mutate files.  If this test fails it means a
    /// developer added a dangerous tool to a profile that bypasses all
    /// permission checks — that would silently allow arbitrary code execution.
    #[test]
    fn test_skip_permissions_only_with_readonly_tools() {
        let opts = InvocationOptions::for_tailoring(None);
        assert!(opts.skip_permissions);
        let forbidden = ["Bash", "Write", "Edit", "WebFetch"];
        for tool in &forbidden {
            assert!(!opts.tools.contains(tool));
            for allowed in &opts.allowed_tools {
                assert!(!allowed.starts_with(tool));
            }
        }
    }

    #[test]
    fn test_skip_permissions_false_omits_flag() {
        let mut opts = InvocationOptions::for_tailoring(None);
        opts.skip_permissions = false;
        let args = opts.to_args();
        assert!(!args.contains(&"--allow-dangerously-skip-permissions".to_string()));
    }

    #[test]
    fn test_to_args_does_not_include_print() {
        let tailoring_args = InvocationOptions::for_tailoring(None).to_args();

        assert!(!tailoring_args.contains(&"--print".to_string()));
    }

    #[test]
    fn test_clone_produces_independent_copy() {
        let mut opts = InvocationOptions::for_tailoring(None);
        opts.json_schema = Some("schema".to_string());
        opts.max_budget_usd = Some(1.0);
        let cloned = opts.clone();

        assert_eq!(cloned.model, opts.model);
        assert_eq!(cloned.max_turns, opts.max_turns);
        assert_eq!(cloned.tools, opts.tools);
        assert_eq!(cloned.allowed_tools, opts.allowed_tools);
        assert_eq!(cloned.json_schema, opts.json_schema);
        assert_eq!(cloned.max_budget_usd, opts.max_budget_usd);
        assert_eq!(cloned.skip_permissions, opts.skip_permissions);
    }

    #[test]
    fn test_debug_output() {
        let opts = InvocationOptions::for_tailoring(None);
        let debug = format!("{opts:?}");
        assert!(debug.contains("InvocationOptions"));
        assert!(debug.contains("sonnet"));
    }

    /// Assert that `--flag value` appears as consecutive elements.
    fn assert_arg_value(args: &[String], flag: &str, expected: &str) {
        let pos = args.iter().position(|a| a == flag).unwrap();
        assert_eq!(&args[pos + 1], expected);
    }
}
