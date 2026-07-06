import { describe, it, expect } from "vitest";
import { clientRectToRegion, dragToRegion } from "./redaction";

describe("clientRectToRegion", () => {
  const pageRect = { left: 100, top: 50, width: 400, height: 400 }; // 200pt page at zoom 200

  it("converts a CSS-pixel rect into page points at the given zoom", () => {
    const region = clientRectToRegion(
      { left: 150, top: 100, width: 100, height: 20 },
      pageRect,
      3,
      200,
    );
    expect(region).toEqual({
      page: 3,
      rect: { x: 25, y: 25, width: 50, height: 10 },
    });
  });

  it("clamps a rect that bleeds past the page bounds", () => {
    const region = clientRectToRegion(
      { left: 0, top: 0, width: 1000, height: 1000 },
      pageRect,
      1,
      200,
    );
    expect(region).toEqual({
      page: 1,
      rect: { x: 0, y: 0, width: 200, height: 200 },
    });
  });

  it("returns null when the intersection with the page is degenerate", () => {
    // Entirely outside the page.
    expect(
      clientRectToRegion({ left: 0, top: 0, width: 50, height: 50 }, pageRect, 1, 200),
    ).toBeNull();
    // A hairline selection artifact.
    expect(
      clientRectToRegion({ left: 150, top: 100, width: 1, height: 20 }, pageRect, 1, 200),
    ).toBeNull();
  });
});

describe("dragToRegion", () => {
  it("normalizes corners dragged in any direction", () => {
    expect(dragToRegion(2, 80, 90, 30, 40)).toEqual({
      page: 2,
      rect: { x: 30, y: 40, width: 50, height: 50 },
    });
  });

  it("rejects a degenerate drag", () => {
    expect(dragToRegion(1, 10, 10, 11, 11)).toBeNull();
  });
});
