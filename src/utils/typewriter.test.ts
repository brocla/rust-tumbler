import { describe, it, expect, beforeEach, vi } from "vitest";
import { rgbToHex, hexToRgb, fontFamilyCss, newAnnot, commitTypewriter } from "./typewriter";
import { usePdfStore } from "../store/usePdfStore";
import type { TabState } from "../store/usePdfStore";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

function makeTab(overrides: Partial<TabState> = {}): TabState {
  return {
    id: "tab-1",
    docId: "doc-1",
    fileName: "a.pdf",
    filePath: "C:\\a.pdf",
    pageCount: 1,
    pageDimensions: [{ width: 200, height: 200 }],
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

describe("typewriter color helpers", () => {
  it("round-trips hex ↔ rgb", () => {
    expect(rgbToHex([0, 0, 0])).toBe("#000000");
    expect(rgbToHex([1, 1, 1])).toBe("#ffffff");
    expect(hexToRgb("#0000ff")).toEqual([0, 0, 1]);
    // A representative color survives the round trip within rounding.
    const [r, g, b] = hexToRgb(rgbToHex([0.2, 0.4, 0.6]));
    expect(r).toBeCloseTo(0.2, 2);
    expect(g).toBeCloseTo(0.4, 2);
    expect(b).toBeCloseTo(0.6, 2);
  });

  it("falls back to black on a bad hex", () => {
    expect(hexToRgb("nope")).toEqual([0, 0, 0]);
  });

  it("maps families to CSS stacks", () => {
    expect(fontFamilyCss("Times")).toContain("serif");
    expect(fontFamilyCss("Courier")).toContain("monospace");
    expect(fontFamilyCss("Helvetica")).toContain("Helvetica");
  });
});

describe("newAnnot", () => {
  it("builds a note from the point and current style", () => {
    const a = newAnnot(2, 30, 40, {
      fontFamily: "Times",
      bold: true,
      italic: false,
      fontSize: 18,
      color: [1, 0, 0],
    });
    expect(a.page).toBe(2);
    expect(a.x).toBe(30);
    expect(a.y).toBe(40);
    expect(a.text).toBe("");
    expect(a.fontFamily).toBe("Times");
    expect(a.bold).toBe(true);
    expect(a.fontSize).toBe(18);
    expect(a.color).toEqual([1, 0, 0]);
    expect(a.id).toBeTruthy();
  });
});

describe("commitTypewriter", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue(undefined);
    usePdfStore.setState({ tabs: [makeTab()], activeTabId: "tab-1" });
  });

  it("sends only non-empty notes to apply_typewriter", async () => {
    usePdfStore.getState().setTypewriterAnnots("doc-1", [
      { id: "a", page: 1, x: 0, y: 0, width: 100, height: 20, text: "hi", fontFamily: "Helvetica", bold: false, italic: false, fontSize: 12, color: [0, 0, 0] },
      { id: "b", page: 1, x: 0, y: 30, width: 100, height: 20, text: "   ", fontFamily: "Helvetica", bold: false, italic: false, fontSize: 12, color: [0, 0, 0] },
    ]);

    await commitTypewriter("doc-1");

    expect(invoke).toHaveBeenCalledTimes(1);
    const [cmd, args] = invoke.mock.calls[0];
    expect(cmd).toBe("apply_typewriter");
    expect(args.docId).toBe("doc-1");
    expect(args.annots).toHaveLength(1);
    expect(args.annots[0].id).toBe("a");
  });

  it("bumps ocrEpoch so the selectable text layer re-extracts", async () => {
    usePdfStore.getState().setTypewriterAnnots("doc-1", [
      { id: "a", page: 1, x: 0, y: 0, width: 100, height: 20, text: "hi", fontFamily: "Helvetica", bold: false, italic: false, fontSize: 12, color: [0, 0, 0] },
    ]);

    await commitTypewriter("doc-1");

    expect(usePdfStore.getState().tabs[0].ocrEpoch).toBe(1);
  });

  it("surfaces a failure as a notice instead of throwing", async () => {
    invoke.mockRejectedValue("boom");
    usePdfStore.getState().setTypewriterAnnots("doc-1", [
      { id: "a", page: 1, x: 0, y: 0, width: 100, height: 20, text: "hi", fontFamily: "Helvetica", bold: false, italic: false, fontSize: 12, color: [0, 0, 0] },
    ]);

    await expect(commitTypewriter("doc-1")).resolves.toBeUndefined();
    expect(usePdfStore.getState().notice).toContain("boom");
  });
});
