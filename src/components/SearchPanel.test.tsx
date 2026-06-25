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
    ocrEpoch: 0,
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
      matchCase: false,
      wholeWord: false,
      useRegex: false,
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

  // ── Search mode toggle tests (issue #6) ────────────────────────────────────
  // These tests describe the three toggle buttons that will be added to the
  // SearchPanel: "Match case", "Whole word", and "Regular expression".
  // They will fail until the feature is implemented.

  it("renders three search-mode toggle buttons", () => {
    setTab();
    render(<SearchPanel />);

    expect(screen.getByTitle("Match case")).toBeInTheDocument();
    expect(screen.getByTitle("Whole word")).toBeInTheDocument();
    expect(screen.getByTitle("Regular expression")).toBeInTheDocument();
  });

  it("Match case toggle starts unpressed and becomes pressed on click", () => {
    setTab();
    render(<SearchPanel />);

    const btn = screen.getByTitle("Match case");
    expect(btn).toHaveAttribute("aria-pressed", "false");

    fireEvent.click(btn);

    expect(btn).toHaveAttribute("aria-pressed", "true");
  });

  it("Whole word toggle starts unpressed and becomes pressed on click", () => {
    setTab();
    render(<SearchPanel />);

    const btn = screen.getByTitle("Whole word");
    expect(btn).toHaveAttribute("aria-pressed", "false");

    fireEvent.click(btn);

    expect(btn).toHaveAttribute("aria-pressed", "true");
  });

  it("Regular expression toggle starts unpressed and becomes pressed on click", () => {
    setTab();
    render(<SearchPanel />);

    const btn = screen.getByTitle("Regular expression");
    expect(btn).toHaveAttribute("aria-pressed", "false");

    fireEvent.click(btn);

    expect(btn).toHaveAttribute("aria-pressed", "true");
  });

  it("enabling Match case passes matchCase:true to search_document", async () => {
    setTab({ searchQuery: "Test" });
    vi.mocked(invoke).mockResolvedValue([]);

    render(<SearchPanel />);

    // Enable match-case then trigger a new search via input change.
    fireEvent.click(screen.getByTitle("Match case"));

    const input = screen.getByPlaceholderText("Search...");
    await act(async () => {
      fireEvent.change(input, { target: { value: "Test" } });
      // Advance past the 300 ms debounce.
      await new Promise((r) => setTimeout(r, 350));
    });

    expect(invoke).toHaveBeenCalledWith("search_document", {
      docId: "doc-1",
      query: "Test",
      matchCase: true,
      wholeWord: false,
      useRegex: false,
    });
  });

  it("enabling Whole word passes wholeWord:true to search_document", async () => {
    setTab({ searchQuery: "Test" });
    vi.mocked(invoke).mockResolvedValue([]);

    render(<SearchPanel />);

    fireEvent.click(screen.getByTitle("Whole word"));

    const input = screen.getByPlaceholderText("Search...");
    await act(async () => {
      fireEvent.change(input, { target: { value: "Test" } });
      await new Promise((r) => setTimeout(r, 350));
    });

    expect(invoke).toHaveBeenCalledWith("search_document", {
      docId: "doc-1",
      query: "Test",
      matchCase: false,
      wholeWord: true,
      useRegex: false,
    });
  });

  it("enabling Regular expression passes useRegex:true to search_document", async () => {
    setTab({ searchQuery: "Test" });
    vi.mocked(invoke).mockResolvedValue([]);

    render(<SearchPanel />);

    fireEvent.click(screen.getByTitle("Regular expression"));

    const input = screen.getByPlaceholderText("Search...");
    await act(async () => {
      fireEvent.change(input, { target: { value: "Test" } });
      await new Promise((r) => setTimeout(r, 350));
    });

    expect(invoke).toHaveBeenCalledWith("search_document", {
      docId: "doc-1",
      query: "Test",
      matchCase: false,
      wholeWord: false,
      useRegex: true,
    });
  });

  it("toggling a flag after a tab switch does not fire a cross-tab search", async () => {
    // Start on tab A with an active query.
    usePdfStore.setState({
      tabs: [
        makeTab({ id: "tab-a", docId: "doc-a", searchQuery: "foo" }),
        makeTab({ id: "tab-b", docId: "doc-b", searchQuery: "" }),
      ],
      activeTabId: "tab-a",
      activeSidebarTool: "search",
      sidebarWidth: 250,
    });
    vi.mocked(invoke).mockResolvedValue([]);

    const { rerender } = render(<SearchPanel />);

    // Switch to tab B — this should reset isMountedRef so the next toggle
    // skips the search (avoids a cross-tab invoke with doc-a's query).
    await act(async () => {
      usePdfStore.setState({ activeTabId: "tab-b" });
      rerender(<SearchPanel />);
    });

    vi.mocked(invoke).mockClear();

    // Toggle a flag on tab B. Because isMountedRef was reset, the effect
    // skips this first run and does NOT call invoke.
    await act(async () => {
      fireEvent.click(screen.getByTitle("Match case"));
      await new Promise((r) => setTimeout(r, 50));
    });

    // invoke must not have been called with doc-a's stale query.
    const crossTabCall = vi.mocked(invoke).mock.calls.find(
      (call) =>
        call[0] === "search_document" &&
        (call[1] as Record<string, unknown>)["docId"] === "doc-a",
    );
    expect(crossTabCall).toBeUndefined();
  });
});
