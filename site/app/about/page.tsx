import type { Metadata } from "next";
import Image from "next/image";
import { FaLinkedin } from "react-icons/fa";

export const metadata: Metadata = {
    title: "About — actual CLI",
    description: "Learn about Actual AI's mission, founders, advisors, and supporting programs.",
};

const BRAND_GRADIENT = "linear-gradient(90deg, #39eba1 0%, #43bdb7 50.483%, #4d93c8 100%)";

function GradientText({ children }: { children: React.ReactNode }) {
    return (
        <span className="bg-clip-text text-transparent inline-block" style={{ backgroundImage: BRAND_GRADIENT }}>
            {children}
        </span>
    );
}

const FOUNDERS = [
    {
        name: "John Kennedy",
        title: "Chief Executive Officer",
        image: "/headshots/john-kennedy.png",
        linkedin: "https://www.linkedin.com/in/johnakennedy/",
    },
];

const ADVISORS = [
    {
        name: "Eleanor Meegoda",
        image: "/headshots/eleanor-meegoda.png",
        linkedin: "https://www.linkedin.com/in/eleanormeegoda/",
    },
    {
        name: "Justin Emond",
        image: "/headshots/justin-emond.png",
        linkedin: "https://www.linkedin.com/in/justinemond/",
    },
    {
        name: "Simon Poile",
        image: "/headshots/simon-poile.png",
        linkedin: "https://www.linkedin.com/in/simonpoile/",
    },
    {
        name: "Adam Frankl",
        image: "/headshots/adam-frankl.jpeg",
        linkedin: "https://www.linkedin.com/in/adamfrankl/",
    },
];

const PROGRAMS = [
    { src: "/programs/alchemist-accelerator.png", alt: "Alchemist Accelerator" },
    { src: "/programs/foundations.png", alt: "Foundations" },
    { src: "/programs/wtia.png", alt: "WTIA" },
    { src: "/programs/orangedao.svg", alt: "OrangeDAO" },
];

