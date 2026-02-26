import type { Metadata } from "next";

export const metadata: Metadata = {
    title: "Docs — actual CLI",
    description: "Getting started guide and full command reference for the actual CLI.",
};

const BRAND_GRADIENT = "linear-gradient(90deg, #39eba1 0%, #43bdb7 50.483%, #4d93c8 100%)";

const NAV_SECTIONS = [
    { id: "quickstart", label: "Quick Start" },
    { id: "languages", label: "Supported Languages" },
    { id: "commands", label: "Commands" },
    { id: "sync", label: "  actual adr-bot" },
    { id: "auth", label: "  actual auth" },
    { id: "status", label: "  actual status" },
    { id: "config", label: "  actual config" },
    { id: "runners-cmd", label: "  actual runners" },
    { id: "models-cmd", label: "  actual models" },
    { id: "cache-cmd", label: "  actual cache" },
    { id: "runners", label: "Runners" },
    { id: "output-formats", label: "Output Formats" },
    { id: "configuration", label: "Configuration" },
    { id: "troubleshooting", label: "Troubleshooting" },
];

function Section({ id, children }: { id: string; children: React.ReactNode }) {
    return (
        <section id={id} className="scroll-mt-24 flex flex-col gap-[20px]">
            {children}
        </section>
    );
}

function H2({ children }: { children: React.ReactNode }) {
    return (
        <h2 className="text-[22px] font-semibold text-white leading-[1.3] tracking-[-0.015em] border-b border-[#393939] pb-[12px]">
            {children}
        </h2>
    );
}

function H3({ children }: { children: React.ReactNode }) {
    return (
        <h3 className="text-[16px] font-semibold text-white leading-[1.4] tracking-[-0.01em] mt-[8px]">
            {children}
        </h3>
    );
}

function P({ children }: { children: React.ReactNode }) {
    return <p className="text-white/70 text-[15px] leading-[1.75]">{children}</p>;
}

function Code({ children }: { children: React.ReactNode }) {
    return (
        <code className="font-mono text-[13px] text-white/85 bg-[#141414] border border-white/10 rounded-[4px] px-[5px] py-[1px]">
            {children}
        </code>
    );
}

function Pre({ children }: { children: React.ReactNode }) {
    return (
        <div className="bg-[#0d0d0d] border border-[#393939] rounded-[6px] overflow-x-auto">
            <pre className="font-mono text-[13px] text-white/80 leading-[1.7] px-[20px] py-[16px] whitespace-pre">
                {children}
            </pre>
        </div>
    );
}

function Table({ headers, rows }: { headers: string[]; rows: string[][] }) {
    return (
        <div className="overflow-x-auto rounded-[6px] border border-[#393939]">
            <table className="w-full text-[13px] leading-[1.6]">
                <thead>
                    <tr className="border-b border-[#393939] bg-[#0d0d0d]">
                        {headers.map((h) => (
                            <th key={h} className="text-left px-[16px] py-[10px] font-semibold text-white/60 uppercase text-[11px] tracking-[0.07em] whitespace-nowrap">
                                {h}
                            </th>
                        ))}
                    </tr>
                </thead>
                <tbody>
                    {rows.map((row, i) => (
                        <tr key={i} className="border-b border-[#393939]/60 last:border-0 hover:bg-white/[0.02] transition-colors">
                            {row.map((cell, j) => (
                                <td key={j} className="px-[16px] py-[10px] text-white/70 align-top">
                                    <span className="font-mono text-white/85">{j === 0 ? cell : ""}</span>
                                    {j !== 0 && cell}
                                </td>
                            ))}
                        </tr>
                    ))}
                </tbody>
            </table>
        </div>
    );
}

