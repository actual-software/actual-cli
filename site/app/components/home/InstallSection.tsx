"use client";

import { useState } from "react";

const SECTION_GRADIENT = "linear-gradient(90deg, #39eba1 0%, #43bdb7 50.483%, #4d93c8 100%)";

function CopyButton({ text }: { text: string }) {
    const [copied, setCopied] = useState(false);

    const handleCopy = async () => {
        await navigator.clipboard.writeText(text);
        setCopied(true);
        setTimeout(() => setCopied(false), 2000);
    };

    return (
        <button
            onClick={handleCopy}
            className="flex-shrink-0 text-[12px] font-medium text-white/40 hover:text-white/80 transition-colors px-2 py-1 rounded border border-white/10 hover:border-white/25"
            aria-label="Copy to clipboard"
        >
            {copied ? "Copied!" : "Copy"}
        </button>
    );
}

function CodeBlock({ children, copyText }: { children: string; copyText?: string }) {
    return (
        <div className="flex items-center justify-between gap-4 bg-[#0d0d0d] border border-[#393939] rounded-[6px] px-[16px] py-[12px]">
            <code className="font-mono text-[14px] text-white/85 leading-[1.6] whitespace-pre">
                {children}
            </code>
            <CopyButton text={copyText ?? children} />
        </div>
    );
}

export default function InstallSection() {
    return (
        <section
            id="install"
            className="w-full bg-[#030301] flex flex-col items-center py-[80px] px-6 gap-[64px]"
        >
            {/* Section header */}
            <div className="flex flex-col items-center gap-[24px] w-full max-w-[1400px]">
                <div className="bg-[#141414] border border-white/15 rounded-[50px] px-[18px] py-[10px]">
                    <p className="text-[16px] leading-[26px] text-white tracking-[-0.0016px]">
                        Installation
                    </p>
                </div>

                <div className="flex flex-wrap justify-center items-baseline gap-x-[10px] text-center">
                    <p className="font-light text-[28px] md:text-[32px] xl:text-[36px] leading-[1.5] tracking-[-0.015em] text-white">
                        Up and running in
                    </p>
                    <p
                        className="font-light text-[28px] md:text-[32px] xl:text-[36px] leading-[1.5] tracking-[-0.015em] bg-clip-text text-transparent"
                        style={{ backgroundImage: SECTION_GRADIENT }}
                    >
                        one command
                    </p>
                </div>
            </div>

            {/* Install cards — two columns on desktop */}
            <div className="grid grid-cols-1 md:grid-cols-2 gap-[24px] w-full max-w-[900px]">

                {/* Homebrew */}
                <div className="flex flex-col gap-[20px] border border-[#393939] rounded-[4px] p-[24px] bg-[#030301]">
                    <div className="flex flex-col gap-[4px]">
                        <p className="text-[11px] font-semibold uppercase tracking-[0.1em] text-[#04ed87]">
                            Homebrew
                        </p>
                        <h3 className="text-white text-[18px] font-medium leading-[28px]">
                            macOS &amp; Linux
                        </h3>
                    </div>
                    <div className="flex flex-col gap-[8px]">
                        <CodeBlock copyText="brew install actual-software/actual/actual">
                            brew install actual-software/actual/actual
                        </CodeBlock>
                    </div>
                    <p className="text-white/50 text-[13px] leading-[1.6]">
                        Installs the native binary. Run{" "}
                        <code className="font-mono text-white/70">actual adr-bot</code> from
                        any repo.
                    </p>
                    {/* Then run */}
                    <div className="flex flex-col gap-[8px] pt-[4px] border-t border-[#393939]">
                        <p className="text-white/40 text-[11px] uppercase tracking-[0.08em] font-medium">
                            Then run
                        </p>
                        <CodeBlock>actual adr-bot</CodeBlock>
                    </div>
                </div>

                {/* npx */}
                <div className="flex flex-col gap-[20px] border border-[#393939] rounded-[4px] p-[24px] bg-[#030301]">
                    <div className="flex flex-col gap-[4px]">
                        <p className="text-[11px] font-semibold uppercase tracking-[0.1em] text-[#1ae5b5]">
                            npx
                        </p>
                        <h3 className="text-white text-[18px] font-medium leading-[28px]">
                            Zero install
                        </h3>
                    </div>
                    <div className="flex flex-col gap-[8px]">
                        <CodeBlock>npx @actualai/actual adr-bot</CodeBlock>
                    </div>
                    <p className="text-white/50 text-[13px] leading-[1.6]">
                        No install needed. Works anywhere Node is available — great for CI
                        and quick tries.
                    </p>
                </div>
            </div>

        </section>
    );
}
