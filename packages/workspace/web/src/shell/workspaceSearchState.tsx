import { createContext, useContext, useRef, type ReactNode } from "react";

type SearchSnapshot = {
  selectedIndex: number;
  scrollTop: number;
  libraryResponse: unknown;
  sessionResponse: unknown;
};
const SearchHistory = createContext<Map<string, SearchSnapshot> | null>(null);

// The workspace layout keys this provider by authenticated user and workspace.
// Result pages stay in memory; URLs retain the query for reloads and sharing.
export function WorkspaceSearchHistory({ children }: { children: ReactNode }) {
  const entries = useRef(new Map<string, SearchSnapshot>());
  return <SearchHistory value={entries.current}>{children}</SearchHistory>;
}

export function useSearchHistory() { return useContext(SearchHistory); }
