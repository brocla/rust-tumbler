import type { RedactRegion } from "../store/usePdfStore";

/**
 * Geometry helpers for redaction (issue #1).
 *
 * Regions live in PDF points with a top-left origin — the coordinate space of
 * `TextRect` on the backend and of the TextLayer/HighlightLayer overlays. The
 * viewer displays pages scaled by `zoom / 100`, so converting a CSS-pixel rect
 * measured against a page element is a straight divide by that scale.
 */

interface Box {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** Ignore selections / drags smaller than this many points in either axis. */
const MIN_REGION_PTS = 2;

/**
 * Converts a CSS-pixel rect (viewport coordinates) into a redaction region on
 * `page`, given the page element's own viewport rect and the current zoom.
 * Returns null when the intersection with the page is degenerate.
 */
export function clientRectToRegion(
  rect: Box,
  pageRect: Box,
  page: number,
  zoom: number,
): RedactRegion | null {
  const scale = zoom / 100;
  if (scale <= 0) return null;

  // Clamp to the page bounds first (a selection rect can bleed into margins).
  const left = Math.max(rect.left, pageRect.left);
  const top = Math.max(rect.top, pageRect.top);
  const right = Math.min(rect.left + rect.width, pageRect.left + pageRect.width);
  const bottom = Math.min(rect.top + rect.height, pageRect.top + pageRect.height);

  const x = (left - pageRect.left) / scale;
  const y = (top - pageRect.top) / scale;
  const width = (right - left) / scale;
  const height = (bottom - top) / scale;
  if (width < MIN_REGION_PTS || height < MIN_REGION_PTS) return null;

  return { page, rect: { x, y, width, height } };
}

/**
 * Converts a marquee drag (two corners, in points already) into a normalized
 * region. Returns null for a degenerate drag.
 */
export function dragToRegion(
  page: number,
  x1: number,
  y1: number,
  x2: number,
  y2: number,
): RedactRegion | null {
  const x = Math.min(x1, x2);
  const y = Math.min(y1, y2);
  const width = Math.abs(x2 - x1);
  const height = Math.abs(y2 - y1);
  if (width < MIN_REGION_PTS || height < MIN_REGION_PTS) return null;
  return { page, rect: { x, y, width, height } };
}

/**
 * Reads the current DOM text selection and converts every selected text-layer
 * box into a redaction region. Walks each `.page-slot-wrapper[data-page]`
 * element and intersects the selection's client rects with it, so a selection
 * spanning multiple pages yields regions on each.
 */
export function selectionToRegions(zoom: number): RedactRegion[] {
  const selection = window.getSelection();
  if (!selection || selection.isCollapsed || selection.rangeCount === 0) return [];

  const clientRects: Box[] = [];
  for (let i = 0; i < selection.rangeCount; i++) {
    const rects = selection.getRangeAt(i).getClientRects();
    for (let j = 0; j < rects.length; j++) {
      const r = rects[j];
      clientRects.push({ left: r.left, top: r.top, width: r.width, height: r.height });
    }
  }

  const regions: RedactRegion[] = [];
  const wrappers = document.querySelectorAll<HTMLElement>(".page-slot-wrapper[data-page]");
  wrappers.forEach((wrapper) => {
    const page = parseInt(wrapper.dataset.page ?? "0", 10);
    if (page <= 0) return;
    const pr = wrapper.getBoundingClientRect();
    const pageRect: Box = { left: pr.left, top: pr.top, width: pr.width, height: pr.height };
    for (const rect of clientRects) {
      const region = clientRectToRegion(rect, pageRect, page, zoom);
      if (region) regions.push(region);
    }
  });
  return regions;
}
