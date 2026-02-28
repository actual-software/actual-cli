"use client";

import { LANGUAGES } from "../../../lib/docs-data";

const SECTION_GRADIENT = "linear-gradient(90deg, #39eba1 0%, #43bdb7 50.483%, #4d93c8 100%)";

const FRAMEWORKS = LANGUAGES.flatMap(({ language, color, frameworks }) =>
    frameworks.map((fw) => ({ language, color, framework: fw }))
);

// Duplicate so the second copy fills the gap when the first scrolls off
const DOUBLED = [...FRAMEWORKS, ...FRAMEWORKS];

export default function FrameworksCarousel() {
    return (
        <section
            id="frameworks"
            className="w-full bg-[#030301] flex flex-col items-center py-[80px] gap-[48px] overflow-hidden"
        >
            {/* Section header */}
            <div className="flex flex-col items-center gap-[16px] w-full max-w-[1400px] px-6">
                <div className="bg-[#141414] border border-white/15 rounded-[50px] px-[18px] py-[10px]">
                    <p className="text-[16px] leading-[26px] text-white tracking-[-0.0016px]">
                        Supported Frameworks
                    </p>
                </div>
                <div className="flex flex-wrap justify-center items-baseline gap-x-[10px] text-center">
                    <p className="font-light text-[28px] md:text-[32px] xl:text-[36px] leading-[1.5] tracking-[-0.015em] text-white">
                        Works with your
                    </p>
                    <p
                        className="font-light text-[28px] md:text-[32px] xl:text-[36px] leading-[1.5] tracking-[-0.015em] bg-clip-text text-transparent"
                        style={{ backgroundImage: SECTION_GRADIENT }}
                    >
                        stack
                    </p>
                </div>
                <p className="text-white/50 text-[15px] leading-[1.65] text-center max-w-[480px]">
                    Curated ADR banks for every framework below — so architectural decisions are always tailored to your tech.
                </p>
            </div>

            {/* Marquee track */}
            <div className="relative w-1/2 overflow-hidden">
                {/* Edge fades */}
                <div
                    className="absolute inset-y-0 left-0 w-[120px] z-10 pointer-events-none"
                    style={{ background: "linear-gradient(to right, #030301, transparent)" }}
                />
                <div
                    className="absolute inset-y-0 right-0 w-[120px] z-10 pointer-events-none"
                    style={{ background: "linear-gradient(to left, #030301, transparent)" }}
                />

                {/* Scrolling cards — inline-flex so the element width = content width,
                    making translateX(-50%) loop exactly at the halfway point */}
                <div
                    className="inline-flex gap-[16px]"
                    style={{ animation: "marquee 28s linear infinite" }}
                >
                    {DOUBLED.map(({ language, color, framework }, i) => (
                        <div
                            key={i}
                            className="flex-shrink-0 w-[240px] border border-[#393939] rounded-[6px] bg-[#030301] p-[24px] flex flex-col gap-[14px]"
                        >
                            <div className="flex items-center gap-[8px]">
                                <span
                                    className="size-[8px] rounded-full flex-shrink-0"
                                    style={{ background: color }}
                                />
                                <span
                                    className="text-[11px] font-semibold uppercase tracking-[0.08em]"
                                    style={{ color }}
                                >
                                    {language}
                                </span>
                            </div>
                            <p className="text-white text-[20px] font-medium leading-[1.3] tracking-[-0.01em]">
                                {framework}
                            </p>
                        </div>
                    ))}
                </div>
            </div>
        </section>
    );
}
