import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, fireEvent } from "@testing-library/react";
import { RedactLayer } from "./RedactLayer";
import { usePdfStore } from "../store/usePdfStore";
import type { TabState } from "../store/usePdfStore";

function makeTab(overrides: Partial<TabState> = {}): TabState {
  return {
    id: "tab-1",
    docId: "doc-1",
    fileName: "a.pdf",
    filePath: "C:\\a.pdf",
    pageCount: 2,
    pageDimensions: [
      { width: 200, height: 200 },
      { width: 200, height: 200 },
    ],
    currentPage: 1,
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

describe("RedactLayer", () => {
  beforeEach(() => {
    usePdfStore.setState({
      tabs: [makeTab()],
      activeTabId: "tab-1",
      redactDrawMode: false,
    });
  });

  it("renders nothing when the page has no regions and draw mode is off", () => {
    const { container } = render(<RedactLayer docId="doc-1" pageNumber={1} zoom={100} />);
    expect(container.firstChild).toBeNull();
  });

  it("draws this page's regions scaled by zoom, skipping other pages", () => {
    usePdfStore.getState().addRedactRegions("doc-1", [
      { page: 1, rect: { x: 10, y: 20, width: 50, height: 30 } },
      { page: 2, rect: { x: 0, y: 0, width: 10, height: 10 } },
    ]);
    const { container } = render(<RedactLayer docId="doc-1" pageNumber={1} zoom={150} />);

    const boxes = container.querySelectorAll(".redact-region");
    expect(boxes).toHaveLength(1);
    const style = (boxes[0] as HTMLElement).style;
    expect(style.left).toBe("15px"); // 10 × 1.5
    expect(style.top).toBe("30px"); // 20 × 1.5
    expect(style.width).toBe("75px"); // 50 × 1.5
    expect(style.height).toBe("45px"); // 30 × 1.5
  });

  it("clicking a region removes it", () => {
    usePdfStore.getState().addRedactRegions("doc-1", [
      { page: 1, rect: { x: 10, y: 20, width: 50, height: 30 } },
    ]);
    const { container } = render(<RedactLayer docId="doc-1" pageNumber={1} zoom={100} />);

    fireEvent.click(container.querySelector(".redact-region")!);
    expect(usePdfStore.getState().tabs[0].redactRegions).toEqual([]);
  });

  it("a marquee drag in draw mode adds a region in page points", () => {
    usePdfStore.setState({ redactDrawMode: true });
    const { getByTestId } = render(<RedactLayer docId="doc-1" pageNumber={1} zoom={200} />);
    const layer = getByTestId("redact-layer-1");
    // Zoom 200 → scale 2: the layer is 400×400 CSS px for a 200pt page.
    vi.spyOn(layer, "getBoundingClientRect").mockReturnValue({
      left: 0,
      top: 0,
      right: 400,
      bottom: 400,
      width: 400,
      height: 400,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });

    fireEvent.mouseDown(layer, { clientX: 100, clientY: 60, button: 0 });
    fireEvent.mouseMove(layer, { clientX: 200, clientY: 160 });
    fireEvent.mouseUp(layer);

    const regions = usePdfStore.getState().tabs[0].redactRegions!;
    expect(regions).toHaveLength(1);
    expect(regions[0]).toEqual({
      page: 1,
      rect: { x: 50, y: 30, width: 50, height: 50 }, // CSS px ÷ 2
    });
  });

  it("a degenerate drag adds nothing", () => {
    usePdfStore.setState({ redactDrawMode: true });
    const { getByTestId } = render(<RedactLayer docId="doc-1" pageNumber={1} zoom={100} />);
    const layer = getByTestId("redact-layer-1");
    vi.spyOn(layer, "getBoundingClientRect").mockReturnValue({
      left: 0, top: 0, right: 200, bottom: 200, width: 200, height: 200,
      x: 0, y: 0, toJSON: () => ({}),
    });

    fireEvent.mouseDown(layer, { clientX: 50, clientY: 50, button: 0 });
    fireEvent.mouseUp(layer);

    expect(usePdfStore.getState().tabs[0].redactRegions ?? []).toEqual([]);
  });
});
