import { describe, it, expect, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import { StatusBar } from "./StatusBar";
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
    loading: false,
    pagesVersion: 0,
    contentEpoch: 0,
    sidebarScrollPage: 1,
    ocrEpoch: 0,
    ...overrides,
  };
}

describe("StatusBar", () => {
  beforeEach(() => {
    usePdfStore.setState({ tabs: [], activeTabId: null });
  });

  it("shows 'Verified Signed Document' for a verified active tab", () => {
    usePdfStore.setState({
      tabs: [makeTab({ signatureStatus: "verified" })],
      activeTabId: "tab-1",
    });
    render(<StatusBar />);
    expect(screen.getByText("Verified Signed Document")).toBeTruthy();
  });

  it("shows a warning label when modified after signing", () => {
    usePdfStore.setState({
      tabs: [makeTab({ signatureStatus: "modifiedAfter" })],
      activeTabId: "tab-1",
    });
    render(<StatusBar />);
    expect(screen.getByText("Signed — modified after signing")).toBeTruthy();
  });

  it("shows a warning label when the signature could not be verified", () => {
    usePdfStore.setState({
      tabs: [makeTab({ signatureStatus: "invalid" })],
      activeTabId: "tab-1",
    });
    render(<StatusBar />);
    expect(screen.getByText("Signed — signature could not be verified")).toBeTruthy();
  });

  it("shows no badge for an unsigned (or unverified) tab", () => {
    usePdfStore.setState({
      tabs: [makeTab({ signatureStatus: "unsigned" })],
      activeTabId: "tab-1",
    });
    const { container } = render(<StatusBar />);
    expect(container.querySelector(".signature-badge")).toBeNull();
  });

  it("reflects the active tab only", () => {
    usePdfStore.setState({
      tabs: [
        makeTab({ id: "tab-1", signatureStatus: "verified" }),
        makeTab({ id: "tab-2", docId: "doc-2", signatureStatus: "invalid" }),
      ],
      activeTabId: "tab-2",
    });
    const { rerender } = render(<StatusBar />);
    // Active is tab-2 (invalid) → its badge, not tab-1's.
    expect(screen.getByText("Signed — signature could not be verified")).toBeTruthy();
    expect(screen.queryByText("Verified Signed Document")).toBeNull();

    usePdfStore.setState({ activeTabId: "tab-1" });
    rerender(<StatusBar />);
    expect(screen.getByText("Verified Signed Document")).toBeTruthy();
  });
});
