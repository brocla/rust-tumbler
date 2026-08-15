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
    isDirty: false,
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
      { page: 1, matches: [{ rects: [{ x: 0, y: 0, width: 10, height: 10 }] }] },
      { page: 3, matches: [{ rects: [{ x: 0, y: 0, width: 10, height: 10 }] }] },
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
          {
            page: 2,
            matches: [{ rects: [{ x: 0, y: 0, width: 10, height: 10 }] }],
          },
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

  describe("Enter steps through matches", () => {
    const withMatches = () =>
      setTab({
        searchQuery: "x",
        searchResultIndex: 0,
        searchResults: [
          {
            page: 1,
            matches: [
              { rects: [{ x: 0, y: 0, width: 9, height: 9 }] },
              { rects: [{ x: 0, y: 20, width: 9, height: 9 }] },
            ],
          },
          { page: 3, matches: [{ rects: [{ x: 0, y: 0, width: 9, height: 9 }] }] },
        ],
      });

    const index = () => usePdfStore.getState().tabs[0].searchResultIndex;

    it("advances from the query box", () => {
      withMatches();
      render(<SearchPanel />);

      fireEvent.keyDown(screen.getByPlaceholderText("Search..."), { key: "Enter" });
      expect(index()).toBe(1);

      fireEvent.keyDown(screen.getByPlaceholderText("Search..."), {
        key: "Enter",
        shiftKey: true,
      });
      expect(index()).toBe(0);
    });

    /**
     * The reported bug: after clicking anywhere in the document, focus is no
     * longer in the query box and Enter had nothing bound to it.
     */
    it("advances when focus is out in the document", () => {
      withMatches();
      render(<SearchPanel />);

      fireEvent.keyDown(document.body, { key: "Enter" });
      expect(index()).toBe(1);

      fireEvent.keyDown(document.body, { key: "Enter", shiftKey: true });
      expect(index()).toBe(0);
    });

    it("advances exactly once when pressed in the query box", () => {
      withMatches();
      render(<SearchPanel />);

      // The input has its own handler and the event also reaches the window
      // listener; only one of them may act.
      fireEvent.keyDown(screen.getByPlaceholderText("Search..."), { key: "Enter" });
      expect(index()).toBe(1);
    });

    it("leaves Enter alone for other text fields and focused buttons", () => {
      withMatches();
      render(<SearchPanel />);

      // A form field elsewhere in the app.
      const field = document.createElement("textarea");
      document.body.appendChild(field);
      fireEvent.keyDown(field, { key: "Enter" });
      expect(index()).toBe(0);
      field.remove();

      // A focused control keeps native keyboard activation — that is how a
      // keyboard user works the mode toggles at all.
      fireEvent.keyDown(screen.getByTitle("Regular expression"), { key: "Enter" });
      expect(index()).toBe(0);
    });

    it("does nothing when the search found no matches", () => {
      setTab({ searchQuery: "zzz", searchResults: [], searchResultIndex: -1 });
      render(<SearchPanel />);

      fireEvent.keyDown(document.body, { key: "Enter" });
      expect(usePdfStore.getState().tabs[0].searchResultIndex).toBe(-1);
    });

    it("stops listening once the panel is closed", () => {
      withMatches();
      const { unmount } = render(<SearchPanel />);
      unmount();

      fireEvent.keyDown(document.body, { key: "Enter" });
      expect(index()).toBe(0);
    });

    /**
     * Clicking a mode button focuses it, and Enter on a focused button
     * activates that button — so Enter would re-toggle the mode instead of
     * stepping to the next match. Focus goes back to the query box.
     */
    it("returns focus to the query box after a mode toggle", () => {
      withMatches();
      render(<SearchPanel />);

      // A real click focuses the button; jsdom's fireEvent.click does not, so
      // focus it explicitly or this asserts nothing.
      const button = screen.getByTitle("Regular expression");
      button.focus();
      expect(document.activeElement).toBe(button);

      fireEvent.click(button);

      expect(document.activeElement).toBe(screen.getByPlaceholderText("Search..."));
    });
  });

  /**
   * Typing arms a 300ms debounce that captures the flags as they were at that
   * keystroke. Reaching for a mode button straight after typing — the natural
   * way to use these toggles — used to let that stale timer fire *after* the
   * toggle's own search, overwriting correct results with a search in the old
   * mode. The visible symptom was a button that appeared to do nothing.
   */
  it("toggling a mode cancels a debounce armed by an earlier keystroke", async () => {
    vi.useFakeTimers();
    try {
      setTab();
      vi.mocked(invoke).mockResolvedValue([]);
      render(<SearchPanel />);

      const input = screen.getByPlaceholderText("Search...");
      fireEvent.change(input, { target: { value: "^Print" } });

      // Toggle before the debounce elapses.
      await act(async () => {
        fireEvent.click(screen.getByTitle("Regular expression"));
      });

      const afterToggle = vi.mocked(invoke).mock.calls.length;
      expect(afterToggle).toBe(1);
      expect(vi.mocked(invoke).mock.calls[0][1]).toMatchObject({
        query: "^Print",
        useRegex: true,
      });

      // Let the old timer's deadline pass: it must not fire.
      await act(async () => {
        vi.advanceTimersByTime(600);
      });

      expect(vi.mocked(invoke).mock.calls.length).toBe(afterToggle);
    } finally {
      vi.useRealTimers();
    }
  });

  it("a debounced search armed before unmount never fires", async () => {
    vi.useFakeTimers();
    try {
      setTab();
      vi.mocked(invoke).mockResolvedValue([]);
      const { unmount } = render(<SearchPanel />);

      fireEvent.change(screen.getByPlaceholderText("Search..."), {
        target: { value: "Print" },
      });
      unmount();

      await act(async () => {
        vi.advanceTimersByTime(600);
      });

      expect(invoke).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
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

    await act(async () => {
      usePdfStore.setState({ activeTabId: "tab-b" });
      rerender(<SearchPanel />);
    });

    vi.mocked(invoke).mockClear();

    await act(async () => {
      fireEvent.click(screen.getByTitle("Match case"));
      await new Promise((r) => setTimeout(r, 50));
    });

    // The search must run against the tab now in front, never the one left
    // behind. `doSearch` reads the current docId, so this was never actually
    // at risk — the guard that used to swallow this toggle entirely was
    // protecting against something that could not happen, at the cost of a
    // button that did nothing once per tab switch.
    const calls = vi
      .mocked(invoke)
      .mock.calls.filter((call) => call[0] === "search_document");
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toMatchObject({ docId: "doc-b", matchCase: true });
    expect(
      calls.some((c) => (c[1] as Record<string, unknown>)["docId"] === "doc-a"),
    ).toBe(false);
  });
});