export default function AboutPage() {
    return (
        <main className="relative flex-1 w-full bg-[#030301] text-white">
            {/* Background glow */}
            <div className="pointer-events-none absolute inset-0 -z-10 overflow-hidden">
                <div className="absolute right-0 top-0 h-[1082px] w-[1440px] opacity-90">
                    <Image src="/figma/v2/bg-right.svg" alt="" fill className="object-cover" priority={false} />
                </div>
            </div>

            <div className="mx-auto w-full max-w-[1200px] px-6 pt-[120px] pb-[120px]">

                {/* Hero */}
                <div className="text-center mb-[80px]">
                    <h1 className="text-[48px] md:text-[64px] leading-[1.05] tracking-[-0.02em] font-semibold text-white">
                        About <GradientText>Actual AI</GradientText>
                    </h1>
                    <p className="mt-[16px] text-[20px] leading-[31px] tracking-[-0.002px] text-white/80 max-w-[820px] mx-auto">
                        We are building an agent for engineering managers.
                    </p>
                </div>

                {/* Mission */}
                <div className="w-full mb-20">
                    <div className="grid grid-cols-1 md:max-w-2xl m-auto gap-12 items-center">
                        <div>
                            <h2 className="text-[28px] md:text-[32px] font-medium mb-6 text-center">
                                Our Mission
                            </h2>
                            <p className="text-[16px] leading-[26px] text-white/80 mb-6">
                                At Actual AI, our mission is to upgrade every software team to AI-powered software
                                development. We&apos;re revolutionizing how engineering teams are managed and shaped,
                                optimizing for AI tools and workflows.
                            </p>
                            <p className="text-[16px] leading-[26px] text-white/80 mb-6">
                                Our platform uses an advanced LLM-based pipeline to analyze code contributions and
                                development tickets. Using this analysis we provide guardrails for AI-powered software
                                development including agent-agnostic context engineering using an agent-native system of
                                record for architectural decisions, actionable insights, automated progress reporting,
                                velocity measurement, and code governance for AI-Agents.
                            </p>
                            <p className="text-[16px] leading-[26px] text-white/80">
                                For enterprises, we give engineering leadership complete visibility over the software
                                development process and the ability to ensure Agents and AI-enabled developers are
                                writing code that is consistent with design decisions, architectural standards, and
                                product goals across codebases and teams.
                            </p>
                        </div>
                    </div>
                </div>

                {/* Founders */}
                <div className="w-full mb-20">
                    <h2 className="text-[28px] md:text-[32px] font-medium mb-12 text-center">
                        Founders
                    </h2>
                    <div className="flex justify-center">
                        {FOUNDERS.map((f) => (
                            <div key={f.name} className="border border-[#393939] rounded-[4px] p-8 bg-[#030301] w-full md:w-[320px]">
                                <div className="relative h-[200px] w-[200px] mx-auto mb-6 rounded-full overflow-hidden">
                                    <Image src={f.image} alt={f.name} fill className="object-cover" />
                                </div>
                                <div className="flex items-center justify-center gap-4 mb-2">
                                    <h3 className="text-2xl font-semibold">{f.name}</h3>
                                    <a
                                        href={f.linkedin}
                                        target="_blank"
                                        rel="noopener noreferrer"
                                        className="text-white/70 hover:text-white transition-colors"
                                    >
                                        <FaLinkedin size={24} />
                                    </a>
                                </div>
                                <p className="text-white/70 font-medium text-center">{f.title}</p>
                            </div>
                        ))}
                    </div>
                </div>

                {/* Advisors */}
                <div className="w-full mb-24">
                    <h2 className="text-[28px] md:text-[32px] font-medium mb-12 text-center">
                        Advisors
                    </h2>
                    <div className="grid grid-cols-1 md:grid-cols-2 gap-8 max-w-2xl mx-auto">
                        {ADVISORS.map((a) => (
                            <div key={a.name} className="border border-[#393939] rounded-[4px] p-8 bg-[#030301]">
                                <div className="relative h-[200px] w-[200px] mx-auto mb-6 rounded-full overflow-hidden">
                                    <Image src={a.image} alt={a.name} fill className="object-cover" />
                                </div>
                                <div className="flex items-center justify-center gap-4">
                                    <h3 className="text-2xl font-semibold">{a.name}</h3>
                                    <a
                                        href={a.linkedin}
                                        target="_blank"
                                        rel="noopener noreferrer"
                                        className="text-white/70 hover:text-white transition-colors"
                                    >
                                        <FaLinkedin size={24} />
                                    </a>
                                </div>
                            </div>
                        ))}
                    </div>
                </div>

                {/* Supporting Programs */}
                <div className="w-full mb-4">
                    <h2 className="text-[28px] md:text-[32px] font-medium mb-12 text-center">
                        Supporting Programs
                    </h2>
                    <div className="grid grid-cols-2 md:grid-cols-4 gap-x-6 gap-y-6 max-w-4xl mx-auto items-center">
                        {PROGRAMS.map((p) => (
                            <div key={p.alt}>
                                <div className="relative h-[100px] w-full mx-auto rounded-xl overflow-hidden">
                                    <Image src={p.src} alt={p.alt} fill className="object-contain" />
                                </div>
                            </div>
                        ))}
                    </div>
                </div>

                {/* CTA */}
                <div className="w-full relative flex flex-col items-center justify-center py-20 mt-16">
                    <div className="absolute inset-0 -z-10 overflow-hidden">
                        <div className="absolute inset-0 bg-gradient-to-br from-[#2076BB]/15 via-[#04ed87]/10 to-[#2076BB]/15" />
                        <div className="absolute inset-0 bg-gradient-to-b from-transparent via-transparent to-black/50" />
                    </div>
                    <div className="max-w-5xl w-full px-8 py-24 flex flex-col items-center justify-center mx-auto">
                        <h2 className="text-[32px] md:text-[40px] text-white text-center font-medium tracking-[-0.02em]">
                            Ready to transform your engineering team?
                        </h2>
                        <p className="text-[18px] md:text-[20px] text-white/80 mt-6 text-center font-normal leading-[31px] tracking-[-0.002px]">
                            Join the growing number of companies using Actual AI to build high-performing teams.
                        </p>
                        <div className="mt-12">
                            <a
                                href="https://cal.com/john-actual/demo"
                                target="_blank"
                                rel="noopener noreferrer"
                                className="flex items-center justify-center rounded-full px-[24px] py-[12px] text-[16px] font-medium bg-white text-black hover:bg-white/90 transition-colors w-fit"
                            >
                                <span className="leading-[1.5]">Book A Demo</span>
                            </a>
                        </div>
                    </div>
                </div>

            </div>
        </main>
    );
}
