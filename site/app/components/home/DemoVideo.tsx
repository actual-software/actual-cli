export default function DemoVideo() {
    return (
        <section className="w-full bg-[#030301] px-6 pb-[80px]">
            <div className="mx-auto w-full max-w-[1200px]">
                {/* Outer glow ring */}
                <div
                    className="rounded-[12px] p-[1px]"
                    style={{
                        background:
                            "linear-gradient(135deg, rgba(57,235,161,0.35) 0%, rgba(67,189,183,0.2) 50%, rgba(77,147,200,0.35) 100%)",
                    }}
                >
                    {/* Dark bezel */}
                    <div className="bg-[#0a0a0a] rounded-[11px] overflow-hidden">
                        {/* Fake window chrome — matches the dark terminal aesthetic */}
                        <div className="flex items-center gap-[6px] px-[14px] py-[10px] bg-[#111111] border-b border-white/5">
                            <span className="size-[10px] rounded-full bg-[#ff5f57]" />
                            <span className="size-[10px] rounded-full bg-[#febc2e]" />
                            <span className="size-[10px] rounded-full bg-[#28c840]" />
                            <span className="ml-2 text-[12px] text-white/30 font-mono">
                                actual adr-bot
                            </span>
                        </div>

                        {/* Video */}
                        <video
                            src="/video/LoopClip.mp4"
                            autoPlay
                            loop
                            muted
                            playsInline
                            className="w-full block"
                        />
                    </div>
                </div>
            </div>
        </section>
    );
}
