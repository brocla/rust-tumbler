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
