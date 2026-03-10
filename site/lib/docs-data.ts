// Shared structured data for docs — imported by both /docs/page.tsx and /docs.md/route.ts.
// When adding new flags, runners, config keys, etc., update here and both consumers pick it up.

export const ADR_BOT_FLAGS: { flag: string; type: string; desc: string }[] = [
    { flag: "--dry-run", type: "bool", desc: "Preview what would change without writing any files." },
    { flag: "--full", type: "bool", desc: "With --dry-run, print the full rendered file to stdout." },
    { flag: "--force", type: "bool", desc: "Skip confirmation prompts and bypass all caches." },
    { flag: "--no-tailor", type: "bool", desc: "Skip AI tailoring; write raw ADRs as-is from the bank." },
    { flag: "--project <PATH>", type: "string", desc: "Target a specific sub-project in a monorepo (repeatable)." },
    { flag: "--output-format <FMT>", type: "enum", desc: "claude-md (default) | agents-md | cursor-rules" },
    { flag: "--runner <RUNNER>", type: "enum", desc: "AI backend: claude-cli | anthropic-api | openai-api | codex-cli | cursor-cli" },
    { flag: "--model <MODEL>", type: "string", desc: "Override AI model. Runner is auto-inferred from the model name." },
    { flag: "--max-budget-usd <N>", type: "float", desc: "Spending cap per tailoring invocation (USD)." },
    { flag: "--reset-rejections", type: "bool", desc: "Clear remembered ADR rejections and show all ADRs again." },
    { flag: "--verbose", type: "bool", desc: "Show detailed progress and AI runner output." },
    { flag: "--show-errors", type: "bool", desc: "Stream runner stderr in real time — useful for diagnosing hangs or auth failures." },
    { flag: "--no-tui", type: "bool", desc: "Disable the TUI; use plain line output instead." },
    { flag: "--api-url <URL>", type: "string", desc: "Override the ADR bank API endpoint." },
];

export const ADR_BOT_EXAMPLES: { comment: string; cmd: string }[] = [
    { comment: "Preview without writing", cmd: "actual adr-bot --dry-run" },
    { comment: "Force fresh sync (bypass cache)", cmd: "actual adr-bot --force" },
    { comment: "Generate AGENTS.md instead of CLAUDE.md", cmd: "actual adr-bot --output-format agents-md" },
    { comment: "Use Anthropic API directly instead of Claude Code CLI", cmd: "actual adr-bot --runner anthropic-api" },
    { comment: "Use a specific model (runner auto-inferred)", cmd: "actual adr-bot --model gpt-5.2" },
    { comment: "Monorepo: target specific sub-projects", cmd: "actual adr-bot --project packages/api --project packages/web" },
    { comment: "Debug a hang", cmd: "actual adr-bot --verbose --show-errors" },
];

export const RUNNERS: {
    name: string;
    color: string;
    req: string;
    model: string;
    setup: string;
    note: string;
}[] = [
    {
        name: "claude-cli",
        color: "#39eba1",
        req: "claude binary",
        model: "claude-sonnet-4-6",
        setup: "npm install -g @anthropic-ai/claude-code\nclaude auth login",
        note: "Default runner. Uses the Claude Code CLI subprocess. Short model aliases (sonnet, opus, haiku) work with this runner.",
    },
    {
        name: "anthropic-api",
        color: "#43bdb7",
        req: "ANTHROPIC_API_KEY",
        model: "claude-sonnet-4-6",
        setup: "export ANTHROPIC_API_KEY=sk-ant-...\n# or: actual config set anthropic_api_key sk-ant-...",
        note: "Calls the Anthropic Messages API directly. Requires a full model name (not short aliases).",
    },
    {
        name: "openai-api",
        color: "#43bdb7",
        req: "OPENAI_API_KEY",
        model: "gpt-5.2",
        setup: "export OPENAI_API_KEY=sk-...",
        note: "Calls the OpenAI Responses API directly.",
    },
    {
        name: "codex-cli",
        color: "#4d93c8",
        req: "codex binary",
        model: "gpt-5.2-codex",
        setup: "npm install -g @openai/codex\ncodex login   # or set OPENAI_API_KEY",
        note: "Uses the Codex CLI subprocess. ChatGPT OAuth (codex login) only supports the default model; for custom models, set OPENAI_API_KEY.",
    },
    {
        name: "cursor-cli",
        color: "#4d93c8",
        req: "agent binary",
        model: "opus-4.6-thinking",
        setup: "curl https://cursor.com/install -fsS | bash\ncursor-agent login   # or set CURSOR_API_KEY",
        note: "Uses the Cursor agent CLI subprocess.",
    },
];

export const OUTPUT_FORMATS: { value: string; file: string; tool: string; color: string }[] = [
    { value: "claude-md", file: "CLAUDE.md", tool: "Claude Code", color: "#39eba1" },
    { value: "agents-md", file: "AGENTS.md", tool: "Codex CLI, OpenCode", color: "#43bdb7" },
    { value: "cursor-rules", file: ".cursor/rules/actual-policies.mdc", tool: "Cursor IDE", color: "#4d93c8" },
];

