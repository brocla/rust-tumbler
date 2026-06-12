import { create } from "zustand";

export interface PageDimension {
  width: number;
  height: number;
}

export type ZoomMode = "numeric" | "fit-width" | "fit-page";
export type DisplayMode = "normal" | "invert" | "sepia";

export interface TabState {
  id: string;
  docId: string;
  fileName: string;
  pageCount: number;
  pageDimensions: PageDimension[];
  currentPage: number;
  scrollTop: number;
  zoom: number;
  zoomMode: ZoomMode;
  displayMode: DisplayMode;
  searchQuery: string;
  searchResults: SearchResult[];
  searchResultIndex: number;
  metadataDirty: boolean;
  loading: boolean;
}

export interface SearchResult {
  page: number;
  rects: { x: number; y: number; width: number; height: number }[];
}

interface PdfStore {
  // Tab state
  tabs: TabState[];
  activeTabId: string | null;

  // Global state
  activeSidebarTool: "thumbnails" | "search" | "metadata" | null;
  sidebarWidth: number;

  // Actions
  setActiveTab: (id: string) => void;
  setSidebarTool: (tool: PdfStore["activeSidebarTool"]) => void;
  setSidebarWidth: (width: number) => void;

  addTab: (tab: TabState) => void;
  removeTab: (id: string) => void;
  updateTab: (id: string, updates: Partial<TabState>) => void;

  getActiveTab: () => TabState | undefined;
}

export const usePdfStore = create<PdfStore>((set, get) => ({
  tabs: [],
  activeTabId: null,
  activeSidebarTool: null,
  sidebarWidth: 250,

  setActiveTab: (id) => set({ activeTabId: id }),

  setSidebarTool: (tool) =>
    set((state) => ({
      activeSidebarTool: state.activeSidebarTool === tool ? null : tool,
    })),

  setSidebarWidth: (width) => set({ sidebarWidth: width }),

  addTab: (tab) =>
    set((state) => ({
      tabs: [...state.tabs, tab],
      activeTabId: tab.id,
    })),

  removeTab: (id) =>
    set((state) => {
      const idx = state.tabs.findIndex((t) => t.id === id);
      const newTabs = state.tabs.filter((t) => t.id !== id);
      let newActiveId: string | null = null;
      if (newTabs.length > 0) {
        if (state.activeTabId === id) {
          const newIdx = Math.min(idx, newTabs.length - 1);
          newActiveId = newTabs[newIdx].id;
        } else {
          newActiveId = state.activeTabId;
        }
      }
      return { tabs: newTabs, activeTabId: newActiveId };
    }),

  updateTab: (id, updates) =>
    set((state) => ({
      tabs: state.tabs.map((t) => (t.id === id ? { ...t, ...updates } : t)),
    })),

  getActiveTab: () => {
    const state = get();
    return state.tabs.find((t) => t.id === state.activeTabId);
  },
}));
