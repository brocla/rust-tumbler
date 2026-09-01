import { useEffect, useLayoutEffect, useRef, useCallback, useMemo } from "react";
import { usePdfStore } from "../store/usePdfStore";
import { PageSlot } from "./PageSlot";
import type { PageDimension, SearchMatch, SearchResult, ZoomMode } from "../store/usePdfStore";

/**
 * The dimensions a fit mode measures against: the **widest** width and the
 * **tallest** height across the whole document. Null for an empty document.
 *
 * Not a real page — it is a bound, so "fit width" means *every* page fits the
 * window rather than whichever one happens to be on screen.
 *
 * Fitting the current page instead is what made the zoom oscillate (issue
 * #125). Zoom was computed from `pageDimensions[currentPage - 1]` while the
 * IntersectionObserver derived `currentPage` from the layout that zoom
 * produced, so on a document with two page widths — a rotated page among
 * upright ones, or a landscape page in a portrait document — each value drove
 * the other to the opposite one, forever. Measuring against the document
 * rather than the current page breaks that cycle structurally: zoom no longer
 * depends on where the user is scrolled.
 *
 * The trade-off is deliberate: on a mixed-size document a narrower page now
 * renders smaller than it would if it were fitted on its own.
 */
export function fitReference(dims: PageDimension[]): PageDimension | null {
  if (dims.length === 0) return null;
  return dims.reduce(
    (acc, d) => ({ width: Math.max(acc.width, d.width), height: Math.max(acc.height, d.height) }),
    { width: 0, height: 0 },
  );
}

/**
 * Zoom percentage for a fit mode, clamped to the [10, 400] range. "fit-width"
 * fills the available width; "fit-page" fits the page height; "fit-width-90"
 * is the one-shot open default (issue #38) — 90% of fit-width. Returns null for
 * "numeric" (no fit to compute).
 *
 * `dim` should come from [`fitReference`], not from a single page.
 */
export function fitZoom(
  zoomMode: ZoomMode,
  dim: PageDimension,
  clientWidth: number,
  clientHeight: number,
  padding: number,
): number | null {
  if (zoomMode === "numeric") return null;
  // Floored, not rounded. Rounding up overshoots by up to half a percent,
  // which is enough to push the fitted page past the edge of the box it is
  // supposed to fit inside — bringing in the very scrollbar the caller
  // reserved space for, and re-arming the feedback loop (issue #126). A
  // "fit" that does not fit is not a fit.
  const clamp = (z: number) => Math.max(10, Math.min(400, Math.floor(z)));
  const fitW = ((clientWidth - padding) / dim.width) * 100;
  if (zoomMode === "fit-width") return clamp(fitW);
  if (zoomMode === "fit-width-90") return clamp(fitW * 0.9);
  return clamp(((clientHeight - padding) / dim.height) * 100);
}

/**
 * Returns the {page, rect} to scroll to for a given global *match* index, or
 * null. A match that wraps a line has several rects; the first one (topmost,
 * as the backend orders them) is where the match begins, so that is what the
 * viewer scrolls to.
 */
