import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, act } from "@testing-library/react";
import { ContinuousViewer } from "./ContinuousViewer";
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
  vi.stubGlobal("IntersectionObserver", FakeIntersectionObserver);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(CONTAINER_WIDTH);
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(900);
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
  // (1562 - 32) / 792 * 100, the widest page — so both pages fit.
  const FITTED = 193;

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
    // The widest page fits exactly; the narrower one has room to spare.
    expect((ROTATED.width * tab().zoom) / 100).toBeLessThanOrEqual(CONTAINER_WIDTH - 32);
  });

  it("still resolves the one-shot open zoom to 90% of the document fit", () => {
    setTab({
      zoomMode: "fit-width-90",
      pageCount: 2,
      pageDimensions: [UPRIGHT, ROTATED],
    });
    render(<ContinuousViewer />);

    expect(tab().zoom).toBe(Math.round(FITTED * 0.9));
    // One-shot: it hands the tab back to numeric so it never refits (issue #38).
    expect(tab().zoomMode).toBe("numeric");
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
