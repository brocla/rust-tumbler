import { describe, it, expect } from "vitest";
import { activeSearchRect, fitZoom, scrollTargetForRect } from "./ContinuousViewer";
import type { SearchResult } from "../store/usePdfStore";

describe("fitZoom", () => {
  // 100x200 page, 1032px-wide container minus 32px padding = 1000px usable.
  // fit-width = 1000 / 100 * 100 = 1000% → clamped to 400.
  const dim = { width: 100, height: 200 };

  it("returns null for numeric mode", () => {
    expect(fitZoom("numeric", dim, 1032, 800, 32)).toBeNull();
  });

  it("fits width to the container", () => {
    // container 232 wide → usable 200 → 200/100*100 = 200%
    expect(fitZoom("fit-width", dim, 232, 800, 32)).toBe(200);
  });

  it("fit-width-90 is 90% of fit-width", () => {
    // fit-width = 200% → 90% = 180%
    expect(fitZoom("fit-width-90", dim, 232, 800, 32)).toBe(180);
  });

  it("fits page height to the container", () => {
    // container 432 tall → usable 400 → 400/200*100 = 200%
    expect(fitZoom("fit-page", dim, 1032, 432, 32)).toBe(200);
  });

  it("clamps above 400", () => {
    expect(fitZoom("fit-width", dim, 1032, 800, 32)).toBe(400);
  });

  it("clamps below 10", () => {
    // usable width 8 → 8/100*100 = 8% → clamped to 10
    expect(fitZoom("fit-width", dim, 40, 800, 32)).toBe(10);
  });
});

describe("activeSearchRect", () => {
  const results: SearchResult[] = [
    { page: 1, rects: [{ x: 0, y: 10, width: 50, height: 12 }, { x: 0, y: 50, width: 50, height: 12 }] },
    { page: 3, rects: [{ x: 0, y: 200, width: 50, height: 12 }] },
  ];

  it("returns null for index -1", () => {
    expect(activeSearchRect(results, -1)).toBeNull();
  });

  it("returns null for empty results", () => {
    expect(activeSearchRect([], 0)).toBeNull();
  });

  it("returns null for out-of-range index", () => {
    expect(activeSearchRect(results, 3)).toBeNull();
  });

  it("returns the first rect on the first page", () => {
    expect(activeSearchRect(results, 0)).toEqual({ page: 1, rect: results[0].rects[0] });
  });

  it("returns the second rect on the first page", () => {
    expect(activeSearchRect(results, 1)).toEqual({ page: 1, rect: results[0].rects[1] });
  });

  it("returns the rect on the second page (index spans across pages)", () => {
    expect(activeSearchRect(results, 2)).toEqual({ page: 3, rect: results[1].rects[0] });
  });
});

describe("scrollTargetForRect", () => {
  // pageSlotOffsetTop=100, rect.y=50, rect.height=20, zoom=100
  // rectTop = 100 + 50 = 150, rectBottom = 170
  const SLOT_TOP = 100;
  const RECT = { y: 50, height: 20 };
  const ZOOM = 100;

  it("returns null when rect is fully visible", () => {
    // visible window: scrollTop=0, clientHeight=400 → visTop=0, visBottom=400
    // rectTop=150, rectBottom=170 — fully inside
    expect(scrollTargetForRect(SLOT_TOP, RECT, ZOOM, 0, 400)).toBeNull();
  });

  it("returns a scroll target when rect is above the viewport", () => {
    // visible window: scrollTop=200, clientHeight=100 → visTop=200, visBottom=300
    // rectTop=150 < 200 → off screen above
    const target = scrollTargetForRect(SLOT_TOP, RECT, ZOOM, 200, 100);
    expect(target).not.toBeNull();
    // center = (150+170)/2 = 160; target = 160 - 100/2 = 110
    expect(target).toBe(110);
  });

  it("returns a scroll target when rect is below the viewport", () => {
    // visible window: scrollTop=0, clientHeight=100 → visTop=0, visBottom=100
    // rectBottom=170 > 100 → off screen below
    const target = scrollTargetForRect(SLOT_TOP, RECT, ZOOM, 0, 100);
    expect(target).not.toBeNull();
    // center = 160; target = 160 - 50 = 110
    expect(target).toBe(110);
  });

  it("returns null when rect near top of page is visible at scrollTop=0", () => {
    // rectTop=5, rectBottom=15; visTop=0, visBottom=400 → fully inside
    expect(scrollTargetForRect(0, { y: 5, height: 10 }, 100, 0, 400)).toBeNull();
  });

  it("clamps negative scroll target to 0", () => {
    // rect near top: slotTop=0, rect.y=2, height=4; center=4; clientHeight=400
    // target = 4 - 200 = -196 → 0
    const target = scrollTargetForRect(0, { y: 2, height: 4 }, 100, 500, 400);
    // scrollTop=500, visBottom=900; rectBottom=6 < 500 → off screen above
    expect(target).toBe(0);
  });

  it("applies zoom scaling to rect position", () => {
    // zoom=200: scale=2; rectTop = 100 + 50*2 = 200; rectBottom = 200 + 20*2 = 240
    // center = 220; clientHeight=100; target = 220 - 50 = 170
    // viewport: scrollTop=0, visBottom=100 → rectTop=200 > 100 → off screen
    const target = scrollTargetForRect(SLOT_TOP, RECT, 200, 0, 100);
    expect(target).toBe(170);
  });
});
