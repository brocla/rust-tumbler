import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ChevronUp, ChevronDown } from "lucide-react";
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
  const [resultPage, setResultPage] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout>>();

  const docId = activeTab?.docId ?? "";
  const tabId = activeTab?.id ?? "";
  const searchResults = activeTab?.searchResults ?? [];

  const totalMatches = searchResults.reduce((sum, r) => sum + r.rects.length, 0);
  const resultIndex = activeTab?.searchResultIndex ?? -1;

  const doSearch = useCallback(
    async (q: string) => {
      if (!tabId || !docId) return;

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
        console.error("Search failed:", err);
      } finally {
        setSearching(false);
      }
    },
    [tabId, docId, updateTab],
  );

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

  // Focus and select input when panel opens
  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

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

      {searching && (
        <div className="search-status">Searching...</div>
      )}

      {!searching && query.length > 0 && (
        <div className="search-status">
          {totalMatches === 0
            ? "No matches found"
            : `${resultIndex + 1} of ${totalMatches} matches on ${searchResults.length} pages`}
        </div>
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
                {result.rects.length} {result.rects.length === 1 ? "match" : "matches"}
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