function FlagTable({ rows }: { rows: { flag: string; type: string; desc: string }[] }) {
    return (
        <div className="overflow-x-auto rounded-[6px] border border-[#393939]">
            <table className="w-full text-[13px] leading-[1.6]">
                <thead>
                    <tr className="border-b border-[#393939] bg-[#0d0d0d]">
                        <th className="text-left px-[16px] py-[10px] font-semibold text-white/60 uppercase text-[11px] tracking-[0.07em]">Flag</th>
                        <th className="text-left px-[16px] py-[10px] font-semibold text-white/60 uppercase text-[11px] tracking-[0.07em]">Type</th>
                        <th className="text-left px-[16px] py-[10px] font-semibold text-white/60 uppercase text-[11px] tracking-[0.07em]">Description</th>
                    </tr>
                </thead>
                <tbody>
                    {rows.map((r, i) => (
                        <tr key={i} className="border-b border-[#393939]/60 last:border-0 hover:bg-white/[0.02] transition-colors">
                            <td className="px-[16px] py-[10px] align-top whitespace-nowrap">
                                <code className="font-mono text-[#39eba1] text-[12px]">{r.flag}</code>
                            </td>
                            <td className="px-[16px] py-[10px] align-top whitespace-nowrap">
                                <span className="font-mono text-white/40 text-[12px]">{r.type}</span>
                            </td>
                            <td className="px-[16px] py-[10px] align-top text-white/70">{r.desc}</td>
                        </tr>
                    ))}
                </tbody>
            </table>
        </div>
    );
}

function Badge({ color, children }: { color: string; children: React.ReactNode }) {
    return (
        <span
            className="inline-block text-[11px] font-semibold uppercase tracking-[0.08em] px-[8px] py-[2px] rounded-[4px]"
            style={{ color, background: `${color}18`, border: `1px solid ${color}30` }}
        >
            {children}
        </span>
    );
}