export const CONFIG_KEYS: { key: string; default_: string; desc: string }[] = [
    { key: "runner", default_: "claude-cli", desc: "AI backend to use for tailoring" },
    { key: "model", default_: "—", desc: "Default model (runner is auto-inferred)" },
    { key: "output_format", default_: "claude-md", desc: "claude-md | agents-md | cursor-rules" },
    { key: "batch_size", default_: "15", desc: "ADRs per API request batch" },
    { key: "concurrency", default_: "10", desc: "Max concurrent API requests" },
    { key: "invocation_timeout_secs", default_: "600", desc: "Runner timeout in seconds" },
    { key: "max_budget_usd", default_: "—", desc: "Spending cap per tailoring invocation" },
    { key: "max_turns", default_: "—", desc: "Max conversation turns (claude-cli only)" },
    { key: "max_per_framework", default_: "—", desc: "Max ADRs per detected framework" },
    { key: "include_general", default_: "—", desc: "Include language-agnostic ADRs" },
    { key: "include_categories", default_: "—", desc: "Only include these ADR categories (comma-separated)" },
    { key: "exclude_categories", default_: "—", desc: "Exclude these ADR categories (comma-separated)" },
    { key: "anthropic_api_key", default_: "—", desc: "Anthropic API key (stored at mode 0600)" },
    { key: "openai_api_key", default_: "—", desc: "OpenAI API key" },
    { key: "cursor_api_key", default_: "—", desc: "Cursor API key" },
    { key: "telemetry.enabled", default_: "true", desc: "Enable or disable telemetry" },
];

export const CONFIG_EXAMPLES: string[] = [
    "actual config set runner anthropic-api",
    "actual config set model claude-sonnet-4-6",
    "actual config set output_format agents-md",
    "actual config set invocation_timeout_secs 1200",
    "actual config set telemetry.enabled false",
    'actual config set anthropic_api_key "sk-ant-..."',
];

export const ENV_VARS: { name: string; purpose: string }[] = [
    { name: "ACTUAL_CONFIG", purpose: "Exact path to config file (highest precedence)" },
    { name: "ACTUAL_CONFIG_DIR", purpose: "Directory for config (must be absolute)" },
    { name: "ANTHROPIC_API_KEY", purpose: "Anthropic API key (overrides config value)" },
    { name: "OPENAI_API_KEY", purpose: "OpenAI API key (overrides config value)" },
    { name: "CURSOR_API_KEY", purpose: "Cursor API key (overrides config value)" },
    { name: "CLAUDE_BINARY", purpose: "Override path to the claude binary" },
    { name: "RUST_LOG", purpose: "Log level (default: warn)" },
];

export const COMMON_ERRORS: { err: string; fix: string }[] = [
    { err: "Claude Code is not installed", fix: "npm install -g @anthropic-ai/claude-code" },
    { err: "Claude Code is not authenticated", fix: "claude auth login" },
    { err: "No runner available for model '…'", fix: "Install a runner or set an API key. Run actual runners to see what's available." },
    { err: "Runner timed out after 600s", fix: "actual config set invocation_timeout_secs 1200" },
    { err: "Insufficient credits", fix: "Add credits at your provider's billing page, or use --max-budget-usd to cap spend." },
    { err: "Analysis returned no projects", fix: "Make sure you're running from inside a git repo. For monorepos, use --project <PATH>." },
];

export const EXIT_CODES: { code: string; meaning: string }[] = [
    { code: "0", meaning: "Success" },
    { code: "1", meaning: "General runtime error" },
    { code: "2", meaning: "Auth / setup error (binary not found, not authenticated, API key missing)" },
    { code: "3", meaning: "Billing / API error (credits too low)" },
    { code: "4", meaning: "User cancelled" },
    { code: "5", meaning: "I/O error (permissions, disk space)" },
];

export const SKILL_INSTALL_METHODS: {
    name: string;
    color: string;
    tool: string;
    command: string;
    note: string;
}[] = [
    {
        name: "npx skills add",
        color: "#39eba1",
        tool: "Claude Code",
        command: "npx skills add actual-software/actual-skill",
        note: "One-command install using the skills CLI. Downloads the skill into your project and configures it for Claude Code automatically.",
    },
    {
        name: "Claude Code (Plugin)",
        color: "#39eba1",
        tool: "Claude Code",
        command: "claude mcp add-plugin https://github.com/actual-software/actual-skill",
        note: "Installs as a Claude Code plugin. The skill auto-activates when the model detects you're working with the actual CLI — no slash command needed.",
    },
    {
        name: "Claude Code (Manual)",
        color: "#39eba1",
        tool: "Claude Code",
        command: "# Add to your project's .claude/skills/ directory\ncurl -fsSL https://raw.githubusercontent.com/actual-software/actual-skill/main/skills/actual/SKILL.md \\\n  -o .claude/skills/actual/SKILL.md\n\n# Or clone the full skill with references\ngit clone https://github.com/actual-software/actual-skill.git .claude/plugins/actual-skill",
        note: "Download the skill file directly into your project. Useful when you want version-controlled skill files or can't install plugins.",
    },
    {
        name: "Codex CLI",
        color: "#4d93c8",
        tool: "Codex CLI",
        command: "# Add the agents/openai.yaml to your project\ncurl -fsSL https://raw.githubusercontent.com/actual-software/actual-skill/main/agents/openai.yaml \\\n  -o agents/openai.yaml",
        note: "Uses the OpenAI agents.yaml format. Place in your project root for Codex to pick up automatically.",
    },
    {
        name: "ClawdHub",
        color: "#43bdb7",
        tool: "ClawdHub / OpenClaw",
        command: "clawhub install actual",
        note: "Install from ClawdHub (clawhub.ai). Includes an ADR Pre-Check section that nudges agents to check for ADR context before creating new components.",
    },
];

export const LANGUAGES: { language: string; color: string; frameworks: string[] }[] = [
    { language: "TypeScript", color: "#3178c6", frameworks: ["Next.js", "HeroUI"] },
    { language: "Rust", color: "#ce422b", frameworks: ["Ratatui"] },
    { language: "Python", color: "#f7c948", frameworks: ["FastAPI", "Django"] },
    { language: "Java", color: "#e76f00", frameworks: ["Spring Boot"] },
];
