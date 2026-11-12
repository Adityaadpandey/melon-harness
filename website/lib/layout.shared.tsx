import type { BaseLayoutProps } from "fumadocs-ui/layouts/shared";
import { REPO } from "@/lib/docs";

export function baseOptions(): BaseLayoutProps {
  return {
    nav: {
      title: (
        <span className="logo">
          zerd<i className="logo-tick" aria-hidden />
        </span>
      ),
    },
    links: [
      { text: "GitHub", url: `https://github.com/${REPO}`, external: true },
      { text: "Releases", url: `https://github.com/${REPO}/releases`, external: true },
    ],
  };
}
