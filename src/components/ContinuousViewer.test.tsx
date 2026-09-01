import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, act } from "@testing-library/react";
import { ContinuousViewer, fitZoom } from "./ContinuousViewer";
import { usePdfStore } from "../store/usePdfStore";
import type { PageDimension, TabState } from "../store/usePdfStore";

// The page slots themselves are irrelevant here — the wrapper divs carrying
// `data-page` belong to ContinuousViewer, and rendering real slots would drag
// in pdfium invokes and canvas APIs jsdom does not have.
vi.mock("./PageSlot", () => ({ PageSlot: () => null }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

/** The live fake observer, so a test can ask what it is actually watching. */
let observed: Set<Element>;

class FakeIntersectionObserver {
  constructor(_cb: IntersectionObserverCallback, _opts?: IntersectionObserverInit) {}
  observe(el: Element) {
    observed.add(el);
  }
  unobserve(el: Element) {
    observed.delete(el);
  }
  disconnect() {
    observed.clear();
  }
  takeRecords() {
    return [];
  }
}

/** jsdom reports every element as 0x0, which makes any fit zoom degenerate. */
const CONTAINER_WIDTH = 1562;
const CONTAINER_HEIGHT = 900;
/** Kept in step with SCROLLBAR_ALLOWANCE / the ::-webkit-scrollbar width. */
const SCROLLBAR = 10;

/** Fires the container's ResizeObserver, as a scrollbar toggling would. */
let resized: (() => void) | null;
/** What the container currently reports; a test can shrink it mid-run. */
let clientBox: { width: number; height: number };

function makeTab(o: Partial<TabState> = {}): TabState {
  return {
    id: "tab-1", docId: "doc-1", fileName: "t.pdf", filePath: "C:\\t.pdf",
    pageCount: 3, pageDimensions: [{ width: 200, height: 200 }], currentPage: 1,
    scrollTop: 0, zoom: 100, zoomMode: "numeric", displayMode: "normal",
    searchQuery: "", searchResults: [], searchResultIndex: -1,
    metadataDirty: false, isDirty: false, loading: false, pagesVersion: 0,
    contentEpoch: 0, sidebarScrollPage: 1, ocrEpoch: 0, ...o,
  } as TabState;
}

function setTab(o: Partial<TabState> = {}) {
  usePdfStore.setState({ tabs: [makeTab(o)], activeTabId: "tab-1" });
}

const tab = () => usePdfStore.getState().tabs[0];
const slotsInDom = (c: HTMLElement) => Array.from(c.querySelectorAll("[data-page]"));

/**
 * Asserts the observer is watching exactly `els` — **by identity**.
 *
 * Comparing element arrays with `toEqual` looks equivalent and is not: it
 * compares DOM nodes structurally, and a remounted slot is structurally
 * identical to the one it replaced. Written that way these tests passed
 * against the very bug they exist to catch. `Set.has` compares references,
 * which is the whole question here.
 */
function expectObserving(els: Element[]) {
  expect(observed.size).toBe(els.length);
  for (const el of els) {
    expect(observed.has(el)).toBe(true);
  }
}

beforeEach(() => {
  observed = new Set();
  resized = null;
  // Starts equal to the offset box: no scrollbar showing.
  clientBox = { width: CONTAINER_WIDTH, height: CONTAINER_HEIGHT };
  vi.stubGlobal("IntersectionObserver", FakeIntersectionObserver);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      constructor(cb: () => void) {
        resized = cb;
      }
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  // The offset box spans the scrollbar gutter, so it does not move when a
  // scrollbar appears; the client box does.
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(CONTAINER_WIDTH);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(CONTAINER_HEIGHT);
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockImplementation(() => clientBox.width);
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockImplementation(() => clientBox.height);
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("fit modes on a document with mixed page sizes", () => {
  // An upright Letter page and the same page rotated. Their widths are what
  // the two zoom levels in issue #125 came from.
  const UPRIGHT: PageDimension = { width: 612, height: 792 };
  const ROTATED: PageDimension = { width: 792, height: 612 };
  // floor((1562 - 10 - 32) / 792 * 100): the offset box, less a reserved
  // scrollbar gutter, less padding, against the widest page.
  const FITTED = 191;

  /**
   * **The bug in issue #125.** Zoom was computed from the current page's
   * width while the IntersectionObserver derived the current page from the
   * layout that zoom produced. Scrolling onto a page of a different width
   * flipped the zoom, which flipped the current page back, forever — the
   * reported 193% <-> 250% oscillation, which was very hard to escape.
   *
   * Changing the current page must therefore leave the zoom alone. Before the
   * fix this settled on 250% for the upright page and 193% for the rotated
   * one; now it stays on the document-wide fit whatever page is in view.
   */
  it("does not change zoom when the current page changes", () => {
    setTab({
      zoomMode: "fit-width",
      pageCount: 2,
      pageDimensions: [UPRIGHT, ROTATED],
      currentPage: 1,
    });
    render(<ContinuousViewer />);

    expect(tab().zoom).toBe(FITTED);

    // Scroll onto the rotated page, as the observer would.
    act(() => usePdfStore.getState().updateTab("tab-1", { currentPage: 2 }));
    expect(tab().zoom).toBe(FITTED);

    // ...and back again. A loop would have shown a different value by now.
    act(() => usePdfStore.getState().updateTab("tab-1", { currentPage: 1 }));
    expect(tab().zoom).toBe(FITTED);
  });

  /**
   * The widest page is the one that has to fit, whichever page the document
   * opens on — fitting the first page would let a wider one overflow.
   */
  it("fits the widest page even when it is not the current page", () => {
    setTab({
      zoomMode: "fit-width",
      pageCount: 2,
      pageDimensions: [UPRIGHT, ROTATED],
      currentPage: 1,
    });
    render(<ContinuousViewer />);

    expect(tab().zoom).toBe(FITTED);
    // The widest page fits inside the box even with a scrollbar showing.
    expect((ROTATED.width * tab().zoom) / 100).toBeLessThanOrEqual(
      CONTAINER_WIDTH - SCROLLBAR - 32,
    );
  });

  /**
   * "Fit to height" had the identical loop, computed from the current page's
   * *height* instead of its width — and it was left untested when the width
   * case was fixed. Rotating a page changes its height just as it changes its
   * width, so this oscillated between 110% and 142% on the same document.
   */
  it("does not change zoom when the current page changes, in fit-page", () => {
    setTab({
      zoomMode: "fit-page",
      pageCount: 2,
      pageDimensions: [UPRIGHT, ROTATED],
      currentPage: 1,
    });
    render(<ContinuousViewer />);

    const fitted = Math.floor(((CONTAINER_HEIGHT - SCROLLBAR - 32) / UPRIGHT.height) * 100);
    expect(tab().zoom).toBe(fitted);

    act(() => usePdfStore.getState().updateTab("tab-1", { currentPage: 2 }));
    expect(tab().zoom).toBe(fitted);

    act(() => usePdfStore.getState().updateTab("tab-1", { currentPage: 1 }));
    expect(tab().zoom).toBe(fitted);
  });

  it("fits the tallest page even when it is not the current page", () => {
    // Current page is the *shorter* one, so fitting it would zoom in further.
    setTab({ zoomMode: "fit-page", pageCount: 2, pageDimensions: [ROTATED, UPRIGHT], currentPage: 1 });
    render(<ContinuousViewer />);

    const avail = CONTAINER_HEIGHT - SCROLLBAR - 32;
    const fitTallest = Math.floor((avail / UPRIGHT.height) * 100);
    const fitCurrent = Math.floor((avail / ROTATED.height) * 100);
    expect(fitTallest).not.toBe(fitCurrent); // the two must be distinguishable
    expect(tab().zoom).toBe(fitTallest);
  });

  it("still resolves the one-shot open zoom to 90% of the document fit", () => {
    setTab({
      zoomMode: "fit-width-90",
      pageCount: 2,
      pageDimensions: [UPRIGHT, ROTATED],
    });
    render(<ContinuousViewer />);

    expect(tab().zoom).toBe(Math.floor(((CONTAINER_WIDTH - SCROLLBAR - 32) / 792) * 100 * 0.9));
    // One-shot: it hands the tab back to numeric so it never refits (issue #38).
    expect(tab().zoomMode).toBe("numeric");
  });
});

describe("fit zoom against a toggling scrollbar (issue #126)", () => {
  const ROTATED: PageDimension = { width: 792, height: 612 };

  /**
   * **The loop.** `clientHeight` shrinks when a horizontal scrollbar appears,
   * and the zoom computed from it is what decides whether that scrollbar
   * appears at all — so measuring it makes the fit's input depend on its own
   * output. The ResizeObserver closes the circuit: fit, overflow, scrollbar,
   * smaller box, smaller fit, no overflow, no scrollbar, round again.
   *
   * Driven here the way it really happens: shrink the client box by a
   * scrollbar's width, leave the offset box alone, and fire the observer. The
   * zoom must not budge. jsdom has no real scrollbars, so this pins the
   * structure — the measurement no longer depends on the scrollbar — rather
   * than the symptom.
   */
  it("does not move when a scrollbar shrinks the client box", () => {
    setTab({ zoomMode: "fit-page", pageCount: 1, pageDimensions: [ROTATED] });
    render(<ContinuousViewer />);
    const settled = tab().zoom;

    act(() => {
      clientBox = { width: CONTAINER_WIDTH, height: CONTAINER_HEIGHT - SCROLLBAR };
      resized?.();
    });
    expect(tab().zoom).toBe(settled);

    // ...and back, as it would if the smaller zoom removed the overflow.
    act(() => {
      clientBox = { width: CONTAINER_WIDTH, height: CONTAINER_HEIGHT };
      resized?.();
    });
    expect(tab().zoom).toBe(settled);
  });

  it("does not move when a vertical scrollbar shrinks the client box, in fit-width", () => {
    setTab({ zoomMode: "fit-width", pageCount: 1, pageDimensions: [ROTATED] });
    render(<ContinuousViewer />);
    const settled = tab().zoom;

    act(() => {
      clientBox = { width: CONTAINER_WIDTH - SCROLLBAR, height: CONTAINER_HEIGHT };
      resized?.();
    });
    expect(tab().zoom).toBe(settled);
  });

  /**
   * A fit that overflows the box is not a fit — and an overflow is what
   * summons the scrollbar the reservation exists to absorb. Rounding up by
   * half a percent was enough to do it, so this covers the arithmetic across
   * a spread of sizes rather than one worked example.
   */
  it("never fits a page larger than the box it was given", () => {
    const PADDING = 32;
    for (const w of [200, 612, 792, 1000]) {
      for (const h of [200, 612, 792, 1000]) {
        for (const box of [500, 800, 1562, 2000]) {
          const dim = { width: w, height: h };
          const wide = fitZoom("fit-width", dim, box, box, PADDING)!;
          // A clamp floor of 10% can legitimately overflow a tiny box; the
          // fit itself must never be the reason.
          if (wide > 10 && wide < 400) {
            expect((w * wide) / 100).toBeLessThanOrEqual(box - PADDING);
          }
          const tall = fitZoom("fit-page", dim, box, box, PADDING)!;
          if (tall > 10 && tall < 400) {
            expect((h * tall) / 100).toBeLessThanOrEqual(box - PADDING);
          }
        }
      }
    }
  });
});

describe("page observation across a page operation", () => {
  /**
   * **The second bug in issue #125.** A slot's key folds in `pagesVersion`, so
   * any page operation remounts every wrapper as a fresh node. Observation was
   * a one-off `querySelectorAll` sweep whose effect did not depend on
   * `pagesVersion`, so the observer kept watching the detached nodes. The
   * current page then stopped tracking the scroll, the render window froze
   * where it was, and every page beyond it stayed a blank placeholder.
   *
   * The invariant that cannot go stale: the observer watches exactly the
   * elements currently in the DOM.
   */
  it("observes the remounted slots after a page operation", () => {
    setTab({ pageCount: 4, pageDimensions: Array(4).fill({ width: 200, height: 200 }) });
    const { container } = render(<ContinuousViewer />);

    const before = slotsInDom(container);
    expect(before).toHaveLength(4);
    expectObserving(before);

    // A rotate/delete/merge bumps pagesVersion, remounting every slot.
    act(() => usePdfStore.getState().updateTab("tab-1", { pagesVersion: 1 }));

    const after = slotsInDom(container);
    expect(after).toHaveLength(4);
    // Sanity: these really are new nodes, so the check below means something.
    expect(after.some((el) => before.includes(el))).toBe(false);
    expectObserving(after);
  });

  it("releases a slot's element when the page count shrinks", () => {
    setTab({ pageCount: 3, pageDimensions: Array(3).fill({ width: 200, height: 200 }) });
    const { container } = render(<ContinuousViewer />);
    expect(observed.size).toBe(3);

    act(() =>
      usePdfStore.getState().updateTab("tab-1", {
        pageCount: 2,
        pageDimensions: Array(2).fill({ width: 200, height: 200 }),
      }),
    );

    expectObserving(slotsInDom(container));
  });
});
