import { describe, it, expect, beforeEach } from "vitest";
import { render, fireEvent } from "@testing-library/react";
import { InkLayer } from "./InkLayer";
import { usePdfStore } from "../store/usePdfStore";
import { INK_COLOR } from "../utils/ink";

/**
 * jsdom gives every element a zero-sized rect, so pointer coordinates would
 * all map to the same point. Pin the layer's box so the conversion from client
 * pixels to page points is actually exercised.
 */
function pinLayer(el: Element, left = 100, top = 50) {
  const rect: DOMRect = {
    left, top, x: left, y: top,
    right: left + 400, bottom: top + 400, width: 400, height: 400,
    toJSON: () => ({}),
  };
  el.getBoundingClientRect = () => rect;
}

const draw = (layer: Element, points: [number, number][]) => {
  fireEvent.pointerDown(layer, { button: 0, clientX: points[0][0], clientY: points[0][1] });
  for (const [x, y] of points.slice(1)) {
    fireEvent.pointerMove(layer, { clientX: x, clientY: y });
  }
  fireEvent.pointerUp(layer);
};

describe("InkLayer", () => {
  beforeEach(() => {
    usePdfStore.setState({ ink: null });
  });

  it("records a stroke in page points, not screen pixels", () => {
    const { container } = render(<InkLayer docId="doc-1" pageNumber={2} zoom={200} />);
    const layer = container.querySelector(".ink-layer")!;
    pinLayer(layer);

    // At 200% zoom, 100 screen px from the layer's left edge is 50 page points.
    draw(layer, [[200, 150], [300, 250]]);

    const ink = usePdfStore.getState().ink!;
    expect(ink.docId).toBe("doc-1");
    expect(ink.page).toBe(2);
    expect(ink.strokes).toHaveLength(1);
    expect(ink.strokes[0][0][0]).toBeCloseTo(50);
    expect(ink.strokes[0][0][1]).toBeCloseTo(50);
    expect(ink.strokes[0][1][0]).toBeCloseTo(100);
  });

  it("draws the pending strokes in the agreed blue, scaled to the zoom", () => {
    const { container } = render(<InkLayer docId="doc-1" pageNumber={1} zoom={100} />);
    const layer = container.querySelector(".ink-layer")!;
    pinLayer(layer, 0, 0);
    draw(layer, [[10, 10], [20, 20]]);

    const line = container.querySelector("polyline")!;
    expect(line.getAttribute("stroke")).toBe(INK_COLOR);
    expect(line.getAttribute("points")).toContain("10,10");
  });

  it("keeps each pointer gesture as its own stroke", () => {
    const { container } = render(<InkLayer docId="doc-1" pageNumber={1} zoom={100} />);
    const layer = container.querySelector(".ink-layer")!;
    pinLayer(layer, 0, 0);

    draw(layer, [[0, 0], [5, 5]]);
    draw(layer, [[20, 20], [25, 25]]);

    expect(usePdfStore.getState().ink!.strokes).toHaveLength(2);
  });

  it("shows only the strokes belonging to this page", () => {
    usePdfStore.setState({
      ink: { docId: "doc-1", page: 1, strokes: [[[1, 1], [2, 2]]], redo: [] },
    });
    const { container } = render(<InkLayer docId="doc-1" pageNumber={7} zoom={100} />);

    expect(container.querySelectorAll("polyline")).toHaveLength(0);
  });

  it("ignores non-primary buttons, so a right-click does not start a stroke", () => {
    const { container } = render(<InkLayer docId="doc-1" pageNumber={1} zoom={100} />);
    const layer = container.querySelector(".ink-layer")!;
    pinLayer(layer, 0, 0);

    fireEvent.pointerDown(layer, { button: 2, clientX: 10, clientY: 10 });
    fireEvent.pointerUp(layer);

    expect(usePdfStore.getState().ink).toBeNull();
  });
});

describe("ink group state", () => {
  beforeEach(() => {
    usePdfStore.setState({ ink: null });
    usePdfStore.getState().inkBegin("doc-1", 3);
  });

  const strokes = () => usePdfStore.getState().ink?.strokes ?? [];

  it("undo and redo walk the stroke list", () => {
    const s = usePdfStore.getState();
    s.inkAddStroke([[0, 0]]);
    s.inkAddStroke([[1, 1]]);
    expect(strokes()).toHaveLength(2);

    usePdfStore.getState().inkUndo();
    expect(strokes()).toHaveLength(1);

    usePdfStore.getState().inkRedo();
    expect(strokes()).toHaveLength(2);
  });

  it("a new stroke clears the redo stack, as in any editor", () => {
    usePdfStore.getState().inkAddStroke([[0, 0]]);
    usePdfStore.getState().inkUndo();
    usePdfStore.getState().inkAddStroke([[9, 9]]);
    usePdfStore.getState().inkRedo();

    expect(strokes()).toHaveLength(1);
    expect(strokes()[0][0]).toEqual([9, 9]);
  });

  it("undo and redo stop at the ends instead of throwing", () => {
    usePdfStore.getState().inkUndo();
    usePdfStore.getState().inkRedo();
    expect(strokes()).toHaveLength(0);
  });

  /**
   * Two close triggers can fire together — Done and a page change, say. Taking
   * the group clears it, so the second commit finds nothing and the same
   * strokes cannot be flattened onto the page twice.
   */
  it("taking the group hands it over exactly once", () => {
    usePdfStore.getState().inkAddStroke([[0, 0]]);

    const first = usePdfStore.getState().inkTake();
    expect(first?.strokes).toHaveLength(1);
    expect(first?.page).toBe(3);
    expect(usePdfStore.getState().inkTake()).toBeNull();
  });

  it("an empty group is nothing to commit", () => {
    expect(usePdfStore.getState().inkTake()).toBeNull();
  });

  it("discard throws the group away", () => {
    usePdfStore.getState().inkAddStroke([[0, 0]]);
    usePdfStore.getState().inkDiscard();
    expect(usePdfStore.getState().ink).toBeNull();
  });
});
