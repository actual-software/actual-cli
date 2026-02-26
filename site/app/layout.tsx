import type { Metadata } from "next";
import { Inter, Geist_Mono } from "next/font/google";
import "./globals.css";
import Header from "./components/Header";
import Footer from "./components/Footer";

export const metadata: Metadata = {
    title: "actual — AI context for your codebase",
    description:
        "actual analyzes your repo, fetches matching Architectural Decisions, and writes them into AI context files — keeping every coding agent aligned, automatically.",
    openGraph: {
        title: "actual — AI context for your codebase",
        description:
            "actual analyzes your repo, fetches matching Architectural Decisions, and writes them into AI context files — keeping every coding agent aligned, automatically.",
        url: "https://cli.actual.ai",
        siteName: "Actual AI",
        locale: "en_US",
        type: "website",
    },
};

const inter = Inter({
    variable: "--font-inter",
    subsets: ["latin"],
    weight: ["100", "300", "400", "500", "600", "700"],
});

const geistMono = Geist_Mono({
    variable: "--font-geist-mono",
    subsets: ["latin"],
});

export default function RootLayout({
    children,
}: Readonly<{
    children: React.ReactNode;
}>) {
    return (
        <html lang="en">
            <body className={`${inter.variable} ${geistMono.variable} antialiased`}>
                <Header />
                <div className="min-h-screen flex flex-col relative text-white">
                    {children}
                    <Footer />
                </div>
            </body>
        </html>
    );
}
