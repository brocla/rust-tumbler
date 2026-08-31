import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { InkPanel } from "./InkPanel";
import { usePdfStore } from "../store/usePdfStore";
import type { TabState } from "../store/usePdfStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

function makeTab(o: Partial<TabState> = {}): TabState {
  return {
    id: "tab-1", docId: "doc-1", fileName: "t.pdf", filePath: "C:\t.pdf",
    pageCount: 3, pageDimensions: [{ width: 200, height: 200 }], currentPage: 1,
    scrollTop: 0, zoom: 100, zoomMode: "numeric", displayMode: "normal",
    searchQuery: "", searchResults: [], searchResultIndex: -1,
    metadataDirty: false, isDirty: false, loading: false, pagesVersion: 0,
    contentEpoch: 0, sidebarScrollPage: 1, ocrEpoch: 0, ...o,
  } as TabState;
}

function setTab(o: Partial<TabState> = {}) {
  usePdfStore.setState({
    tabs: [makeTab(o)], activeTabId: "tab-1",
    activeSidebarTool: "ink", sidebarWidth: 250, ink: null,
  });
}

const withStrokes = (page = 1) =>
  usePdfStore.setState({
    ink: { docId: "doc-1", page, strokes: [[[1, 1], [2, 2]]], redo: [] },
  });

const inkCalls = () =>
  vi.mocked(invoke).mock.calls.filter((c) => c[0] === "apply_ink");

describe("InkPanel", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    // page_rotation answers 0 (unrotated) unless a test says otherwise.
    vi.mocked(invoke).mockResolvedValue(0 as never);
    setTab();
  });

  it("commits the group when Done is pressed", async () => {
    withStrokes();
    render(<InkPanel />);

    await act(async () => {
      fireEvent.click(screen.getByTitle("Apply the signature to the page"));
    });

    expect(inkCalls()).toHaveLength(1);
    expect(inkCalls()[0][1]).toMatchObject({ docId: "doc-1", page: 1 });
    expect(usePdfStore.getState().ink).toBeNull();
  });

  it("commits the group when the page changes", async () => {
    withStrokes(1);
    const { rerender } = render(<InkPanel />);

    await act(async () => {
      usePdfStore.setState({ tabs: [makeTab({ currentPage: 2 })] });
      rerender(<InkPanel />);
    });

    // The strokes belong to page 1, even though the view has moved to page 2.
    expect(inkCalls()).toHaveLength(1);
    expect(inkCalls()[0][1]).toMatchObject({ page: 1 });
  });

  it("commits the group when the tool closes", async () => {
    withStrokes();
    const { unmount } = render(<InkPanel />);

    await act(async () => {
      unmount();
    });

    expect(inkCalls()).toHaveLength(1);
  });

  it("Esc discards the group instead of committing it", async () => {
    withStrokes();
    render(<InkPanel />);

    await act(async () => {
      fireEvent.keyDown(window, { key: "Escape" });
    });

    expect(usePdfStore.getState().ink).toBeNull();
    expect(inkCalls()).toHaveLength(0);
  });

  it("an empty group commits nothing, so the document stays clean", async () => {
    render(<InkPanel />);

    await act(async () => {
      fireEvent.keyDown(window, { key: "Escape" });
    });
    expect(inkCalls()).toHaveLength(0);
  });

  it("Ctrl+Z undoes a stroke and Ctrl+Y puts it back", async () => {
    withStrokes();
    usePdfStore.getState().inkAddStroke([[9, 9]]);
    render(<InkPanel />);

    fireEvent.keyDown(window, { key: "z", ctrlKey: true });
    expect(usePdfStore.getState().ink!.strokes).toHaveLength(1);

    fireEvent.keyDown(window, { key: "y", ctrlKey: true });
    expect(usePdfStore.getState().ink!.strokes).toHaveLength(2);
  });

  /**
   * The guard that matters: undo while typing belongs to the text field, not
   * to the signature. Without it a stroke would vanish somewhere off-screen.
   */
  it("leaves Ctrl+Z alone when the user is typing", () => {
    withStrokes();
    render(<InkPanel />);

    const field = document.createElement("textarea");
    document.body.appendChild(field);
    fireEvent.keyDown(field, { key: "z", ctrlKey: true });

    expect(usePdfStore.getState().ink!.strokes).toHaveLength(1);
    field.remove();
  });

  it("stops listening for undo once the tool closes", () => {
    withStrokes();
    const { unmount } = render(<InkPanel />);
    unmount();
    withStrokes();

    fireEvent.keyDown(window, { key: "z", ctrlKey: true });

    expect(usePdfStore.getState().ink!.strokes).toHaveLength(1);
  });

  it("refuses to draw on a rotated page rather than misplace the ink", async () => {
    vi.mocked(invoke).mockResolvedValue(90 as never);

    await act(async () => {
      render(<InkPanel />);
    });

    expect(screen.getByText(/rotated 90°/)).toBeInTheDocument();
  });

  it("says plainly that this is not a digital signature", () => {
    render(<InkPanel />);
    expect(screen.getByText(/not a\s+digital signature/)).toBeInTheDocument();
  });
});

/**
 * The group has to reach the buffer before anything else rewrites it, or the
 * strokes are flattened onto a document that has already moved on — or lost
 * entirely. These call sites are easy to add and easy to forget, so the
 * contract is pinned here rather than left to review.
 */
describe("commitOpenInk", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockResolvedValue(undefined as never);
    setTab();
  });

  it("flattens the open group and clears it", async () => {
    withStrokes(2);
    const { commitOpenInk } = await import("../utils/inkCommit");

    await commitOpenInk();

    expect(inkCalls()).toHaveLength(1);
    expect(inkCalls()[0][1]).toMatchObject({ docId: "doc-1", page: 2 });
    expect(usePdfStore.getState().ink).toBeNull();
  });

  it("is a no-op when nothing is pending, so every call site can call it freely", async () => {
    const { commitOpenInk } = await import("../utils/inkCommit");

    await commitOpenInk();
    await commitOpenInk();

    expect(inkCalls()).toHaveLength(0);
  });
});

/**
 * Pending ink is unsaved work the backend has not seen: the buffer is untouched
 * until the group closes, so `isDirty` stays false. Everything that asks "are
 * there unsaved changes?" has to account for it — otherwise Save is greyed out
 * over a drawn signature, and closing the tab throws it away without asking.
 */
describe("pending ink counts as unsaved work", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockResolvedValue(undefined as never);
    setTab();
  });

  it("is reported for the document being drawn on", async () => {
    const { hasPendingInk } = await import("../utils/inkCommit");

    expect(hasPendingInk("doc-1")).toBe(false);
    withStrokes();
    expect(hasPendingInk("doc-1")).toBe(true);
    expect(hasPendingInk("other-doc")).toBe(false);
  });

  it("is not reported for a group with no strokes yet", async () => {
    const { hasPendingInk } = await import("../utils/inkCommit");
    usePdfStore.getState().inkBegin("doc-1", 1);

    expect(hasPendingInk("doc-1")).toBe(false);
  });

  /**
   * A failed commit used to destroy the signature: the group is cleared on the
   * way out, so an error left nothing to retry with and nothing on screen.
   */
  it("puts the strokes back when the commit fails", async () => {
    withStrokes();
    vi.mocked(invoke).mockRejectedValueOnce(new Error("disk on fire") as never);
    const { commitOpenInk } = await import("../utils/inkCommit");

    await expect(commitOpenInk()).rejects.toThrow("disk on fire");

    expect(usePdfStore.getState().ink?.strokes).toHaveLength(1);
  });
});
