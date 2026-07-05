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

describe("IconRail (issue #12 view-only gating)", () => {
  beforeEach(() => {
    usePdfStore.setState({ tabs: [], activeTabId: null, activeSidebarTool: null });
  });

  it("enables every tool for an unencrypted document", () => {
    usePdfStore.setState({ tabs: [makeTab()], activeTabId: "tab-1" });
    render(<IconRail />);
    for (const title of ["Thumbnails", "Search", "Metadata", "Page Operations", "Compress"]) {
      expect(screen.getByTitle(title)).toBeEnabled();
    }
  });

  it("disables the lopdf-backed tools for an encrypted document, keeping view tools", () => {
    usePdfStore.setState({ tabs: [makeTab({ encrypted: true })], activeTabId: "tab-1" });
    render(<IconRail />);

    // View tools stay live.
    expect(screen.getByTitle("Thumbnails")).toBeEnabled();
    expect(screen.getByTitle("Search")).toBeEnabled();

    // Edit/metadata tools are disabled, with an explanatory tooltip.
    for (const label of ["Metadata", "Page operations", "Compression"]) {
      const btn = screen.getByTitle(new RegExp(`${label} isn't available`));
      expect(btn).toBeDisabled();
    }
  });
});
