import { useEffect, useRef, useCallback, useMemo } from "react";
import { usePdfStore } from "../store/usePdfStore";
import { PageSlot } from "./PageSlot";
import type { SearchResult } from "../store/usePdfStore";

/** Returns the {page, rect} for a given global result index, or null. */
export function activeSearchRect(
  results: SearchResult[],
  index: number,
): { page: number; rect: { x: number; y: number; width: number; height: number } } | null {
  if (index < 0) return null;
  let offset = 0;
  for (const result of results) {
    const end = offset + result.rects.length;
    if (index >= offset && index < end) {
      return { page: result.page, rect: result.rects[index - offset] };
    }
    offset += result.rects.length;
  }
  return null;
}

/**
 * Returns the scrollTop needed to center a rect in the viewport, or null if
 * the rect is already fully visible (no scroll needed).
 */
export function scrollTargetForRect(
  pageSlotOffsetTop: number,
  rect: { y: number; height: number },
  zoom: number,
  scrollTop: number,
  clientHeight: number,
): number | null {
  const scale = zoom / 100;
  const rectTop = pageSlotOffsetTop + rect.y * scale;
  const rectBottom = rectTop + rect.height * scale;
  if (rectTop >= scrollTop && rectBottom <= scrollTop + clientHeight) return null;
  return Math.max(0, (rectTop + rectBottom) / 2 - clientHeight / 2);
}

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
  // Running set of page numbers currently intersecting the viewport. Updated
  // incrementally by the IntersectionObserver (which delivers diffs, not
  // snapshots). Cleared when the observer is recreated.
  const visiblePagesRef = useRef<Set<number>>(new Set());

  const docId = activeTab?.docId ?? "";
  const pageCount = activeTab?.pageCount ?? 0;
  const pageDimensions = activeTab?.pageDimensions ?? [];
  const currentPage = activeTab?.currentPage ?? 1;
  const zoom = activeTab?.zoom ?? 100;
  const displayMode = activeTab?.displayMode ?? "normal";
  const tabId = activeTab?.id ?? "";
  const pagesVersion = activeTab?.pagesVersion ?? 0;
  const contentEpoch = activeTab?.contentEpoch ?? 0;
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

  // IntersectionObserver to track the topmost visible page.
  // We pick the minimum page number among all currently-intersecting pages
  // rather than the page with the highest ratio. This avoids the case where a
  // tall page n is at the top of the viewport but page n+1 (which fits entirely
  // in the remaining space) has a higher ratio and "wins".
  useEffect(() => {
    const container = containerRef.current;
    if (!container || pageCount === 0) return;

    visiblePagesRef.current.clear();

    const observer = new IntersectionObserver(
      (entries) => {
        // Always keep the set current — skipping updates during suppression
        // would leave stale entries that corrupt topPage once suppress lifts.
        for (const entry of entries) {
          const pageNum = parseInt(
            (entry.target as HTMLElement).dataset.page ?? "0",
            10,
          );
          if (pageNum > 0) {
            if (entry.isIntersecting) visiblePagesRef.current.add(pageNum);
            else visiblePagesRef.current.delete(pageNum);
          }
        }

        if (suppressObserver.current) return;

        const visible = visiblePagesRef.current;
        if (visible.size === 0) return;
        const topPage = Math.min(...visible);

        if (topPage !== currentPageRef.current && tabId) {
          lastObserverPage.current = topPage;
          updateTab(tabId, { currentPage: topPage });
        }
      },
      {
        root: container,
        threshold: 0,
      },
    );

    // Observe all page slots
    const slots = container.querySelectorAll("[data-page]");
    slots.forEach((slot) => observer.observe(slot));

    return () => {
      observer.disconnect();
      visiblePagesRef.current.clear();
    };
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
      }, 1000);
    }
  }, [currentPage]);

  // When the active search result changes, scroll so the matched rect is
  // visible — not just the page. This handles the case where the page is
  // zoomed large enough that the match is off-screen even though the page
  // itself is partly in view.
  useEffect(() => {
    const hit = activeSearchRect(searchResults, searchResultIndex);
    if (!hit) return;

    const pageSlot = pageRefsMap.current.get(hit.page);
    const container = containerRef.current;
    if (!pageSlot || !container) return;

    const target = scrollTargetForRect(
      pageSlot.offsetTop,
      hit.rect,
      zoom,
      container.scrollTop,
      container.clientHeight,
    );
    if (target === null) return;

    suppressObserver.current = true;
    container.scrollTo({ top: target, behavior: "smooth" });
    setTimeout(() => {
      suppressObserver.current = false;
    }, 1000);
  }, [searchResultIndex, searchResults, zoom]);

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
            key={`${pageNum}-v${pagesVersion}`}
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
              contentEpoch={contentEpoch}
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
