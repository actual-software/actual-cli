import Hero from "./components/home/Hero";
import DemoVideo from "./components/home/DemoVideo";
import ProductSection from "./components/home/ProductSection";
import InstallSection from "./components/home/InstallSection";

export default function Home() {
    return (
        <main className="bg-[#030301] flex flex-col items-center w-full overflow-x-hidden">
            <Hero />
            <DemoVideo />
            <ProductSection />
            <InstallSection />
        </main>
    );
}
