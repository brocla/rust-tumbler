import { describe, it, expect, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import { IconRail } from "./IconRail";
import { usePdfStore } from "../store/usePdfStore";
import type { TabState } from "../store/usePdfStore";

function makeTab(overrides: Partial<TabState> = {}): TabState {
  return {
    id: "tab-1",
    docId: "doc-1",
    fileName: "test.pdf",
    filePath: "C:\\test.pdf",
    pageCount: 1,
    pageDimensions: [{ width: 200, height: 200 }],
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

describe("IconRail", () => {
  beforeEach(() => {
    usePdfStore.setState({ tabs: [], activeTabId: null, activeSidebarTool: null });
  });

  it("enables every tool for an unencrypted document", () => {
    usePdfStore.setState({ tabs: [makeTab()], activeTabId: "tab-1" });
    render(<IconRail />);
    for (const title of ["Thumbnails", "Search", "Metadata", "Page Operations", "Web Optimization"]) {
      expect(screen.getByTitle(title)).toBeEnabled();
    }
  });

  // Encrypted documents are decrypted into the buffer at open (issue #57),
  // so every tool — including the lopdf-backed ones — stays enabled.
  it("enables every tool for an encrypted document too", () => {
    usePdfStore.setState({ tabs: [makeTab({ encrypted: true })], activeTabId: "tab-1" });
    render(<IconRail />);
    for (const title of ["Thumbnails", "Search", "Metadata", "Page Operations", "Web Optimization"]) {
      expect(screen.getByTitle(title)).toBeEnabled();
    }
  });
});
