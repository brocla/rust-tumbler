import { describe, it, expect, beforeEach } from "vitest";
import { usePdfStore } from "./usePdfStore";
import type { TabState } from "./usePdfStore";

function makeTab(overrides: Partial<TabState> = {}): TabState {
  return {
    id: crypto.randomUUID(),
    docId: `doc-${Math.random()}`,
    fileName: "test.pdf",
    filePath: "C:\\Users\\test\\test.pdf",
    pageCount: 10,
    pageDimensions: Array.from({ length: 10 }, () => ({
      width: 612,
      height: 792,
    })),
    currentPage: 1,
    scrollTop: 0,
    zoom: 100,
    zoomMode: "numeric",
    displayMode: "normal",
    searchQuery: "",
    searchResults: [],
    searchResultIndex: -1,
    metadataDirty: false,
    isDirty: false,
    loading: false,
    pagesVersion: 0,
    contentEpoch: 0,
    sidebarScrollPage: 1,
    ocrEpoch: 0,
    ...overrides,
  };
}

function resetStore() {
  usePdfStore.setState({
    tabs: [],
    activeTabId: null,
    activeSidebarTool: null,
    sidebarWidth: 250,
  });
}

describe("usePdfStore", () => {
  beforeEach(resetStore);

  describe("addTab", () => {
    it("adds a tab and makes it active", () => {
      const tab = makeTab({ id: "tab-1" });
      usePdfStore.getState().addTab(tab);

      const state = usePdfStore.getState();
      expect(state.tabs).toHaveLength(1);
      expect(state.tabs[0].id).toBe("tab-1");
      expect(state.activeTabId).toBe("tab-1");
    });

    it("switches active tab to the newly added one", () => {
      const tab1 = makeTab({ id: "tab-1" });
      const tab2 = makeTab({ id: "tab-2" });

      usePdfStore.getState().addTab(tab1);
      usePdfStore.getState().addTab(tab2);

      expect(usePdfStore.getState().activeTabId).toBe("tab-2");
      expect(usePdfStore.getState().tabs).toHaveLength(2);
    });
  });

  describe("removeTab", () => {
    it("removes the tab from the list", () => {
      const tab = makeTab({ id: "tab-1" });
      usePdfStore.getState().addTab(tab);
      usePdfStore.getState().removeTab("tab-1");

      expect(usePdfStore.getState().tabs).toHaveLength(0);
    });

    it("sets activeTabId to null when last tab is closed", () => {
      const tab = makeTab({ id: "tab-1" });
      usePdfStore.getState().addTab(tab);
      usePdfStore.getState().removeTab("tab-1");

      expect(usePdfStore.getState().activeTabId).toBeNull();
    });

    it("activates the next tab when the active tab is closed", () => {
      const tab1 = makeTab({ id: "tab-1" });
      const tab2 = makeTab({ id: "tab-2" });
      const tab3 = makeTab({ id: "tab-3" });

      usePdfStore.getState().addTab(tab1);
      usePdfStore.getState().addTab(tab2);
      usePdfStore.getState().addTab(tab3);

      // tab-3 is active. Switch to tab-2, then close it.
      usePdfStore.getState().setActiveTab("tab-2");
      usePdfStore.getState().removeTab("tab-2");

      // Should activate tab-3 (was at index 2, now at index 1 — the min logic picks it)
      expect(usePdfStore.getState().activeTabId).toBe("tab-3");
      expect(usePdfStore.getState().tabs).toHaveLength(2);
    });

    it("activates the previous tab when the last tab in list is closed", () => {
      const tab1 = makeTab({ id: "tab-1" });
      const tab2 = makeTab({ id: "tab-2" });

      usePdfStore.getState().addTab(tab1);
      usePdfStore.getState().addTab(tab2);

      // tab-2 is active (last added), close it
      usePdfStore.getState().removeTab("tab-2");

      expect(usePdfStore.getState().activeTabId).toBe("tab-1");
    });

    it("does not change activeTabId when a non-active tab is closed", () => {
      const tab1 = makeTab({ id: "tab-1" });
      const tab2 = makeTab({ id: "tab-2" });

      usePdfStore.getState().addTab(tab1);
      usePdfStore.getState().addTab(tab2);

      // tab-2 is active, close tab-1
      usePdfStore.getState().removeTab("tab-1");

      expect(usePdfStore.getState().activeTabId).toBe("tab-2");
      expect(usePdfStore.getState().tabs).toHaveLength(1);
    });
  });

  describe("updateTab", () => {
    it("updates specific fields of a tab", () => {
      const tab = makeTab({ id: "tab-1", currentPage: 1, zoom: 100 });
      usePdfStore.getState().addTab(tab);

      usePdfStore.getState().updateTab("tab-1", {
        currentPage: 5,
        zoom: 150,
      });

      const updated = usePdfStore.getState().tabs[0];
      expect(updated.currentPage).toBe(5);
      expect(updated.zoom).toBe(150);
      expect(updated.fileName).toBe("test.pdf"); // unchanged
    });

    it("does not affect other tabs", () => {
      const tab1 = makeTab({ id: "tab-1", currentPage: 1 });
      const tab2 = makeTab({ id: "tab-2", currentPage: 1 });

      usePdfStore.getState().addTab(tab1);
      usePdfStore.getState().addTab(tab2);

      usePdfStore.getState().updateTab("tab-1", { currentPage: 7 });

      expect(usePdfStore.getState().tabs[0].currentPage).toBe(7);
      expect(usePdfStore.getState().tabs[1].currentPage).toBe(1);
    });
  });

  describe("reorderTabs", () => {
    it("moves a tab from one index to another", () => {
      const tab1 = makeTab({ id: "tab-1" });
      const tab2 = makeTab({ id: "tab-2" });
      const tab3 = makeTab({ id: "tab-3" });

      usePdfStore.getState().addTab(tab1);
      usePdfStore.getState().addTab(tab2);
      usePdfStore.getState().addTab(tab3);

      // Move tab-1 (index 0) to the end (index 2)
      usePdfStore.getState().reorderTabs(0, 2);

      expect(usePdfStore.getState().tabs.map((t) => t.id)).toEqual([
        "tab-2",
        "tab-3",
        "tab-1",
      ]);
    });

    it("moves a tab backward", () => {
      const tab1 = makeTab({ id: "tab-1" });
      const tab2 = makeTab({ id: "tab-2" });
      const tab3 = makeTab({ id: "tab-3" });

      usePdfStore.getState().addTab(tab1);
      usePdfStore.getState().addTab(tab2);
      usePdfStore.getState().addTab(tab3);

      // Move tab-3 (index 2) to the front (index 0)
      usePdfStore.getState().reorderTabs(2, 0);

      expect(usePdfStore.getState().tabs.map((t) => t.id)).toEqual([
        "tab-3",
        "tab-1",
        "tab-2",
      ]);
    });

    it("does not change activeTabId", () => {
      const tab1 = makeTab({ id: "tab-1" });
      const tab2 = makeTab({ id: "tab-2" });

      usePdfStore.getState().addTab(tab1);
      usePdfStore.getState().addTab(tab2);
      usePdfStore.getState().setActiveTab("tab-1");

      usePdfStore.getState().reorderTabs(0, 1);

      expect(usePdfStore.getState().activeTabId).toBe("tab-1");
    });

    it("does nothing when indices are equal or out of range", () => {
      const tab1 = makeTab({ id: "tab-1" });
      const tab2 = makeTab({ id: "tab-2" });

      usePdfStore.getState().addTab(tab1);
      usePdfStore.getState().addTab(tab2);

      usePdfStore.getState().reorderTabs(0, 0);
      usePdfStore.getState().reorderTabs(0, 5);
      usePdfStore.getState().reorderTabs(-1, 1);

      expect(usePdfStore.getState().tabs.map((t) => t.id)).toEqual([
        "tab-1",
        "tab-2",
      ]);
    });
  });

  describe("setActiveTab", () => {
    it("switches the active tab", () => {
      const tab1 = makeTab({ id: "tab-1" });
      const tab2 = makeTab({ id: "tab-2" });

      usePdfStore.getState().addTab(tab1);
      usePdfStore.getState().addTab(tab2);

      usePdfStore.getState().setActiveTab("tab-1");
      expect(usePdfStore.getState().activeTabId).toBe("tab-1");
    });
  });

  describe("getActiveTab", () => {
    it("returns the active tab", () => {
      const tab = makeTab({ id: "tab-1", fileName: "report.pdf" });
      usePdfStore.getState().addTab(tab);

      const active = usePdfStore.getState().getActiveTab();
      expect(active?.fileName).toBe("report.pdf");
    });

    it("returns undefined when no tabs are open", () => {
      expect(usePdfStore.getState().getActiveTab()).toBeUndefined();
    });
  });

  describe("sidebar", () => {
    it("toggles sidebar tool on when clicking a new tool", () => {
      usePdfStore.getState().setSidebarTool("search");
      expect(usePdfStore.getState().activeSidebarTool).toBe("search");
    });

    it("toggles sidebar off when clicking the active tool", () => {
      usePdfStore.getState().setSidebarTool("search");
      usePdfStore.getState().setSidebarTool("search");
      expect(usePdfStore.getState().activeSidebarTool).toBeNull();
    });

    it("switches to a different tool", () => {
      usePdfStore.getState().setSidebarTool("search");
      usePdfStore.getState().setSidebarTool("thumbnails");
      expect(usePdfStore.getState().activeSidebarTool).toBe("thumbnails");
    });

    it("persists sidebar width", () => {
      usePdfStore.getState().setSidebarWidth(400);
      expect(usePdfStore.getState().sidebarWidth).toBe(400);
    });
  });

  describe("search navigation", () => {
    it("nextSearchResult advances through results and wraps around", () => {
      const tab = makeTab({
        id: "tab-1",
        searchResults: [
          { page: 1, rects: [{ x: 0, y: 0, width: 10, height: 10 }] },
          {
            page: 3,
            rects: [
              { x: 0, y: 0, width: 10, height: 10 },
              { x: 0, y: 20, width: 10, height: 10 },
            ],
          },
        ],
        searchResultIndex: -1,
      });
      usePdfStore.getState().addTab(tab);

      // First next → index 0 (page 1)
      usePdfStore.getState().nextSearchResult();
      let state = usePdfStore.getState().tabs[0];
      expect(state.searchResultIndex).toBe(0);
      expect(state.currentPage).toBe(1);

      // Second next → index 1 (page 3, first rect)
      usePdfStore.getState().nextSearchResult();
      state = usePdfStore.getState().tabs[0];
      expect(state.searchResultIndex).toBe(1);
      expect(state.currentPage).toBe(3);

      // Third next → index 2 (page 3, second rect)
      usePdfStore.getState().nextSearchResult();
      state = usePdfStore.getState().tabs[0];
      expect(state.searchResultIndex).toBe(2);
      expect(state.currentPage).toBe(3);

      // Fourth next → wraps to index 0 (page 1)
      usePdfStore.getState().nextSearchResult();
      state = usePdfStore.getState().tabs[0];
      expect(state.searchResultIndex).toBe(0);
      expect(state.currentPage).toBe(1);
    });

    it("prevSearchResult goes backward and wraps around", () => {
      const tab = makeTab({
        id: "tab-1",
        searchResults: [
          { page: 1, rects: [{ x: 0, y: 0, width: 10, height: 10 }] },
          { page: 2, rects: [{ x: 0, y: 0, width: 10, height: 10 }] },
        ],
        searchResultIndex: 0,
      });
      usePdfStore.getState().addTab(tab);

      // Prev from 0 → wraps to last (index 1, page 2)
      usePdfStore.getState().prevSearchResult();
      const state = usePdfStore.getState().tabs[0];
      expect(state.searchResultIndex).toBe(1);
      expect(state.currentPage).toBe(2);
    });

    it("does nothing when there are no search results", () => {
      const tab = makeTab({ id: "tab-1", searchResults: [] });
      usePdfStore.getState().addTab(tab);

      usePdfStore.getState().nextSearchResult();
      expect(usePdfStore.getState().tabs[0].searchResultIndex).toBe(-1);
    });
  });
});
