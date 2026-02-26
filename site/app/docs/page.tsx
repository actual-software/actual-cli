import type { Metadata } from "next";
import {
    ADR_BOT_FLAGS,
    ADR_BOT_EXAMPLES,
    RUNNERS,
    OUTPUT_FORMATS,
    CONFIG_KEYS,
    CONFIG_EXAMPLES,
    ENV_VARS,
    COMMON_ERRORS,
    EXIT_CODES,
    LANGUAGES,
} from "../../lib/docs-data";

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
npx @actualai/actual adr-bot`}</Pre>
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
                            {LANGUAGES.map(({ language, color, frameworks }) => (
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
                        <FlagTable rows={ADR_BOT_FLAGS} />
                        <div className="flex flex-col gap-[12px]">
                            <H3>Examples</H3>
                            <Pre>{ADR_BOT_EXAMPLES.map((e) => `# ${e.comment}\n${e.cmd}`).join("\n\n")}</Pre>
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
                                    {CONFIG_KEYS.map((k, i) => (
                                        <tr key={i} className="hover:bg-white/[0.02] transition-colors">
                                            <td className="px-[16px] py-[10px] align-top">
                                                <code className="font-mono text-[#39eba1] text-[12px]">{k.key}</code>
                                            </td>
                                            <td className="px-[16px] py-[10px] align-top whitespace-nowrap">
                                                <code className="font-mono text-white/40 text-[12px]">{k.default_}</code>
                                            </td>
                                            <td className="px-[16px] py-[10px] align-top text-white/70">{k.desc}</td>
                                        </tr>
                                    ))}
                                </tbody>
                            </table>
                        </div>
                        <Pre>{`# Examples\n${CONFIG_EXAMPLES.join("\n")}`}</Pre>
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
                            {RUNNERS.map((r) => (
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
                            {OUTPUT_FORMATS.map((f) => (
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
                                    {ENV_VARS.map((v, i) => (
                                        <tr key={i} className="hover:bg-white/[0.02] transition-colors">
                                            <td className="px-[16px] py-[10px]">
                                                <code className="font-mono text-[#39eba1] text-[12px]">{v.name}</code>
                                            </td>
                                            <td className="px-[16px] py-[10px] text-white/70">{v.purpose}</td>
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
                            {COMMON_ERRORS.map((e, i) => (
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
                                    {EXIT_CODES.map((c, i) => (
                                        <tr key={i} className="hover:bg-white/[0.02] transition-colors">
                                            <td className="px-[16px] py-[10px]">
                                                <code className="font-mono text-[#39eba1] text-[13px] font-bold">{c.code}</code>
                                            </td>
                                            <td className="px-[16px] py-[10px] text-white/70">{c.meaning}</td>
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
