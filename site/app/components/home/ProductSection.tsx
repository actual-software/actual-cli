const SECTION_GRADIENT = "linear-gradient(90deg, #39eba1 0%, #43bdb7 50.483%, #4d93c8 100%)";

const CARDS = [
    {
        color: "#04ed87",
        label: "Codebase Analysis",
        title: "Understand your repo instantly",
        description:
            "actual inspects your languages, frameworks, and existing conventions to build a precise taxonomy — no manual config required.",
    },
    {
        color: "#1ae5b5",
        label: "Context File Management",
        title: "Context files that stay in sync",
        description:
            "Architectural Decisions are fetched, tailored to your codebase, and written into CLAUDE.md, AGENTS.md, or Cursor rules — updated on every sync.",
    },
    {
        color: "#01a4fc",
        label: "Architectural Guardrails",
        title: "Keep every agent on the same page",
        description:
            "Your team's decisions become hard constraints for AI agents. No more architectural drift, no more inconsistent patterns across PRs.",
    },
];

export default function ProductSection() {
    return (
        <section className="w-full bg-[#030301] flex flex-col items-center py-[80px] px-6 gap-[64px]">

            {/* Section header */}
            <div className="flex flex-col items-center gap-[24px] w-full max-w-[1400px]">
                <div className="bg-[#141414] border border-white/15 rounded-[50px] px-[18px] py-[10px]">
                    <p className="text-[16px] leading-[26px] text-white tracking-[-0.0016px]">
                        How it works
                    </p>
                </div>

                <div className="flex flex-wrap justify-center items-baseline gap-x-[10px] text-center">
                    <p className="font-light text-[28px] md:text-[32px] xl:text-[36px] leading-[1.5] tracking-[-0.015em] text-white">
                        Three steps to
                    </p>
                    <p
                        className="font-light text-[28px] md:text-[32px] xl:text-[36px] leading-[1.5] tracking-[-0.015em] bg-clip-text text-transparent"
                        style={{ backgroundImage: SECTION_GRADIENT }}
                    >
                        architecturally aligned agents
                    </p>
                </div>
            </div>

            {/* Cards */}
            <div className="grid grid-cols-1 md:grid-cols-3 gap-[24px] w-full max-w-[1400px]">
                {CARDS.map((card) => (
                    <div
                        key={card.title}
                        className="group relative border border-[#393939] rounded-[4px] p-[24px] bg-[#030301] overflow-hidden flex flex-col gap-[12px]"
                    >
                        {/* Hover glow — exact match to actual.ai */}
                        <div
                            className="absolute inset-0 opacity-0 transition-opacity duration-300 group-hover:opacity-100 pointer-events-none"
                            style={{
                                background:
                                    "radial-gradient(120% 120% at 100% 0%, rgba(255,255,255,0.16) 0%, rgba(255,255,255,0.02) 40%, transparent 70%)",
                            }}
                        />

                        {/* Color label */}
                        <p
                            className="text-[11px] font-semibold uppercase tracking-[0.1em]"
                            style={{ color: card.color }}
                        >
                            {card.label}
                        </p>

                        {/* Title */}
                        <h3 className="text-white text-[18px] font-medium leading-[28px] tracking-[-0.002px]">
                            {card.title}
                        </h3>

                        {/* Description */}
                        <p className="text-white/70 text-[15px] leading-[1.65]">
                            {card.description}
                        </p>
                    </div>
                ))}
            </div>
        </section>
    );
}