export default function DocsPage() {
    return (
        <div className="bg-[#030301] min-h-screen w-full">
            {/* Page header */}
            <div className="border-b border-[#393939] w-full">
                <div className="mx-auto max-w-[1400px] px-6 lg:px-12 xl:px-[100px] py-[48px] flex flex-col gap-[12px]">
                    <div className="flex items-center gap-[8px]">
                        <Badge color="#39eba1">Reference</Badge>
                    </div>
                    <h1 className="text-[36px] md:text-[48px] font-semibold text-white leading-[1.1] tracking-[-0.03em]">
                        Getting{" "}
                        <span className="bg-clip-text text-transparent" style={{ backgroundImage: BRAND_GRADIENT }}>
                            started
                        </span>
                    </h1>
                    <p className="text-white/60 text-[16px] leading-[1.65] max-w-[560px]">
                        Full command reference, runner setup, output formats, and configuration for the <Code>actual</Code> CLI.
                    </p>
                </div>
            </div>

            {/* Two-column layout */}
            <div className="mx-auto max-w-[1400px] px-6 lg:px-12 xl:px-[100px] flex gap-[60px] py-[60px]">

                {/* Sidebar */}
                <aside className="hidden lg:block w-[200px] flex-shrink-0 self-start sticky top-[90px]">
                    <nav className="flex flex-col gap-[4px]">
                        {NAV_SECTIONS.map((s) => (
                            <a
                                key={s.id}
                                href={`#${s.id}`}
                                className={`text-[13px] leading-[1.5] py-[5px] px-[8px] rounded-[4px] transition-colors hover:text-white hover:bg-white/5 ${
                                    s.label.startsWith("  ")
                                        ? "text-white/40 pl-[20px]"
                                        : "text-white/60 font-medium"
                                }`}
                            >
                                {s.label.trim()}
                            </a>
                        ))}
                    </nav>
                </aside>

                {/* Main content */}
                <main className="flex-1 min-w-0 flex flex-col gap-[60px]">

                    {/* Quick Start */}
                    <Section id="quickstart">
                        <H2>Quick Start</H2>
                        <P>
                            <Code>actual</Code> analyzes your repo, fetches your team&apos;s Architectural Decision Records (ADRs),
                            and writes them into AI context files — keeping every coding agent architecturally aligned, automatically.
                        </P>

                        <div className="flex flex-col gap-[24px]">
                            {/* Step 1 */}
                            <div className="flex gap-[16px]">
                                <div className="flex-shrink-0 size-[28px] rounded-full flex items-center justify-center text-[12px] font-bold text-black" style={{ background: BRAND_GRADIENT }}>
                                    1
                                </div>
                                <div className="flex flex-col gap-[10px] pt-[3px] flex-1">
                                    <H3>Install</H3>
                                    <P>Choose your preferred install method:</P>
                                    <Pre>{`# Homebrew (macOS & Linux)
brew install actual-software/actual/actual

# Zero-install (Node required — no Gatekeeper issues)
npx actual adr-bot`}</Pre>
                                    <div className="flex flex-col gap-[8px] rounded-[6px] border border-amber-500/20 bg-amber-500/5 px-[14px] py-[12px]">
                                        <p className="text-[11px] font-semibold uppercase tracking-[0.08em] text-amber-400/80">macOS Gatekeeper</p>
                                        <p className="text-white/50 text-[13px] leading-[1.7]">
                                            The binary isn&apos;t codesigned yet — our Apple Developer Program application is pending.
                                            After <Code>brew install</Code>, macOS will block it on first run. Remove the quarantine flag once:
                                        </p>
                                        <Pre>{`xattr -dr com.apple.quarantine $(which actual)`}</Pre>
                                        <p className="text-white/40 text-[12px] leading-[1.6]">
                                            Alternatively, use <Code>npx actual adr-bot</Code> — it runs through Node.js and isn&apos;t subject to Gatekeeper.
                                        </p>
                                    </div>
                                </div>
                            </div>

                            {/* Step 2 */}
                            <div className="flex gap-[16px]">
                                <div className="flex-shrink-0 size-[28px] rounded-full flex items-center justify-center text-[12px] font-bold text-black" style={{ background: BRAND_GRADIENT }}>
                                    2
                                </div>
                                <div className="flex flex-col gap-[10px] pt-[3px] flex-1">
                                    <H3>Authenticate</H3>
                                    <P>
                                        By default, <Code>actual</Code> uses Claude Code as the AI backend.
                                        Make sure it&apos;s installed and authenticated:
                                    </P>
                                    <Pre>{`npm install -g @anthropic-ai/claude-code
claude auth login
actual auth   # verify`}</Pre>
                                    <P>
                                        Prefer a different AI backend? See{" "}
                                        <a href="#runners" className="text-[#39eba1] hover:text-[#43bdb7] transition-colors underline underline-offset-2">
                                            Runners
                                        </a>{" "}
                                        for API key alternatives.
                                    </P>
                                </div>
                            </div>

                            {/* Step 3 */}
                            <div className="flex gap-[16px]">
                                <div className="flex-shrink-0 size-[28px] rounded-full flex items-center justify-center text-[12px] font-bold text-black" style={{ background: BRAND_GRADIENT }}>
                                    3
                                </div>
                                <div className="flex flex-col gap-[10px] pt-[3px] flex-1">
                                    <H3>Sync</H3>
                                    <P>From any git repo, run:</P>
                                    <Pre>{`actual adr-bot`}</Pre>
                                    <P>
                                        This analyzes your codebase, fetches matching ADRs, tailors them to your stack,
                                        and writes <Code>CLAUDE.md</Code>. Run it again whenever your architecture evolves.
                                    </P>
                                </div>
                            </div>
                        </div>
                    </Section>

                    {/* Supported Languages & Frameworks */}
                    <Section id="languages">
                        <H2>Supported Languages &amp; Frameworks</H2>
                        <P>
                            <Code>actual</Code> ships with a curated ADR bank covering the ecosystems below.
                            Each entry represents a language + framework combination with dedicated architectural
                            decision records tailored to real-world patterns in that stack.
                        </P>

                        <div className="grid grid-cols-1 sm:grid-cols-2 gap-[12px]">
                            {[
                                {
                                    language: "TypeScript",
                                    color: "#3178c6",
                                    frameworks: ["Next.js", "HeroUI"],
                                },
                                {
                                    language: "Rust",
                                    color: "#ce422b",
                                    frameworks: ["Ratatui"],
                                },
                                {
                                    language: "Python",
                                    color: "#f7c948",
                                    frameworks: ["FastAPI", "Django"],
                                },
                                {
                                    language: "Java",
                                    color: "#e76f00",
                                    frameworks: ["Spring Boot"],
                                },
                            ].map(({ language, color, frameworks }) => (
                                <div key={language} className="border border-[#393939] rounded-[6px] overflow-hidden bg-[#030301]">
                                    <div className="flex items-center gap-[8px] px-[14px] py-[10px] bg-[#0d0d0d] border-b border-[#393939]">
                                        <span className="size-[8px] rounded-full flex-shrink-0" style={{ background: color }} />
                                        <span className="text-[13px] font-semibold text-white/90">{language}</span>
                                    </div>
                                    <div className="flex flex-wrap gap-[6px] px-[14px] py-[12px]">
                                        {frameworks.map((fw) => (
                                            <span
                                                key={fw}
                                                className="text-[12px] font-medium text-white/70 bg-white/5 border border-white/10 rounded-[4px] px-[8px] py-[3px]"
                                            >
                                                {fw}
                                            </span>
                                        ))}
                                    </div>
                                </div>
                            ))}
                        </div>

                        <div className="bg-[#141414] border border-white/10 rounded-[6px] p-[16px] flex flex-col gap-[6px]">
                            <p className="text-[11px] font-semibold uppercase tracking-[0.07em] text-white/40">More coming soon</p>
                            <p className="text-white/60 text-[13px] leading-[1.65]">
                                We&apos;re hard at work expanding language and framework coverage and continuously
                                improving ADR quality. If your stack isn&apos;t listed yet, <Code>actual adr-bot</Code> will
                                still analyze your repo and apply any general-purpose ADRs that fit.
                            </p>
                        </div>
                    </Section>

                    {/* Commands */}
                    <Section id="commands">
                        <H2>Commands</H2>
                        <P>All subcommands and their flags.</P>
                    </Section>

                    {/* actual adr-bot */}
                    <Section id="sync">
                        <div className="flex flex-col gap-[4px]">
                            <Badge color="#39eba1">Command</Badge>
                            <H2>actual adr-bot</H2>
                        </div>
                        <P>
                            The main command. Analyzes your repo, fetches ADRs from the bank, tailors them to your codebase,
                            and writes the output file. Prompts for confirmation before writing.
                        </P>
                        <Pre>{`actual adr-bot [flags]`}</Pre>
                        <FlagTable rows={[
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
                        ]} />
                        <div className="flex flex-col gap-[12px]">
                            <H3>Examples</H3>
                            <Pre>{`# Preview without writing
actual adr-bot --dry-run

# Force fresh sync (bypass cache)
actual adr-bot --force

# Generate AGENTS.md instead of CLAUDE.md
actual adr-bot --output-format agents-md

# Use Anthropic API directly instead of Claude Code CLI
actual adr-bot --runner anthropic-api

# Use a specific model (runner auto-inferred)
actual adr-bot --model gpt-5.2

# Monorepo: target specific sub-projects
actual adr-bot --project packages/api --project packages/web

# Debug a hang
actual adr-bot --verbose --show-errors`}</Pre>
                        </div>
                    </Section>

                    {/* actual auth */}
                    <Section id="auth">
                        <div className="flex flex-col gap-[4px]">
                            <Badge color="#43bdb7">Command</Badge>
                            <H2>actual auth</H2>
                        </div>
                        <P>Check whether the default runner (Claude Code) is installed and authenticated.</P>
                        <Pre>{`actual auth`}</Pre>
                        <P>
                            Prints <Code>✔ Claude Code: authenticated</Code> on success, or an error with a fix hint on failure.
                            Use <Code>actual runners</Code> to check all backends at once.
                        </P>
                    </Section>

                    {/* actual status */}
                    <Section id="status">
                        <div className="flex flex-col gap-[4px]">
                            <Badge color="#43bdb7">Command</Badge>
                            <H2>actual status</H2>
                        </div>
                        <P>Show current config and the state of all managed output files in the current repo.</P>
                        <Pre>{`actual status
actual status --verbose`}</Pre>
                        <P>
                            <Code>--verbose</Code> additionally shows the runner, cached analysis details, ADR counts,
                            and telemetry status.
                        </P>
                    </Section>

                    {/* actual config */}
                    <Section id="config">
                        <div className="flex flex-col gap-[4px]">
                            <Badge color="#43bdb7">Command</Badge>
                            <H2>actual config</H2>
                        </div>
                        <P>
                            View or edit the global config file at{" "}
                            <Code>~/.actualai/actual/config.yaml</Code>.
                        </P>
                        <Pre>{`actual config show          # print current config (API keys redacted)
actual config path          # print config file location
actual config set <KEY> <VALUE>`}</Pre>
                        <H3>Settable keys</H3>
                        <div className="overflow-x-auto rounded-[6px] border border-[#393939]">
                            <table className="w-full text-[13px] leading-[1.6]">
                                <thead>
                                    <tr className="border-b border-[#393939] bg-[#0d0d0d]">
                                        <th className="text-left px-[16px] py-[10px] font-semibold text-white/60 uppercase text-[11px] tracking-[0.07em]">Key</th>
                                        <th className="text-left px-[16px] py-[10px] font-semibold text-white/60 uppercase text-[11px] tracking-[0.07em]">Default</th>
                                        <th className="text-left px-[16px] py-[10px] font-semibold text-white/60 uppercase text-[11px] tracking-[0.07em]">Description</th>
                                    </tr>
                                </thead>
                                <tbody className="divide-y divide-[#393939]/60">
                                    {[
                                        ["runner", "claude-cli", "AI backend to use for tailoring"],
                                        ["model", "—", "Default model (runner is auto-inferred)"],
                                        ["output_format", "claude-md", "claude-md | agents-md | cursor-rules"],
                                        ["batch_size", "15", "ADRs per API request batch"],
                                        ["concurrency", "10", "Max concurrent API requests"],
                                        ["invocation_timeout_secs", "600", "Runner timeout in seconds"],
                                        ["max_budget_usd", "—", "Spending cap per tailoring invocation"],
                                        ["max_turns", "—", "Max conversation turns (claude-cli only)"],
                                        ["max_per_framework", "—", "Max ADRs per detected framework"],
                                        ["include_general", "—", "Include language-agnostic ADRs"],
                                        ["include_categories", "—", "Only include these ADR categories (comma-separated)"],
                                        ["exclude_categories", "—", "Exclude these ADR categories (comma-separated)"],
                                        ["anthropic_api_key", "—", "Anthropic API key (stored at mode 0600)"],
                                        ["openai_api_key", "—", "OpenAI API key"],
                                        ["cursor_api_key", "—", "Cursor API key"],
                                        ["telemetry.enabled", "true", "Enable or disable telemetry"],
                                    ].map(([key, def, desc], i) => (
                                        <tr key={i} className="hover:bg-white/[0.02] transition-colors">
                                            <td className="px-[16px] py-[10px] align-top">
                                                <code className="font-mono text-[#39eba1] text-[12px]">{key}</code>
                                            </td>
                                            <td className="px-[16px] py-[10px] align-top whitespace-nowrap">
                                                <code className="font-mono text-white/40 text-[12px]">{def}</code>
                                            </td>
                                            <td className="px-[16px] py-[10px] align-top text-white/70">{desc}</td>
                                        </tr>
                                    ))}
                                </tbody>
                            </table>
                        </div>
                        <Pre>{`# Examples
actual config set runner anthropic-api
actual config set model claude-sonnet-4-6
actual config set output_format agents-md
actual config set invocation_timeout_secs 1200
actual config set telemetry.enabled false
actual config set anthropic_api_key "sk-ant-..."`}</Pre>
                    </Section>

                    {/* actual runners */}
                    <Section id="runners-cmd">
                        <div className="flex flex-col gap-[4px]">
                            <Badge color="#4d93c8">Command</Badge>
                            <H2>actual runners</H2>
                        </div>
                        <P>List all available AI backend runners and their current availability status.</P>
                        <Pre>{`actual runners`}</Pre>
                    </Section>

                    {/* actual models */}
                    <Section id="models-cmd">
                        <div className="flex flex-col gap-[4px]">
                            <Badge color="#4d93c8">Command</Badge>
                            <H2>actual models</H2>
                        </div>
                        <P>
                            List known model names grouped by runner. By default fetches live model lists from Anthropic
                            and OpenAI APIs and annotates newly discovered models.
                        </P>
                        <Pre>{`actual models
actual models --no-fetch   # skip live API fetch; show hardcoded list only`}</Pre>
                    </Section>

                    {/* actual cache */}
                    <Section id="cache-cmd">
                        <div className="flex flex-col gap-[4px]">
                            <Badge color="#4d93c8">Command</Badge>
                            <H2>actual cache</H2>
                        </div>
                        <P>
                            Clear the local analysis and tailoring cache. Cache entries expire after 7 days automatically.
                            Use <Code>--force</Code> on <Code>actual adr-bot</Code> to bypass the cache without clearing it.
                        </P>
                        <Pre>{`actual cache clear`}</Pre>
                    </Section>

                    {/* Runners */}
                    <Section id="runners">
                        <H2>Runners</H2>
                        <P>
                            A <em className="text-white/85 not-italic font-medium">runner</em> is the AI backend{" "}
                            <Code>actual</Code> uses to tailor ADRs to your codebase.
                            The default is <Code>claude-cli</Code> (Claude Code). Set a different runner with{" "}
                            <Code>--runner</Code> or <Code>actual config set runner</Code>.
                        </P>
                        <div className="flex flex-col gap-[16px]">
                            {[
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
                            ].map((r) => (
                                <div key={r.name} className="border border-[#393939] rounded-[6px] p-[20px] flex flex-col gap-[12px] bg-[#030301]">
                                    <div className="flex items-center justify-between gap-[12px] flex-wrap">
                                        <code className="font-mono text-[15px] font-semibold" style={{ color: r.color }}>
                                            {r.name}
                                        </code>
                                        <div className="flex items-center gap-[8px] text-[12px] text-white/40">
                                            <span>requires <code className="font-mono text-white/60">{r.req}</code></span>
                                            <span>·</span>
                                            <span>default model: <code className="font-mono text-white/60">{r.model}</code></span>
                                        </div>
                                    </div>
                                    <p className="text-white/60 text-[13px] leading-[1.65]">{r.note}</p>
                                    <Pre>{r.setup}</Pre>
                                </div>
                            ))}
                        </div>

                        <div className="bg-[#141414] border border-white/10 rounded-[6px] p-[16px] flex flex-col gap-[8px]">
                            <p className="text-[12px] font-semibold uppercase tracking-[0.07em] text-white/40">Runner auto-detection</p>
                            <p className="text-white/60 text-[13px] leading-[1.65]">
                                When no <Code>--runner</Code> is specified, the runner is inferred from the model name:
                                Anthropic aliases (<Code>sonnet</Code>, <Code>opus</Code>, <Code>haiku</Code>) → <Code>claude-cli</Code> then <Code>anthropic-api</Code>;
                                full <Code>claude-*</Code> IDs → <Code>anthropic-api</Code> then <Code>claude-cli</Code>;
                                <Code>gpt-*</Code> / <Code>o1*</Code> / <Code>o3*</Code> / <Code>o4*</Code> → <Code>codex-cli</Code> then <Code>openai-api</Code>.
                            </p>
                        </div>
                    </Section>

                    {/* Output Formats */}
                    <Section id="output-formats">
                        <H2>Output Formats</H2>
                        <P>
                            Choose the output format with <Code>--output-format</Code> or{" "}
                            <Code>actual config set output_format</Code>. All formats use identical managed-section
                            markers so content outside the markers is always preserved.
                        </P>
                        <div className="grid grid-cols-1 md:grid-cols-3 gap-[16px]">
                            {[
                                { value: "claude-md", file: "CLAUDE.md", tool: "Claude Code", color: "#39eba1" },
                                { value: "agents-md", file: "AGENTS.md", tool: "Codex CLI, OpenCode", color: "#43bdb7" },
                                { value: "cursor-rules", file: ".cursor/rules/actual-policies.mdc", tool: "Cursor IDE", color: "#4d93c8" },
                            ].map((f) => (
                                <div key={f.value} className="border border-[#393939] rounded-[6px] p-[16px] flex flex-col gap-[8px]">
                                    <code className="font-mono text-[13px] font-semibold" style={{ color: f.color }}>{f.value}</code>
                                    <p className="text-white text-[13px] font-medium">{f.file}</p>
                                    <p className="text-white/50 text-[12px]">{f.tool}</p>
                                </div>
                            ))}
                        </div>
                        <P>
                            Managed sections are delimited by <Code>{`<!-- managed:actual-start -->`}</Code> and{" "}
                            <Code>{`<!-- managed:actual-end -->`}</Code> markers. Any content you write outside
                            these markers is never touched by <Code>actual adr-bot</Code>.
                        </P>
                    </Section>

                    {/* Configuration */}
                    <Section id="configuration">
                        <H2>Configuration</H2>
                        <P>
                            Config lives at <Code>~/.actualai/actual/config.yaml</Code> (created automatically on first run,
                            mode 0600 on Unix). Override the path with the <Code>ACTUAL_CONFIG</Code> env var.
                        </P>
                        <H3>Environment variables</H3>
                        <div className="overflow-x-auto rounded-[6px] border border-[#393939]">
                            <table className="w-full text-[13px] leading-[1.6]">
                                <thead>
                                    <tr className="border-b border-[#393939] bg-[#0d0d0d]">
                                        <th className="text-left px-[16px] py-[10px] font-semibold text-white/60 uppercase text-[11px] tracking-[0.07em]">Variable</th>
                                        <th className="text-left px-[16px] py-[10px] font-semibold text-white/60 uppercase text-[11px] tracking-[0.07em]">Purpose</th>
                                    </tr>
                                </thead>
                                <tbody className="divide-y divide-[#393939]/60">
                                    {[
                                        ["ACTUAL_CONFIG", "Exact path to config file (highest precedence)"],
                                        ["ACTUAL_CONFIG_DIR", "Directory for config (must be absolute)"],
                                        ["ANTHROPIC_API_KEY", "Anthropic API key (overrides config value)"],
                                        ["OPENAI_API_KEY", "OpenAI API key (overrides config value)"],
                                        ["CURSOR_API_KEY", "Cursor API key (overrides config value)"],
                                        ["CLAUDE_BINARY", "Override path to the claude binary"],
                                        ["RUST_LOG", "Log level (default: warn)"],
                                    ].map(([k, v], i) => (
                                        <tr key={i} className="hover:bg-white/[0.02] transition-colors">
                                            <td className="px-[16px] py-[10px]">
                                                <code className="font-mono text-[#39eba1] text-[12px]">{k}</code>
                                            </td>
                                            <td className="px-[16px] py-[10px] text-white/70">{v}</td>
                                        </tr>
                                    ))}
                                </tbody>
                            </table>
                        </div>
                        <H3>Cache</H3>
                        <P>
                            Analysis and tailoring results are cached locally for 7 days (keyed to git HEAD + config hash).
                            Run <Code>actual cache clear</Code> to wipe, or pass <Code>--force</Code> to bypass on a single sync.
                        </P>
                    </Section>

                    {/* Troubleshooting */}
                    <Section id="troubleshooting">
                        <H2>Troubleshooting</H2>

                        <H3>Common errors</H3>
                        <div className="flex flex-col gap-[12px]">
                            {[
                                {
                                    err: "Claude Code is not installed",
                                    fix: "npm install -g @anthropic-ai/claude-code",
                                },
                                {
                                    err: "Claude Code is not authenticated",
                                    fix: "claude auth login",
                                },
                                {
                                    err: "No runner available for model '…'",
                                    fix: "Install a runner or set an API key. Run actual runners to see what's available.",
                                },
                                {
                                    err: "Runner timed out after 600s",
                                    fix: "actual config set invocation_timeout_secs 1200",
                                },
                                {
                                    err: "Insufficient credits",
                                    fix: "Add credits at your provider's billing page, or use --max-budget-usd to cap spend.",
                                },
                                {
                                    err: "Analysis returned no projects",
                                    fix: "Make sure you're running from inside a git repo. For monorepos, use --project <PATH>.",
                                },
                            ].map((e, i) => (
                                <div key={i} className="border border-[#393939] rounded-[6px] overflow-hidden">
                                    <div className="bg-[#0d0d0d] border-b border-[#393939] px-[16px] py-[10px]">
                                        <code className="font-mono text-[12px] text-white/60">{e.err}</code>
                                    </div>
                                    <div className="px-[16px] py-[10px] text-white/70 text-[13px] leading-[1.65]">
                                        {e.fix}
                                    </div>
                                </div>
                            ))}
                        </div>

                        <H3>Debug flags</H3>
                        <Pre>{`# See full AI runner output
actual adr-bot --verbose

# Stream runner stderr in real time (auth prompts, permission errors)
actual adr-bot --show-errors

# Disable TUI for plain output (useful in CI)
actual adr-bot --no-tui

# Full debug logging
RUST_LOG=debug actual adr-bot --no-tui 2>&1`}</Pre>

                        <H3>Exit codes</H3>
                        <div className="overflow-x-auto rounded-[6px] border border-[#393939]">
                            <table className="w-full text-[13px] leading-[1.6]">
                                <thead>
                                    <tr className="border-b border-[#393939] bg-[#0d0d0d]">
                                        <th className="text-left px-[16px] py-[10px] font-semibold text-white/60 uppercase text-[11px] tracking-[0.07em]">Code</th>
                                        <th className="text-left px-[16px] py-[10px] font-semibold text-white/60 uppercase text-[11px] tracking-[0.07em]">Meaning</th>
                                    </tr>
                                </thead>
                                <tbody className="divide-y divide-[#393939]/60">
                                    {[
                                        ["0", "Success"],
                                        ["1", "General runtime error"],
                                        ["2", "Auth / setup error (binary not found, not authenticated, API key missing)"],
                                        ["3", "Billing / API error (credits too low)"],
                                        ["4", "User cancelled"],
                                        ["5", "I/O error (permissions, disk space)"],
                                    ].map(([code, meaning], i) => (
                                        <tr key={i} className="hover:bg-white/[0.02] transition-colors">
                                            <td className="px-[16px] py-[10px]">
                                                <code className="font-mono text-[#39eba1] text-[13px] font-bold">{code}</code>
                                            </td>
                                            <td className="px-[16px] py-[10px] text-white/70">{meaning}</td>
                                        </tr>
                                    ))}
                                </tbody>
                            </table>
                        </div>
                    </Section>

                </main>
            </div>
        </div>
    );
}
