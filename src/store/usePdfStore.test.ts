import { describe, it, expect, beforeEach } from "vitest";
import { usePdfStore } from "./usePdfStore";
import type { TabState } from "./usePdfStore";

function makeTab(overrides: Partial<TabState> = {}): TabState {
  return {
    id: crypto.randomUUID(),
    docId: `doc-${Math.random()}`,
    fileName: "test.pdf",
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
    loading: false,
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
});
