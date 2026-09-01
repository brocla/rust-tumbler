import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, fireEvent } from "@testing-library/react";
import { TypewriterLayer } from "./TypewriterLayer";
import { usePdfStore } from "../store/usePdfStore";
import type { TabState, TypewriterAnnot } from "../store/usePdfStore";

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

function makeAnnot(overrides: Partial<TypewriterAnnot> = {}): TypewriterAnnot {
  return {
    id: "note-1",
    page: 1,
    x: 10,
    y: 20,
    width: 100,
    height: 30,
    rotation: 0,
    text: "Hello",
    fontFamily: "Helvetica",
    bold: false,
    italic: false,
    fontSize: 12,
    color: [0, 0, 0],
    ...overrides,
  };
}

function stubRect(el: HTMLElement, size: number) {
  vi.spyOn(el, "getBoundingClientRect").mockReturnValue({
    left: 0, top: 0, right: size, bottom: size, width: size, height: size,
    x: 0, y: 0, toJSON: () => ({}),
  });
}

describe("TypewriterLayer", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue(undefined);
    usePdfStore.setState({
      tabs: [makeTab()],
      activeTabId: "tab-1",
      typewriterMode: false,
      activeTypewriterId: null,
      typewriterStyle: {
        fontFamily: "Helvetica",
        bold: false,
        italic: false,
        fontSize: 12,
        color: [0, 0, 0],
      },
    });
  });

  it("renders nothing when the page has no notes and the tool is disarmed", () => {
    const { container } = render(<TypewriterLayer docId="doc-1" pageNumber={1} zoom={100} />);
    expect(container.firstChild).toBeNull();
  });

  it("draws this page's notes scaled by zoom, skipping other pages", () => {
    usePdfStore.getState().setTypewriterAnnots("doc-1", [
      makeAnnot({ id: "a", page: 1, x: 10, y: 20 }),
      makeAnnot({ id: "b", page: 2 }),
    ]);
    const { container } = render(<TypewriterLayer docId="doc-1" pageNumber={1} zoom={150} />);
    const notes = container.querySelectorAll(".typewriter-note");
    expect(notes).toHaveLength(1);
    expect((notes[0] as HTMLElement).style.left).toBe("15px"); // 10 × 1.5
    expect((notes[0] as HTMLElement).style.top).toBe("30px"); // 20 × 1.5
  });

  it("clicking empty space while armed places a new note and activates it", () => {
    usePdfStore.setState({ typewriterMode: true });
    const { getByTestId } = render(<TypewriterLayer docId="doc-1" pageNumber={1} zoom={100} />);
    const layer = getByTestId("typewriter-layer-1");
    stubRect(layer, 200);

    fireEvent.mouseDown(layer, { clientX: 40, clientY: 50, button: 0 });

    const notes = usePdfStore.getState().tabs[0].typewriterAnnots!;
    expect(notes).toHaveLength(1);
    expect(notes[0].x).toBe(40);
    expect(notes[0].y).toBe(50);
    expect(usePdfStore.getState().activeTypewriterId).toBe(notes[0].id);
  });

  it("typing in the active note updates the store", () => {
    usePdfStore.getState().setTypewriterAnnots("doc-1", [makeAnnot({ text: "" })]);
    usePdfStore.setState({ activeTypewriterId: "note-1" });
    const { container } = render(<TypewriterLayer docId="doc-1" pageNumber={1} zoom={100} />);

    const textarea = container.querySelector(".typewriter-input") as HTMLTextAreaElement;
    expect(textarea).toBeTruthy();
    fireEvent.change(textarea, { target: { value: "Typed" } });

    expect(usePdfStore.getState().tabs[0].typewriterAnnots![0].text).toBe("Typed");
  });

  it("double-clicking a note activates it for editing (armed)", () => {
    usePdfStore.setState({ typewriterMode: true });
    usePdfStore.getState().setTypewriterAnnots("doc-1", [makeAnnot()]);
    const { getByTestId } = render(<TypewriterLayer docId="doc-1" pageNumber={1} zoom={100} />);

    fireEvent.doubleClick(getByTestId("typewriter-note-note-1"));
    expect(usePdfStore.getState().activeTypewriterId).toBe("note-1");
  });

  it("double-clicking a committed note re-activates it via hit-test when disarmed", () => {
    usePdfStore.setState({ typewriterMode: false, activeTypewriterId: null });
    usePdfStore.getState().setTypewriterAnnots("doc-1", [
      makeAnnot({ x: 10, y: 20, width: 100, height: 30 }),
    ]);
    const { getByTestId } = render(<TypewriterLayer docId="doc-1" pageNumber={1} zoom={100} />);
    stubRect(getByTestId("typewriter-layer-1"), 200);

    // A point inside the note box (x 10–110, y 20–50) at scale 1.
    fireEvent.dblClick(window, { clientX: 30, clientY: 30 });
    expect(usePdfStore.getState().activeTypewriterId).toBe("note-1");
  });

  it("deleting the active note removes it and commits", () => {
    usePdfStore.getState().setTypewriterAnnots("doc-1", [makeAnnot()]);
    usePdfStore.setState({ activeTypewriterId: "note-1" });
    const { container } = render(<TypewriterLayer docId="doc-1" pageNumber={1} zoom={100} />);

    fireEvent.click(container.querySelector(".typewriter-delete")!);

    expect(usePdfStore.getState().tabs[0].typewriterAnnots).toEqual([]);
    expect(usePdfStore.getState().activeTypewriterId).toBeNull();
    expect(invoke).toHaveBeenCalledWith("apply_typewriter", expect.objectContaining({ docId: "doc-1" }));
  });

  describe("a note turned by a page rotation (issue #124)", () => {
    // 100x30 note at (10, 20), turned a quarter: it covers 30 wide by 100
    // tall from that point, with its own box centred inside that footprint.
    // left = 10 + (30 - 100)/2 = -25, top = 20 + (100 - 30)/2 = 55.
    const turned = () => makeAnnot({ x: 10, y: 20, width: 100, height: 30, rotation: 90 });

    /**
     * Notes are drawn entirely by this overlay — the page render leaves
     * annotations off — so a note the file has turned still appeared upright
     * until the overlay learned to turn it. That is what "text does not rotate
     * with the page, but ink does" was: ink is in the bitmap, notes are over
     * it.
     */
    it("draws the note rotated, at its own dimensions", () => {
      usePdfStore.getState().setTypewriterAnnots("doc-1", [turned()]);
      const { container } = render(<TypewriterLayer docId="doc-1" pageNumber={1} zoom={100} />);

      const note = container.querySelector(".typewriter-note") as HTMLElement;
      expect(note.style.transform).toBe("rotate(90deg)");
      // Its own box, so the text still wraps the way it was typed.
      expect(note.style.width).toBe("100px");
      expect(note.style.height).toBe("30px");
      // Centred in the footprint it covers, so it sits where the note is.
      expect(note.style.left).toBe("-25px");
      expect(note.style.top).toBe("55px");
    });

    it("leaves an upright note untransformed", () => {
      usePdfStore.getState().setTypewriterAnnots("doc-1", [makeAnnot({ x: 10, y: 20 })]);
      const { container } = render(<TypewriterLayer docId="doc-1" pageNumber={1} zoom={100} />);

      const note = container.querySelector(".typewriter-note") as HTMLElement;
      expect(note.style.transform).toBe("");
      expect(note.style.left).toBe("10px");
      expect(note.style.top).toBe("20px");
    });

    /** Re-editing hit-tests the area the note covers, not its own box. */
    it("hit-tests the turned footprint, not the unturned box", () => {
      usePdfStore.setState({ typewriterMode: false, activeTypewriterId: null });
      usePdfStore.getState().setTypewriterAnnots("doc-1", [turned()]);
      const { getByTestId } = render(<TypewriterLayer docId="doc-1" pageNumber={1} zoom={100} />);
      stubRect(getByTestId("typewriter-layer-1"), 200);

      // (20, 90) is inside the turned footprint (x 10–40, y 20–120) and
      // outside the unturned box (x 10–110, y 20–50).
      fireEvent.dblClick(window, { clientX: 20, clientY: 90 });
      expect(usePdfStore.getState().activeTypewriterId).toBe("note-1");
    });

    it("does not hit-test where only the unturned box would reach", () => {
      usePdfStore.setState({ typewriterMode: false, activeTypewriterId: null });
      usePdfStore.getState().setTypewriterAnnots("doc-1", [turned()]);
      const { getByTestId } = render(<TypewriterLayer docId="doc-1" pageNumber={1} zoom={100} />);
      stubRect(getByTestId("typewriter-layer-1"), 200);

      // (90, 30) is inside the unturned box but outside the turned footprint.
      fireEvent.dblClick(window, { clientX: 90, clientY: 30 });
      expect(usePdfStore.getState().activeTypewriterId).toBeNull();
    });

    /**
     * The resize handle drags the note's corner *as drawn*. On a note turned
     * 90°, dragging down the screen runs along the note's own width, so an
     * unmapped delta grows the wrong axis and the handle fights the pointer.
     */
    it("resizes along the note's own axes, not the screen's", () => {
      usePdfStore.setState({ typewriterMode: true, activeTypewriterId: "note-1" });
      usePdfStore.getState().setTypewriterAnnots("doc-1", [turned()]);
      const { container } = render(<TypewriterLayer docId="doc-1" pageNumber={1} zoom={100} />);

      const handle = container.querySelector(".typewriter-resize") as HTMLElement;
      fireEvent.mouseDown(handle, { clientX: 0, clientY: 0 });
      // Drag 40px down the screen: on a 90° note that is +40 along its width.
      fireEvent.mouseMove(window, { clientX: 0, clientY: 40 });
      fireEvent.mouseUp(window);

      const note = usePdfStore.getState().tabs[0].typewriterAnnots![0];
      expect(note.width).toBe(140);
      expect(note.height).toBe(30);
    });
  });

  it("clicking away commits and drops an empty note", () => {
    usePdfStore.getState().setTypewriterAnnots("doc-1", [makeAnnot({ text: "" })]);
    usePdfStore.setState({ activeTypewriterId: "note-1" });
    render(<TypewriterLayer docId="doc-1" pageNumber={1} zoom={100} />);

    // A click outside any note box (bare document body) deactivates.
    fireEvent.mouseDown(document.body);

    expect(usePdfStore.getState().activeTypewriterId).toBeNull();
    expect(usePdfStore.getState().tabs[0].typewriterAnnots).toEqual([]);
  });
});
