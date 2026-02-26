import Link from "next/link";
import Image from "next/image";

const BRAND_GRADIENT = "linear-gradient(90deg, #39eba1 0%, #43bdb7 50.483%, #4d93c8 100%)";

export default function Hero() {
    return (
        <section className="relative overflow-hidden bg-[#030301] min-h-[calc(100svh-66px)] flex flex-col items-center justify-center px-6 text-center">

            {/* Background — same as actual.ai */}
            <div className="absolute inset-0 -z-10">
                <Image
                    src="/figma/v2/bg.svg"
                    alt=""
                    fill
                    className="object-cover opacity-80"
                    priority
                />
                <div className="absolute inset-0 bg-hero-grid opacity-40" />
                <div className="absolute inset-0 hero-vignette" />
            </div>

            <div className="flex flex-col items-center gap-[20px] w-full max-w-[820px]">

                {/* Monospace badge — the binary name */}
                <div className="bg-[#141414] border border-white/15 rounded-[50px] px-[18px] py-[8px]">
                    <code className="text-[14px] leading-[22px] font-mono text-white/80 tracking-wide">
                        $ actual adr-bot
                    </code>
                </div>

                {/* Headline */}
                <h1 className="text-[44px] md:text-[64px] xl:text-[72px] font-semibold leading-[1.1] tracking-[-0.03em] text-white">
                    AI context for your{" "}
                    <br className="hidden md:block" />
                    <span
                        className="bg-clip-text text-transparent"
                        style={{ backgroundImage: BRAND_GRADIENT }}
                    >
                        codebase.
                    </span>
                    {" "}Always current.
                </h1>

                {/* Description */}
                <p className="text-white/70 text-[17px] md:text-[19px] leading-[1.65] max-w-[600px]">
                    <code className="font-mono text-white/90">actual</code> analyzes your
                    repo, matches it against your team&apos;s Architectural Decisions, and
                    writes them into AI context files — keeping every coding agent aligned,
                    automatically.
                </p>

                {/* CTAs */}
                <div className="flex flex-wrap items-center justify-center gap-[12px] mt-[4px]">
                    <Link
                        href="#install"
                        className="inline-flex items-center justify-center rounded-full px-[24px] py-[10px] text-[16px] font-medium bg-white text-black hover:bg-white/90 transition-colors"
                    >
                        Get Started
                    </Link>
                    <Link
                        href="https://actual.ai"
                        className="inline-flex items-center justify-center rounded-full px-[24px] py-[10px] text-[16px] font-medium border border-white/20 text-white hover:bg-white/5 transition-colors"
                    >
                        More about Actual AI →
                    </Link>
                </div>
            </div>
        </section>
    );
}
