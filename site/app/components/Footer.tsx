import Image from "next/image";
import Link from "next/link";
import { FaLinkedin, FaDiscord, FaSlack } from "react-icons/fa";
import { FaXTwitter } from "react-icons/fa6";

const SOCIAL_SIZE = 24;

export default function Footer() {
    return (
        <footer className="border-t border-[#616161] pt-[100px] pb-[25px] w-full">
            <div className="mx-auto w-full max-w-[1496px] px-6">
                <div className="flex flex-col gap-[50px]">
                    <div className="grid grid-cols-2 gap-y-[40px] gap-x-[40px] md:grid-cols-4">
                        <div className="flex flex-col gap-[16px]">
                            <p className="text-[20px] leading-[31px] tracking-[-0.002px] text-white">
                                Company
                            </p>
                            <div className="flex flex-col gap-[8px] text-[#b6b6b9] text-[16px] tracking-[-0.0016px]">
                                <Link href="https://actual.ai/about" className="hover:text-white transition-colors">
                                    About
                                </Link>
                                <Link href="https://actual.ai/careers" className="hover:text-white transition-colors">
                                    Careers
                                </Link>
                            </div>
                        </div>

                        <div className="flex flex-col gap-[16px]">
                            <p className="text-[20px] leading-[31px] tracking-[-0.002px] text-white">
                                Product
                            </p>
                            <div className="flex flex-col gap-[8px] text-[#b6b6b9] text-[16px] tracking-[-0.0016px]">
                                <Link href="https://actual.ai/pricing" className="hover:text-white transition-colors">
                                    Pricing
                                </Link>
                                <Link href="https://actual.ai/security" className="hover:text-white transition-colors">
                                    Security
                                </Link>
                                <a
                                    href="https://cal.com/john-actual/demo"
                                    className="hover:text-white transition-colors"
                                >
                                    Book a Demo
                                </a>
                            </div>
                        </div>

                        <div className="flex flex-col gap-[16px]">
                            <p className="text-[20px] leading-[31px] tracking-[-0.002px] text-white">
                                Legal
                            </p>
                            <div className="flex flex-col gap-[8px] text-[#b6b6b9] text-[16px] tracking-[-0.0016px]">
                                <Link href="https://actual.ai/privacy" className="hover:text-white transition-colors">
                                    Privacy Policy
                                </Link>
                                <Link href="https://actual.ai/terms" className="hover:text-white transition-colors">
                                    Terms of Service
                                </Link>
                                <Link href="https://actual.ai/eula" className="hover:text-white transition-colors">
                                    EULA
                                </Link>
                            </div>
                        </div>

                        <div className="flex flex-col gap-[16px]">
                            <p className="text-[20px] leading-[31px] tracking-[-0.002px] text-white">
                                Contact
                            </p>
                            <div className="flex flex-col gap-[8px]">
                                <Link
                                    href="https://actual.ai/support"
                                    className="text-[#b6b6b9] text-[16px] leading-[31px] tracking-[-0.0016px] hover:text-white transition-colors"
                                >
                                    Contact Us
                                </Link>
                                <div className="flex gap-[8px] pt-[4px] items-center">
                                    <a
                                        href="https://www.linkedin.com/company/actualai/"
                                        target="_blank"
                                        rel="noopener noreferrer"
                                        className="size-[24px] flex items-center justify-center text-[#b6b6b9] hover:text-white transition-colors flex-shrink-0"
                                        aria-label="LinkedIn"
                                    >
                                        <FaLinkedin size={SOCIAL_SIZE} />
                                    </a>
                                    <a
                                        href="https://twitter.com/actual_ai_"
                                        target="_blank"
                                        rel="noopener noreferrer"
                                        className="size-[24px] flex items-center justify-center text-[#b6b6b9] hover:text-white transition-colors flex-shrink-0"
                                        aria-label="X"
                                    >
                                        <FaXTwitter size={SOCIAL_SIZE} />
                                    </a>
                                    <a
                                        href="https://discord.gg/b5vEUMq6CZ"
                                        target="_blank"
                                        rel="noopener noreferrer"
                                        className="size-[24px] flex items-center justify-center text-[#b6b6b9] hover:text-white transition-colors flex-shrink-0"
                                        aria-label="Discord"
                                    >
                                        <FaDiscord size={SOCIAL_SIZE} />
                                    </a>
                                    <a
                                        href="https://join.slack.com/t/actualaiusercommunity/shared_invite/zt-3o7kogn46-ADqsWZCJwQLKylNyOJMgUA"
                                        target="_blank"
                                        rel="noopener noreferrer"
                                        className="size-[24px] flex items-center justify-center text-[#b6b6b9] hover:text-white transition-colors flex-shrink-0"
                                        aria-label="Slack"
                                    >
                                        <FaSlack size={SOCIAL_SIZE} />
                                    </a>
                                </div>
                            </div>
                        </div>
                    </div>

                    <div className="border-t border-[#616161] pt-[32px] flex items-center gap-[12px]">
                        <Image
                            src="/figma/v2/nav-logo.png"
                            alt="Actual AI Logo"
                            width={40}
                            height={40}
                        />
                        <p className="text-[#b6b6b9] text-[16px] leading-[31px] tracking-[-0.0016px]">
                            © 2026 Actual Software, Inc.
                        </p>
                    </div>
                </div>
            </div>
        </footer>
    );
}
