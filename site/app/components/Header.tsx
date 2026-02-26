"use client";

import Image from "next/image";
import Link from "next/link";

function JoinNowButton({ className = "" }: { className?: string }) {
    return (
        <Link
            href="/contact"
            className={`flex items-center justify-center rounded-full px-[16px] py-[4px] text-[16px] font-medium bg-white text-black hover:bg-white/90 transition-colors ${className}`}
        >
            <span className="leading-[1.5]">Join Now</span>
        </Link>
    );
}

export default function Header() {
    return (
        <header className="sticky top-0 left-0 right-0 z-[100] w-full">
            {/* Desktop */}
            <div className="hidden md:block border-b border-[#353534] bg-black/60 backdrop-blur-xl w-full">
                <div className="mx-auto max-w-[1440px] px-6 py-[13px] lg:px-12 xl:px-[100px]">
                    <div className="flex items-center justify-between w-full max-w-[1400px] mx-auto">
                        {/* Logo + brand name */}
                        <Link href="/" className="flex items-center gap-[8px] shrink-0">
                            <div className="relative size-[40px]">
                                <Image
                                    src="/figma/v2/nav-logo.png"
                                    alt="Actual AI Logo"
                                    width={40}
                                    height={40}
                                    priority
                                />
                            </div>
                            <span className="text-white text-[16px] font-bold leading-[26px] tracking-[-0.0016px]">
                                Actual AI
                            </span>
                        </Link>

                        {/* Nav links */}
                        <nav className="flex items-center justify-center gap-[30px] px-[40px] py-[8px] rounded-[60px] text-[16px] font-normal text-white tracking-[-0.0016px]">
                            {/* Agents dropdown */}
                            <div className="relative group">
                                <button
                                    type="button"
                                    aria-haspopup="menu"
                                    className="flex items-center gap-[3px]"
                                >
                                    <span className="leading-[26px]">Agents</span>
                                    <Image
                                        src="/figma/v2/caret-down.svg"
                                        alt=""
                                        width={14}
                                        height={14}
                                    />
                                </button>
                                <div className="pointer-events-none absolute left-0 top-full w-56 pt-3 opacity-0 translate-y-1 transition-all duration-150 group-hover:pointer-events-auto group-hover:opacity-100 group-hover:translate-y-0 group-focus-within:pointer-events-auto group-focus-within:opacity-100 group-focus-within:translate-y-0">
                                    <div className="rounded-[8px] border border-white/10 bg-black/80 backdrop-blur-md shadow-[0_10px_30px_rgba(0,0,0,0.55)]">
                                        <Link href="https://actual.ai/agents/architecture-agent" className="block px-4 py-2.5 hover:bg-white/5 transition-colors">
                                            Architecture Agent
                                        </Link>
                                        <Link href="https://actual.ai/agents/management-agent" className="block px-4 py-2.5 hover:bg-white/5 transition-colors">
                                            Management Agent
                                        </Link>
                                    </div>
                                </div>
                            </div>

                            <Link href="/docs" className="leading-[26px] hover:text-white/80 transition-colors">
                                Docs
                            </Link>
                            <Link href="https://actual.ai/pricing" className="leading-[26px] hover:text-white/80 transition-colors">
                                Pricing
                            </Link>
                            <Link href="https://actual.ai/about" className="leading-[26px] hover:text-white/80 transition-colors">
                                About
                            </Link>
                            <Link href="https://actual.ai/blog" className="leading-[26px] hover:text-white/80 transition-colors">
                                Blog
                            </Link>
                        </nav>

                        <JoinNowButton />
                    </div>
                </div>
            </div>

            {/* Mobile */}
            <div className="md:hidden border-b border-white/10 bg-black/60 backdrop-blur-xl w-full">
                <div className="mx-auto max-w-7xl px-4 py-4">
                    <div className="flex items-center justify-between">
                        <Link href="/" className="flex items-center gap-[8px] shrink-0">
                            <Image
                                src="/figma/v2/nav-logo.png"
                                alt="Actual AI Logo"
                                width={32}
                                height={32}
                                priority
                            />
                            <span className="text-white text-[14px] font-bold leading-[20px]">
                                Actual AI
                            </span>
                        </Link>
                        <details className="group">
                            <summary className="list-none cursor-pointer select-none rounded-lg p-2 hover:bg-white/5">
                                <span className="sr-only">Open menu</span>
                                <div className="flex flex-col gap-1.5">
                                    <span className="h-0.5 w-6 bg-white/90" />
                                    <span className="h-0.5 w-6 bg-white/90" />
                                    <span className="h-0.5 w-6 bg-white/90" />
                                </div>
                            </summary>
                            <div className="absolute left-0 right-0 top-full border-b border-white/10 bg-black/85 backdrop-blur-md">
                                <div className="mx-auto max-w-7xl px-4 py-4">
                                    <nav className="flex flex-col gap-3 text-sm font-medium text-white/85">
                                        <details className="group/agents">
                                            <summary className="list-none cursor-pointer rounded-lg px-2 py-2 hover:bg-white/5 flex items-center justify-between">
                                                Agents
                                                <span className="text-white/60 group-open/agents:rotate-180 transition-transform">
                                                    <svg className="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                                                        <path strokeLinecap="round" strokeLinejoin="round" d="M19 9l-7 7-7-7" />
                                                    </svg>
                                                </span>
                                            </summary>
                                            <div className="mt-1 ml-2 flex flex-col gap-1">
                                                <Link href="https://actual.ai/agents/architecture-agent" className="rounded-lg px-2 py-2 hover:bg-white/5">
                                                    Architecture Agent
                                                </Link>
                                                <Link href="https://actual.ai/agents/management-agent" className="rounded-lg px-2 py-2 hover:bg-white/5">
                                                    Management Agent
                                                </Link>
                                            </div>
                                        </details>
                                        <Link href="/docs" className="rounded-lg px-2 py-2 hover:bg-white/5">Docs</Link>
                                        <Link href="https://actual.ai/pricing" className="rounded-lg px-2 py-2 hover:bg-white/5">Pricing</Link>
                                        <Link href="https://actual.ai/about" className="rounded-lg px-2 py-2 hover:bg-white/5">About</Link>
                                        <Link href="https://actual.ai/blog" className="rounded-lg px-2 py-2 hover:bg-white/5">Blog</Link>
                                        <div className="pt-2">
                                            <JoinNowButton className="w-full" />
                                        </div>
                                    </nav>
                                </div>
                            </div>
                        </details>
                    </div>
                </div>
            </div>
        </header>
    );
}
