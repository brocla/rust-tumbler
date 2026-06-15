import { useEffect, useRef, useCallback, useMemo } from "react";
import { usePdfStore } from "../store/usePdfStore";
import { PageSlot } from "./PageSlot";

// Floor for the render radius, used before the container has been measured
// and as a sane minimum at high zoom where few pages are visible.
const MIN_RENDER_RADIUS = 2;
// Extra pages to render past each edge of the viewport, so scrolling doesn't
// immediately reveal placeholders.
const RENDER_MARGIN_PAGES = 1;
const PAGE_GAP = 16;

export function ContinuousViewer() {
  const activeTab = usePdfStore((s) =>
    s.tabs.find((t) => t.id === s.activeTabId),
  );
  const updateTab = usePdfStore((s) => s.updateTab);
  const containerRef = useRef<HTMLDivElement>(null);
  const pageRefsMap = useRef<Map<number, HTMLDivElement>>(new Map());
  const suppressObserver = useRef(false);
  // Set by the IntersectionObserver when it updates currentPage from scroll
  // position, so the jump-to-page effect below can tell "the user scrolled
  // here" apart from "the user asked to go to this page" (toolbar, search,
  // thumbnails, PageUp/Down) and only scrollIntoView for the latter.
  const lastObserverPage = useRef<number | null>(null);
  // Mirrors currentPage for the IntersectionObserver callback below, so that
  // callback can read the latest value without making currentPage a
  // dependency of that effect. Recreating the observer on every currentPage
  // change would re-`observe()` every slot, which immediately re-evaluates
  // ratios and can override an explicit page change (e.g. a thumbnail click)
  // before the user ever sees it take effect.
  const currentPageRef = useRef(1);

  const docId = activeTab?.docId ?? "";
  const pageCount = activeTab?.pageCount ?? 0;
  const pageDimensions = activeTab?.pageDimensions ?? [];
  const currentPage = activeTab?.currentPage ?? 1;
  const zoom = activeTab?.zoom ?? 100;
  const displayMode = activeTab?.displayMode ?? "normal";
  const tabId = activeTab?.id ?? "";
  const searchResults = activeTab?.searchResults ?? [];
  const searchResultIndex = activeTab?.searchResultIndex ?? -1;

  // Build per-page highlight data: { rects, activeIndex } for each page
  const pageHighlights = useMemo(() => {
    const map = new Map<number, { rects: { x: number; y: number; width: number; height: number }[]; rectStartIndex: number }>();
    let globalIdx = 0;
    for (const result of searchResults) {
      map.set(result.page, { rects: result.rects, rectStartIndex: globalIdx });
      globalIdx += result.rects.length;
    }
    return map;
  }, [searchResults]);

  // Track which pages are in the render window. The radius is sized so the
  // window covers however many pages actually fit in the viewport at the
  // current zoom — at low zoom many pages can be visible at once, so a fixed
  // radius leaves pages near the edges of the viewport as unrendered
  // placeholders.
  const isInRenderWindow = useCallback(
    (pageNum: number) => {
      const container = containerRef.current;
      const avgPageHeight =
        pageDimensions.length > 0
          ? (pageDimensions.reduce((sum, d) => sum + d.height, 0) / pageDimensions.length) *
              (zoom / 100) +
            PAGE_GAP
          : 0;

      let radius = MIN_RENDER_RADIUS;
      if (container && avgPageHeight > 0) {
        const visiblePages = Math.ceil(container.clientHeight / avgPageHeight);
        radius = Math.max(MIN_RENDER_RADIUS, visiblePages + RENDER_MARGIN_PAGES);
      }

      return Math.abs(pageNum - currentPage) <= radius;
    },
    [currentPage, pageDimensions, zoom],
  );

  useEffect(() => {
    currentPageRef.current = currentPage;
  }, [currentPage]);

  // IntersectionObserver to track the most-visible page
  useEffect(() => {
    const container = containerRef.current;
    if (!container || pageCount === 0) return;

    const observer = new IntersectionObserver(
      (entries) => {
        if (suppressObserver.current) return;

        let bestPage = currentPageRef.current;
        let bestRatio = 0;

        for (const entry of entries) {
          const pageNum = parseInt(
            (entry.target as HTMLElement).dataset.page ?? "0",
            10,
          );
          if (pageNum > 0 && entry.intersectionRatio > bestRatio) {
            bestRatio = entry.intersectionRatio;
            bestPage = pageNum;
          }
        }

        if (bestRatio > 0 && bestPage !== currentPageRef.current && tabId) {
          lastObserverPage.current = bestPage;
          updateTab(tabId, { currentPage: bestPage });
        }
      },
      {
        root: container,
        threshold: [0, 0.25, 0.5, 0.75, 1.0],
      },
    );

    // Observe all page slots
    const slots = container.querySelectorAll("[data-page]");
    slots.forEach((slot) => observer.observe(slot));

    return () => observer.disconnect();
  }, [pageCount, tabId, updateTab, zoom]);

  // Jump to page when currentPage changes via toolbar/keyboard/search/thumbnails.
  // Skip changes that came from the scroll-driven IntersectionObserver above —
  // those reflect where the user already is, and re-centering them with
  // scrollIntoView would fight the user's scroll gesture.
  useEffect(() => {
    if (lastObserverPage.current === currentPage) {
      lastObserverPage.current = null;
      return;
    }

    if (!containerRef.current) return;

    const slot = pageRefsMap.current.get(currentPage);
    if (!slot) return;

    const container = containerRef.current;
    const containerRect = container.getBoundingClientRect();
    const slotRect = slot.getBoundingClientRect();

    // Only jump if the target page is not significantly visible
    const visibleTop = Math.max(slotRect.top, containerRect.top);
    const visibleBottom = Math.min(slotRect.bottom, containerRect.bottom);
    const visibleHeight = Math.max(0, visibleBottom - visibleTop);
    const visibleRatio = visibleHeight / slotRect.height;

    if (visibleRatio < 0.3) {
      suppressObserver.current = true;
      slot.scrollIntoView({ behavior: "smooth", block: "start" });
      setTimeout(() => {
        suppressObserver.current = false;
      }, 500);
    }
  }, [currentPage]);

  // Save/restore scroll position when switching tabs
  useEffect(() => {
    const container = containerRef.current;
    if (!container || !activeTab) return;

    // Restore scroll position
    if (activeTab.scrollTop > 0) {
      container.scrollTop = activeTab.scrollTop;
    }
  }, [tabId]); // Only on tab switch

  // Save scroll position on scroll
  const handleScroll = useCallback(() => {
    const container = containerRef.current;
    if (!container || !tabId) return;
    updateTab(tabId, { scrollTop: container.scrollTop });
  }, [tabId, updateTab]);

  // Ctrl+Scroll wheel zoom
  const handleWheel = useCallback(
    (e: WheelEvent) => {
      if (!e.ctrlKey || !activeTab) return;
      e.preventDefault();

      const delta = e.deltaY > 0 ? -12 : 12;
      const newZoom = Math.max(10, Math.min(400, activeTab.zoom + delta));
      if (newZoom !== activeTab.zoom) {
        updateTab(tabId, { zoom: newZoom, zoomMode: "numeric" });
      }
    },
    [activeTab, tabId, updateTab],
  );

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    container.addEventListener("wheel", handleWheel, { passive: false });
    return () => container.removeEventListener("wheel", handleWheel);
  }, [handleWheel]);

  // Keyboard shortcuts
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (!activeTab) return;

      // Don't capture when typing in an input
      const target = e.target as HTMLElement;
      if (target.tagName === "INPUT" || target.tagName === "SELECT") return;

      if (e.key === "PageDown") {
        e.preventDefault();
        if (currentPage < pageCount) {
          updateTab(tabId, { currentPage: currentPage + 1 });
        }
      } else if (e.key === "PageUp") {
        e.preventDefault();
        if (currentPage > 1) {
          updateTab(tabId, { currentPage: currentPage - 1 });
        }
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [activeTab, currentPage, pageCount, tabId, updateTab]);

  // Register page slot refs
  const setPageRef = useCallback(
    (pageNum: number) => (el: HTMLDivElement | null) => {
      if (el) {
        pageRefsMap.current.set(pageNum, el);
      } else {
        pageRefsMap.current.delete(pageNum);
      }
    },
    [],
  );

  if (!activeTab || pageCount === 0) return null;

  return (
    <div
      ref={containerRef}
      className="continuous-viewer"
      onScroll={handleScroll}
    >
      {Array.from({ length: pageCount }, (_, i) => {
        const pageNum = i + 1;
        const dim = pageDimensions[i];
        return (
          <div
            key={pageNum}
            ref={setPageRef(pageNum)}
            data-page={pageNum}
            className="page-slot-wrapper"
            style={{ marginBottom: i < pageCount - 1 ? PAGE_GAP : 0 }}
          >
            <PageSlot
              docId={docId}
              pageNumber={pageNum}
              pageWidth={dim.width}
              pageHeight={dim.height}
              zoom={zoom}
              isInRenderWindow={isInRenderWindow(pageNum)}
              displayMode={displayMode}
              highlightRects={pageHighlights.get(pageNum)?.rects ?? []}
              activeHighlightIndex={
                searchResultIndex >= 0 && pageHighlights.has(pageNum)
                  ? searchResultIndex - (pageHighlights.get(pageNum)?.rectStartIndex ?? 0)
                  : -1
              }
            />
          </div>
        );
      })}
    </div>
  );
}
