import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { SearchPanel } from "./SearchPanel";
import { usePdfStore } from "../store/usePdfStore";
import type { TabState, SearchResult } from "../store/usePdfStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

function makeTab(overrides: Partial<TabState> = {}): TabState {
  return {
    id: "tab-1",
    docId: "doc-1",
    fileName: "test.pdf",
    filePath: "C:\\Users\\test\\test.pdf",
    pageCount: 3,
    pageDimensions: [{ width: 200, height: 200 }],
    currentPage: 2,
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
    ...overrides,
  };
}

function setTab(overrides: Partial<TabState> = {}) {
  usePdfStore.setState({
    tabs: [makeTab(overrides)],
    activeTabId: "tab-1",
    activeSidebarTool: "search",
    sidebarWidth: 250,
  });
}

describe("SearchPanel", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it("renders the result list for a query with matches", () => {
    const results: SearchResult[] = [
      { page: 1, rects: [{ x: 0, y: 0, width: 10, height: 10 }] },
      { page: 3, rects: [{ x: 0, y: 0, width: 10, height: 10 }] },
    ];
    setTab({ searchQuery: "test", searchResults: results, searchResultIndex: 0 });

    render(<SearchPanel />);

    expect(screen.getByText(/1 of 2 matches on 2 pages/)).toBeInTheDocument();
    expect(screen.getByText("Page 1")).toBeInTheDocument();
    expect(screen.getByText("Page 3")).toBeInTheDocument();
    // No OCR prompt while there are matches.
    expect(screen.queryByText(/Run OCR/)).not.toBeInTheDocument();
  });

  it("offers OCR when a query finds no matches and re-searches after running it", async () => {
    setTab({ searchQuery: "banana", searchResults: [], searchResultIndex: -1 });

    // ocr_page succeeds; the follow-up search then finds a (fallback) hit.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === "ocr_page") return Promise.resolve([]);
      if (cmd === "search_document")
        return Promise.resolve([
          { page: 2, rects: [{ x: 0, y: 0, width: 10, height: 10 }] },
        ] as SearchResult[]);
      return Promise.resolve(undefined);
    });

    render(<SearchPanel />);

    expect(screen.getByText("No matches found")).toBeInTheDocument();
    const button = screen.getByRole("button", { name: /Run OCR on this page/ });

    await act(async () => {
      fireEvent.click(button);
      await new Promise((r) => setTimeout(r, 0));
    });

    // OCR ran against the current page (2), then a search re-ran.
    expect(invoke).toHaveBeenCalledWith("ocr_page", { docId: "doc-1", page: 2 });
    expect(invoke).toHaveBeenCalledWith("search_document", {
      docId: "doc-1",
      query: "banana",
    });
    // The re-search surfaced a match via the OCR fallback.
    expect(usePdfStore.getState().tabs[0].searchResults).toHaveLength(1);
  });

  it("shows an error message when OCR fails", async () => {
    setTab({ searchQuery: "banana", searchResults: [], searchResultIndex: -1 });

    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === "ocr_page")
        return Promise.reject("OCR is not available — install an OCR language pack");
      return Promise.resolve(undefined);
    });

    render(<SearchPanel />);

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /Run OCR on this page/ }));
      await new Promise((r) => setTimeout(r, 0));
    });

    expect(screen.getByText(/OCR failed:/)).toBeInTheDocument();
    expect(screen.getByText(/install an OCR language pack/)).toBeInTheDocument();
  });
});
