"use client";
import type { ReactNode } from "react";
import { RootProvider } from "fumadocs-ui/provider/next";
import {
  SearchDialog, SearchDialogClose, SearchDialogContent, SearchDialogHeader, SearchDialogIcon,
  SearchDialogInput, SearchDialogList, SearchDialogOverlay, type SharedProps,
} from "fumadocs-ui/components/dialog/search";
import { useStaticSearch } from "fumadocs-core/search/client";

// Search runs in the browser against an index exported at build time, no server or external service.
function StaticSearchDialog(props: SharedProps) {
  const state = useStaticSearch({ from: "/api/search" });
  return (
    <SearchDialog {...state} {...props}>
      <SearchDialogOverlay />
      <SearchDialogContent>
        <SearchDialogHeader>
          <SearchDialogIcon />
          <SearchDialogInput />
          <SearchDialogClose />
        </SearchDialogHeader>
        <SearchDialogList />
      </SearchDialogContent>
    </SearchDialog>
  );
}

export function Provider({ children }: { children: ReactNode }) {
  return <RootProvider theme={{ forcedTheme: "dark", defaultTheme: "dark" }} search={{ SearchDialog: StaticSearchDialog }}>{children}</RootProvider>;
}
