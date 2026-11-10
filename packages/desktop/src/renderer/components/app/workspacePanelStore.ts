import { createStore } from "zustand/vanilla";
import type { SummaryPanelTab } from "../SummaryPanel";

type PanelState = {
  tabs: SummaryPanelTab[];
  activeTabId: string | null;
  isOpen: boolean;
};

type PanelAction =
  | { type: "clear" }
  | { type: "collapse" }
  | { type: "show" }
  | { type: "select"; tabId: string }
  | { type: "browse-file"; tabId: string }
  | { type: "open-tab"; tab: SummaryPanelTab }
  | { type: "focus-or-open-tab"; tab: SummaryPanelTab; fromNewTabId?: string }
  | { type: "replace-tab"; tabId: string; tab: SummaryPanelTab }
  | { type: "close-tab"; tabId: string }
  | { type: "remove-agent-sessions"; sessionIds: ReadonlySet<string> };

const initialPanelState: PanelState = {
  tabs: [],
  activeTabId: null,
  isOpen: true,
};

const removeTabs = (
  state: PanelState,
  shouldRemove: (tab: SummaryPanelTab) => boolean,
): PanelState => {
  const removedIndex = state.tabs.findIndex(
    (tab) => tab.id === state.activeTabId && shouldRemove(tab),
  );
  const tabs = state.tabs.filter((tab) => !shouldRemove(tab));
  const activeTabId =
    removedIndex >= 0
      ? (tabs[Math.min(removedIndex, Math.max(tabs.length - 1, 0))]?.id ?? null)
      : state.activeTabId && tabs.some((tab) => tab.id === state.activeTabId)
        ? state.activeTabId
        : (tabs[0]?.id ?? null);
  return { ...state, tabs, activeTabId };
};

// Three retained files plus one replaceable preview. Non-file views do not
// consume these slots. Reopening a retained file swaps it with the preview.
const placeFileTab = (current: SummaryPanelTab[], incoming: SummaryPanelTab): SummaryPanelTab[] => {
  const tabs = current.filter((tab) => tab.kind !== "files");
  const existingIndex = tabs.findIndex((tab) => tab.id === incoming.id);
  const previewIndex = tabs.findIndex((tab) => tab.kind === "file" && tab.isPreview);
  if (existingIndex >= 0) {
    tabs[existingIndex] = { ...incoming, isPreview: tabs[existingIndex].isPreview };
    if (previewIndex >= 0 && previewIndex !== existingIndex) {
      const previous = tabs[previewIndex];
      tabs[previewIndex] = { ...tabs[existingIndex], isPreview: true };
      tabs[existingIndex] = { ...previous, isPreview: false };
    }
    return tabs;
  }
  const retainedCount = tabs.filter((tab) => tab.kind === "file" && !tab.isPreview).length;
  const next = { ...incoming, isPreview: retainedCount >= 3 };
  if (previewIndex >= 0) {
    if (next.isPreview) tabs[previewIndex] = next;
    else tabs.splice(previewIndex, 0, next);
  } else {
    tabs.push(next);
  }
  return tabs;
};

const panelReducer = (state: PanelState, action: PanelAction): PanelState => {
  switch (action.type) {
    case "clear":
      return { ...state, tabs: [], activeTabId: null };
    case "collapse":
      return { ...state, isOpen: false };
    case "show":
      return { ...state, isOpen: true };
    case "select":
      return state.tabs.some((tab) => tab.id === action.tabId)
        ? { ...state, activeTabId: action.tabId }
        : state;
    case "browse-file": {
      const existing = state.tabs.find((tab) => tab.id === action.tabId);
      return existing
        ? {
            tabs: placeFileTab(state.tabs, existing),
            activeTabId: existing.id,
            isOpen: true,
          }
        : state;
    }
    case "open-tab": {
      const tabs = placeFileTab(state.tabs, action.tab);
      return {
        tabs,
        activeTabId: action.tab.id,
        isOpen: true,
      };
    }
    case "focus-or-open-tab": {
      const source = action.fromNewTabId;
      // A closed/departed launcher cannot publish a late asynchronous result.
      if (source && !state.tabs.some(tab => tab.id === source && tab.kind === "new")) return state;
      const existing = state.tabs.find((tab) => tab.id === action.tab.id);
      return {
        tabs: existing ? source ? state.tabs.filter(tab => tab.id !== source) : state.tabs
          : source ? state.tabs.map(tab => tab.id === source ? action.tab : tab)
          : [...state.tabs, action.tab],
        activeTabId: action.tab.id,
        isOpen: true,
      };
    }
    case "replace-tab":
      return {
        ...state,
        tabs: state.tabs.map((tab) =>
          tab.id === action.tabId ? { ...action.tab, isPreview: tab.isPreview } : tab,
        ),
      };
    case "close-tab":
      return removeTabs(state, (tab) => tab.id === action.tabId);
    case "remove-agent-sessions":
      return removeTabs(
        state,
        (tab) =>
          (tab.kind === "agent" || tab.kind === "tasks") &&
          Boolean(
            (tab.sessionId && action.sessionIds.has(tab.sessionId)) ||
              (tab.parentSessionId && action.sessionIds.has(tab.parentSessionId)),
          ),
      );
  }
};

type WorkspacePanelStoreState = PanelState & {
  savedScopes: ReadonlyMap<string, PanelState>;
  dispatch: (action: PanelAction) => void;
  saveScope: (key: string) => void;
  restoreScope: (key: string) => void;
};

// Each Desktop owner creates one store. Saved scopes retain views within that
// owner; neither another window nor a newly mounted App can inherit them.
export function createWorkspacePanelStore() {
  return createStore<WorkspacePanelStoreState>()((set, get) => ({
    ...initialPanelState,
    savedScopes: new Map(),
    dispatch: (action) => set(state => panelReducer(state, action)),
    saveScope: (key) => {
      const { tabs, activeTabId, isOpen, savedScopes } = get();
      set({ savedScopes: new Map(savedScopes).set(key, { tabs, activeTabId, isOpen }) });
    },
    restoreScope: (key) => {
      const saved = get().savedScopes.get(key);
      // Departed reads cannot complete in this view. Reopening retries them.
      set(saved ? {
        ...saved,
        tabs: saved.tabs.map(tab => tab.loading
          ? { ...tab, loading: false, error: "Preview interrupted. Reopen this file to retry." }
          : tab),
      } : initialPanelState);
    },
  }));
}
