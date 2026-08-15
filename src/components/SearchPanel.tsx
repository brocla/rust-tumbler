import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ChevronUp, ChevronDown, CaseSensitive, WholeWord, Regex } from "lucide-react";
import { usePdfStore } from "../store/usePdfStore";
import type { SearchResult } from "../store/usePdfStore";

const RESULTS_PER_PAGE = 20;

export function SearchPanel() {
  const activeTab = usePdfStore((s) =>
    s.tabs.find((t) => t.id === s.activeTabId),
  );
  const updateTab = usePdfStore((s) => s.updateTab);
  const nextSearchResult = usePdfStore((s) => s.nextSearchResult);
  const prevSearchResult = usePdfStore((s) => s.prevSearchResult);

  const [query, setQuery] = useState(activeTab?.searchQuery ?? "");
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [resultPage, setResultPage] = useState(0);
  const [ocrRunning, setOcrRunning] = useState(false);
  const [ocrError, setOcrError] = useState<string | null>(null);
  // Pages already OCR'd this session, so we don't re-prompt for them.
  const [ocrDonePages, setOcrDonePages] = useState<Set<number>>(new Set());
  const [matchCase, setMatchCase] = useState(false);
  const [wholeWord, setWholeWord] = useState(false);
  const [useRegex, setUseRegex] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout>>();
  // Set once, on the first render, and never again: the search-mode effect
  // uses it only to skip its initial (no-op) run. Nothing else may reset it —
  // a ref written by one effect and read by another is how the first toggle
  // after a tab switch used to be swallowed.
  const didMountRef = useRef(false);

  const docId = activeTab?.docId ?? "";
  const tabId = activeTab?.id ?? "";
  const currentPage = activeTab?.currentPage ?? 1;
  const searchResults = activeTab?.searchResults ?? [];

  const totalMatches = searchResults.reduce((sum, r) => sum + r.matches.length, 0);
  const resultIndex = activeTab?.searchResultIndex ?? -1;

  const doSearch = useCallback(
    async (q: string) => {
      if (!tabId || !docId) return;

      setSearchError(null);

      if (q.length === 0) {
        updateTab(tabId, {
          searchQuery: "",
          searchResults: [],
          searchResultIndex: -1,
        });
        return;
      }

      setSearching(true);
      try {
        const results = await invoke<SearchResult[]>("search_document", {
          docId,
          query: q,
          matchCase,
          wholeWord,
          useRegex,
        });
        updateTab(tabId, {
          searchQuery: q,
          searchResults: results,
          searchResultIndex: results.length > 0 ? 0 : -1,
        });
        setResultPage(0);

        // Jump to first result
        if (results.length > 0) {
          updateTab(tabId, { currentPage: results[0].page });
        }
      } catch (err) {
        setSearchError(String(err));
      } finally {
        setSearching(false);
      }
    },
    [tabId, docId, updateTab, matchCase, wholeWord, useRegex],
  );

  const runOcr = useCallback(async () => {
    if (!docId || !query) return;
    setOcrError(null);
    setOcrRunning(true);
    try {
      // Recognize text on the current page; the backend caches it so the
      // re-run search below can find matches via its OCR fallback.
      await invoke("ocr_page", { docId, page: currentPage });
      setOcrDonePages((prev) => new Set(prev).add(currentPage));
      // Refresh the text overlay so the recognized words are selectable/copyable.
      if (activeTab) updateTab(tabId, { ocrEpoch: activeTab.ocrEpoch + 1 });
      await doSearch(query);
    } catch (err) {
      setOcrError(String(err));
    } finally {
      setOcrRunning(false);
    }
  }, [docId, query, currentPage, doSearch, activeTab, tabId, updateTab]);

  const handleInputChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const val = e.target.value;
    setQuery(val);

    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => doSearch(val), 300);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") {
      e.preventDefault();
      if (e.shiftKey) {
        prevSearchResult();
      } else {
        nextSearchResult();
      }
    }
  };

  /**
   * Flips a search mode and hands focus back to the query box.
   *
   * Clicking a button focuses it, and Enter on a focused button activates that
   * button — so after toggling a mode, Enter would re-toggle it instead of
   * stepping to the next match. Returning focus to the input keeps the one
   * key that means "next match" pointed at the thing the user is searching.
   */
  const toggleMode = (set: React.Dispatch<React.SetStateAction<boolean>>) => () => {
    set((v) => !v);
    inputRef.current?.focus();
  };

  // Focus and select input when panel opens
  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  // Drop any armed keystroke search on the way out, so a panel closed
  // mid-typing doesn't fire a search against an unmounted component.
  useEffect(
    () => () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    },
    [],
  );

  // Enter steps to the next match from anywhere on the page, not only from
  // inside the query box. Clicking the document — the natural thing to do
  // while reading a hit — used to leave Enter with nothing bound to it.
  //
  // This listener lives here, so it exists only while the search panel is
  // open. It deliberately declines the event in two cases: anything editable
  // keeps Enter for its own purposes (the query box included — it has its own
  // handler, and taking it here as well would advance twice), and a focused
  // button or link keeps native keyboard activation, which is how a keyboard
  // user operates the mode toggles at all.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Enter" || e.ctrlKey || e.altKey || e.metaKey) return;
      if (totalMatches === 0) return;

      const target = e.target as HTMLElement | null;
      if (
        target &&
        (["INPUT", "TEXTAREA", "SELECT", "BUTTON", "A"].includes(target.tagName) ||
          target.isContentEditable)
      ) {
        return;
      }

      e.preventDefault();
      if (e.shiftKey) {
        prevSearchResult();
      } else {
        nextSearchResult();
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [totalMatches, nextSearchResult, prevSearchResult]);

  // Clear any stale search/OCR state when switching tabs.
  useEffect(() => {
    setSearchError(null);
    setOcrError(null);
    setOcrDonePages(new Set());
  }, [tabId]);

  // Re-run search when a mode toggle changes (if there is an active query).
  // Skip the initial mount — the effect only matters when the user toggles
  // a mode after the component is already showing results.
  useEffect(() => {
    if (!didMountRef.current) {
      didMountRef.current = true;
      return;
    }
    // A recent keystroke may have armed a debounced search that still carries
    // the *previous* flags — it was queued before this toggle. Letting it fire
    // would overwrite the results below with a search in the old mode, a
    // fraction of a second after they appear. Typing "^Print" and reaching for
    // the regex button is exactly that sequence.
    if (debounceRef.current) clearTimeout(debounceRef.current);
    if (query.length > 0) {
      doSearch(query);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [matchCase, wholeWord, useRegex]);

  const handleResultClick = (page: number) => {
    if (!tabId) return;
    updateTab(tabId, { currentPage: page });
  };

  // Pagination
  const totalResultPages = Math.ceil(searchResults.length / RESULTS_PER_PAGE);
  const visibleResults = searchResults.slice(
    resultPage * RESULTS_PER_PAGE,
    (resultPage + 1) * RESULTS_PER_PAGE,
  );

  return (
    <div className="search-panel">
      <div className="search-input-row">
        <input
          ref={inputRef}
          className="search-input"
          type="text"
          placeholder="Search..."
          value={query}
          onChange={handleInputChange}
          onKeyDown={handleKeyDown}
        />
        <button
          className="toolbar-button"
          onClick={prevSearchResult}
          disabled={totalMatches === 0}
          title="Previous (Shift+Enter)"
        >
          <ChevronUp size={16} />
        </button>
        <button
          className="toolbar-button"
          onClick={nextSearchResult}
          disabled={totalMatches === 0}
          title="Next (Enter)"
        >
          <ChevronDown size={16} />
        </button>
      </div>

      <div className="search-mode-row">
        <button
          className={`toolbar-button${matchCase ? " active" : ""}`}
          onClick={toggleMode(setMatchCase)}
          title="Match case"
          aria-pressed={matchCase}
        >
          <CaseSensitive size={16} />
        </button>
        <button
          className={`toolbar-button${wholeWord ? " active" : ""}`}
          onClick={toggleMode(setWholeWord)}
          title="Whole word"
          aria-pressed={wholeWord}
        >
          <WholeWord size={16} />
        </button>
        <button
          className={`toolbar-button${useRegex ? " active" : ""}`}
          onClick={toggleMode(setUseRegex)}
          title="Regular expression"
          aria-pressed={useRegex}
        >
          <Regex size={16} />
        </button>
      </div>

      {searching && (
        <div className="search-status">Searching...</div>
      )}

      {!searching && searchError && (
        <div className="search-status search-error">Search failed: {searchError}</div>
      )}

      {!searching && !searchError && query.length > 0 && (
        <div className="search-status">
          {totalMatches === 0
            ? "No matches found"
            : `${resultIndex + 1} of ${totalMatches} matches on ${searchResults.length} pages`}
        </div>
      )}

      {/* When a search finds nothing, the current page may be a scanned image
          with no text layer. Offer to OCR it so search/copy can work. */}
      {!searching &&
        !searchError &&
        query.length > 0 &&
        totalMatches === 0 &&
        !ocrDonePages.has(currentPage) && (
          <div className="search-ocr-prompt">
            <span>Page {currentPage} may be a scan with no text.</span>
            <button
              className="search-ocr-button"
              onClick={runOcr}
              disabled={ocrRunning}
            >
              {ocrRunning ? "Running OCR…" : "Run OCR on this page"}
            </button>
          </div>
        )}

      {ocrError && (
        <div className="search-status search-error">OCR failed: {ocrError}</div>
      )}

      {visibleResults.length > 0 && (
        <div className="search-results-list">
          {visibleResults.map((result) => (
            <button
              key={result.page}
              className="search-result-item"
              onClick={() => handleResultClick(result.page)}
            >
              Page {result.page}
              <span className="search-result-count">
                {result.matches.length}{" "}
                {result.matches.length === 1 ? "match" : "matches"}
              </span>
            </button>
          ))}
        </div>
      )}

      {totalResultPages > 1 && (
        <div className="search-pagination">
          <button
            className="toolbar-button"
            disabled={resultPage <= 0}
            onClick={() => setResultPage((p) => p - 1)}
          >
            <ChevronUp size={14} />
          </button>
          <span className="page-label">
            {resultPage + 1} / {totalResultPages}
          </span>
          <button
            className="toolbar-button"
            disabled={resultPage >= totalResultPages - 1}
            onClick={() => setResultPage((p) => p + 1)}
          >
            <ChevronDown size={14} />
          </button>
        </div>
      )}
    </div>
  );
}
