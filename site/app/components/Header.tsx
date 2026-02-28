"use client";

import Image from "next/image";
import Link from "next/link";

function JoinNowButton({ className = "" }: { className?: string }) {
    return (
        <Link
            href="https://forms.gle/7RkKyAHfDHyKVmce7"
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
                            <Link href="/docs" className="leading-[26px] hover:text-white/80 transition-colors">
                                Docs
                            </Link>
                            <Link href="/about" className="leading-[26px] hover:text-white/80 transition-colors">
                                About
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
                                        <Link href="/docs" className="rounded-lg px-2 py-2 hover:bg-white/5">Docs</Link>
                                        <Link href="/about" className="rounded-lg px-2 py-2 hover:bg-white/5">About</Link>
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
