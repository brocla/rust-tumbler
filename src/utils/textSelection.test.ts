import { describe, it, expect } from "vitest";
import { groupIntoLines, reconstructCopyText, GAP_THRESHOLD, type CopyToken } from "./textSelection";

describe("groupIntoLines", () => {
  it("returns line 0 for a single item", () => {
    expect(groupIntoLines([{ y: 10, height: 12 }])).toEqual([0]);
  });

  it("keeps items on the same line when their bounds overlap", () => {
    const items = [
      { y: 10, height: 12 }, // [10, 22]
      { y: 12, height: 8 }, // [12, 20] overlaps with previous
    ];
    expect(groupIntoLines(items)).toEqual([0, 0]);
  });

  it("starts a new line when bounds don't overlap", () => {
    const items = [
      { y: 10, height: 10 }, // [10, 20]
      { y: 20, height: 10 }, // [20, 30] touches but doesn't overlap
    ];
    expect(groupIntoLines(items)).toEqual([0, 1]);
  });

  it("groups a larger-font list number with smaller-font item text on the same line", () => {
    const items = [
      { y: 10, height: 20 }, // list number, larger font: [10, 30]
      { y: 14, height: 12 }, // item text, smaller font: [14, 26], overlaps
    ];
    expect(groupIntoLines(items)).toEqual([0, 0]);
  });

  it("assigns sequential line indices across multiple non-overlapping lines", () => {
    const items = [
      { y: 0, height: 10 },
      { y: 10, height: 10 },
      { y: 20, height: 10 },
    ];
    expect(groupIntoLines(items)).toEqual([0, 1, 2]);
  });
});

describe("reconstructCopyText", () => {
  const span = (overrides: Partial<CopyToken>): CopyToken => ({
    text: "",
    line: null,
    x: 0,
    right: 0,
    fontSize: 0,
    ...overrides,
  });

  it("returns plain text for a single span with no following content", () => {
    const tokens: CopyToken[] = [
      span({ line: "1-0", x: 0, right: 10, fontSize: 12 }),
      { text: "Hello", line: null, x: 0, right: 0, fontSize: 0 },
    ];
    expect(reconstructCopyText(tokens)).toBe("Hello");
  });

  it("inserts a newline between items on different lines", () => {
    const tokens: CopyToken[] = [
      span({ line: "1-0", x: 0, right: 50, fontSize: 12 }),
      { text: "First", line: null, x: 0, right: 0, fontSize: 0 },
      span({ line: "1-1", x: 0, right: 40, fontSize: 12 }),
      { text: "Second", line: null, x: 0, right: 0, fontSize: 0 },
    ];
    expect(reconstructCopyText(tokens)).toBe("First\nSecond");
  });

  it("does not insert a tab when same-line items are adjacent", () => {
    const fontSize = 12;
    const tokens: CopyToken[] = [
      span({ line: "1-0", x: 0, right: 20, fontSize }),
      { text: "1.", line: null, x: 0, right: 0, fontSize: 0 },
      // gap below threshold (0.2 * 12 = 2.4)
      span({ line: "1-0", x: 21, right: 80, fontSize }),
      { text: "Item", line: null, x: 0, right: 0, fontSize: 0 },
    ];
    expect(reconstructCopyText(tokens)).toBe("1.Item");
  });

  it("inserts a tab across a real spatial gap on the same line", () => {
    const fontSize = 12;
    const tokens: CopyToken[] = [
      span({ line: "1-0", x: 0, right: 20, fontSize }),
      { text: "1.", line: null, x: 0, right: 0, fontSize: 0 },
      // gap above threshold (0.2 * 12 = 2.4)
      span({ line: "1-0", x: 35, right: 100, fontSize }),
      { text: "Item", line: null, x: 0, right: 0, fontSize: 0 },
    ];
    expect(reconstructCopyText(tokens)).toBe("1.\tItem");
  });

  it("aligns list numbers of different widths to the same tab stop", () => {
    const fontSize = 12;
    const tabStop = 40;

    const tokensFor = (numberText: string, numberRight: number): CopyToken[] => [
      span({ line: "1-0", x: 0, right: numberRight, fontSize }),
      { text: numberText, line: null, x: 0, right: 0, fontSize: 0 },
      span({ line: "1-0", x: tabStop, right: tabStop + 50, fontSize }),
      { text: "Item", line: null, x: 0, right: 0, fontSize: 0 },
    ];

    // "1." is narrower than "12." but both gaps exceed GAP_THRESHOLD * fontSize
    expect(reconstructCopyText(tokensFor("1.", 8))).toBe("1.\tItem");
    expect(reconstructCopyText(tokensFor("12.", 18))).toBe("12.\tItem");
  });

  it("uses GAP_THRESHOLD as the cutoff between adjacent and gapped runs", () => {
    const fontSize = 10;
    const right = 20;

    const justBelow: CopyToken[] = [
      span({ line: "1-0", x: 0, right, fontSize }),
      { text: "A", line: null, x: 0, right: 0, fontSize: 0 },
      span({ line: "1-0", x: right + fontSize * GAP_THRESHOLD - 0.01, right: 40, fontSize }),
      { text: "B", line: null, x: 0, right: 0, fontSize: 0 },
    ];
    expect(reconstructCopyText(justBelow)).toBe("AB");

    const justAbove: CopyToken[] = [
      span({ line: "1-0", x: 0, right, fontSize }),
      { text: "A", line: null, x: 0, right: 0, fontSize: 0 },
      span({ line: "1-0", x: right + fontSize * GAP_THRESHOLD + 0.01, right: 40, fontSize }),
      { text: "B", line: null, x: 0, right: 0, fontSize: 0 },
    ];
    expect(reconstructCopyText(justAbove)).toBe("A\tB");
  });
});