export function activeSearchRect(
  results: SearchResult[],
  index: number,
): { page: number; rect: { x: number; y: number; width: number; height: number } } | null {
  if (index < 0) return null;
  let offset = 0;
  for (const result of results) {
    const end = offset + result.matches.length;
    if (index >= offset && index < end) {
      const rect = result.matches[index - offset].rects[0];
      return rect ? { page: result.page, rect } : null;
    }
    offset += result.matches.length;
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
// Space held back for a scrollbar, whether or not one is showing. Matches the
// `::-webkit-scrollbar` width in global.css — these are classic, space-taking
// scrollbars rather than overlay ones, so they really do shrink the content
// box. See the fit effect for why the reservation is unconditional.
const SCROLLBAR_ALLOWANCE = 10;

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
  // The live observer, reached by `setPageRef` so a slot is observed as it
  // mounts. See that callback for why sweeping the DOM instead was a bug.
  const observerRef = useRef<IntersectionObserver | null>(null);

  const docId = activeTab?.docId ?? "";
  const pageCount = activeTab?.pageCount ?? 0;
  const pageDimensions = activeTab?.pageDimensions ?? [];
  const currentPage = activeTab?.currentPage ?? 1;
  const zoom = activeTab?.zoom ?? 100;
  const zoomMode = activeTab?.zoomMode ?? "numeric";
  // True only while the one-shot open zoom (issue #38) is still the
  // placeholder value the tab was created with; the layout effect below
  // replaces it with the real fitted zoom and flips the mode to "numeric".
  // The persistent fit modes are excluded deliberately — they never become
  // "numeric", so gating on them would stall rendering forever.
  const fitPending = zoomMode === "fit-width-90";
  const displayMode = activeTab?.displayMode ?? "normal";
  const tabId = activeTab?.id ?? "";
  const pagesVersion = activeTab?.pagesVersion ?? 0;
  const contentEpoch = activeTab?.contentEpoch ?? 0;
  const searchResults = activeTab?.searchResults ?? [];
  const searchResultIndex = activeTab?.searchResultIndex ?? -1;
  const redactedPreview = !!activeTab?.redactPreview;

  // Build per-page highlight data. `matchStartIndex` is where this page's
  // matches begin in the global match numbering, so the active index can be
  // rebased to a page-local one below.
  const pageHighlights = useMemo(() => {
    const map = new Map<number, { matches: SearchMatch[]; matchStartIndex: number }>();
    let globalIdx = 0;
    for (const result of searchResults) {
      map.set(result.page, { matches: result.matches, matchStartIndex: globalIdx });
      globalIdx += result.matches.length;
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

  // Fit-mode: recompute zoom whenever the container or current page changes.
  //
  // A layout effect, not a passive one: React runs layout effects (parent
  // included) before any child's passive effect, so the one-shot open zoom is
  // settled before PageSlot's render effect ever fires. As a passive effect
  // this ran *after* PageSlot's, so every page was first rendered at the
  // placeholder zoom and then again at the fitted one — see the render gate
  // below for why that was expensive rather than merely wasteful.
  useLayoutEffect(() => {
    if (zoomMode === "numeric") return;
    const container = containerRef.current;
    if (!container || pageDimensions.length === 0 || !tabId) return;

    const PADDING = 32; // 16px each side (--page-gap)

    const recalc = () => {
      // Measured against the whole document, never the current page — see
      // fitReference for why that distinction is load-bearing.
      const dim = fitReference(pageDimensions);
      if (!dim) return;
      // Measured against `offset*`, not `client*`, and with a scrollbar's
      // width always held back.
      //
      // `clientHeight` shrinks when a horizontal scrollbar appears — and the
      // zoom computed from it is what decides whether that scrollbar appears
      // at all. Measuring it here would make this function's input depend on
      // its own output, and the ResizeObserver below closes that loop: fit,
      // overflow, scrollbar, smaller box, smaller fit, no overflow, no
      // scrollbar, larger box, and round again (issue #126). `offsetWidth`
      // and `offsetHeight` span the scrollbar gutter, so they do not move
      // when a scrollbar toggles; reserving the gutter unconditionally then
      // keeps the page inside the box in both states. It costs 1-2% of zoom
      // when no scrollbar is showing, and buys a fit that cannot oscillate.
      const zoom = fitZoom(
        zoomMode,
        dim,
        container.offsetWidth - SCROLLBAR_ALLOWANCE,
        container.offsetHeight - SCROLLBAR_ALLOWANCE,
        PADDING,
      );
      if (zoom === null) return;
      // "fit-width-90" is a one-shot open default: apply it once, then hand the
      // tab back to numeric so it no longer refits on resize (issue #38).
      updateTab(tabId, zoomMode === "fit-width-90" ? { zoom, zoomMode: "numeric" } : { zoom });
    };

    recalc();
    const ro = new ResizeObserver(recalc);
    ro.observe(container);
    return () => ro.disconnect();
  }, [zoomMode, pageDimensions, tabId, updateTab]);

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
    // Slots already mounted: refs are assigned before effects run, so this
    // map is current. Anything mounting later is picked up by `setPageRef`.
    observerRef.current = observer;
    for (const el of pageRefsMap.current.values()) observer.observe(el);

    return () => {
      observer.disconnect();
      observerRef.current = null;
      visiblePagesRef.current.clear();
    };
    // `zoom` is deliberately not a dependency. The callback does not read it,
    // and re-running on every zoom change was churn that fed the oscillation
    // in issue #125 — it cleared the visible set and re-observed everything
    // each time the fit effect moved the zoom.
  }, [pageCount, tabId, updateTab]);

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
      if (newZoom !== activeTab.zoom || activeTab.zoomMode !== "numeric") {
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

      // Don't capture when typing in a form field (incl. the multiline
      // textarea) or any editable element.
      const target = e.target as HTMLElement;
      if (
        target.tagName === "INPUT" ||
        target.tagName === "SELECT" ||
        target.tagName === "TEXTAREA" ||
        target.isContentEditable
      ) {
        return;
      }

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

  // Register page slot refs, and keep the IntersectionObserver watching
  // exactly the elements that are mounted.
  //
  // Observation used to be a one-off `querySelectorAll` sweep in the effect
  // above. A slot's key folds in `pagesVersion`, so every page operation
  // remounts all of them as fresh nodes — and the sweep never ran again,
  // leaving the observer watching detached elements. `currentPage` then
  // stopped tracking the scroll, the render window stayed frozen where it
  // was, and every page beyond it rendered as a blank placeholder (issue
  // #125). Registering here cannot go stale: a node is observed when it
  // mounts and released when it unmounts.
  const setPageRef = useCallback(
    (pageNum: number) => (el: HTMLDivElement | null) => {
      const prev = pageRefsMap.current.get(pageNum);
      if (prev && prev !== el) {
        observerRef.current?.unobserve(prev);
        // The detached node will never report again, so drop its entry rather
        // than leave a stale page number to skew the topmost-page pick.
        visiblePagesRef.current.delete(pageNum);
      }
      if (el) {
        pageRefsMap.current.set(pageNum, el);
        observerRef.current?.observe(el);
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
              // Hold off until the open zoom is real: rendering at the
              // placeholder size wastes a full raster per page, and on a
              // large page that oversized bitmap overflows pdfium's image
              // cache, so the render that follows re-decodes from scratch
              // instead of reusing the decode (~1 s per page on a scan).
              isInRenderWindow={!fitPending && isInRenderWindow(pageNum)}
              contentEpoch={contentEpoch}
              displayMode={displayMode}
              highlightMatches={pageHighlights.get(pageNum)?.matches ?? []}
              activeMatchIndex={
                searchResultIndex >= 0 && pageHighlights.has(pageNum)
                  ? searchResultIndex - (pageHighlights.get(pageNum)?.matchStartIndex ?? 0)
                  : -1
              }
              redactedPreview={redactedPreview}
            />
          </div>
        );
      })}
    </div>
  );
}
