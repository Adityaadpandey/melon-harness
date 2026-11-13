import type { Metadata } from "next";
import { Bricolage_Grotesque, Instrument_Serif, JetBrains_Mono } from "next/font/google";
import { Provider } from "@/components/provider";
import { JsonLd } from "@/lib/jsonld";
import { DESCRIPTION, NAME, SITE, TAGLINE } from "@/lib/site";
import { REPO } from "@/lib/docs";
import "./globals.css";

const sans = Bricolage_Grotesque({ subsets: ["latin"], variable: "--font-sans" });
const serif = Instrument_Serif({ subsets: ["latin"], weight: "400", style: ["italic", "normal"], variable: "--font-serif" });
const mono = JetBrains_Mono({ subsets: ["latin"], variable: "--font-mono" });

export const metadata: Metadata = {
  metadataBase: new URL(SITE),
  title: { default: `${NAME}, ${TAGLINE}`, template: `%s · ${NAME}` },
  description: DESCRIPTION,
  applicationName: NAME,
  keywords: ["coding agent", "terminal coding agent", "AI coding assistant", "CLI", "Claude Code alternative", "code review", "Rust"],
  alternates: { canonical: "/" },
  openGraph: { type: "website", siteName: NAME, url: SITE, title: `${NAME}, ${TAGLINE}`, description: DESCRIPTION },
  twitter: { card: "summary_large_image", title: `${NAME}, ${TAGLINE}`, description: DESCRIPTION },
  robots: { index: true, follow: true, googleBot: { "max-snippet": -1, "max-image-preview": "large" } },
};

// Tells search and answer engines what zerd is and who makes it.
const schema = {
  "@context": "https://schema.org",
  "@graph": [
    { "@type": "WebSite", "@id": `${SITE}/#site`, url: SITE, name: NAME, description: DESCRIPTION },
    {
      "@type": "SoftwareApplication", "@id": `${SITE}/#app`, name: NAME, url: SITE, description: DESCRIPTION,
      applicationCategory: "DeveloperApplication", operatingSystem: "macOS, Linux, Windows",
      offers: { "@type": "Offer", price: "0", priceCurrency: "USD" },
      codeRepository: `https://github.com/${REPO}`, downloadUrl: `${SITE}/docs/install`,
    },
  ],
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" className={`${sans.variable} ${serif.variable} ${mono.variable}`} suppressHydrationWarning>
      <body>
        <JsonLd data={schema} />
        <Provider>{children}</Provider>
      </body>
    </html>
  );
}
