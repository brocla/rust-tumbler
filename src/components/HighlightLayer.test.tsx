import { describe, it, expect } from "vitest";
import { render } from "@testing-library/react";
import { HighlightLayer, highlightBox } from "./HighlightLayer";

describe("highlightBox", () => {
  it("pads the rect by a constant on every side", () => {
    const box = highlightBox({ x: 100, y: 50, width: 40, height: 20 }, 1);

    // 3px each side.
    expect(box.left).toBeCloseTo(97);
    expect(box.top).toBeCloseTo(47);
    expect(box.width).toBeCloseTo(46);
    expect(box.height).toBeCloseTo(26);
  });

  it("pads in screen pixels, not PDF points, so low zoom is not shrunk too", () => {
    const rect = { x: 100, y: 50, width: 40, height: 20 };
    const half = highlightBox(rect, 0.5);
    const full = highlightBox(rect, 1);

    // The rect halves (20x10) but the padding does not: 20+6 and 10+6.
    expect(full.width - 40).toBeCloseTo(6);
    expect(half.width - 20).toBeCloseTo(6);
  });

  it("floors the box so a thin line stays findable when zoomed out", () => {
    // A 5pt line at 30% zoom is 1.5px tall — invisible even once padded.
    const box = highlightBox({ x: 0, y: 100, width: 60, height: 5 }, 0.3);

    expect(box.height).toBe(14);
    expect(box.width).toBeGreaterThanOrEqual(6);
  });

  it("grows about the centre so the box stays over its text", () => {
    const rect = { x: 100, y: 50, width: 40, height: 5 };
    const box = highlightBox(rect, 1);

    const rectCenterY = 52.5;
    expect(box.top + box.height / 2).toBeCloseTo(rectCenterY);
    expect(box.left + box.width / 2).toBeCloseTo(120);
  });
});

describe("HighlightLayer", () => {
  const matches = [
    { rects: [{ x: 0, y: 0, width: 30, height: 10 }] },
    // A match broken across a line break: one result, two boxes.
    {
      rects: [
        { x: 200, y: 0, width: 20, height: 10 },
        { x: 0, y: 20, width: 15, height: 10 },
      ],
    },
  ];

  it("renders one box per rect and marks only the active match", () => {
    const { container } = render(
      <HighlightLayer matches={matches} activeIndex={0} zoom={100} />,
    );

    expect(container.querySelectorAll(".search-highlight")).toHaveLength(3);
    expect(container.querySelectorAll(".search-highlight.active")).toHaveLength(1);
  });

  it("marks every box of a line-wrapped active match", () => {
    const { container } = render(
      <HighlightLayer matches={matches} activeIndex={1} zoom={100} />,
    );

    // Both halves light up: half a highlight would read as a separate result.
    expect(container.querySelectorAll(".search-highlight.active")).toHaveLength(2);
  });

  it("marks nothing active when the active match is on another page", () => {
    const { container } = render(
      <HighlightLayer matches={matches} activeIndex={-1} zoom={100} />,
    );

    expect(container.querySelectorAll(".search-highlight.active")).toHaveLength(0);
  });

  /**
   * The pulse is a CSS animation, and an animation only replays if the element
   * is remounted — which is what the active index folded into the `key` buys.
   * Nothing else in the DOM would show this, so a "simplified" key that drops
   * the index would silently kill the cue for a match already on screen.
   */
  it("remounts a box when it becomes the active match, so its pulse replays", () => {
    const { container, rerender } = render(
      <HighlightLayer matches={matches} activeIndex={0} zoom={100} />,
    );
    // DOM order is match 0's rect, then match 1's two rects.
    const before = container.querySelectorAll(".search-highlight")[1];
    expect(before.classList.contains("active")).toBe(false);

    rerender(<HighlightLayer matches={matches} activeIndex={1} zoom={100} />);

    const after = container.querySelectorAll(".search-highlight")[1];
    expect(after.classList.contains("active")).toBe(true);
    expect(after).not.toBe(before);
  });

  it("renders nothing when there are no matches", () => {
    const { container } = render(
      <HighlightLayer matches={[]} activeIndex={-1} zoom={100} />,
    );

    expect(container.querySelector(".highlight-layer")).toBeNull();
  });
});
