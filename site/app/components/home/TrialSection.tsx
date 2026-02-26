import Image from "next/image";
import Link from "next/link";

export default function TrialSection() {
    return (
        <section className="w-full bg-[#030301] px-[24px] pb-[80px]">
            <div className="flex flex-col xl:flex-row gap-[24px] w-full">

                {/* ── Trial card (main, wide) ────────────────────── */}
                <div className="flex-1 min-w-0 py-[24px]">
                    <div
                        className="relative w-full overflow-hidden rounded-[8px] border border-[#DBEAFE]"
                        style={{
                            height: "900px",
                            background:
                                "linear-gradient(90deg, rgba(239,251,243,1) 1%, rgba(232,240,252,1) 100%)",
                        }}
                    >
                        {/* Full-card background illustration */}
                        <Image
                            src="/images/card-header.svg"
                            alt="Actual AI CLI app"
                            fill
                            className="object-cover object-top-left"
                        />

                        {/* Logo + title overlay — Figma absolute position x:660, y:50 */}
                        <div className="absolute top-[50px] left-[660px] flex items-center gap-[13px]">
                            <div className="relative size-[55px] flex-shrink-0">
                                <Image
                                    src="/figma/v2/nav-logo.png"
                                    alt="Actual AI"
                                    width={55}
                                    height={55}
                                    className="rounded-[8px] object-cover"
                                />
                            </div>
                            <h2 className="text-white text-[48px] font-semibold leading-[1] tracking-[-0.0125em] whitespace-nowrap drop-shadow-lg">
                                Welcome to Actual AI
                            </h2>
                        </div>
                    </div>
                </div>

                {/* ── Sidebar (Context File preview) ───────────────── */}
                <div className="xl:w-[632px] flex-shrink-0 flex flex-col bg-white rounded-[8px] overflow-hidden self-start">
                    {/* Header */}
                    <div className="flex flex-col items-center px-[8px] py-[16px] border-b border-[#DFE4ED]">
                        <div className="flex justify-center w-full px-[16px] py-[6px]">
                            <span className="text-[16px] font-semibold leading-[1.25] text-[#1F2328]">
                                Context File...
                            </span>
                        </div>
                    </div>

                    {/* Body text */}
                    <div className="px-[16px] py-[8px]">
                        <p className="text-[14px] leading-[1.714] text-[#6B7280]">
                            Based on your complete decision history.
                        </p>
                    </div>

                    {/* Screenshot */}
                    <Image
                        src="/images/screenshot.png"
                        alt="Context file screenshot"
                        width={632}
                        height={215}
                        className="w-full object-cover object-top"
                    />
                </div>

            </div>

            {/* ── Bottom CTA ────────────────────────────────────── */}
            <div className="flex justify-center pt-[64px]">
                <Link
                    href="/contact"
                    className="inline-flex items-center justify-center rounded-full px-[32px] py-[14px] text-[18px] font-medium bg-white text-black hover:bg-white/90 transition-colors"
                >
                    Get Started with Actual AI
                </Link>
            </div>
        </section>
    );
}
